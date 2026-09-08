"use client";
/// 通用小件：状态徽章 / 进度条 / 速率胶囊 / 空态。

import { fmtBytes, fmtPct, fmtSpeed } from "@/lib/daemon/format";
const STATE_TONE: Record<string, { label: string; badge: string; bar: string }> = {
  Downloading: { label: "下载中", badge: "qoder-badge--primary", bar: "run" },
  Seeding: { label: "做种中", badge: "qoder-badge--info", bar: "run" },
  Transferring: { label: "传输中", badge: "qoder-badge--primary", bar: "run" },
  FallbackProvider: { label: "云兜底", badge: "qoder-badge--info", bar: "run" },
  Evaluating: { label: "评估中", badge: "qoder-badge--info", bar: "queued" },
  Queued: { label: "排队中", badge: "qoder-badge--default", bar: "queued" },
  Paused: { label: "已暂停", badge: "qoder-badge--warning", bar: "paused" },
  Completed: { label: "已完成", badge: "qoder-badge--success", bar: "done" },
  Stopped: { label: "已停止", badge: "qoder-badge--default", bar: "paused" },
  Failed: { label: "失败", badge: "qoder-badge--error", bar: "error" },
};

export function StateBadge({ state }: { state: string }) {
  const meta = STATE_TONE[state] ?? { label: state, badge: "qoder-badge--default", bar: "" };
  const live = ["Downloading", "Seeding", "Transferring", "FallbackProvider"].includes(state);
  return (
    <span className={`qoder-badge ${meta.badge}`}>
      {live && <span className="dl-live-dot" />}
      {meta.label}
    </span>
  );
}

export function TaskProgress({ task, thin }: { task: { state: string; done: number; total: number }; thin?: boolean }) {
  const meta = STATE_TONE[task.state] ?? { bar: "" };
  const pct = task.total > 0 ? (task.done / task.total) * 100 : 0;
  return (
    <div
      className={`dl-progress ${thin ? "dl-progress--thin" : ""}`}
      role="progressbar"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <span className={`dl-bar dl-bar--${meta.bar || "run"}`} style={{ width: `${pct}%` }} />
    </div>
  );
}

export function SpeedPill({
  dir,
  value,
  live,
}: {
  dir: "down" | "up";
  value: number;
  live?: boolean;
}) {
  if (!value && !live) return <span className="dl-pill">— B/s</span>;
  return (
    <span className={`dl-pill dl-pill--${dir}`}>
      {live && value > 0 && <span className="dl-live-dot" />}
      <span className={`qoder-icon qoder-icon--${dir === "down" ? "arrow-down" : "arrow-up"}`} />
      {fmtSpeed(value)}
    </span>
  );
}

export function EmptyState({ icon, title, hint }: { icon: string; title: string; hint?: string }) {
  return (
    <div className="dl-empty">
      <div className="dl-orb">
        <span className={`qoder-icon qoder-icon--${icon}`} style={{ fontSize: 22 }} />
      </div>
      <div style={{ fontWeight: 600 }}>{title}</div>
      {hint && <div style={{ fontSize: 12 }}>{hint}</div>}
    </div>
  );
}

export function SizeText({ task }: { task: { done: number; total: number } }) {
  return (
    <span style={{ fontVariantNumeric: "tabular-nums", color: "var(--text-secondary)" }}>
      {fmtBytes(task.done)} / {fmtBytes(task.total)} · {fmtPct(task.done, task.total)}
    </span>
  );
}
