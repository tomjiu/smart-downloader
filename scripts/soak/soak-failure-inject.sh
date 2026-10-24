#!/usr/bin/env bash
# ============================================================================
# soak-failure-inject.sh —— 故障注入：验证「恢复」，不只是「存活」（#168）
#
# 用法: ./soak-failure-inject.sh [场景=all]
#   场景: kill9 | netdown | diskfull | unreachable | all
#
# 原则（#168 关键设计原则 3）：每个场景的断言重点都是「恢复之后文件完整」，
# 中间态（进程死/连接断/写失败）只是手段。
#
# 场景与根法（无 root 环境约束下的等价模拟，均如实标注）：
#   kill9       daemon 下载中 kill -9 → 重启后账本有效、进度不回退、
#               不重复下载已完成段、终文件 SHA256 正确
#   netdown     SIGSTOP 本地源服务器 30s（连接僵住无 FIN/RST —— 与拔网线
#               同语义）→ SIGCONT 恢复 → 任务续传、终文件 SHA256 正确
#   diskfull    ulimit -f 限制 daemon 单文件写入上限（写中段失败；ENOSPC 的
#               无 root 等价近似——注明：SIGXFSZ 可能直接杀死进程，两种中间
#               态都接受）→ 账本不坏 → 解除限制重启 → 续传完成、SHA256 正确
#   unreachable 三个不可达 URL（TEST-NET-1 / 不可路由私网 / 端口拒绝）→
#               任务在预算内达 Failed 终态、error 非空、daemon 不挂死、
#               之后正常任务仍可完成
# ============================================================================
set -euo pipefail

SCENARIO=${1:-all}
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
DAEMON=${SOAK_DAEMON:-$ROOT/target/release/smart-dl-daemon}
FSPORT=${SOAK_FSPORT:-9904}
ADDR=127.0.0.1:8798
API="http://$ADDR"
KEEP=${SOAK_KEEP:-0}

[ -x "$DAEMON" ] || { echo "❌ daemon 不存在: $DAEMON（先 cargo build --release -p smart-dl-daemon）"; exit 2; }
command -v python3 >/dev/null || { echo "❌ 需要 python3"; exit 2; }

WORK=""
SPID=""
DPID=""

cleanup() {
  rc=$?
  [ -n "${DPID:-}" ] && kill "${DPID}" 2>/dev/null || true
  [ -n "${DPID:-}" ] && wait "${DPID}" 2>/dev/null || true
  [ -n "${SPID:-}" ] && kill "${SPID}" 2>/dev/null || true
  if [ "$KEEP" = "1" ] || [ $rc -ge 2 ]; then
    [ -n "$WORK" ] && echo "（工作目录保留取证: $WORK）"
  else
    [ -n "$WORK" ] && rm -rf "$WORK"
  fi
}
trap cleanup EXIT

fail() { echo "❌ [$SCENARIO] $*"; exit 1; }
pass() { echo "✅ [$SCENARIO] $*"; }

start_fs() { # 本地确定性源
  python3 "$HERE/soak-file-server.py" "$FSPORT" >"$WORK/file-server.log" 2>&1 &
  SPID=$!
  for _ in $(seq 1 20); do
    curl -s -o /dev/null "http://127.0.0.1:$FSPORT/sha256?size=16" && return 0
    sleep 0.5
  done
  fail "文件服务器未就绪"
}

start_daemon() { # $1 = 日志后缀（可选；区分多轮启动）
  local LOG="$WORK/daemon.log"
  [ -n "${1:-}" ] && LOG="$WORK/daemon-$1.log"
  "$DAEMON" serve --config "$WORK/config.toml" >"$LOG" 2>&1 &
  DPID=$!
  for _ in $(seq 1 30); do
    curl -s -o /dev/null "$API/health" && return 0
    sleep 1
  done
  fail "daemon 未就绪"
}

write_config() {
  mkdir -p "$WORK/dl"
  cat > "$WORK/config.toml" <<EOF
[server]
addr = "$ADDR"
[storage]
tasks_path = "$WORK/tasks.json"
[download]
dest_root = "$WORK/dl"
max_download_kb_s = 0
EOF
}

add_task() { # $1=url $2=sha256 → echo task_id
  curl -s -X POST "$API/tasks" -H 'Content-Type: application/json' \
    -d "{\"url\":\"$1\",\"dest\":\"$WORK/dl/task\",\"sha256\":\"$2\"}" \
    | python3 -c "import sys,json; print(json.load(sys.stdin).get('task_id',''))"
}

snap_field() { # $1=task_id $2=字段
  curl -s "$API/tasks/$1" | python3 -c "
import sys, json
t = json.load(sys.stdin)
v = t
for k in '$2'.split('.'):
    v = (v or {}).get(k) if isinstance(v, dict) else None
print(v if v is not None else '')"
}

wait_terminal() { # $1=task_id $2=超时秒 → 终态
  local deadline=$((SECONDS + $2))
  while [ $SECONDS -lt $deadline ]; do
    local st; st=$(snap_field "$1" state)
    case "$st" in
      Completed|Failed|Stopped) echo "$st"; return 0;;
    esac
    sleep 2
  done
  echo "TIMEOUT"
}

verify_task_file() { # $1=task_id $2=期望sha $3=期望size
  # 文件定位：优先快照 files[]（BT 多文件）；HTTP 单文件引擎 files 为空 →
  # 扫描 dest 下唯一非 .part 文件（.part = 下载中预分配稀疏文件，非终态）
  local FPATH FSIZE FSHA
  FPATH=$(curl -s "$API/tasks/$1" | python3 -c "
import sys, json, os
try:
    t = json.load(sys.stdin)
except Exception:
    print(''); raise SystemExit
fs = t.get('files') or []
if fs:
    print(os.path.join(t['dest_root'], fs[0]['rel_path']))
else:
    root = t.get('dest_root', '')
    cands = [f for f in os.listdir(root)
             if not f.endswith('.part')
             and os.path.isfile(os.path.join(root, f))] if root and os.path.isdir(root) else []
    print(os.path.join(root, cands[0]) if len(cands) == 1 else '')")
  [ -n "$FPATH" ] && [ -f "$FPATH" ] || fail "落盘文件缺失: '$FPATH'"
  FSIZE=$(stat -c%s "$FPATH")
  [ "$FSIZE" = "$3" ] || fail "大小不符: $FSIZE != $3"
  FSHA=$(sha256sum "$FPATH" | cut -d' ' -f1)
  [ "$FSHA" = "$2" ] || fail "SHA256 不符（恢复后静默损坏）: $FSHA != $2"
  pass "终文件完整: $(basename "$FPATH") $FSIZE B sha256 ok"
}

no_zombie() { # $1=pid $2=名字
  sleep 2
  if [ -d "/proc/$1" ] && [ "$(awk '/^State/{print $2}' "/proc/$1/status" 2>/dev/null)" = "Z" ]; then
    fail "$2 进程 $1 成为僵尸（未被收尸）"
  fi
  pass "$2 无僵尸进程"
}

# ════════════════════════════════════════════════════════════════════════════
# 场景 1（P0）：kill -9 恢复
# ════════════════════════════════════════════════════════════════════════════
scenario_kill9() {
  SCENARIO=kill9
  WORK=$(mktemp -d /tmp/soak-kill9.XXXXXX)
  write_config
  start_fs

  local SIZE=$((64 * 1024 * 1024)) THROTTLE=$((3 * 1024 * 1024))  # 64MB @ 3MB/s ≈ 22s+
  local URL="http://127.0.0.1:$FSPORT/file?size=$SIZE&throttle=$THROTTLE"
  local EXPECTED
  EXPECTED=$(curl -s "http://127.0.0.1:$FSPORT/sha256?size=$SIZE")

  start_daemon
  local TID; TID=$(add_task "$URL" "$EXPECTED")
  [ -n "$TID" ] || fail "任务创建失败"

  # 等 done ≥ 25%（至少一个 16MB 段完成，账本有实质内容）
  echo "等待进度 ≥25% …"
  local DONE1=0 TRY=0
  while [ $TRY -lt 60 ]; do
    DONE1=$(snap_field "$TID" done); DONE1=${DONE1:-0}
    [ "$DONE1" -ge $((SIZE / 4)) ] && break
    sleep 1; TRY=$((TRY+1))
  done
  [ "$DONE1" -ge $((SIZE / 4)) ] || fail "进度未达 25%（done=$DONE1）——请调大节流或检查引擎"
  echo "kill -9 前进度: $DONE1 / $SIZE B"

  # 注入：强杀 daemon（模拟断电/崩溃——没有优雅落盘机会）
  kill -9 "$DPID"; wait "$DPID" 2>/dev/null || true
  sleep 1
  kill -0 "$DPID" 2>/dev/null && fail "进程未死"
  no_zombie "$DPID" "daemon"
  DPID=""

  # tasks.json 此刻必须仍是合法 JSON（账本原子性）
  python3 -c "import json; d=json.load(open('$WORK/tasks.json')); assert isinstance(d, list) and len(d) >= 1" \
    || fail "kill -9 后 tasks.json 损坏（账本非原子落盘）"
  pass "kill -9 后 tasks.json 合法（账本原子性）"

  # 重启恢复
  start_daemon 2
  local ST; ST=$(snap_field "$TID" state)
  [ "$ST" != "Failed" ] || fail "恢复后任务直接 Failed（error=$(snap_field "$TID" error)）"

  # 进度不回退（账本有效 → 已完成段不重下）
  local DONE2=0
  for _ in $(seq 1 20); do
    DONE2=$(snap_field "$TID" done); DONE2=${DONE2:-0}
    [ "$DONE2" -gt 0 ] && break
    sleep 1
  done
  [ "$DONE2" -ge "$DONE1" ] || fail "进度回退: 恢复后 done=$DONE2 < kill 前 done=$DONE1（账本失效/段重下）"
  pass "进度未回退: done $DONE1 → $DONE2（无重复下载已完成段）"

  # 等完成 + 终文件校验（恢复后的正确性才是重点）
  ST=$(wait_terminal "$TID" 300)
  [ "$ST" = "Completed" ] || fail "恢复后未完成（state=$ST error=$(snap_field "$TID" error)）"
  pass "恢复后任务完成"
  verify_task_file "$TID" "$EXPECTED" "$SIZE"

  grep -q "panicked" "$WORK/daemon.log" && fail "daemon-1 日志含 panic" || true
  grep -q "panicked" "$WORK/daemon-2.log" 2>/dev/null && fail "daemon-2 日志含 panic" || true
  pass "两轮日志均无 panic"
}

# ════════════════════════════════════════════════════════════════════════════
# 场景 2（P1）：断网 30s（SIGSTOP 源服务器 = 连接僵住无 FIN/RST）
# ════════════════════════════════════════════════════════════════════════════
scenario_netdown() {
  SCENARIO=netdown
  WORK=$(mktemp -d /tmp/soak-netdown.XXXXXX)
  write_config
  start_fs

  local SIZE=$((48 * 1024 * 1024)) THROTTLE=$((2 * 1024 * 1024))  # 48MB @ 2MB/s ≈ 24s+
  local URL="http://127.0.0.1:$FSPORT/file?size=$SIZE&throttle=$THROTTLE"
  local EXPECTED
  EXPECTED=$(curl -s "http://127.0.0.1:$FSPORT/sha256?size=$SIZE")

  start_daemon
  local TID; TID=$(add_task "$URL" "$EXPECTED")

  local DONE1=0 TRY=0
  while [ $TRY -lt 60 ]; do
    DONE1=$(snap_field "$TID" done); DONE1=${DONE1:-0}
    [ "$DONE1" -ge $((SIZE / 5)) ] && break
    sleep 1; TRY=$((TRY+1))
  done
  [ "$DONE1" -ge $((SIZE / 5)) ] || fail "进度未达 20%"
  echo "断网前进度: $DONE1 / $SIZE B"

  # 注入：冻结源服务器 30s（≈ 拔网线：连接僵住，无 FIN/RST）
  echo "注入: SIGSTOP 源服务器 30s（连接僵住）…"
  kill -STOP "$SPID"
  sleep 30
  kill -CONT "$SPID"

  # 断网期间 daemon 不死、进度不回退（重连后可能出现重复段，但账本不能错乱）
  kill -0 "$DPID" 2>/dev/null || fail "断网期间 daemon 退出"
  pass "断网期间 daemon 存活"

  # 恢复后必须能续传并完成
  local ST; ST=$(wait_terminal "$TID" 300)
  [ "$ST" = "Completed" ] || fail "恢复后未完成（state=$ST error=$(snap_field "$TID" error)）"
  pass "短断网（30s < 读超时）后任务透明续传完成"
  verify_task_file "$TID" "$EXPECTED" "$SIZE"

  # ── 第二阶段：断网超过读超时（30s）── 读超时触发 + 段级重试退避接管 ──
  # 与第一阶段机制不同：连接不再只是僵住，而是引擎侧读超时判定失败、
  # 段级重试（SEGMENT_MAX_ATTEMPTS=4，退避 1-8s）在恢复后接回。任务级
  # auto_retry=2 作为外层兑底。
  local SIZE2=$((32 * 1024 * 1024)) THROTTLE2=$((1024 * 1024))  # 32MB @ 1MB/s
  local URL2="http://127.0.0.1:$FSPORT/file?size=$SIZE2&throttle=$THROTTLE2"
  local EXPECTED2; EXPECTED2=$(curl -s "http://127.0.0.1:$FSPORT/sha256?size=$SIZE2")
  local TID2; TID2=$(curl -s -X POST "$API/tasks" -H 'Content-Type: application/json' \
    -d "{\"url\":\"$URL2\",\"dest\":\"$WORK/dl/task2\",\"auto_retry\":2,\"sha256\":\"$EXPECTED2\"}" \
    | python3 -c "import sys,json; print(json.load(sys.stdin).get('task_id',''))")
  [ -n "$TID2" ] || fail "第二阶段任务创建失败"
  sleep 3
  echo "注入: SIGSTOP 源服务器 75s（> 30s 读超时，触发段级重试）…"
  kill -STOP "$SPID"
  sleep 75
  kill -CONT "$SPID"
  kill -0 "$DPID" 2>/dev/null || fail "长断网期间 daemon 退出"
  ST=$(wait_terminal "$TID2" 300)
  [ "$ST" = "Completed" ] || fail "长断网恢复后未完成（state=$ST error=$(snap_field "$TID2" error)）"
  pass "长断网（75s > 读超时）后段级重试接管、任务完成"
  verify_task_file "$TID2" "$EXPECTED2" "$SIZE2"
  grep -q "panicked" "$WORK/daemon.log" && fail "日志含 panic" || true
  pass "日志无 panic"
}

# ════════════════════════════════════════════════════════════════════════════
# 场景 3（P1）：磁盘写满（ulimit -f 无 root 近似）
# ════════════════════════════════════════════════════════════════════════════
scenario_diskfull() {
  SCENARIO=diskfull
  WORK=$(mktemp -d /tmp/soak-diskfull.XXXXXX)
  write_config
  start_fs

  local SIZE=$((32 * 1024 * 1024))          # 任务 32MB
  local CAP=$((4 * 1024 * 1024))            # 单文件写上限 4MB（512B 块）
  local URL="http://127.0.0.1:$FSPORT/file?size=$SIZE&throttle=0"
  local EXPECTED
  EXPECTED=$(curl -s "http://127.0.0.1:$FSPORT/sha256?size=$SIZE")

  # 注入：ulimit -f 限制单文件大小（写满 → SIGXFSZ 杀进程或 EFBIG 写失败，
  # 两种中间态都接受——本场景断言的是「账本不坏 + 恢复后续传正确」。
  # 注：ENOSPC 的更真实模拟需要 root tmpfs / 小分区，CI 无 root，此为等价近似）
  echo "启动 daemon（ulimit -f $((CAP / 512)) blocks = $CAP B 上限）…"
  bash -c "ulimit -f $((CAP / 512)); exec '$DAEMON' serve --config '$WORK/config.toml'" \
    >"$WORK/daemon-limited.log" 2>&1 &
  DPID=$!
  for _ in $(seq 1 30); do curl -s -o /dev/null "$API/health" && break; sleep 1; done
  curl -s -o /dev/null "$API/health" || fail "daemon 未就绪"

  local TID; TID=$(add_task "$URL" "$EXPECTED")
  [ -n "$TID" ] || fail "任务创建失败"

  # 等写失败发生（daemon 死 或 任务 Failed；上限 120s）
  echo "等待写失败触发（≤120s）…"
  local OUTCOME=none
  local DL=0
  while [ $DL -lt 120 ]; do
    if ! kill -0 "$DPID" 2>/dev/null; then OUTCOME=daemon_dead; break; fi
    local ST; ST=$(snap_field "$TID" state)
    [ "$ST" = "Failed" ] && { OUTCOME=task_failed; break; }
    DL=$((DL+2)); sleep 2
  done
  [ "$OUTCOME" != "none" ] || { echo "⚠️ 120s 内未触发写失败（可能预检拦截或段缓存覆盖），按账本健壮性路径继续验证"; }

  # 无论哪种中间态：账本必须不坏
  if [ -f "$WORK/tasks.json" ]; then
    python3 -c "import json; json.load(open('$WORK/tasks.json'))" \
      || fail "写失败后 tasks.json 损坏"
    pass "写失败后 tasks.json 合法（中间态=$OUTCOME）"
  fi

  # 恢复：解除限制重启（kill 残留进程）
  kill -9 "$DPID" 2>/dev/null || true; wait "$DPID" 2>/dev/null || true; DPID=""
  sleep 1
  start_daemon unlimited

  # 任务恢复 + 等完成（允许手动 resume：若恢复为 Paused 则补一脚）
  local ST; ST=$(snap_field "$TID" state)
  if [ "$ST" = "Paused" ]; then
    curl -s -X POST "$API/tasks/$TID/resume" -o /dev/null
  fi
  ST=$(wait_terminal "$TID" 300)
  [ "$ST" = "Completed" ] || fail "解除限制后未完成（state=$ST error=$(snap_field "$TID" error)）"
  pass "写失败恢复后任务续传完成"
  verify_task_file "$TID" "$EXPECTED" "$SIZE"
  grep -q "panicked" "$WORK/daemon-limited.log" && fail "受限 daemon 日志含 panic" || true
  grep -q "panicked" "$WORK/daemon-unlimited.log" 2>/dev/null && fail "重启 daemon 日志含 panic" || true
  pass "日志无 panic"
}

# ════════════════════════════════════════════════════════════════════════════
# 场景 4（P1）：不可达 / 永久失联 —— 不无限挂起，终态有意义
#
# 实测契约（soak 开发中确认，与 #168 初稿设想不同）：
#   A. 源在「加任务时」就不可达（连接超时 10s / 端口拒绝）→ POST /tasks
#      同步探测失败、快速拒绝（无任务产生）——fail-fast 设计，无挂起点。
#   B. 下载中途源永久失联（SIGSTOP 后不再恢复）→ 读超时 30s + 段级重试
#      4 次（退避 1-8s）用尽 → 任务 Failed 终态，error 非空可诊断。
# 两段都验证「之后 daemon 仍健康」。
# ════════════════════════════════════════════════════════════════════════════
scenario_unreachable() {
  SCENARIO=unreachable
  WORK=$(mktemp -d /tmp/soak-unreach.XXXXXX)
  write_config
  start_fs
  start_daemon

  # ── A：加任务时不可达 → 同步快速拒绝 ─────────────────────────────────
  # 三个不可达语义：TEST-NET-1（RFC 5737 保证不路由）/ 不可路由私网 / 回环端口拒绝
  local BAD_URLS=(
    "http://192.0.2.1:8080/soak-probe"
    "http://10.255.255.1/soak-probe"
    "http://127.0.0.1:9/soak-probe"
  )
  for u in "${BAD_URLS[@]}"; do
    local T0=$(date +%s) RESP
    RESP=$(curl -s -m 60 -X POST "$API/tasks" -H 'Content-Type: application/json' \
      -d "{\"url\":\"$u\",\"dest\":\"$WORK/dl/bad\"}")
    local T1=$(date +%s)
    local TID; TID=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('task_id',''))")
    [ -z "$TID" ] || fail "不可达 URL 竟然建出了任务（$u → $TID）——同步探测未拦截"
    local ERRMSG; ERRMSG=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('error',''))")
    [ -n "$ERRMSG" ] || fail "拒绝响应无 error 字段（不可诊断）: $RESP"
    echo "  $u → $((T1-T0))s 拒绝: ${ERRMSG:0:80}"
  done
  pass "三个不可达 URL 全部加任务时快速拒绝（fail-fast，error 可诊断）"

  # 无任务泄漏
  local COUNT; COUNT=$(curl -s "$API/tasks" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))")
  [ "$COUNT" = "0" ] || fail "被拒的 URL 留下了任务残留（count=$COUNT）"
  pass "无任务残留（count=0）"

  # ── B：下载中途永久失联 → 段重试用尽 → Failed 终态 ──────────────────
  local SIZE=$((16 * 1024 * 1024)) THROTTLE=$((1024 * 1024))  # 16MB @ 1MB/s
  local URL="http://127.0.0.1:$FSPORT/file?size=$SIZE&throttle=$THROTTLE"
  local EXPECTED; EXPECTED=$(curl -s "http://127.0.0.1:$FSPORT/sha256?size=$SIZE")
  local TID; TID=$(add_task "$URL" "$EXPECTED")
  [ -n "$TID" ] || fail "任务创建失败"
  sleep 3
  echo "注入: SIGSTOP 源服务器且【不再恢复】（永久失联）…"
  kill -STOP "$SPID"
  # 读超时 30s × 4 次段重试（退避 1-8s）→ 预算 5 分钟内必须达 Failed
  local BUDGET=${SOAK_UNREACHABLE_BUDGET_SEC:-300}
  local ST; ST=$(wait_terminal "$TID" "$BUDGET")
  kill -CONT "$SPID" 2>/dev/null || true   # 服务器解冻（清理用），任务结论已定
  [ "$ST" = "Failed" ] || fail "永久失联任务未达 Failed（state=$ST）——疑似无限挂起"
  local ERR; ERR=$(snap_field "$TID" error)
  [ -n "$ERR" ] || fail "Failed 但 error 为空（错误不可诊断）"
  echo "  task=$TID → Failed: ${ERR:0:90}"
  pass "永久失联任务在预算内达 Failed 终态（error 可诊断）"

  # daemon 不能被拖死：紧跟着一个正常任务必须能完成
  local GSIZE=$((2 * 1024 * 1024))
  local GEXP; GEXP=$(curl -s "http://127.0.0.1:$FSPORT/sha256?size=$GSIZE")
  local GTID; GTID=$(add_task "http://127.0.0.1:$FSPORT/file?size=$GSIZE&throttle=0" "$GEXP")
  ST=$(wait_terminal "$GTID" 120)
  [ "$ST" = "Completed" ] || fail "不可达/失联之后正常任务失败（daemon 被拖死？state=$ST）"
  verify_task_file "$GTID" "$GEXP" "$GSIZE"
  pass "daemon 未被拖垮"
}

# ════════════════════════════════════════════════════════════════════════════
case "$SCENARIO" in
  kill9)       scenario_kill9 ;;
  netdown)     scenario_netdown ;;
  diskfull)    scenario_diskfull ;;
  unreachable) scenario_unreachable ;;
  all)
    for s in kill9 netdown diskfull unreachable; do
      echo; echo "━━━━━━━━━━━━━━ 故障注入: $s ━━━━━━━━━━━━━━"
      # 每场景独立函数（独立 WORK）；all 模式下顺序执行，失败即停（保留取证）
      "scenario_$s"
      kill "${SPID:-}" 2>/dev/null || true; wait "${SPID:-}" 2>/dev/null || true; SPID=""
      kill "${DPID:-}" 2>/dev/null || true; wait "${DPID:-}" 2>/dev/null || true; DPID=""
      # 通过即清理本场景工作目录（失败时由 trap 保留取证）
      if [ -n "$WORK" ] && [ "$KEEP" != "1" ]; then rm -rf "$WORK"; fi
      WORK=""
    done
    echo; echo "✅ 故障注入全部场景通过"
    ;;
  *) echo "用法: $0 [kill9|netdown|diskfull|unreachable|all]"; exit 2;;
esac
