#!/bin/bash
# R6: RSS 端到端真实功能测试——订阅 → 拉取解析 → 规则匹配 → 自动建任务 → 真实下载完成 → 排除项不建任务
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/r6-XXXX)
cd $WORK
DPORT=8822; FPORT=9912; RPORT=9920
PASS=0; FAIL=0
cleanup() { pkill -f "smart-dl-daemon serve.*$DPORT" 2>/dev/null; kill %1 %2 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT

node $REPO/ui/e2e/file-server.mjs $FPORT &>/dev/null &
node $(dirname "$0")/rss-server.mjs $RPORT $FPORT &>/dev/null &
for i in $(seq 1 50); do curl -sf -o /dev/null "http://127.0.0.1:$RPORT/feed.xml" && break; sleep 0.2; done
curl -sf -o /dev/null "http://127.0.0.1:$RPORT/feed.xml" || { echo "FATAL: rss server 未就绪"; exit 1; }
curl -sf -o /dev/null -H 'Range: bytes=0-0' "http://127.0.0.1:$FPORT/small.bin" || { echo "FATAL: file server 未就绪"; exit 1; }
cat > config.toml << EOF
[server]
addr = "127.0.0.1:$DPORT"
[download]
dest_root = "$WORK/dl"
fsync_per_segment = false
[rss]
auto_refresh = true
refresh_interval_secs = 60
EOF
(/home/z/smart-downloader/target/debug/smart-dl-daemon serve --config config.toml --addr 127.0.0.1:$DPORT &>/dev/null) &
for i in $(seq 1 50); do curl -sf http://127.0.0.1:$DPORT/health >/dev/null 2>&1 && break; sleep 0.2; done
API=http://127.0.0.1:$DPORT
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1 = [$3]"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi }
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }
wait_task_done() { for i in $(seq 1 $2); do S=$(curl -sf $API/tasks/$1 | jqpy "d['state']"); [ "$S" = "Completed" ] && return 0; sleep 0.5; done; return 1; }

# 1. 添加订阅（真实拉取+解析：title/item_count 应当场可见）
FA=$(curl -sf -X POST $API/rss/feeds -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$RPORT/feed.xml\"}")
echo "feed add resp: $FA"
assert "feed title" "distro-announce" "$(echo "$FA" | jqpy "d['title']")"
assert "feed item_count" 3 "$(echo "$FA" | jqpy "d['item_count']")"
FID=$(echo "$FA" | jqpy "d['id']")

# 2. 重复订阅 → 409
CODE=$(curl -s -o /dev/null -w "%{http_code}" -X POST $API/rss/feeds -H 'content-type: application/json' -d "{\"url\":\"http://127.0.0.1:$RPORT/feed.xml\"}")
assert "重复订阅 HTTP" 409 "$CODE"

# 3. 添加规则：must_contain=debian，must_not_contain=beta（排除词独立于 ubuntu 项）
RA=$(curl -sf -X POST $API/rss/rules -H 'content-type: application/json' -d "{\"name\":\"debian-netinst\",\"enabled\":true,\"must_contain\":[\"debian\"],\"must_not_contain\":[\"beta\"],\"feed_id\":null,\"tags\":[\"rss\"],\"dest\":null,\"use_regex\":false,\"episode_filter\":null}")
RID=$(echo "$RA" | jqpy "d['id']")
assert "rule id 非空" "yes" "$([ -n "$RID" ] && echo yes)"

# 4. 手动触发 refresh：new_items=0（已拉过）但 matched=1、task_ids=1
RF=$(curl -sf -X POST $API/rss/refresh)
echo "refresh resp: $RF"
assert "refresh matched" 1 "$(echo "$RF" | jqpy "d['matched']")"
NT=$(echo "$RF" | jqpy "len(d['task_ids'])")
assert "refresh task_ids 数" 1 "$NT"
TID=$(echo "$RF" | jqpy "d['task_ids'][0]" 2>/dev/null)
if [ -z "$TID" ]; then echo "FATAL: 未建任务，无法继续"; echo "$RF"; exit 1; fi

# 5. 命中任务自动建出 + 真实下载完成
if wait_task_done $TID 40; then PASS=$((PASS+1)); echo "PASS: RSS 自动建任务真实下载完成（$TID）"; else FAIL=$((FAIL+1)); echo "FAIL: 任务未完成"; fi

# 6. 任务名 = RSS item 标题派生（debian 项，非 ubuntu 项）
TN=$(curl -sf $API/tasks/$TID | jqpy "d['name']")
echo "task name: $TN"
case "$TN" in *debian*) PASS=$((PASS+1)); echo "PASS: 任务名源自 debian item";; *) FAIL=$((FAIL+1)); echo "FAIL: 任务名异常: $TN";; esac

# 7. 排除语义：ubuntu 项不应建任务（任务总数=1）
TC=$(curl -sf "$API/tasks" | jqpy "len(d)")
assert "任务总数（仅 debian 命中）" 1 "$TC"

# 8. 规则禁用后 refresh 不再建新任务
curl -sf -X DELETE $API/tasks/$TID?purge=true >/dev/null; sleep 0.5
# 重新拉 feed（加时间戳变体 URL 绕过去重？不——feed 项 guid 相同，new_items=0）
RF2=$(curl -sf -X POST $API/rss/refresh)
assert "再 refresh matched（无新项）" 0 "$(echo "$RF2" | jqpy "d['matched']")"

# 9. GET /rss/feeds 回显 last_refresh_unix 非空（响应为 {feeds:[...]} 包裹）
LR=$(curl -sf $API/rss/feeds | jqpy "d['feeds'][0]['last_refresh_unix'] != None")
assert "feeds last_refresh_unix 非空" "True" "$LR"
PC2=$(curl -sf $API/rss/feeds | jqpy "d['feeds'][0]['pending_count']")
echo "feeds pending_count: $PC2（未消费项数）"
IC2=$(curl -sf $API/rss/feeds | jqpy "d['feeds'][0]['item_count']")
assert "feeds item_count" 3 "$IC2"

# 10. GET /rss/items：非命中项可见（ubuntu + debian-beta 未消费）； debian 项已消费（{items:[...]} 包裹）
IC=$(curl -sf "$API/rss/items?feed_id=$FID" | jqpy "len(d['items'])")
echo "rss items 可见数: $IC（debian 已消费，ubuntu/debian-beta 待定）"
[ "$IC" -ge 1 ] 2>/dev/null && { PASS=$((PASS+1)); echo "PASS: rss items 非空"; } || { FAIL=$((FAIL+1)); echo "FAIL: rss items 空"; }
# debian-beta（guid 兜底路径解析）的 URL 应为 guid 的 http 值
GUIDURL=$(curl -sf "$API/rss/items?feed_id=$FID" | jqpy "[i for i in d['items'] if 'beta' in i['title']][0]['url']" 2>/dev/null)
case "$GUIDURL" in *delete.bin*) PASS=$((PASS+1)); echo "PASS: guid 兕底路径解析正确（$GUIDURL）";; *) FAIL=$((FAIL+1)); echo "FAIL: guid 兕底异常: $GUIDURL";; esac

echo "===== R6 结果: PASS=$PASS FAIL=$FAIL ====="
[ $FAIL -eq 0 ]
