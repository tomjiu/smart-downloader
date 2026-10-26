#!/bin/bash
# rt-09: BT 多文件子文件优先级真实功能测试（R5/R10 战役固化；#178 修复的 e2e 实证）
# 架构：手写 bencode 双 daemon 本地 swarm（seeder A :8796 / downloader B :8797，
#   直注 peer，无公网依赖）
# 断言：
#   1. 创建时 file_priorities（skip 中间文件）→ readback [4,0,4]、total 排除 skip
#   2. 非跳过文件 SHA256 正确、跳过文件不落盘
#   3. 错误路径：越界下标 400 / priority>7 400 / magnet 携带 400
#   4. set 端点（创建后改设）响应与持久化取「请求合并期望值」（意图优先）
# 前置：LT 环境（~/bt-native，scripts/ci/bt-linux-setup.sh --no-root）+
#   cargo build -p smart-dl-daemon --features bt
# 背景（为什么这个测试存在）：#178 前「创建后补设优先级」与 libtorrent 启动
# 序列竞态——同脚本两跑 total_wanted 16MB/24MB 分叉，skip 非确定性丢失。
set -u
REPO=$(cd "$(dirname "$0")/../../.." && pwd)
WORK=$(mktemp -d /tmp/rt09-XXXX)
PASS=0; FAIL=0
A_ADDR=127.0.0.1:8831; B_ADDR=127.0.0.1:8832
A_PORT_BT=16901; B_PORT_BT=16902
API_A="http://$A_ADDR"; API_B="http://$B_ADDR"
DAEMON=$REPO/target/debug/smart-dl-daemon

# LT 环境门（缺即跳过，与 soak-bt 同口径）
if [ ! -f "$HOME/bt-native/lib/liblt_kernel.a" ] && [ ! -f "$HOME/bt-native/build/lt_kernel.lib" ]; then
  echo "SKIP: LT 链接环境缺失（~/bt-native）——bash scripts/ci/bt-linux-setup.sh --no-root ~/bt-native"
  exit 77
fi
# shellcheck disable=SC1091
[ -f "$HOME/bt-native/env.sh" ] && source "$HOME/bt-native/env.sh"
[ -x "$DAEMON" ] || { echo "SKIP: daemon 未构建（cargo build -p smart-dl-daemon --features bt）"; exit 77; }

APID=""; BPID=""
cleanup() {
  [ -n "${APID:-}" ] && kill "$APID" 2>/dev/null || true
  [ -n "${BPID:-}" ] && kill "$BPID" 2>/dev/null || true
  sleep 0.3
  rm -rf "$WORK"
}
trap cleanup EXIT

assert() { if [ "$2" = "$3" ]; then PASS=$((PASS+1)); echo "PASS: $1"; else FAIL=$((FAIL+1)); echo "FAIL: $1 期望[$2] 实际[$3]"; fi; }
jqpy() { python3 -c "import json,sys;d=json.load(sys.stdin);print($1)" 2>/dev/null; }

# ── 造 3×8MB 多文件 torrent（确定性内容）───────────────────────────────────
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
files = [('a.bin', 8*1024*1024), ('b.bin', 8*1024*1024), ('c.bin', 8*1024*1024)]
blob = b''
sums = []
for rel, size in files:
    rng = hashlib.sha256(rel.encode()).digest()
    data = (rng * (size // 32 + 1))[:size]
    p = os.path.join(work, 'content', 'rt09sw', rel)
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, 'wb').write(data); blob += data
    sums.append((hashlib.sha256(data).hexdigest(), rel))
pieces = b''.join(hashlib.sha1(blob[i:i+PIECE]).digest() for i in range(0, len(blob), PIECE))
t = {b'announce': b'http://127.0.0.1:1/announce', b'info': {
    b'name': b'rt09sw', b'piece length': PIECE, b'pieces': pieces,
    b'files': [{b'path': [r.encode()], b'length': s} for r, s in files]}}
open(os.path.join(work, 'rt09sw.torrent'), 'wb').write(benc(t))
with open(os.path.join(work, 'sums'), 'w') as f:
    for h, r in sums: f.write('%s %s\n' % (h, r))
PYEOF
[ -f "$WORK/rt09sw.torrent" ] || { echo "FAIL: torrent 生成"; exit 1; }

# ── 双 daemon（各自子目录 cwd：daemon.lock 是 cwd 相对路径）────────────────
mkdir -p "$WORK/A" "$WORK/B"
cp -r "$WORK/content/rt09sw" "$WORK/A/rt09sw"
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

TB64=$(base64 -w0 "$WORK/rt09sw.torrent")
TA=$(curl -sf -X POST "$API_A/tasks" -H 'Content-Type: application/json' -d "{\"torrent_b64\":\"$TB64\"}" | jqpy "d['task_id']")
for i in $(seq 1 40); do
  S=$(curl -sf "$API_A/tasks/$TA" | jqpy "d['state']")
  [ "$S" = "Seeding" ] && break; sleep 0.5
done
assert "seeder 哈希检查进入 Seeding" "Seeding" "$S"

# ── 核心：创建时携带优先级（#178 根治路径）─────────────────────────────────
T1=$(curl -sf -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d "{\"torrent_b64\":\"$TB64\",\"file_priorities\":[{\"index\":1,\"priority\":0}]}" | jqpy "d['task_id']")
[ -n "$T1" ] && curl -sf -X POST "$API_B/tasks/$T1/peers" -H 'Content-Type: application/json' \
  -d "{\"addrs\":[\"127.0.0.1:$A_PORT_BT\"]}" >/dev/null

# 错误路径（对 TorrentFile 任务）
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d "{\"torrent_b64\":\"$TB64\",\"file_priorities\":[{\"index\":9,\"priority\":0}]}")
assert "创建时越界下标 → 400" "400" "$CODE"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d "{\"torrent_b64\":\"$TB64\",\"file_priorities\":[{\"index\":0,\"priority\":8}]}")
assert "创建时 priority 8 → 400" "400" "$CODE"
CODE=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API_B/tasks" -H 'Content-Type: application/json' \
  -d '{"url":"magnet:?xt=urn:btih:0000000000000000000000000000000000000000","file_priorities":[{"index":0,"priority":0}]}')
assert "magnet 携带优先级 → 400" "400" "$CODE"

# 等完成（16MB 本地秒级）
for i in $(seq 1 60); do
  S=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['state']")
  { [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && break; sleep 1
done
{ [ "$S" = "Seeding" ] || [ "$S" = "Completed" ]; } && echo "PASS: 下载完成（$S）" && PASS=$((PASS+1)) \
  || { echo "FAIL: 未完成（$S）"; FAIL=$((FAIL+1)); }

# readback（空 priorities = 纯读）
PRI=$(curl -sf -X POST "$API_B/tasks/$T1/files/priority" -H 'Content-Type: application/json' -d '{"priorities":[]}' | jqpy "d['priorities']")
assert "优先级 readback [4, 0, 4]" "[4, 0, 4]" "$PRI"

# total = total_wanted 排除 skip（16MB）
TOT=$(curl -sf "$API_B/tasks/$T1" | jqpy "d['total']")
assert "total=16MB（skip 文件确定性排除）" "16777216" "$TOT"

# 文件级：a/c SHA256 对；b 不落盘
A_SHA=$(awk '$2=="a.bin"{print $1}' "$WORK/sums"); C_SHA=$(awk '$2=="c.bin"{print $1}' "$WORK/sums")
assert "a.bin SHA256" "$A_SHA" "$(sha256sum "$WORK/B/rt09sw/a.bin" 2>/dev/null | awk '{print $1}')"
assert "c.bin SHA256" "$C_SHA" "$(sha256sum "$WORK/B/rt09sw/c.bin" 2>/dev/null | awk '{print $1}')"
if [ -f "$WORK/B/rt09sw/b.bin" ]; then
  echo "FAIL: b.bin 竟落盘（skip 失效）"; FAIL=$((FAIL+1))
else
  echo "PASS: b.bin 未落盘（skip 生效）"; PASS=$((PASS+1))
fi

# ── set 端点意图合并（改设 c→0，未提及下标保持现值）───────────────────────
R=$(curl -sf -X POST "$API_B/tasks/$T1/files/priority" -H 'Content-Type: application/json' \
  -d '{"priorities":[{"index":2,"priority":0}]}')
assert "set 响应合并期望值 [4, 0, 0]" "[4, 0, 0]" "$(echo "$R" | jqpy "d['priorities']")"

echo ""
echo "rt-09 结果: PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "✅ rt-09 全部通过" || echo "❌ rt-09 有失败项"
exit $FAIL
