#!/bin/bash
# rt-13: BT 磁力链接公网 swarm 元数据抓取真实功能测试（R4 战役固化）
# 对真实 Debian 公网 swarm 的 magnet → .torrent 抓取（既有 magnet 测试全部
# 用本地 seed_main；公网 tracker 路径的真实盲区）。
# 网络依赖（与 soak-bt 同口径）：不可达 → SKIP 不误报。
# 断言：
#   1. POST /bt/metadata 对公网 swarm 磁链抓取成功（本地 bencode 交叉校验 infohash）
#   2. name/total_size 一致、files 非空
#   3. save_to 落盘 .torrent 同 infohash
#   4. 抓取产物建任务 → 真实下载启动（done > 1MB）
#   5. 错误路径：坏 magnet → 400
# 前置：LT 环境（~/bt-native）+ cargo build -p smart-dl-daemon --features bt
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/rt13-XXXX)
PASS=0; FAIL=0
B_ADDR=127.0.0.1:8839
API_B="http://$B_ADDR"
DAEMON=$REPO/target/debug/smart-dl-daemon

if [ ! -f "$HOME/bt-native/lib/liblt_kernel.a" ] && [ ! -f "$HOME/bt-native/build/lt_kernel.lib" ]; then
  echo "SKIP: LT 链接环境缺失（~/bt-native）"; exit 77
fi
# shellcheck disable=SC1091
[ -f "$HOME/bt-native/env.sh" ] && source "$HOME/bt-native/env.sh"
[ -x "$DAEMON" ] || { echo "SKIP: daemon 未构建（--features bt）"; exit 77; }

DPID=""
cleanup() { [ -n "${DPID:-}" ] && kill "$DPID" 2>/dev/null || true; sleep 0.3; rm -rf "$WORK"; }
trap cleanup EXIT
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }

# 动态发现 Debian netinst 种子（版本不硬编码——点版本约 2 月一轮会 404）
BTCD_DIR="https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/"
DISCOVERED=$(curl -sL --max-time 30 "$BTCD_DIR" \
  | grep -o 'href="debian-[0-9.]*-amd64-netinst\.iso\.torrent"' \
  | head -1 | sed 's/href="//;s/"//')
[ -n "$DISCOVERED" ] || { echo "SKIP: cdimage 目录不可达（公网依赖）"; exit 77; }
TORRENT_URL="$BTCD_DIR$DISCOVERED"
curl -sL --max-time 60 -o "$WORK/seed.torrent" "$TORRENT_URL"
[ -s "$WORK/seed.torrent" ] || { echo "SKIP: 种子下载失败（公网依赖）"; exit 77; }
echo "== 种子: $DISCOVERED"

# 本地独立基准：infohash + announce + 大小（bencode 手解析）
read -r LOCAL_IH ANNOUNCE TOTAL < <(python3 - "$WORK/seed.torrent" <<'PYEOF'
import hashlib, sys
data = open(sys.argv[1], 'rb').read()
def span(buf, pos):
    c = buf[pos:pos+1]
    if c == b'i': return buf.index(b'e', pos) + 1
    if c in (b'l', b'd'):
        pos += 1
        while buf[pos:pos+1] != b'e':
            pos = span(buf, pos)
            if c == b'd': pos = span(buf, pos)
        return pos + 1
    colon = buf.index(b':', pos)
    return colon + 1 + int(buf[pos:colon])
i = data.find(b'4:info'); start = i + 6
print(hashlib.sha1(data[start:span(data, start)]).hexdigest(), end=' ')
def field(buf, key):
    k = buf.find(key)
    if k < 0: return b''
    colon = buf.index(b':', k + len(key))
    n = int(buf[k + len(key):colon])
    return buf[colon+1:colon+1+n]
print(field(data, b'8:announce').decode('utf-8', 'replace'), end=' ')
ln = data.find(b'6:length')
tot = 0
if ln >= 0 and data[ln+8:ln+9] == b'i':
    e = data.index(b'e', ln+8); tot = int(data[ln+9:e])
print(tot)
PYEOF
)
[ ${#LOCAL_IH} -eq 40 ] || { echo "SKIP: infohash 解析失败"; exit 77; }
echo "== 基准: ih=$LOCAL_IH total=$TOTAL announce=$ANNOUNCE"

mkdir -p "$WORK/dl"
cat > "$WORK/cfg.toml" <<EOF
[server]
addr = "$B_ADDR"
[storage]
tasks_path = "$WORK/tasks.json"
[download]
dest_root = "$WORK/dl"
[bt]
enabled = true
enable_dht = false
EOF
(cd "$WORK/dl" && exec "$DAEMON" serve --config "$WORK/cfg.toml") &>/dev/null &
DPID=$!
for i in $(seq 1 50); do curl -sf -o /dev/null "$API_B/health" && break; sleep 0.2; done
curl -sf -o /dev/null "$API_B/health" || { echo "FAIL: daemon 未就绪"; exit 1; }

# 错误路径
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API_B/bt/metadata" -H 'Content-Type: application/json' -d '{"magnet":"not-a-magnet"}')
assert "坏 magnet → 400" "400" "$CODE"

# 公网抓取（DHT off 纯 HTTP tracker 路径——确定性）
TR_ENC=$(python3 -c "import urllib.parse;print(urllib.parse.quote('$ANNOUNCE', safe=''))")
MAGNET="magnet:?xt=urn:btih:${LOCAL_IH}&dn=${DISCOVERED%.torrent}&tr=${TR_ENC}"
CODE=$(curl -s -o "$WORK/meta.json" -w '%{http_code}' -X POST "$API_B/bt/metadata" \
  -H 'Content-Type: application/json' \
  -d "{\"magnet\":\"$MAGNET\",\"dht\":false,\"timeout_s\":150,\"save_to\":\"rt13-fetched.torrent\"}")
assert "公网 swarm magnet 抓取 200" "200" "$CODE"
if [ "$CODE" = "200" ]; then
  assert "infohash 交叉校验一致" "$LOCAL_IH" "$(jqpy "d['infohash']" < "$WORK/meta.json")"
  assert "total_size 一致" "$TOTAL" "$(jqpy "d['total_size']" < "$WORK/meta.json")"
  NF=$(jqpy "len(d['files'])" < "$WORK/meta.json")
  if [ "${NF:-0}" -ge 1 ]; then echo "PASS: files 非空（$NF）"; PASS=$((PASS+1)); else echo "FAIL: files 空"; FAIL=$((FAIL+1)); fi
  # save_to 落盘同 infohash
  F_IH=$(python3 - "$WORK/dl/rt13-fetched.torrent" <<'PYEOF'
import hashlib, sys
data = open(sys.argv[1], 'rb').read()
def span(buf, pos):
    c = buf[pos:pos+1]
    if c == b'i': return buf.index(b'e', pos) + 1
    if c in (b'l', b'd'):
        pos += 1
        while buf[pos:pos+1] != b'e':
            pos = span(buf, pos)
            if c == b'd': pos = span(buf, pos)
        return pos + 1
    colon = buf.index(b':', pos)
    return colon + 1 + int(buf[pos:colon])
i = data.find(b'4:info'); s = i + 6
print(hashlib.sha1(data[s:span(data, s)]).hexdigest())
PYEOF
)
  assert "save_to 落盘 .torrent 同 infohash" "$LOCAL_IH" "$F_IH"

  # 抓取产物建任务 → 真实下载启动（磁盘余量门：ISO 全量 + 10% 裕量；
  # 不足则跳过下载段——本脚本核心价值在公网元数据抓取，下载为附加实证）
  FREE_KB=$(df -Pk "$WORK" | awk 'NR==2 {print $4}')
  NEED_KB=$(( TOTAL / 1024 * 11 / 10 ))
  if [ "${FREE_KB:-0}" -lt "$NEED_KB" ]; then
    echo "SKIP: 磁盘余量不足（free=${FREE_KB}KB < need=${NEED_KB}KB）——下载段跳过（元数据断言已全过）"
    echo ""
    echo "rt-13 结果: PASS=$PASS FAIL=$FAIL"
    [ "$FAIL" -eq 0 ] && echo "✅ rt-13 全部通过（下载段 SKIP）" || echo "❌ rt-13 有失败项"
    exit $FAIL
  fi
  T1=$(curl -sf -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
    -d "{\"torrent_b64\":\"$(base64 -w0 "$WORK/dl/rt13-fetched.torrent")\"}" | jqpy "d['task_id']")
  [ -n "$T1" ] && echo "PASS: 抓取产物建任务（$T1）" && PASS=$((PASS+1)) \
    || { echo "FAIL: 建任务失败"; FAIL=$((FAIL+1)); }
  STARTED=0
  for i in $(seq 1 30); do
    sleep 5
    DN=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['done']")
    if [ "${DN:-0}" -gt 1048576 ] 2>/dev/null; then STARTED=1; break; fi
  done
  if [ "$STARTED" = "1" ]; then echo "PASS: 真实下载已启动（done>1MB）"; PASS=$((PASS+1)); else echo "FAIL: 150s 内未启动"; FAIL=$((FAIL+1)); fi
fi

echo ""
echo "rt-13 结果: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "✅ rt-13 全部通过" || echo "❌ rt-13 有失败项"
exit $FAIL
