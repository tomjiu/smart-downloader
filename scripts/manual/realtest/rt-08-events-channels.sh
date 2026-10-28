#!/bin/bash
# R11: 事件三通道（SSE / WS / REST 补拉）真实功能测试
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r11-XXXX)
cd $WORK
DPORT=8827; FPORT=9917
PASS=0; FAIL=0
cleanup() { pkill -f "smart-dl-daemon serve.*$DPORT" 2>/dev/null; kill %1 2>/dev/null; pkill -f "node.*r11ws" 2>/dev/null; wait 2>/dev/null; }
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

# --- A. SSE 流：订阅 → 添加任务 → 完成 → 流里收到事件 ---
timeout 12 curl -sN "$API/events/stream" > sse.log 2>&1 &
SSE_PID=$!
sleep 1  # SSE 建连
T1=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/small.bin\"}" | jqpy "d['task_id']")
sleep 4  # 下载完成 + 事件落流
# REST 补拉对照
curl -sf "$API/events?limit=100" > rest-events.json
wait $SSE_PID 2>/dev/null

SSE_N=$(rg -c "^data:" sse.log || echo 0)
echo "SSE 收到 data 帧数: $SSE_N"
[ "$SSE_N" -ge 1 ] && { PASS=$((PASS+1)); echo "PASS: SSE 有数据帧"; } || { FAIL=$((FAIL+1)); echo "FAIL: SSE 无数据帧"; head -5 sse.log; }
# SSE 帧应含 task 相关事件（Added/Speed/Progress/Completed 任一）
SSE_T=$(rg -c "$T1" sse.log || echo 0)
[ "$SSE_T" -ge 1 ] && { PASS=$((PASS+1)); echo "PASS: SSE 帧含目标任务 id"; } || { FAIL=$((FAIL+1)); echo "FAIL: SSE 未见目标任务"; }
# keep-alive 注释行存在（保活契约）
SSE_KA=$(rg -c "^: " sse.log || echo 0)
echo "SSE keep-alive 注释行: $SSE_KA"

# --- B. REST 补拉：事件列表 + type 过滤 ---
RE_N=$(cat rest-events.json | jqpy "len(d['events'])")
echo "REST 事件数: $RE_N"
[ "$RE_N" -ge 1 ] && { PASS=$((PASS+1)); echo "PASS: REST 补拉非空"; } || { FAIL=$((FAIL+1)); echo "FAIL: REST 补拉为空"; }
RE_C=$(cat rest-events.json | jqpy "len([e for e in d['events'] if e.get('event',{}).get('type')=='Completed' or 'Completed' in str(e)])")
echo "REST Completed 事件: $RE_C"
[ "$RE_C" -ge 1 ] && { PASS=$((PASS+1)); echo "PASS: REST 含 Completed 事件"; } || { FAIL=$((FAIL+1)); echo "FAIL: REST 无 Completed"; head -c 400 rest-events.json; echo; }
# type 过滤（真实参数名为 type，serde rename；event_type 会被静默忽略）
FILT=$(curl -sf "$API/events?type=completed&limit=10" | jqpy "len(d['events'])")
assert "type=completed 过滤恰 1 条" 1 "$FILT"
FILTM=$(curl -sf "$API/events?type=state_changed,completed&limit=10" | jqpy "len(d['events'])")
assert "type 多值过滤 3 条" 3 "$FILTM"
FILT_ALL=$(curl -s -o /dev/null -w "%{http_code}" "$API/events?type=Bogus")
assert "非法 type HTTP 400" 400 "$FILT_ALL"

# --- C. WS：node 原生 WebSocket 客户端 ---
cat > r11ws.mjs << 'EOF'
const ws = new WebSocket(`ws://127.0.0.1:${process.argv[2]}/ws`);
let n = 0; let sawTask = false; const tid = process.argv[3];
const timer = setTimeout(() => { console.log(`WS_FRAMES=${n} WS_SAW_TASK=${sawTask}`); process.exit(0); }, 9000);
ws.onmessage = (ev) => {
  n++;
  if (String(ev.data).includes(tid)) sawTask = true;
  if (n === 1) {
    // 收到首帧后触发一次任务操作（resume 一个暂停任务产生事件）
    fetch(`http://127.0.0.1:${process.argv[2]}/tasks`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ url: `http://127.0.0.1:${process.argv[4]}/small.bin?w=1` }) }).catch(() => {});
  }
};
ws.onopen = () => console.log('WS_OPEN');
ws.onerror = (e) => { console.log('WS_ERR'); };
EOF
node r11ws.mjs $DPORT $T1 $FPORT > ws.log 2>&1
cat ws.log
WSO=$(rg -c "WS_OPEN" ws.log || echo 0)
assert "WS 建连" 1 "$WSO"
WSF=$(rg -o "WS_FRAMES=[0-9]+" ws.log | cut -d= -f2)
[ -n "$WSF" ] && [ "$WSF" -ge 1 ] && { PASS=$((PASS+1)); echo "PASS: WS 收到 $WSF 帧"; } || { FAIL=$((FAIL+1)); echo "FAIL: WS 帧数异常: $WSF"; }

echo "===== R11 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
