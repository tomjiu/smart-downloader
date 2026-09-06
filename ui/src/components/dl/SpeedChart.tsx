"use client";
/// 速率曲线（SVG）：Catmull-Rom 平滑 + 辉光 + 端点呼吸。
/// 数据由 DlApp 轮询 /stats 累积（down/up 两条序列）。

import { useMemo } from "react";

export interface SpeedPoint {
  down: number;
  up: number;
}

export default function SpeedChart({
  data,
  height = 180,
}: {
  data: SpeedPoint[];
  height?: number;
}) {
  const W = 640;
  const H = height;
  const pad = { l: 44, r: 10, t: 12, b: 20 };

  const { downPath, upPath, areaPath, maxY, lastPoint, ticks } = useMemo(() => {
    const pts = data.slice(-120);
    if (pts.length < 2) {
      return { downPath: "", upPath: "", areaPath: "", maxY: 1, lastPoint: null as null | { x: number; y: number }, ticks: [] as { y: number; label: string }[] };
    }
    const maxYRaw = Math.max(1024, ...pts.map((p) => Math.max(p.down, p.up)));
    const maxY = maxYRaw * 1.15;
    const iw = W - pad.l - pad.r;
    const ih = H - pad.t - pad.b;
    const x = (i: number) => pad.l + (i / (pts.length - 1)) * iw;
    const y = (v: number) => pad.t + ih - (v / maxY) * ih;

    const smooth = (vals: number[]): string => {
      const p = vals.map((v, i) => [x(i), y(v)] as const);
      let d = `M ${p[0][0].toFixed(1)} ${p[0][1].toFixed(1)}`;
      for (let i = 0; i < p.length - 1; i++) {
        const p0 = p[Math.max(0, i - 1)];
        const p1 = p[i];
        const p2 = p[i + 1];
        const p3 = p[Math.min(p.length - 1, i + 2)];
        const c1x = p1[0] + (p2[0] - p0[0]) / 6;
        const c1y = p1[1] + (p2[1] - p0[1]) / 6;
        const c2x = p2[0] - (p3[0] - p1[0]) / 6;
        const c2y = p2[1] - (p3[1] - p1[1]) / 6;
        d += ` C ${c1x.toFixed(1)} ${c1y.toFixed(1)}, ${c2x.toFixed(1)} ${c2y.toFixed(1)}, ${p2[0].toFixed(1)} ${p2[1].toFixed(1)}`;
      }
      return d;
    };

    const down = smooth(pts.map((p) => p.down));
    const up = smooth(pts.map((p) => p.up));
    const area = `${down} L ${x(pts.length - 1).toFixed(1)} ${y(0)} L ${x(0).toFixed(1)} ${y(0)} Z`;

    const tickCount = 4;
    const ticks = Array.from({ length: tickCount + 1 }, (_, i) => {
      const v = (maxY / tickCount) * i;
      return { y: y(v), label: fmtAxis(v) };
    });
    const lastP = pts[pts.length - 1];
    return {
      downPath: down,
      upPath: up,
      areaPath: area,
      maxY,
      lastPoint: { x: x(pts.length - 1), y: y(lastP.down) },
      ticks,
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [data, H]);

  return (
    <svg viewBox={`0 0 ${W} ${H}`} style={{ width: "100%", height: "auto", display: "block" }}>
      <defs>
        <linearGradient id="dl-area" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="var(--accent)" stopOpacity="0.28" />
          <stop offset="100%" stopColor="var(--accent)" stopOpacity="0" />
        </linearGradient>
        <filter id="dl-glow" x="-30%" y="-30%" width="160%" height="160%">
          <feGaussianBlur stdDeviation="3.2" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
      </defs>
      {ticks.map((t, i) => (
        <g key={i}>
          <line
            x1={pad.l}
            x2={W - pad.r}
            y1={t.y}
            y2={t.y}
            stroke="var(--border-subtle)"
            strokeDasharray="3 5"
          />
          <text x={pad.l - 6} y={t.y + 3.5} textAnchor="end" fontSize="10" fill="var(--text-tertiary)">
            {t.label}
          </text>
        </g>
      ))}
      {areaPath && <path d={areaPath} fill="url(#dl-area)" />}
      {downPath && (
        <path
          d={downPath}
          fill="none"
          stroke="var(--accent)"
          strokeWidth="2.2"
          strokeLinecap="round"
          filter="url(#dl-glow)"
        />
      )}
      {upPath && (
        <path
          d={upPath}
          fill="none"
          stroke="var(--info)"
          strokeWidth="1.6"
          strokeDasharray="5 4"
          strokeLinecap="round"
          opacity="0.85"
        />
      )}
      {lastPoint && (
        <circle cx={lastPoint.x} cy={lastPoint.y} r="3.4" fill="var(--accent)">
          <animate attributeName="r" values="2.6;4.4;2.6" dur="1.4s" repeatCount="indefinite" />
          <animate attributeName="opacity" values="1;0.5;1" dur="1.4s" repeatCount="indefinite" />
        </circle>
      )}
    </svg>
  );
}

function fmtAxis(v: number): string {
  if (v >= 1024 * 1024 * 1024) return `${(v / 1024 ** 3).toFixed(1)}G`;
  if (v >= 1024 * 1024) return `${(v / 1024 ** 2).toFixed(0)}M`;
  if (v >= 1024) return `${(v / 1024).toFixed(0)}K`;
  return `${Math.round(v)}`;
}
