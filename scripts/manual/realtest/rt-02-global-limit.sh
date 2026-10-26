#!/bin/bash
# R5: #164 全局限速总阀门 + /providers 状态 真实功能测试（行为学口径）
# 对照组：无限制 128MB ~14s 完成；限制组：全局 128KiB/s 下 25s 必不完成；放宽后完成
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r5-XXXX)
cd $WORK
DPORT=8821; FPORT=9908
PASS=0; FAIL=0
cleanup() { pkill -f "smart-dl-daemon serve.*$DPORT" 2>/dev/null; kill %1 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT

node $REPO/ui/e2e/file-server.mjs $FPORT &>/dev/null &
for i in $(seq 1 50); do curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$FPORT/small.bin" && break; sleep 0.2; done
cat > config.toml << EOF
[server]
addr = "127.0.0.1:$DPORT"
[download]
dest_root = "$WORK/dl"
fsync_per_segment = false
[provider]
enabled = true
mock = true
EOF
(/home/z/smart-downloader/target/debug/smart-dl-daemon serve --config config.toml --addr 127.0.0.1:$DPORT &>/dev/null) &
for i in $(seq 1 50); do curl -sf http://127.0.0.1:$DPORT/health >/dev/null 2>&1 && break; sleep 0.2; done
API=http://127.0.0.1:$DPORT
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1 = [$3]"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi }
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
wait_state() { # task_id target_state max_loops(0.5s)
  for i in $(seq 1 $3); do
    S=$(curl -sf $API/tasks/$1 | jqpy "d['state']")
    [ "$S" = "$2" ] && return 0
    sleep 0.5
  done
  return 1
}

# --- A. /providers 状态列表（[provider] enabled=true → MockProvider 注册；无凭据 = 未认证态）---
PR=$(curl -sf $API/providers | python3 -c "
import json,sys
d=json.load(sys.stdin)
keys={'provider','enabled','authenticated','busy'}
ok=len(d)>=1 and all(keys<=set(p.keys()) for p in d)
print('schema-ok' if ok else 'schema-bad')")
assert "providers schema（真实 DTO 字段）" "schema-ok" "$PR"
PC=$(curl -sf $API/providers | jqpy "len(d)")
echo "providers 数量: $PC"
[ "$PC" -ge 1 ] 2>/dev/null && { PASS=$((PASS+1)); echo "PASS: providers 非空（MockProvider 注册）"; } || { FAIL=$((FAIL+1)); echo "FAIL: providers 为空"; }
PM=$(curl -sf $API/providers | jqpy "[p for p in d if p['provider']=='mock'][0]['enabled']" 2>/dev/null)
assert "mock provider enabled" "True" "$PM"

# --- B. 对照组：无全局限制 128MB 完成时间 ---
T1=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/limit.bin\"}" | jqpy "d['task_id']")
if wait_state $T1 Completed 60; then PASS=$((PASS+1)); echo "PASS: 对照组（无限制）完成"; else FAIL=$((FAIL+1)); echo "FAIL: 对照组 30s 未完成（测试前提不成立）"; fi
curl -sf -X DELETE "$API/tasks/$T1?purge=true" >/dev/null; sleep 0.5

# --- C. 全局 128KiB/s：同文件 25s 必不完成（对照 ~14s）---
curl -sf -X POST $API/config/limit -H 'content-type: application/json' -d '{"max_download_kb_s":128,"max_upload_kb_s":null}' >/dev/null
CV=$(curl -sf $API/config | jqpy "d['max_download_kb_s']")
assert "GET /config 回显 max_download_kb_s" 128 "$CV"
T2=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/limit.bin\"}" | jqpy "d['task_id']")
sleep 25
S2=$(curl -sf $API/tasks/$T2 | jqpy "d['state']")
D2=$(curl -sf $API/tasks/$T2 | jqpy "d['done']")
if [ "$S2" != "Completed" ]; then PASS=$((PASS+1)); echo "PASS: 全局 128KiB/s 下 25s 未完成（state=$S2 done=$D2，对照组 ~14s 完成）"; else FAIL=$((FAIL+1)); echo "FAIL: 限速未生效，25s 内完成了 128MB"; fi

# --- D. 放宽（0=不限）：40s 内恢复完成 ---
curl -sf -X POST $API/config/limit -H 'content-type: application/json' -d '{"max_download_kb_s":0}' >/dev/null
CV0=$(curl -sf $API/config | jqpy "d['max_download_kb_s']")
assert "GET /config 回显 0=不限" 0 "$CV0"
if wait_state $T2 Completed 80; then PASS=$((PASS+1)); echo "PASS: 放宽后 40s 内完成（限速热改可逆）"; else FAIL=$((FAIL+1)); echo "FAIL: 放宽后未恢复完成"; fi

echo "===== R5 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
