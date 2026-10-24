#!/usr/bin/env python3
"""soak RSS 趋势检查（#168 原则 2：判「单调增长趋势」，不看绝对值）。

输入：soak-http.sh 产出的 samples.csv（timestamp,elapsed_min,rss_kb,...）
判据：
  1. 样本数 < 5 → 无法判定，跳过（短跑验证场景），不判失败
  2. 对 (elapsed_min, rss_kb) 做最小二乘线性回归：
     - 斜率投影全程增长 = slope × 总时长
     - 投影增长 > baseline 的 20%（且 > 30MB 绝对值，过滤小基数毛刺）→ 失败
  3. 同时报告首尾值、峰值，供人工判读

纯 stdlib（CI 无 numpy 也能跑）。
"""
import csv
import sys


def main(path: str, duration_min: float) -> int:
    rows = []
    with open(path) as f:
        for r in csv.DictReader(f):
            try:
                rows.append((float(r["elapsed_min"]), float(r["rss_kb"])))
            except (ValueError, KeyError):
                pass

    if len(rows) < 5:
        print(f"✅ RSS 检查跳过：样本不足（{len(rows)} < 5，短跑场景）")
        return 0

    # 跳过前 2 个样本（JIT/缓存预热期的毛刺不算趋势）
    pts = rows[2:] if len(rows) > 6 else rows
    n = len(pts)
    sx = sum(p[0] for p in pts)
    sy = sum(p[1] for p in pts)
    sxx = sum(p[0] * p[0] for p in pts)
    sxy = sum(p[0] * p[1] for p in pts)
    denom = n * sxx - sx * sx
    slope = (n * sxy - sx * sy) / denom if denom else 0.0
    baseline = rows[0][1]
    peak = max(p[1] for p in rows)
    last = rows[-1][1]
    projected_kb = slope * max(duration_min, pts[-1][0] or 1)
    growth_pct = projected_kb / baseline * 100 if baseline else 0

    print(f"RSS: baseline={baseline/1024:.1f}MB last={last/1024:.1f}MB "
          f"peak={peak/1024:.1f}MB slope={slope:.1f}KB/min")
    print(f"    全程投影增长: {projected_kb/1024:.1f}MB ({growth_pct:.1f}% of baseline)")

    # 判据：投影增长 > 20% 且绝对量 > 30MB（双条件防小基数误报/大基数漏报）
    if growth_pct > 20 and projected_kb > 30 * 1024:
        print(f"❌ RSS 趋势异常：投影 +{growth_pct:.0f}%（阈值 20%），疑似内存泄漏")
        return 1
    print(f"✅ RSS 趋势正常（投影 {growth_pct:.1f}% ≤ 20%）")
    return 0


if __name__ == "__main__":
    csv_path = sys.argv[1] if len(sys.argv) > 1 else "samples.csv"
    dur = float(sys.argv[2]) if len(sys.argv) > 2 else 60.0
    sys.exit(main(csv_path, dur))
