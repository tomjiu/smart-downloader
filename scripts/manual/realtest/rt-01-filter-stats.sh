#!/bin/bash
# R4: #173 服务端列表过滤 + /stats 聚合真实功能测试
# 单调用编排：起 daemon + 文件服务器 → 造多状态任务 → 12 项断言 → 清理
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r4-XXXX)
cd $WORK  # daemon 状态文件（tasks.json/daemon.lock）落 cwd，隔离防跨轮恢复
DPORT=8791; FPORT=9903
PASS=0; FAIL=0

cleanup() { kill %1 %2 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT

node $REPO/ui/e2e/file-server.mjs $FPORT &>/dev/null &
# 等文件服务器就绪（R4 首跑教训：daemon add 路径同步探测源站，竞态会 400）
for i in $(seq 1 50); do curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$FPORT/small.bin" && break; sleep 0.2; done
curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$FPORT/small.bin" || { echo "FATAL: 文件服务器未就绪"; exit 1; }
cat > $WORK/config.toml << EOF
[server]
addr = "127.0.0.1:$DPORT"
[download]
dest_root = "$WORK/dl"
fsync_per_segment = false
EOF
(cd $WORK && exec $REPO/target/debug/smart-dl-daemon serve --config $WORK/config.toml --addr 127.0.0.1:$DPORT &>/dev/null) &

# 等 daemon 就绪
for i in $(seq 1 50); do curl -sf http://127.0.0.1:$DPORT/health >/dev/null 2>&1 && break; sleep 0.2; done
curl -sf http://127.0.0.1:$DPORT/health >/dev/null || { echo "FATAL: daemon 未就绪"; exit 1; }
API=http://127.0.0.1:$DPORT

# 造任务：small→Completed / limit(64KiB/s 任务级限速,长跑)→Downloading / pause→Paused
T1=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/small.bin\"}" | python3 -c "import json,sys;print(json.load(sys.stdin)['task_id'])")
T2=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/limit.bin\",\"down_kb_s\":64}" | python3 -c "import json,sys;print(json.load(sys.stdin)['task_id'])")
T3=$(curl -sf -X POST $API/tasks -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$FPORT/pause.bin\"}" | python3 -c "import json,sys;print(json.load(sys.stdin)['task_id'])")
echo "tasks: $T1 $T2 $T3"

# 等 small 完成（1MB 不限速，秒级）
for i in $(seq 1 60); do
  S=$(curl -sf $API/tasks/$T1 | python3 -c "import json,sys;print(json.load(sys.stdin)['state'])" 2>/dev/null)
  [ "$S" = "Completed" ] && break; sleep 0.5
done
# 等 pause 下到有进度后暂停
sleep 3
curl -sf -X POST $API/tasks/$T3/pause >/dev/null
sleep 1

# 状态快照
for t in $T1 $T2 $T3; do curl -sf $API/tasks/$t | python3 -c "import json,sys;d=json.load(sys.stdin);print(d['state'],d.get('name'))"; done

assert() { # name expected actual
  if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1 = [$3]";
  else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi
}
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)"; }

# 1. 全量列表：3 个任务
assert "全量列表数" 3 "$(curl -sf "$API/tasks" | jqpy "len(d)")"

# 2. state=Completed 过滤
assert "state=Completed 数" 1 "$(curl -sf "$API/tasks?state=Completed" | jqpy "len(d)")"
assert "state=Completed 名" "small.bin" "$(curl -sf "$API/tasks?state=Completed" | jqpy "d[0]['name']")"

# 3. state=Downloading 过滤
assert "state=Downloading 数" 1 "$(curl -sf "$API/tasks?state=Downloading" | jqpy "len(d)")"

# 4. state=Paused 过滤
assert "state=Paused 数" 1 "$(curl -sf "$API/tasks?state=Paused" | jqpy "len(d)")"
assert "state=Paused 名" "pause.bin" "$(curl -sf "$API/tasks?state=Paused" | jqpy "d[0]['name']")"

# 5. 多状态逗号分隔
assert "state=Completed,Paused 数" 2 "$(curl -sf "$API/tasks?state=Completed,Paused" | jqpy "len(d)")"

# 6. 非法 state → 400
CODE=$(curl -s -o /dev/null -w "%{http_code}" "$API/tasks?state=Bogus")
assert "state=Bogus HTTP" 400 "$CODE"

# 7. search 大小写不敏感（任务名子串）
assert "search=PAUSE 数" 1 "$(curl -sf "$API/tasks?search=PAUSE" | jqpy "len(d)")"

# 8. search 匹配 URL 子串
assert "search=9903（URL 子串）数" 3 "$(curl -sf "$API/tasks?search=9903" | jqpy "len(d)")"

# 9. search 无匹配 → 空
assert "search=nomatchxyz 数" 0 "$(curl -sf "$API/tasks?search=nomatchxyz" | jqpy "len(d)")"

# 10. 组合：state=Completed + search=limit → 空（交集语义）
assert "state=Completed&search=limit 数" 0 "$(curl -sf "$API/tasks?state=Completed&search=limit" | jqpy "len(d)")"

# 11. /stats by_state 聚合与全量列表一致
assert "/stats total" 3 "$(curl -sf $API/stats | jqpy "d['total']")"
assert "/stats by_state Completed" 1 "$(curl -sf $API/stats | jqpy "d['by_state']['Completed']")"
assert "/stats by_state Paused" 1 "$(curl -sf $API/stats | jqpy "d['by_state']['Paused']")"

# 12. /metrics 派生口径（实际格式 dimension="state",label="Completed"）
MC=$(curl -sf $API/metrics | rg -c 'smart_dl_tasks_total\{dimension="state",label="Completed"\} 1' || echo 0)
assert "/metrics Completed 行" 1 "$MC"

# 13. limit/offset 分页
assert "limit=2 数" 2 "$(curl -sf "$API/tasks?limit=2" | jqpy "len(d)")"
assert "limit=2&offset=2 数" 1 "$(curl -sf "$API/tasks?limit=2&offset=2" | jqpy "len(d)")"

echo "===== R4 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
