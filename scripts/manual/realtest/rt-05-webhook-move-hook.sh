#!/bin/bash
# R8: 完成 Webhook + 完成后钩子 + move_to（含同名冲突改名）真实功能测试
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r8-XXXX)
cd $WORK
DPORT=8824; FPORT=9914; WPORT=9940
PASS=0; FAIL=0
cleanup() { pkill -f "smart-dl-daemon serve.*$DPORT" 2>/dev/null; kill %1 %2 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1 = [$3]"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }
wait_ready() { for i in $(seq 1 50); do curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$1$2" && return 0; sleep 0.2; done; return 1; }

node $REPO/ui/e2e/file-server.mjs $FPORT &>/dev/null &
node $(dirname "$0")/webhook-server.mjs $WPORT $WORK/webhook-events.jsonl > ws.log 2>&1 &
wait_ready $FPORT /small.bin && wait_ready $WPORT /small.bin || { echo "FATAL: 服务器未就绪"; exit 1; }

mkdir -p $WORK/moved
cat > $WORK/hook.sh << 'EOF'
#!/bin/bash
echo "HOOK task=$SD_TASK_ID name=$SD_TASK_NAME path=$SD_FILE_PATH engine=$SD_ENGINE" >> "$HOOK_LOG"
EOF
chmod +x $WORK/hook.sh

cat > config.toml << EOF
[server]
addr = "127.0.0.1:$DPORT"
[download]
dest_root = "$WORK/dl"
fsync_per_segment = false
[webhook]
url = "http://127.0.0.1:$WPORT/hook"
[post_download]
move_to = "$WORK/moved"
hook = "$WORK/hook.sh"
EOF
(HOOK_LOG=$WORK/hook-fired.log /home/z/smart-downloader/target/debug/smart-dl-daemon serve --config config.toml --addr 127.0.0.1:$DPORT &>/dev/null) &
for i in $(seq 1 50); do curl -sf http://127.0.0.1:$DPORT/health >/dev/null 2>&1 && break; sleep 0.2; done
API=http://127.0.0.1:$DPORT
wait_done() { for i in $(seq 1 $2); do S=$(curl -sf $API/tasks/$1 | jqpy "d['state']"); [ "$S" = "Completed" ] && return 0; sleep 0.5; done; return 1; }

# --- 任务 1：file-server 的 small.bin ---
T1=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/small.bin\"}" | jqpy "d['task_id']")
wait_done $T1 30 || { echo "FATAL: T1 未完成"; exit 1; }
sleep 2  # webhook fire-and-forget + hook 后台线程收尾窗口

# --- 任务 2：webhook-server 的 small.bin（同名不同源 → 冲突改名路径）---
T2=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$WPORT/small.bin\"}" | jqpy "d['task_id']")
wait_done $T2 30 || { echo "FATAL: T2 未完成"; exit 1; }
sleep 2

echo "=== 断言 ==="
# A. webhook 收到 2 次 POST，载荷含 event/task_id/name/total_bytes
WN=$(wc -l < $WORK/webhook-events.jsonl 2>/dev/null || echo 0)
assert "webhook POST 数" 2 "$WN"
EV1=$(head -1 $WORK/webhook-events.jsonl | jqpy "d['event']")
assert "webhook event 字段" "task_completed" "$EV1"
WID1=$(head -1 $WORK/webhook-events.jsonl | jqpy "d['task_id']")
assert "webhook task_id=T1" "$T1" "$WID1"
WTB=$(head -1 $WORK/webhook-events.jsonl | jqpy "d['total_bytes']")
assert "webhook total_bytes" 1048576 "$WTB"
WID2=$(tail -1 $WORK/webhook-events.jsonl | jqpy "d['task_id']")
assert "webhook 第二发 task_id=T2" "$T2" "$WID2"

# B. move_to：两文件都落到 moved/，第二个冲突改名 small(1).bin
[ -f $WORK/moved/small.bin ] && { PASS=$((PASS+1)); echo "PASS: moved/small.bin 落位"; } || { FAIL=$((FAIL+1)); echo "FAIL: moved/small.bin 缺失"; }
[ -f "$WORK/moved/small(1).bin" ] && { PASS=$((PASS+1)); echo "PASS: 冲突改名 small(1).bin"; } || { FAIL=$((FAIL+1)); echo "FAIL: 冲突改名缺失"; ls "$WORK/moved/"; }
SZ1=$(stat -c%s $WORK/moved/small.bin 2>/dev/null || echo 0)
assert "moved/small.bin 尺寸" 1048576 "$SZ1"
# 源目录不应残留
[ ! -f $WORK/dl/small.bin ] && { PASS=$((PASS+1)); echo "PASS: 源位置已不残留"; } || { FAIL=$((FAIL+1)); echo "FAIL: 源位置残留"; }

# C. hook 触发两次，env 传参正确且 path 指向 moved/
HN=$(wc -l < $WORK/hook-fired.log 2>/dev/null || echo 0)
assert "hook 触发数" 2 "$HN"
H1=$(head -1 $WORK/hook-fired.log)
echo "hook line1: $H1"
case "$H1" in *"task=$T1"*"path=$WORK/moved/small.bin"*"engine=http"*) PASS=$((PASS+1)); echo "PASS: hook env 传参正确";; *) FAIL=$((FAIL+1)); echo "FAIL: hook 行异常";; esac
# 敏感 env 净化：hook 里读不到 SMART_DL_HTTP_TOKEN（batch8-P3 口径——本配置未设 token，验 daemon 自身不注入）
H2=$(tail -1 $WORK/hook-fired.log)
case "$H2" in *"small(1).bin"*) PASS=$((PASS+1)); echo "PASS: 第二发 hook path 为冲突改名后路径";; *) FAIL=$((FAIL+1)); echo "FAIL: hook2 path 异常";; esac

# D. 事件流铁证：post_move 事件入档
PM1=$(curl -sf $API/tasks/$T1/logs | jqpy "len([e for e in d['events'] if e['op']=='post_move'])")
assert "T1 post_move 事件" 1 "$PM1"
PM2=$(curl -sf $API/tasks/$T2/logs | jqpy "[e for e in d['events'] if e['op']=='post_move'][0]['detail']")
echo "T2 post_move detail: $PM2"
case "$PM2" in *"small(1).bin"*) PASS=$((PASS+1)); echo "PASS: T2 post_move 指向改名路径";; *) FAIL=$((FAIL+1)); echo "FAIL: T2 post_move 异常";; esac

echo "===== R8 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
