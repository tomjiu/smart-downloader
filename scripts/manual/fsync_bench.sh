#!/usr/bin/env bash
# fsync 落盘策略基准（PR #114 审查 §3.3 跟进项：fsync 性能开关的量化工具）
#
# 目的：在同一块盘上量化「每段 fsync（P0-4 默认）」vs「批量 fsync」的吞吐差。
# 机械盘（HDD）上预期差距显著（审查实测 SSD 13ms/段 fsync → HDD 吞吐被
# 支配到个位数 MB/s）；SSD/云盘上差距小属正常。
#
# 原理：
#   1. 生成本地测试文件（默认 512MB），python3 http.server 提供 Range 支持
#   2. 用两种 [download] fsync 配置各起一个 daemon（不同端口/数据目录）
#   3. 经 REST API 添加同一 URL 的下载任务，轮询到 Completed
#   4. 打印两种模式的墙钟时间与吞吐
#
# 用法：
#   ./scripts/manual/fsync_bench.sh [大小MB]     # 默认 512
# 前置：
#   - cargo build -p smart-dl-daemon（debug 即可）
#   - python3、curl、jq
#
# 注：段粒度 16MB（DEFAULT_MIN_SPLIT），512MB = 32 段；
#     批量模式默认 8 段一冲 = 4 次 fsync vs 每段模式 32 次。

set -euo pipefail

SIZE_MB="${1:-512}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="${ROOT}/target/debug/smart-dl-daemon"
[ -x "$BIN" ] || BIN="${ROOT}/target/release/smart-dl-daemon"
[ -x "$BIN" ] || { echo "未找到 daemon 二进制（先 cargo build -p smart-dl-daemon）"; exit 1; }
command -v python3 >/dev/null || { echo "需要 python3"; exit 1; }
command -v curl >/dev/null || { echo "需要 curl"; exit 1; }
command -v jq >/dev/null || { echo "需要 jq"; exit 1; }

WORK="$(mktemp -d /tmp/fsync-bench.XXXXXX)"
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$WORK"' EXIT

echo "== fsync 基准：${SIZE_MB}MB 测试文件 =="
python3 - "$WORK/data.bin" "$SIZE_MB" << 'PYEOF'
import sys, os
path, mb = sys.argv[1], int(sys.argv[2])
chunk = bytes((i % 251) for i in range(1024 * 1024))
with open(path, "wb") as f:
    for _ in range(mb):
        f.write(chunk)
PYEOF

(cd "$WORK" && python3 -m http.server 9901 --bind 127.0.0.1 >/dev/null 2>&1) &
SRV_PID=$!
sleep 0.5

URL="http://127.0.0.1:9901/data.bin"

run_mode() {
    local name="$1" per_seg="$2" port="$3"
    local dir="$WORK/daemon-$name"
    mkdir -p "$dir"
    cat > "$dir/config.toml" << EOF
[server]
addr = "127.0.0.1:$port"

[download]
dest_root = "$dir/dl"
fsync_per_segment = $per_seg
EOF
    (cd "$dir" && "$BIN" >/dev/null 2>&1) &
    local dpid=$!
    # 等 health
    for _ in $(seq 1 50); do
        curl -sf "http://127.0.0.1:$port/health" >/dev/null 2>&1 && break
        sleep 0.1
    done
    local t0=$(date +%s.%N)
    curl -sf -X POST "http://127.0.0.1:$port/tasks" \
        -H 'Content-Type: application/json' \
        -d "{\"url\": \"$URL\", \"dest\": \"$dir/dl\"}" | jq -r '.task_id' > "$dir/tid"
    local tid=$(cat "$dir/tid")
    # 轮询完成（上限 10 分钟）
    local deadline=$((SECONDS + 600))
    while :; do
        local st=$(curl -sf "http://127.0.0.1:$port/tasks/$tid" | jq -r '.state // .status // empty')
        case "$st" in
            Completed|completed|Done|done) break ;;
            Error*|error*|Failed*) echo "任务失败: $st"; kill $dpid; exit 1 ;;
        esac
        [ $SECONDS -gt $deadline ] && { echo "超时"; kill $dpid; exit 1; }
        sleep 0.5
    done
    local t1=$(date +%s.%N)
    kill $dpid 2>/dev/null || true
    echo "$t1 $t0" | awk '{printf "%.2f", $1 - $2}'
}

echo "-- 模式 A：每段 fsync（fsync_per_segment = true，P0-4 默认） --"
TA=$(run_mode "per-seg" true 9902)
echo "   耗时 ${TA}s"

echo "-- 模式 B：批量 fsync（fsync_per_segment = false，8 段一冲） --"
TB=$(run_mode "batch" false 9903)
echo "   耗时 ${TB}s"

echo
echo "== 结果（${SIZE_MB}MB / 16MB 段 = $((SIZE_MB / 16)) 段） =="
awk -v a="$TA" -v b="$TB" -v s="$SIZE_MB" 'BEGIN {
    printf "%-14s %10s %12s\n", "模式", "耗时(s)", "吞吐(MB/s)";
    printf "%-14s %10.2f %12.1f\n", "每段 fsync", a, s/a;
    printf "%-14s %10.2f %12.1f\n", "批量 fsync", b, s/b;
    if (a > 0 && b > 0) printf "\n批量加速比: %.2fx\n", a/b;
}'
echo
echo "注：SSD 上差距可能不明显（fsync 本身快）；请在机械盘上运行以复现"
echo "    审查报告的量级（每段 fsync 把 HDD 吞吐支配到个位数 MB/s）。"
