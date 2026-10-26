#!/bin/bash
# R7: 失败自动重试（指数退避）真实功能测试（64MB 多段文件，真实「下载中死亡」）
# A: 源第 2 命中后死 + auto_retry=5 → 退避重试 → 源复活 → 续传完成
# B: 同样死亡 + auto_retry=0 → 直达 Failed（有部分进度）
# C: 永久死 + auto_retry=1 → 一次重试预算耗尽 → Failed
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r7-XXXX)
cd $WORK
DPORT=8823
PASS=0; FAIL=0
cleanup() { pkill -f "smart-dl-daemon serve.*$DPORT" 2>/dev/null; kill %1 %2 %3 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1 = [$3]"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }
wait_state_in() { for i in $(seq 1 $3); do S=$(curl -sf $API/tasks/$1 | jqpy "d['state']"); case ",$2," in *",$S,"*) return 0;; esac; sleep 1; done; return 1; }
wait_ready() { for i in $(seq 1 50); do curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$1/revive.bin" && return 0; sleep 0.2; done; return 1; }

VA=9934; VB=9935; VC=9936
# A：die-after=2 —— 就绪探测(1) + add 探测(2) 后死亡，首段即 404 → 重试风暴 → 复活完成
# （注：本地环回单连接即可下完 64MB，多段死亡场景非确定性；段级续传已由 soak kill -9 验证）
node $(dirname "$0")/revive-server.mjs $VA $WORK/ma 2 64 > va.log 2>&1 &
node $(dirname "$0")/revive-server.mjs $VB $WORK/mb 2 64 > vb.log 2>&1 &
node $(dirname "$0")/revive-server.mjs $VC $WORK/mc 2 64 > vc.log 2>&1 &
wait_ready $VA && wait_ready $VB && wait_ready $VC || { echo "FATAL: 复活服务器未就绪"; exit 1; }
cat > config.toml << EOF
[server]
addr = "127.0.0.1:$DPORT"
[download]
dest_root = "$WORK/dl"
fsync_per_segment = false
EOF
(/home/z/smart-downloader/target/debug/smart-dl-daemon serve --config config.toml --addr 127.0.0.1:$DPORT &>/dev/null) &
for i in $(seq 1 50); do curl -sf http://127.0.0.1:$DPORT/health >/dev/null 2>&1 && break; sleep 0.2; done
API=http://127.0.0.1:$DPORT

# --- A: 死源重试 → 复活 → 续传完成 ---
# 就绪探测消耗 1 命中（200）；add 探测 = 命中 2（200）；首段 = 命中 3 起 404 → 下载中死亡
TA=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$VA/revive.bin\",\"auto_retry\":5}" | jqpy "d['task_id']")
echo "A: task=$TA"
sleep 12
SA=$(curl -sf $API/tasks/$TA | jqpy "d['state']")
DA=$(curl -sf $API/tasks/$TA | jqpy "d['done']")
echo "A: 死源 12s 后 state=$SA done=$DA（预算内，不应终态 Failed）"
[ "$SA" != "Failed" ] && { PASS=$((PASS+1)); echo "PASS: A 预算内不终态 Failed"; } || { FAIL=$((FAIL+1)); echo "FAIL: A 过早终态 Failed"; }
touch $WORK/ma  # 管理员「修复」
if wait_state_in $TA Completed 60; then PASS=$((PASS+1)); echo "PASS: A 源复活后重试续传完成"; else FAIL=$((FAIL+1)); echo "FAIL: A 未恢复完成"; fi
DAF=$(curl -sf $API/tasks/$TA | jqpy "d['done']")
assert "A 终态字节数（64MB 全量）" 67108864 "$DAF"

# --- B: auto_retry=0 → 直达 Failed（无 auto_retry 事件、无额外命中）---
TB=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$VB/revive.bin\",\"auto_retry\":0}" | jqpy "d['task_id']")
echo "B: task=$TB"
sleep 12  # 过完可能的退避窗口再验证
SB=$(curl -sf $API/tasks/$TB | jqpy "d['state']")
assert "B 终态 Failed" "Failed" "$SB"
BEV=$(curl -sf $API/tasks/$TB/logs | jqpy "len([e for e in d['events'] if e['op']=='auto_retry'])")
assert "B 无 auto_retry 事件" 0 "$BEV"

# --- C: auto_retry=1 → 恰 1 次重试（事件铁证）→ 终态 Failed ---
TC=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$VC/revive.bin\",\"auto_retry\":1}" | jqpy "d['task_id']")
echo "C: task=$TC"
sleep 12
SC=$(curl -sf $API/tasks/$TC | jqpy "d['state']")
assert "C 终态 Failed" "Failed" "$SC"
CEV=$(curl -sf $API/tasks/$TC/logs | jqpy "len([e for e in d['events'] if e['op']=='auto_retry'])")
assert "C 恰 1 次 auto_retry 事件" 1 "$CEV"
CDT=$(curl -sf $API/tasks/$TC/logs | jqpy "[e for e in d['events'] if e['op']=='auto_retry'][0]['detail'][:20]")
case "$CDT" in *"1/1"*) PASS=$((PASS+1)); echo "PASS: C 重试计数 1/1";; *) FAIL=$((FAIL+1)); echo "FAIL: C 重试计数异常: $CDT";; esac

echo "--- 死源期 404 命中（重试真实发生证据）---"
echo "A: $(rg -c '404' va.log || echo 0)  B: $(rg -c '404' vb.log || echo 0)  C: $(rg -c '404' vc.log || echo 0)"
HA=$(rg -c '404' va.log || echo 0); HC=$(rg -c '404' vc.log || echo 0)
[ "$HA" -ge 2 ] && { PASS=$((PASS+1)); echo "PASS: A 重试请求真实发生"; } || { FAIL=$((FAIL+1)); echo "FAIL: A 404 过少"; }
[ "$HC" -ge 2 ] && { PASS=$((PASS+1)); echo "PASS: C 重试再探测发生（≥2 次 404）"; } || { FAIL=$((FAIL+1)); echo "FAIL: C 404 过少（$HC）"; }

echo "===== R7 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
