#!/bin/bash
# rt-11: BT 顺序下载（sequential，边下边播）真实功能测试（R14 战役固化）
# 双 daemon 本地 swarm：sequential=true 建任务 → 真实下载完成 + SHA256。
# 运行中 /tasks/:id/sequential 端点幂等切换烟测。
# 注：piece 顺序的深度断言受限于 files 数组仅透出聚合条目（无 per-file
#   进度可观测）；本测覆盖功能面（接受 flag + 完成 + 数据正确性）。
# 前置：LT 环境（~/bt-native）+ cargo build -p smart-dl-daemon --features bt
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/rt11-XXXX)
PASS=0; FAIL=0
A_ADDR=127.0.0.1:8835; B_ADDR=127.0.0.1:8836
A_PORT_BT=16905; B_PORT_BT=16906
API_A="http://$A_ADDR"; API_B="http://$B_ADDR"
DAEMON=$REPO/target/debug/smart-dl-daemon

if [ ! -f "$HOME/bt-native/lib/liblt_kernel.a" ] && [ ! -f "$HOME/bt-native/build/lt_kernel.lib" ]; then
  echo "SKIP: LT 链接环境缺失（~/bt-native）"; exit 77
fi
# shellcheck disable=SC1091
[ -f "$HOME/bt-native/env.sh" ] && source "$HOME/bt-native/env.sh"
[ -x "$DAEMON" ] || { echo "SKIP: daemon 未构建（--features bt）"; exit 77; }

APID=""; BPID=""
cleanup() {
  [ -n "${APID:-}" ] && kill "$APID" 2>/dev/null || true
  [ -n "${BPID:-}" ] && kill "$BPID" 2>/dev/null || true
  sleep 0.3; rm -rf "$WORK"
}
trap cleanup EXIT
assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }

python3 - "$WORK" <<'PYEOF'
import hashlib, os, sys
work = sys.argv[1]; PIECE = 262144
def benc(b):
    if isinstance(b, int): return b'i%de' % b
    if isinstance(b, bytes): return b'%d:%s' % (len(b), b)
    if isinstance(b, str): return benc(b.encode())
    if isinstance(b, dict): return b'd' + b''.join(benc(k) + benc(b[k]) for k in sorted(b)) + b'e'
    if isinstance(b, list): return b'l' + b''.join(benc(x) for x in b) + b'e'
    raise TypeError(b)
files = [('d.bin', 8*1024*1024), ('e.bin', 8*1024*1024)]
blob = b''; sums = []
for rel, size in files:
    rng = hashlib.sha256(rel.encode()).digest()
    data = (rng * (size // 32 + 1))[:size]
    p = os.path.join(work, 'content', 'rt11seq', rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, 'wb').write(data); blob += data
    sums.append((hashlib.sha256(data).hexdigest(), rel))
pieces = b''.join(hashlib.sha1(blob[i:i+PIECE]).digest() for i in range(0, len(blob), PIECE))
t = {b'announce': b'http://127.0.0.1:1/announce', b'info': {
    b'name': b'rt11seq', b'piece length': PIECE, b'pieces': pieces,
    b'files': [{b'path': [r.encode()], b'length': s} for r, s in files]}}
open(os.path.join(work, 'rt11seq.torrent'), 'wb').write(benc(t))
with open(os.path.join(work, 'sums'), 'w') as f:
    for h, r in sums: f.write('%s %s\n' % (h, r))
PYEOF

mkdir -p "$WORK/A" "$WORK/B"
cp -r "$WORK/content/rt11seq" "$WORK/A/rt11seq"
mkcfg() { cat > "$3" <<EOF
[server]
addr = "$1"
[storage]
tasks_path = "$4"
[download]
dest_root = "$2"
[bt]
enabled = true
enable_dht = false
listen_port = $5
EOF
}
mkcfg "$A_ADDR" "$WORK/A" "$WORK/cfgA.toml" "$WORK/tasksA.json" $A_PORT_BT
mkcfg "$B_ADDR" "$WORK/B" "$WORK/cfgB.toml" "$WORK/tasksB.json" $B_PORT_BT
(cd "$WORK/A" && exec "$DAEMON" serve --config "$WORK/cfgA.toml") &>/dev/null &
APID=$!
(cd "$WORK/B" && exec "$DAEMON" serve --config "$WORK/cfgB.toml") &>/dev/null &
BPID=$!
for i in $(seq 1 50); do curl -sf -o /dev/null "$API_A/health" && curl -sf -o /dev/null "$API_B/health" && break; sleep 0.2; done
curl -sf -o /dev/null "$API_A/health" || { echo "FAIL: daemon A 未就绪"; exit 1; }
curl -sf -o /dev/null "$API_B/health" || { echo "FAIL: daemon B 未就绪"; exit 1; }

TB64=$(base64 -w0 "$WORK/rt11seq.torrent")
TA=$(curl -sf -X POST "$API_A/tasks" -H 'Content-Type: application/json' -d "{\"torrent_b64\":\"$TB64\"}" | jqpy "d['task_id']")
for i in $(seq 1 40); do
  S=$(curl -sf "$API_A/tasks/$TA" | jqpy "d['state']")
  [ "$S" = "Seeding" ] && break; sleep 0.5
done
assert "seeder 就绪" "Seeding" "$S"

T1=$(curl -sf -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d "{\"torrent_b64\":\"$TB64\",\"sequential\":true}" | jqpy "d['task_id']")
[ -n "$T1" ] && echo "PASS: sequential=true 建任务（$T1）" && PASS=$((PASS+1)) \
  || { echo "FAIL: 建任务失败"; FAIL=$((FAIL+1)); }
curl -sf -X POST "$API_B/tasks/$T1/peers" -H 'Content-Type: application/json' \
  -d "{\"addrs\":[\"127.0.0.1:$A_PORT_BT\"]}" >/dev/null

S=""
for i in $(seq 1 60); do
  S=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['state']")
  { [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && break; sleep 1
done
{ [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && echo "PASS: sequential 下载完成（$S）" && PASS=$((PASS+1)) \
  || { echo "FAIL: 未完成（$S）"; FAIL=$((FAIL+1)); }

D_SHA=$(awk '$2=="d.bin"{print $1}' "$WORK/sums"); E_SHA=$(awk '$2=="e.bin"{print $1}' "$WORK/sums")
assert "d.bin SHA256" "$D_SHA" "$(sha256sum "$WORK/B/rt11seq/d.bin" 2>/dev/null | awk '{print $1}')"
assert "e.bin SHA256" "$E_SHA" "$(sha256sum "$WORK/B/rt11seq/e.bin" 2>/dev/null | awk '{print $1}')"

# 运行中幂等切换（端点烟测）
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API_B/tasks/$T1/sequential" -H 'Content-Type: application/json' -d '{"sequential":false}')
if [ "$CODE" = "200" ] || [ "$CODE" = "204" ]; then
  echo "PASS: 运行中 sequential 端点可用（$CODE）"; PASS=$((PASS+1))
else
  echo "FAIL: sequential 端点 $CODE"; FAIL=$((FAIL+1))
fi

echo ""
echo "rt-11 结果: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "✅ rt-11 全部通过" || echo "❌ rt-11 有失败项"
exit $FAIL
