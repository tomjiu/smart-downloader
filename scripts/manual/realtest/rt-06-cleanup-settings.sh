#!/bin/bash
# R9: 已完成任务自动清扫（E20）真实功能测试（实时可验证面）
# 验证：配置解析 / PUT /settings 热更立即生效 + 持久化 / 刚完成任务不被清扫（保留语义正确）
# 注：10min 清扫周期 + 天级保留的完整链路由单测（cleanup_tests）+ nightly soak 覆盖
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r9-XXXX)
cd $WORK
DPORT=8825; FPORT=9915
PASS=0; FAIL=0
cleanup() { pkill -f "smart-dl-daemon serve.*$DPORT" 2>/dev/null; kill %1 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1 = [$3]"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }

node $REPO/ui/e2e/file-server.mjs $FPORT &>/dev/null &
for i in $(seq 1 50); do curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$FPORT/small.bin" && break; sleep 0.2; done
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

# 1. GET /settings：cleanup 域可见，默认禁用
D0=$(curl -sf $API/settings | jqpy "d['cleanup']['auto_remove_completed_days']")
assert "初始 days（默认禁用）" 0 "$D0"
K0=$(curl -sf $API/settings | jqpy "d['cleanup']['auto_remove_keep_data']")
assert "初始 keep_data（默认保留）" "True" "$K0"

# 2. PUT /settings 热更：days=1 + keep_data=false
PUT=$(curl -sf -X PUT $API/settings -H 'content-type: application/json' -d '{"cleanup":{"auto_remove_completed_days":1,"auto_remove_keep_data":false}}')
echo "settings PUT resp: $(echo $PUT | head -c 300)"
APP=$(echo "$PUT" | jqpy "'cleanup' in (d.get('applied') or d)")
echo "applied 内容: $(echo "$PUT" | jqpy "d.get('applied')")"

# 3. 热更立即生效（GET 回读）
D1=$(curl -sf $API/settings | jqpy "d['cleanup']['auto_remove_completed_days']")
assert "热更后 days" 1 "$D1"
K1=$(curl -sf $API/settings | jqpy "d['cleanup']['auto_remove_keep_data']")
assert "热更后 keep_data" "False" "$K1"

# 4. 持久化：config.toml 回写
CFGD=$(rg -c "auto_remove_completed_days = 1" config.toml 2>/dev/null || echo 0)
assert "config.toml 持久化回写" 1 "$CFGD"

# 5. 保留语义：刚完成的任务不被清扫（龄期 1 天未到）
T1=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/small.bin\"}" | jqpy "d['task_id']")
for i in $(seq 1 40); do S=$(curl -sf $API/tasks/$T1 | jqpy "d['state']"); [ "$S" = "Completed" ] && break; sleep 0.5; done
assert "任务完成" "Completed" "$S"
sleep 3
TC=$(curl -sf "$API/tasks" | jqpy "len(d)")
assert "刚完成任务仍在列表（保留期内不清扫）" 1 "$TC"

# 6. keep_data=false 语义下数据也不该被删（清扫未到期根本不触发）
[ -f "$WORK/dl/small.bin" ] && { PASS=$((PASS+1)); echo "PASS: 数据文件仍在（清扫未触发）"; } || { FAIL=$((FAIL+1)); echo "FAIL: 数据异常缺失"; }

echo "===== R9 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
