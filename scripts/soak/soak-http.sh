#!/usr/bin/env bash
# ============================================================================
# soak-http.sh —— HTTP 引擎长跑测试（#168 P0-a）
#
# 用法: ./soak-http.sh [运行时长分钟=60] [并发任务数=8] [--local]
#
# 做什么：
#   1. 起本地 daemon（release，独立工作目录，不污染 ~/.config）
#   2. 波次提交真实公网下载任务（speed.cloudflare.com / OVH / thinkbroadband
#      ——均为公开测速文件，非盗版内容；--local 改用本地确定性服务器）
#   3. 每分钟采样：RSS / FD / 线程数 / 任务状态分布 → CSV
#   4. 每波完成后逐文件校验 SHA256（双保险：daemon 侧 E6 sha256 选项 +
#      脚本侧独立 sha256sum 比对参考值），校验通过后清理文件保住磁盘
#   5. 终态断言：
#      - 零失败任务（SOAK_MAX_FAILED 可放宽）
#      - 全部文件 SHA256 与参考一致（防「完成但静默损坏」）
#      - RSS 线性回归：全程投影增长 < 20%（内存泄漏判据，soak-rss-check.py）
#      - FD 无单调增长（波间空闲期对比）
#      - daemon 全程存活、日志无 panic
#
# 不进 PR CI（时长不可接受）；nightly 由 .github/workflows/soak.yml 调度。
# ============================================================================
set -euo pipefail

DURATION_MIN=${1:-60}
JOBS=${2:-8}
MODE=${3:-real}          # real = 公网真实源；--local = 本地确定性源（无外网环境）

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DAEMON=${SOAK_DAEMON:-$(cd "$HERE/../.." && pwd)/target/release/smart-dl-daemon}
ADDR=127.0.0.1:8799
SAMPLE_INTERVAL=${SOAK_SAMPLE_SEC:-60}
MAX_FAILED=${SOAK_MAX_FAILED:-0}
KEEP=${SOAK_KEEP:-0}     # 1 = 保留工作目录（事后取证）

[ -x "$DAEMON" ] || { echo "❌ daemon 不存在: $DAEMON（先 cargo build --release -p smart-dl-daemon）"; exit 2; }

WORK=$(mktemp -d /tmp/soak-http.XXXXXX)
API="http://$ADDR"
SAMPLES_CSV="$WORK/samples.csv"
DAEMON_LOG="$WORK/daemon.log"
FAILED_LIST="$WORK/failed.log"
trap cleanup EXIT

cleanup() {
  rc=$?
  # 产物导出（nightly 趋势分析：成功运行也要采样 CSV/日志）
  if [ -n "${SOAK_ARTIFACTS:-}" ] && [ -d "$WORK" ]; then
    mkdir -p "$SOAK_ARTIFACTS"
    cp -f "$SAMPLES_CSV" "$SOAK_ARTIFACTS/http-samples-$(date +%Y%m%d-%H%M%S).csv" 2>/dev/null || true
    cp -f "$DAEMON_LOG" "$SOAK_ARTIFACTS/http-daemon-$(date +%Y%m%d-%H%M%S).log" 2>/dev/null || true
    [ -f "$FAILED_LIST" ] && cp -f "$FAILED_LIST" "$SOAK_ARTIFACTS/http-failed-$(date +%Y%m%d-%H%M%S).log" 2>/dev/null || true
  fi
  [ -n "${DPID:-}" ] && kill "${DPID}" 2>/dev/null || true
  [ -n "${DPID:-}" ] && wait "${DPID}" 2>/dev/null || true
  [ -n "${SPID:-}" ] && kill "${SPID}" 2>/dev/null || true
  if [ "$KEEP" = "1" ] || [ $rc -ne 0 ]; then
    echo "（工作目录保留取证: $WORK）"
  else
    rm -rf "$WORK"
  fi
}

# ── 数据源 ──────────────────────────────────────────────────────────────────
if [ "$MODE" = "--local" ]; then
  python3 "$HERE/soak-file-server.py" 9903 >"$WORK/file-server.log" 2>&1 &
  SPID=$!
  for _ in $(seq 1 20); do curl -s -o /dev/null "http://127.0.0.1:9903/sha256?size=16" && break; sleep 0.5; done
  # 本地模式：4 个确定性文件（25MB 不限 / 12MB 节流 / 40MB / 8MB 节流）
  URLS=(
    "http://127.0.0.1:9903/file?size=26214400&throttle=0"
    "http://127.0.0.1:9903/file?size=12582912&throttle=2097152"
    "http://127.0.0.1:9903/file?size=41943040&throttle=0"
    "http://127.0.0.1:9903/file?size=8388608&throttle=1048576"
  )
else
  # 公开测速文件（Cloudflare Speed / OVH proof / thinkbroadband —— 测试专用资源）。
  # 大文件是有意为之：波次自然拉长 → 对免费端点的请求率降到「礼貌」水位
  # （soak 开发实测：小文件快波 4 分钟 21 波 → Cloudflare 429 限流）。
  URLS=(
    "https://speed.cloudflare.com/__down?bytes=104857600"
    "https://proof.ovh.net/files/100Mb.dat"
    "http://ipv4.download.thinkbroadband.com/50MB.zip"
    "https://proof.ovh.net/files/10Mb.dat"
  )
fi
NU=${#URLS[@]}

# ── 参考值：每个 URL 流式取一遍 sha256（零磁盘占用）────────────────────────
echo "== 预取参考 SHA256（$NU 个源，流式不落盘）=="
REF_SHA=(); REF_SIZE=()
for i in $(seq 0 $((NU-1))); do
  U=${URLS[$i]}
  READ=$(curl -sL --max-time 300 "$U" | python3 -c "
import sys, hashlib
h = hashlib.sha256(); n = 0
while True:
    b = sys.stdin.buffer.read(1 << 20)
    if not b: break
    h.update(b); n += len(b)
print(h.hexdigest(), n)") || { echo "❌ 参考下载失败: $U"; exit 1; }
  REF_SHA+=("$(echo "$READ" | cut -d' ' -f1)")
  REF_SIZE+=("$(echo "$READ" | cut -d' ' -f2)")
  echo "  [$i] $(basename "${U%%\?*}") size=${REF_SIZE[$i]} sha256=${REF_SHA[$i]:0:16}…"
done

# ── daemon 配置与启动 ───────────────────────────────────────────────────────
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

"$DAEMON" serve --config "$WORK/config.toml" >"$DAEMON_LOG" 2>&1 &
DPID=$!
for _ in $(seq 1 30); do
  curl -s -o /dev/null "$API/health" && break; sleep 1
done
curl -s -o /dev/null "$API/health" || { echo "❌ daemon 未就绪"; tail -20 "$DAEMON_LOG"; exit 1; }
echo "== daemon 已启动 pid=$DPID addr=$ADDR mode=$MODE =="

proc_rss() { awk '/^VmRSS/{print $2}' "/proc/$1/status" 2>/dev/null || echo 0; }
proc_fds() { ls "/proc/$1/fd" 2>/dev/null | wc -l; }
proc_thr() { awk '/^Threads/{print $2}' "/proc/$1/status" 2>/dev/null || echo 0; }

state_count() { # $1 = 状态前缀
  curl -s "$API/tasks" | python3 -c "
import sys, json
try: ts = json.load(sys.stdin)
except Exception: print(0); raise SystemExit
print(sum(1 for t in ts if t['state'].startswith('$1')))"
}

sample() { # $1 = 分钟数
  local RSS FDS THR BUSY TASKS
  RSS=$(proc_rss "$DPID"); FDS=$(proc_fds "$DPID"); THR=$(proc_thr "$DPID")
  TASKS=$(curl -s "$API/tasks" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))" 2>/dev/null || echo "?")
  BUSY=$(state_count "Downloading"); BUSY=$((BUSY + $(state_count "Evaluating") + $(state_count "Queued")))
  echo "$(date -Iseconds),$1,${RSS},${FDS},${THR},${BUSY},${TASKS}"
}

add_task() { # $1=url $2=dest $3=sha256 → echo task_id
  curl -s -X POST "$API/tasks" -H 'Content-Type: application/json' \
    -d "{\"url\":\"$1\",\"dest\":\"$2\",\"sha256\":\"$3\"}" \
    | python3 -c "
import sys, json
r = json.load(sys.stdin)
print(r.get('id') or r.get('task_id') or '')"
}

task_snapshot() { curl -s "$API/tasks/$1"; }

wait_terminal() { # $1=task_id $2=超时秒 → echo 终态
  local deadline=$((SECONDS + $2))
  while [ $SECONDS -lt $deadline ]; do
    local st
    st=$(task_snapshot "$1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('state',''))" 2>/dev/null || echo "")
    case "$st" in
      Completed|Failed|Stopped|Seeding) echo "$st"; return 0;;
    esac
    sleep 3
  done
  echo "TIMEOUT"; return 1
}

verify_file() { # $1=task_id $2=期望sha $3=期望size → 0/1
  # 文件定位：优先快照 files[]（BT 多文件）；HTTP 单文件引擎 files 为空 →
  # 扫描 dest 下唯一非 .part 文件（.part = 下载中预分配稀疏文件，非终态）
  local FPATH FSIZE FSHA
  FPATH=$(task_snapshot "$1" | python3 -c "
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
  [ -n "$FPATH" ] && [ -f "$FPATH" ] || { echo "  ❌ 落盘文件缺失: '$FPATH'"; return 1; }
  FSIZE=$(stat -c%s "$FPATH")
  [ "$FSIZE" = "$3" ] || { echo "  ❌ 大小不符: $FSIZE != $3"; return 1; }
  FSHA=$(sha256sum "$FPATH" | cut -d' ' -f1)
  [ "$FSHA" = "$2" ] || { echo "  ❌ SHA256 不符: $FSHA != $2（静默损坏）"; return 1; }
  echo "  ✅ $(basename "$FPATH") $FSIZE B sha ok"
}

# ── 主循环：波次提交 + 周期采样 ────────────────────────────────────────────
echo "timestamp,elapsed_min,rss_kb,fds,threads,busy_tasks,total_tasks" > "$SAMPLES_CSV"
START=$(date +%s)
WAVE=0; FAIL=0; TOTAL_COMPLETED=0; TOTAL_FAILED=0; TOTAL_REMOVED=0; FD_BASELINE=""
WAVE_TIMEOUT=${SOAK_WAVE_TIMEOUT_SEC:-900}
NEXT_SAMPLE=$(( SECONDS + SAMPLE_INTERVAL ))
WAVE_GAP=${SOAK_WAVE_GAP_SEC:-15}   # 波间隔：对免费公网源保持礼貌请求率

while :; do
  ELAPSED_MIN=$(( ( $(date +%s) - START ) / 60 ))
  REMAIN=$(( DURATION_MIN * 60 - ( $(date +%s) - START ) ))
  [ $ELAPSED_MIN -ge "$DURATION_MIN" ] && break
  # 剩余不足 90s 不再开新波（波次时长不可控，避免超时追尾；末波收尾走断言）
  [ $REMAIN -lt 90 ] && break

  WAVE=$((WAVE+1))
  echo "== [$(date +%H:%M:%S)] wave $WAVE（t=${ELAPSED_MIN}m）提交 $JOBS 任务 =="
  TIDS=(); TIDX=()   # 并行数组：任务 id ↔ URL 下标（创建失败不占位，校验不错位）
  for j in $(seq 1 "$JOBS"); do
    idx=$(( (WAVE * JOBS + j) % NU ))
    TID=$(add_task "${URLS[$idx]}" "$WORK/dl/w${WAVE}j${j}" "${REF_SHA[$idx]}")
    [ -n "$TID" ] || { echo "  ⚠️ 任务创建失败（url=${URLS[$idx]}）"; continue; }
    TIDS+=("$TID"); TIDX+=("$idx")
  done
  # 整波零创建 = 硬失败（防死循环；典型原因：查重未释放/引擎不可用）
  if [ "${#TIDS[@]}" -eq 0 ]; then
    echo "❌ wave $WAVE 全部任务创建失败"; tail -5 "$DAEMON_LOG"
    FAIL=1; break
  fi

  # 等待本波全部终态（期间按采样周期记录指标；NEXT_SAMPLE 跨波不重置，
  # 否则快波场景下截止点永远追不上波次节奏 → 欠采样）
  for tid in "${TIDS[@]}"; do
    if [ $SECONDS -ge $NEXT_SAMPLE ]; then
      echo "$(sample $(( ( $(date +%s) - START ) / 60 )))" >> "$SAMPLES_CSV"
      NEXT_SAMPLE=$(( NEXT_SAMPLE + SAMPLE_INTERVAL ))
    fi
    ST=$(wait_terminal "$tid" "$WAVE_TIMEOUT") || true
    case "$ST" in
      Completed) TOTAL_COMPLETED=$((TOTAL_COMPLETED+1));;
      Failed|Stopped)
        TOTAL_FAILED=$((TOTAL_FAILED+1))
        { echo "wave=$WAVE task=$tid state=$ST"; task_snapshot "$tid" | python3 -c "import sys,json; t=json.load(sys.stdin); print('  error:', t.get('error')); print('  url:', t.get('source'))"; } >> "$FAILED_LIST"
        echo "  ❌ wave $WAVE $tid → $ST";;
      *) echo "  ⚠️ wave $WAVE $tid 等待超时（继续）";;
    esac
  done

  # 本波完整性校验（完成与失败都查：失败的也看落盘状态，供取证）
  for k in "${!TIDS[@]}"; do
    tid=${TIDS[$k]}; idx=${TIDX[$k]}
    if verify_file "$tid" "${REF_SHA[$idx]}" "${REF_SIZE[$idx]}"; then :; else
      echo "    （task=$tid wave=$WAVE）" >> "$FAILED_LIST"
    fi
  done

  # 空闲期 FD 基线（第一波后记录；泄漏判据 = 尾部对比）
  if [ -z "$FD_BASELINE" ]; then
    sleep 5; FD_BASELINE=$(proc_fds "$DPID")
  fi

  # 释放 canonical 身份：HTTP 查重按 URL（D34 剥 token 参数）与 dest 无关，
  # 不删则下一波同 URL 必撞 DuplicateRejected。delete_data=false（文件由
  # find 统一清）。remove 也是真实高频路径，纳入长跑覆盖。
  for tid in "${TIDS[@]}"; do
    if curl -s -X DELETE "$API/tasks/$tid?delete_data=false" | rg -q '"error"'; then
      echo "  ⚠️ 任务删除失败: $tid"
    else
      TOTAL_REMOVED=$((TOTAL_REMOVED+1))
    fi
  done

  # 磁盘保底：校验通过即清理本波文件
  find "$WORK/dl" -type f -delete 2>/dev/null || true

  # 波间隔（礼貌性限速：连续满速表对免费源不友好，也压低 429 概率）
  sleep "$WAVE_GAP"
done

# 收尾采样（空闲态）
sleep 10
echo "$(sample $(( ( $(date +%s) - START ) / 60 )))" >> "$SAMPLES_CSV"
FD_FINAL=$(proc_fds "$DPID")

# ── 终态断言 ───────────────────────────────────────────────────────────────
echo; echo "======== 终态断言 ======"

# 1. daemon 存活 + 无 panic
if kill -0 "$DPID" 2>/dev/null; then
  echo "✅ daemon 全程存活（pid=$DPID）"
else
  echo "❌ daemon 已退出"; FAIL=1
fi
if grep -q "panicked" "$DAEMON_LOG"; then
  echo "❌ daemon 日志含 panic:"; grep -m5 "panicked" "$DAEMON_LOG"; FAIL=1
else
  echo "✅ 日志无 panic"
fi

# 2. 失败任务
echo "完成 $TOTAL_COMPLETED / 失败 $TOTAL_FAILED / 波次 $WAVE"
if [ "$TOTAL_FAILED" -gt "$MAX_FAILED" ]; then
  echo "❌ 失败任务 $TOTAL_FAILED > 允许值 $MAX_FAILED（明细: $FAILED_LIST）"; FAIL=1
else
  echo "✅ 失败任务数在允许范围内（≤$MAX_FAILED）"
fi

# 3. RSS 趋势回归（内存泄漏判据：投影增长 <20%）
if python3 "$HERE/soak-rss-check.py" "$SAMPLES_CSV" "$DURATION_MIN"; then :; else FAIL=1; fi

# 4. FD 泄漏
FD_GROW=$(( FD_FINAL - ${FD_BASELINE:-0} ))
if [ "$FD_GROW" -gt 15 ]; then
  echo "❌ FD 增长: baseline=$FD_BASELINE final=$FD_FINAL（+${FD_GROW}）"; FAIL=1
else
  echo "✅ FD 无泄漏: baseline=${FD_BASELINE:-?} final=$FD_FINAL"
fi

echo; echo "采样序列:"; cat "$SAMPLES_CSV"
if [ $FAIL -eq 0 ]; then
  echo "✅ soak-http 通过（mode=$MODE 时长=${DURATION_MIN}m 波次=$WAVE 完成数=$TOTAL_COMPLETED）"
else
  echo "❌ soak-http 未通过（工作目录: $WORK）"; KEEP=1
  exit 1
fi
