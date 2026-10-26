#!/bin/bash
# R10: 并发队列配额（S1）真实功能测试
# max_active_http=2 + 6 个长任务 → 恰 2 Downloading / 4 Queued；暂停一个 → FIFO 递补
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r10-XXXX)
cd $WORK
DPORT=8826; FPORT=9916
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
[queue]
max_active_http = 2
EOF
(/home/z/smart-downloader/target/debug/smart-dl-daemon serve --config config.toml --addr 127.0.0.1:$DPORT &>/dev/null) &
for i in $(seq 1 50); do curl -sf http://127.0.0.1:$DPORT/health >/dev/null 2>&1 && break; sleep 0.2; done
API=http://127.0.0.1:$DPORT

# 6 个长任务（?v= 变体绕过 URL 去重；128MB@4MiB/s 单连接 ~32s 足够观察）
for v in 1 2 3 4 5 6; do
  curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/limit.bin?v=$v\",\"down_kb_s\":128}" | jqpy "d['task_id']" >/dev/null
done
sleep 5
STATS=$(curl -sf "$API/tasks?limit=50")
ND=$(echo "$STATS" | jqpy "len([t for t in d if t['state']=='Downloading'])")
NQ=$(echo "$STATS" | jqpy "len([t for t in d if t['state']=='Queued'])")
NT=$(echo "$STATS" | jqpy "len(d)")
assert "任务总数" 6 "$NT"
assert "Downloading 数（配额 2）" 2 "$ND"
assert "Queued 数（排队 4）" 4 "$NQ"

# FIFO 递补：暂停一个在传任务 → 4s 内 Queued 变 3、Downloading 回 2
FIRST=$(echo "$STATS" | jqpy "[t for t in d if t['state']=='Downloading'][0]['task_id']")
curl -sf -X POST $API/tasks/$FIRST/pause >/dev/null
sleep 4
S2=$(curl -sf "$API/tasks?limit=50")
ND2=$(echo "$S2" | jqpy "len([t for t in d if t['state']=='Downloading'])")
NQ2=$(echo "$S2" | jqpy "len([t for t in d if t['state']=='Queued'])")
NP=$(echo "$S2" | jqpy "len([t for t in d if t['state']=='Paused'])")
assert "递补后 Downloading 仍 2" 2 "$ND2"
assert "递补后 Queued 3" 3 "$NQ2"
assert "Paused 1" 1 "$NP"

# 完成递补路径：把 4 个在传/排队任务全暂停 → 配额全释放 → Queued 全部激活
for tid in $(echo "$S2" | jqpy "','.join([t['task_id'] for t in d if t['state']=='Downloading'])" | tr ',' ' '); do
  curl -sf -X POST $API/tasks/$tid/pause >/dev/null
done
sleep 4
S3=$(curl -sf "$API/tasks?limit=50")
ND3=$(echo "$S3" | jqpy "len([t for t in d if t['state']=='Downloading'])")
NQ3=$(echo "$S3" | jqpy "len([t for t in d if t['state']=='Queued'])")
NP3=$(echo "$S3" | jqpy "len([t for t in d if t['state']=='Paused'])")
echo "全暂停在传后：Downloading=$ND3 Queued=$NQ3 Paused=$NP3"
assert "配额释放后递补 Downloading=2" 2 "$ND3"
assert "Queued 递减至 1（6=2+1+3）" 1 "$NQ3"
assert "Paused 3" 3 "$NP3"

# /stats 口径一致
SBD=$(curl -sf $API/stats | jqpy "d['by_state'].get('Downloading',0)")
assert "/stats Downloading" 2 "$SBD"

echo "===== R10 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
