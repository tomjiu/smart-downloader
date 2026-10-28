#!/bin/bash
# rt-10: BT 优先级跨 kill -9 重启重放真实功能测试（R13 战役固化；#178 缺陷①闭环）
# 场景（缺陷①原始触发链）：skip 任务 → kill -9（无优雅 fastresume）→ 重启
#   → tasks.json 恢复 → 引擎 add 暂停窗口重放优先级 → 续传完成
# 断言：
#   1. 重启后 readback [4,0,4]（持久化记录未被旧 readback 污染）
#   2. 续传完成 total=24MB（skip 跨重启存活）
#   3. a/c SHA256 跨重启续传正确、b.bin 不落盘
# 已知语义（非缺陷）：peer 表不跨重启持久（真实场景 tracker/PEX 兜底；
#   本 hermetic 测试 kill 后重注入）
# 前置：同 rt-09（LT 环境 + bt 构建）
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/rt10-XXXX)
PASS=0; FAIL=0
A_ADDR=127.0.0.1:8833; B_ADDR=127.0.0.1:8834
A_PORT_BT=16903; B_PORT_BT=16904
API_A="http://$A_ADDR"; API_B="http://$B_ADDR"
DAEMON=$REPO/target/debug/smart-dl-daemon

if [ ! -f "$HOME/bt-native/lib/liblt_kernel.a" ] && [ ! -f "$HOME/bt-native/build/lt_kernel.lib" ]; then
  echo "SKIP: LT 链接环境缺失（~/bt-native）"; exit 77
fi
# shellcheck disable=SC1091
[ -f "$HOME/bt-native/env.sh" ] && source "$HOME/bt-native/env.sh"
[ -x "$DAEMON" ] || { echo "SKIP: daemon 未构建（--features bt）"; exit 77; }

cleanup() {
  [ -n "${APID:-}" ] && kill "$APID" 2>/dev/null || true
  [ -n "${BPID:-}" ] && kill "$BPID" 2>/dev/null || true
  sleep 0.3
  rm -rf "$WORK"
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
files = [('a.bin', 12*1024*1024), ('b.bin', 12*1024*1024), ('c.bin', 12*1024*1024)]
blob = b''; sums = []
for rel, size in files:
    rng = hashlib.sha256(rel.encode()).digest()
    data = (rng * (size // 32 + 1))[:size]
    p = os.path.join(work, 'content', 'rt10sw', rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, 'wb').write(data); blob += data
    sums.append((hashlib.sha256(data).hexdigest(), rel))
pieces = b''.join(hashlib.sha1(blob[i:i+PIECE]).digest() for i in range(0, len(blob), PIECE))
t = {b'announce': b'http://127.0.0.1:1/announce', b'info': {
    b'name': b'rt10sw', b'piece length': PIECE, b'pieces': pieces,
    b'files': [{b'path': [r.encode()], b'length': s} for r, s in files]}}
open(os.path.join(work, 'rt10sw.torrent'), 'wb').write(benc(t))
with open(os.path.join(work, 'sums'), 'w') as f:
    for h, r in sums: f.write('%s %s\n' % (h, r))
PYEOF

mkdir -p "$WORK/A" "$WORK/B"
cp -r "$WORK/content/rt10sw" "$WORK/A/rt10sw"
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

TB64=$(base64 -w0 "$WORK/rt10sw.torrent")
TA=$(curl -sf -X POST "$API_A/tasks" -H 'Content-Type: application/json' -d "{\"torrent_b64\":\"$TB64\"}" | jqpy "d['task_id']")
for i in $(seq 1 40); do
  S=$(curl -sf "$API_A/tasks/$TA" | jqpy "d['state']")
  [ "$S" = "Seeding" ] && break; sleep 0.5
done
assert "seeder 就绪" "Seeding" "$S"

# 建任务：skip b.bin + 限速（保证中断窗口）+ 注 peer
T1=$(curl -sf -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d "{\"torrent_b64\":\"$TB64\",\"file_priorities\":[{\"index\":1,\"priority\":0}],\"down_kb_s\":512}" | jqpy "d['task_id']")
curl -sf -X POST "$API_B/tasks/$T1/peers" -H 'Content-Type: application/json' \
  -d "{\"addrs\":[\"127.0.0.1:$A_PORT_BT\"]}" >/dev/null

# 等 ~6s（约 1-3MB 进度）→ kill -9
sleep 6
DONE1=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['done']")
echo "  kill -9 前进度: $DONE1"
kill -9 "$BPID" 2>/dev/null || true
wait "$BPID" 2>/dev/null || true
echo "PASS: daemon B kill -9"; PASS=$((PASS+1))

# 重启 B（同 tasks.json + 落盘目录）
(cd "$WORK/B" && exec "$DAEMON" serve --config "$WORK/cfgB.toml") &>/dev/null &
BPID=$!
for i in $(seq 1 50); do curl -sf -o /dev/null "$API_B/health" && break; sleep 0.2; done
curl -sf -o /dev/null "$API_B/health" || { echo "FAIL: B 重启失败"; exit 1; }
echo "PASS: daemon B 重启恢复"; PASS=$((PASS+1))

# 重注 peer（peer 表不持久——真实场景由 tracker/PEX 重发现）
sleep 2
curl -sf -X POST "$API_B/tasks/$T1/peers" -H 'Content-Type: application/json' \
  -d "{\"addrs\":[\"127.0.0.1:$A_PORT_BT\"]}" >/dev/null

# 等续传完成（24MB wanted @512KiB/s ≤ ~50s）
S=""
for i in $(seq 1 100); do
  S=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['state']")
  { [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && break; sleep 2
done
{ [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && echo "PASS: 续传完成（$S）" && PASS=$((PASS+1)) \
  || { echo "FAIL: 续传未完成（$S）"; FAIL=$((FAIL+1)); }

TOT=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['total']")
assert "total=24MB（skip 跨 kill -9+重启存活）" "25165824" "$TOT"
PRI=$(curl -sf -X POST "$API_B/tasks/$T1/files/priority" -H 'Content-Type: application/json' -d '{"priorities":[]}' | jqpy "d['priorities']")
assert "重启后 readback [4, 0, 4]" "[4, 0, 4]" "$PRI"

A_SHA=$(awk '$2=="a.bin"{print $1}' "$WORK/sums"); C_SHA=$(awk '$2=="c.bin"{print $1}' "$WORK/sums")
assert "a.bin SHA256（跨重启续传）" "$A_SHA" "$(sha256sum "$WORK/B/rt10sw/a.bin" 2>/dev/null | awk '{print $1}')"
assert "c.bin SHA256（跨重启续传）" "$C_SHA" "$(sha256sum "$WORK/B/rt10sw/c.bin" 2>/dev/null | awk '{print $1}')"
if [ -f "$WORK/B/rt10sw/b.bin" ]; then
  echo "FAIL: b.bin 落盘（skip 被重启重放撤销——#178 缺陷①回归）"; FAIL=$((FAIL+1))
else
  echo "PASS: b.bin 未落盘（skip 跨重启存活）"; PASS=$((PASS+1))
fi

echo ""
echo "rt-10 结果: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "✅ rt-10 全部通过" || echo "❌ rt-10 有失败项"
exit $FAIL
