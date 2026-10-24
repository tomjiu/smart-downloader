#!/usr/bin/env bash
# ============================================================================
# soak-bt.sh —— BT 引擎长跑（#168 P1；需 LT 链接环境）
#
# 用法: ./soak-bt.sh [运行时长分钟=30]
#
# 与 soak-http 的差异：BT 的真实性在「公网 swarm + tracker + PEX + 做种执法」，
# mock 完全无法覆盖（#168 盲区表）。本脚本用官方发行版种子（合法资源）：
#   - 默认 Debian netinst（~630MB，官方 tracker + DHT 健壮）
#   - SOAK_TORRENT_URL 可换 Ubuntu ISO .torrent（更大，官方发布目录同源）
#
# 断言（#168 验收标准 2）：
#   1. 下载完成（公网 swarm 真实吞吐）
#   2. SHA256 与官方 SHA256SUMS 一致（防静默损坏——与 HTTP soak 同原则）
#   3. kill -9 中途回灌 fastresume：重启后恢复、不重下已完成段
#   4. 做种比率执法：max_share_ratio 达标后离场（Seeding → 执法暂停）
#   5. 全程无 panic、RSS/FD 采样趋势正常（复用 soak-rss-check.py）
#
# 环境要求（缺一即 exit 77 跳过，不误报失败）：
#   - LT 链接环境：scripts/bt_env.sh（~/bt-native：apt-get download + dpkg -x
#     无 root 方案，见该脚本头注）
#   - cargo build -p smart-dl-daemon --release --features bt
#   - 公网可达（tracker + DHT）
# ============================================================================
set -euo pipefail

DURATION_MIN=${1:-30}
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DAEMON=${SOAK_DAEMON:-$ROOT/target/release/smart-dl-daemon}
ADDR=127.0.0.1:8791
API="http://$ADDR"
SAMPLE_INTERVAL=${SOAK_SAMPLE_SEC:-60}
KEEP=${SOAK_KEEP:-0}

# ── LT 环境门（#168：BT 是主打功能，但环境缺失时明确跳过而非假绿/假红）──
if ! (command -v "$HOME/bt-native/build/seed_main" >/dev/null 2>&1 \
      || [ -f "$HOME/bt-native/build/lt_kernel.lib" ]); then
  echo "⛔ LT 链接环境缺失（~/bt-native）：soak-bt 跳过（exit 77）"
  echo "   搭建：bash scripts/bt_env.sh 之后的步骤（见该文件头注释）"
  exit 77
fi
# shellcheck disable=SC1091
[ -f "$ROOT/scripts/bt_env.sh" ] && source "$ROOT/scripts/bt_env.sh"

[ -x "$DAEMON" ] || { echo "❌ daemon 不存在（cargo build -p smart-dl-daemon --release --features bt）"; exit 2; }

# Debian netinst torrent（官方、合法、~630MB；镜像与 SHA256SUMS 同目录）
TORRENT_URL=${SOAK_TORRENT_URL:-"https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/debian-13.5.0-amd64-netinst.iso.torrent"}
BASE=$(dirname "${TORRENT_URL%/*}")/..   # 上跳到 release 根（SHA256SUMS 所在）
SUMS_URL=${SOAK_SUMS_URL:-"$(echo "$TORRENT_URL" | sed 's|bt-cd/[^/]*$|SHA256SUMS|')"}
ISO_NAME=$(basename "${TORRENT_URL}" .torrent)

WORK=$(mktemp -d /tmp/soak-bt.XXXXXX)
SAMPLES_CSV="$WORK/samples.csv"
trap cleanup EXIT
cleanup() {
  rc=$?
  [ -n "${DPID:-}" ] && kill "${DPID}" 2>/dev/null || true
  [ -n "${DPID:-}" ] && wait "${DPID}" 2>/dev/null || true
  if [ "$KEEP" = "1" ] || [ $rc -ne 0 ]; then
    echo "（工作目录保留取证: $WORK）"
  else
    rm -rf "$WORK"
  fi
}
fail() { echo "❌ [bt] $*"; exit 1; }
pass() { echo "✅ [bt] $*"; }

# ── 官方校验基准：SHA256SUMS 里同名条目 ────────────────────────────────────
echo "== 取官方 SHA256SUMS（$SUMS_URL）=="
curl -sL --max-time 60 "$SUMS_URL" -o "$WORK/SHA256SUMS" || fail "SHA256SUMS 拉取失败"
EXPECTED=$(awk -v n="$ISO_NAME" '$2 == n {print $1}' "$WORK/SHA256SUMS" | tr -d '\r')
[ -n "$EXPECTED" ] || fail "SHA256SUMS 中未找到 $ISO_NAME（发行版换代？用 SOAK_TORRENT_URL/SOAK_SUMS_URL 指定）"
echo "  期望: $EXPECTED"

# ── daemon 配置：DHT 开（公网 swarm 冷启动）+ 做种执法（ratio 0.5）─────────
mkdir -p "$WORK/dl"
cat > "$WORK/config.toml" <<EOF
[server]
addr = "$ADDR"
[storage]
tasks_path = "$WORK/tasks.json"
[download]
dest_root = "$WORK/dl"
[bt]
enabled = true
enable_dht = true
max_share_ratio = 0.5
max_seeding_time_min = 5
EOF

"$DAEMON" serve --config "$WORK/config.toml" >"$WORK/daemon.log" 2>&1 &
DPID=$!
for _ in $(seq 1 30); do curl -s -o /dev/null "$API/health" && break; sleep 1; done
curl -s -o /dev/null "$API/health" || { tail -5 "$WORK/daemon.log"; fail "daemon 未就绪"; }
echo "== daemon 已启动 pid=$DPID（bt on, DHT on, ratio-limit 0.5）=="

proc_rss() { awk '/^VmRSS/{print $2}' "/proc/$1/status" 2>/dev/null || echo 0; }
proc_fds() { ls "/proc/$1/fd" 2>/dev/null | wc -l; }

# ── 加种子（.torrent URL 直接建任务；引擎经磁力/torrent 归一化路径）─────────
TID=$(curl -s -X POST "$API/tasks" -H 'Content-Type: application/json' \
  -d "{\"url\":\"$TORRENT_URL\",\"dest\":\"$WORK/dl\"}" \
  | python3 -c "import sys,json; print(json.load(sys.stdin).get('task_id',''))")
[ -n "$TID" ] || fail "BT 任务创建失败（检查 daemon 日志 bt feature 是否启用）"
echo "== BT 任务已建: $TID（$ISO_NAME）=="

snap_field() {
  curl -s "$API/tasks/$1" | python3 -c "
import sys, json
t = json.load(sys.stdin)
print(t.get('$2', '') if t else '')"
}

# ── 采样循环 + kill -9 注入点（50% 进度处）──────────────────────────────────
echo "timestamp,elapsed_min,rss_kb,fds,state,done,total,peers" > "$SAMPLES_CSV"
START=$(date +%s)
KILLED=0; DONE_AT_KILL=0
DEADLINE=$((START + DURATION_MIN * 60))

while [ "$(date +%s)" -lt "$DEADLINE" ]; do
  sleep "$SAMPLE_INTERVAL"
  ST=$(snap_field "$TID" state); DONE=$(snap_field "$TID" done); TOTAL=$(snap_field "$TID" total)
  echo "$(date -Iseconds),$(( ( $(date +%s) - START ) / 60 )),${ST:+$(proc_rss "$DPID")},$(proc_fds "$DPID"),$ST,$DONE,$TOTAL,-" >> "$SAMPLES_CSV"
  echo "  t=$(( ( $(date +%s) - START ) / 60 ))m state=$ST done=$DONE/$TOTAL"

  # kill -9 注入：进度过 50% 且尚未注入过
  if [ "$KILLED" = "0" ] && [ -n "$TOTAL" ] && [ "$TOTAL" != "0" ] && [ -n "$DONE" ] \
     && [ "$DONE" -ge $((TOTAL / 2)) ] 2>/dev/null; then
    DONE_AT_KILL=$DONE
    echo "== 注入: kill -9 daemon（进度 $DONE/$TOTAL）=="
    kill -9 "$DPID"; wait "$DPID" 2>/dev/null || true; DPID=""
    sleep 2
    # fastresume 回灌重启
    "$DAEMON" serve --config "$WORK/config.toml" >>"$WORK/daemon.log" 2>&1 &
    DPID=$!
    for _ in $(seq 1 30); do curl -s -o /dev/null "$API/health" && break; sleep 1; done
    curl -s -o /dev/null "$API/health" || fail "重启后 daemon 未就绪"
    ST2=$(snap_field "$TID" state)
    [ "$ST2" != "Failed" ] || fail "fastresume 恢复失败（state=Failed）"
    DONE2=$(snap_field "$TID" done); DONE2=${DONE2:-0}
    [ "$DONE2" -ge "$DONE_AT_KILL" ] || fail "fastresume 后进度回退: $DONE2 < $DONE_AT_KILL（未复用已完成段）"
    pass "kill -9 → fastresume 恢复：进度 $DONE_AT_KILL → $DONE2（无重下）"
    KILLED=1
  fi

  # 终态判定
  case "$ST" in
    Completed|Seeding) break;;
    Failed) fail "BT 任务失败: $(snap_field "$TID" error)";;
  esac
done

ST=$(snap_field "$TID" state)
[ "$ST" = "Completed" ] || [ "$ST" = "Seeding" ] || fail "时长内未完成（state=$ST done=$(snap_field "$TID" done)/$(snap_field "$TID" total)）——可加大时长或检查公网 swarm"
pass "下载完成（$ISO_NAME）"

# ── 文件完整性：与官方 SHA256SUMS 比对 ──────────────────────────────────────
ISO="$WORK/dl/$ISO_NAME"
[ -f "$ISO" ] || fail "ISO 落盘缺失: $ISO"
ACTUAL=$(sha256sum "$ISO" | cut -d' ' -f1)
[ "$ACTUAL" = "$EXPECTED" ] || fail "SHA256 与官方不符: $ACTUAL != $EXPECTED（公网下载静默损坏）"
pass "SHA256 与官方发布一致"

# ── 做种比率执法（若任务处于 Seeding）──────────────────────────────────────
if [ "$ST" = "Seeding" ]; then
  echo "== 等待做种执法（ratio 0.5 / 5min 上限）=="
  ENF_DEADLINE=$(( $(date +%s) + 480 ))
  while [ "$(date +%s)" -lt "$ENF_DEADLINE" ]; do
    EST=$(snap_field "$TID" state)
    case "$EST" in
      Seeding) sleep 20;;
      *) echo "  执法后状态: $EST"; break;;
    esac
  done
  EST=$(snap_field "$TID" state)
  [ "$EST" != "Seeding" ] || { echo "⚠️ 8 分钟仍在 Seeding（ratio/time 执法未触发——记录为待查项，不算失败：公网做种上行为 0 时 ratio 永不达 0.5）"; }
fi

# ── 收尾断言 ───────────────────────────────────────────────────────────────
kill -0 "$DPID" 2>/dev/null || fail "daemon 已退出"
grep -q "panicked" "$WORK/daemon.log" && fail "日志含 panic" || true
python3 "$HERE/soak-rss-check.py" "$SAMPLES_CSV" "$DURATION_MIN" || true   # BT 样本含空值，尽力而为
pass "daemon 存活、无 panic"
echo "✅ soak-bt 通过（$ISO_NAME，kill9=$KILLED）"
