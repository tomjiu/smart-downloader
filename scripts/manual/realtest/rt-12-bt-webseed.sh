#!/bin/bash
# rt-12: BT web seed（BEP-19 P2SP）真实功能测试（R15 战役固化）
# 零 peer + 死 tracker：唯一源 = 本地 HTTP 文件服务器（web seed URL）。
# 验证：BT 任务经纯 HTTP web seed 完成下载、SHA256 与源一致、added 回执。
# 前置：LT 环境（~/bt-native）+ cargo build -p smart-dl-daemon --features bt
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/rt12-XXXX)
PASS=0; FAIL=0
B_ADDR=127.0.0.1:8837
B_PORT_BT=16907
API_B="http://$B_ADDR"
HS_PORT=8838
DAEMON=$REPO/target/debug/smart-dl-daemon

if [ ! -f "$HOME/bt-native/lib/liblt_kernel.a" ] && [ ! -f "$HOME/bt-native/build/lt_kernel.lib" ]; then
  echo "SKIP: LT 链接环境缺失（~/bt-native）"; exit 77
fi
# shellcheck disable=SC1091
[ -f "$HOME/bt-native/env.sh" ] && source "$HOME/bt-native/env.sh"
[ -x "$DAEMON" ] || { echo "SKIP: daemon 未构建（--features bt）"; exit 77; }

DPID=""; HSPID=""
cleanup() {
  [ -n "${DPID:-}" ] && kill "$DPID" 2>/dev/null || true
  [ -n "${HSPID:-}" ] && kill "$HSPID" 2>/dev/null || true
  sleep 0.3; rm -rf "$WORK"
}
trap cleanup EXIT
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }

# 单文件 torrent（libtorrent web seed 语义：URL 直指文件本身）
python3 - "$WORK" <<'PYEOF'
import hashlib, os, sys
work = sys.argv[1]; PIECE = 262144; SIZE = 8*1024*1024
rng = hashlib.sha256(b'rt12-webseed.bin').digest()
data = (rng * (SIZE // 32 + 1))[:SIZE]
open(os.path.join(work, 'content.bin'), 'wb').write(data)
pieces = b''.join(hashlib.sha1(data[i:i+PIECE]).digest() for i in range(0, len(data), PIECE))
def benc(b):
    if isinstance(b, int): return b'i%de' % b
    if isinstance(b, bytes): return b'%d:%s' % (len(b), b)
    if isinstance(b, str): return benc(b.encode())
    if isinstance(b, dict): return b'd' + b''.join(benc(k) + benc(b[k]) for k in sorted(b)) + b'e'
    if isinstance(b, list): return b'l' + b''.join(benc(x) for x in b) + b'e'
    raise TypeError(b)
t = {b'announce': b'http://127.0.0.1:1/announce', b'info': {
    b'name': b'content.bin', b'piece length': PIECE, b'pieces': pieces, b'length': SIZE}}
open(os.path.join(work, 'content.bin.torrent'), 'wb').write(benc(t))
PYEOF
EXPECT_SHA=$(python3 -c "
import hashlib
print(hashlib.sha256(open('$WORK/content.bin','rb').read()).hexdigest())")

# HTTP web seed 源
(cd "$WORK" && exec python3 -m http.server $HS_PORT --bind 127.0.0.1) &>/dev/null &
HSPID=$!
sleep 1
curl -sf -o /dev/null "http://127.0.0.1:$HS_PORT/content.bin" || { echo "FAIL: HTTP 源未就绪"; exit 1; }
echo "PASS: HTTP web seed 源就绪（:$HS_PORT）"; PASS=$((PASS+1))

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
listen_port = $B_PORT_BT
EOF
(cd "$WORK/dl" && exec "$DAEMON" serve --config "$WORK/cfg.toml") &>/dev/null &
DPID=$!
for i in $(seq 1 50); do curl -sf -o /dev/null "$API_B/health" && break; sleep 0.2; done
curl -sf -o /dev/null "$API_B/health" || { echo "FAIL: daemon 未就绪"; exit 1; }

T1=$(curl -sf -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d "{\"torrent_b64\":\"$(base64 -w0 "$WORK/content.bin.torrent")\"}" | jqpy "d['task_id']")
[ -n "$T1" ] && echo "PASS: BT 任务建立（零 peer 死 tracker，$T1）" && PASS=$((PASS+1)) \
  || { echo "FAIL: 建任务失败"; FAIL=$((FAIL+1)); }
sleep 2
R=$(curl -sf -X POST "$API_B/tasks/$T1/webseeds" -H 'Content-Type: application/json' \
  -d "{\"urls\":[\"http://127.0.0.1:$HS_PORT/content.bin\"]}")
assert "web seed 注入回执 added=1" "1" "$(echo "$R" | jqpy "d['added']")"

S=""
for i in $(seq 1 60); do
  S=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['state']")
  { [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && break; sleep 1
done
{ [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && echo "PASS: web seed 下载完成（$S）" && PASS=$((PASS+1)) \
  || { echo "FAIL: 未完成（$S）"; FAIL=$((FAIL+1)); }

assert "SHA256 与源一致（web seed 数据完整性）" "$EXPECT_SHA" "$(sha256sum "$WORK/dl/content.bin" 2>/dev/null | awk '{print $1}')"

echo ""
echo "rt-12 结果: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "✅ rt-12 全部通过" || echo "❌ rt-12 有失败项"
exit $FAIL
