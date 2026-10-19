"use client";
/// 通用小件：状态徽章 / 进度条 / 速率胶囊 / 空态。
/// Issue #115 Phase 1：全部改用 dl-* token 体系（旧库 class 已清零）。

import { fmtBytes, fmtPct, fmtSpeed } from "@/lib/daemon/format";
import { Icon } from "./Icon";

const STATE_TONE: Record<string, { label: string; badge: string; bar: string }> = {
  Downloading: { label: "下载中", badge: "dl-badge--primary", bar: "run" },
  Seeding: { label: "做种中", badge: "dl-badge--info", bar: "run" },
  Transferring: { label: "传输中", badge: "dl-badge--primary", bar: "run" },
  FallbackProvider: { label: "云兜底", badge: "dl-badge--info", bar: "run" },
  Evaluating: { label: "评估中", badge: "dl-badge--info", bar: "queued" },
  Queued: { label: "排队中", badge: "", bar: "queued" },
  Paused: { label: "已暂停", badge: "dl-badge--warning", bar: "paused" },
  Completed: { label: "已完成", badge: "dl-badge--success", bar: "done" },
  Stopped: { label: "已停止", badge: "", bar: "paused" },
  Failed: { label: "失败", badge: "dl-badge--error", bar: "error" },
};

export function StateBadge({ state }: { state: string }) {
  const meta = STATE_TONE[state] ?? { label: state, badge: "", bar: "" };
  const live = ["Downloading", "Seeding", "Transferring", "FallbackProvider"].includes(state);
  return (
    <span className={`dl-badge ${meta.badge}`}>
      {live && <span className="dl-live-dot" />}
      {meta.label}
    </span>
  );
}

export function TaskProgress({
  task,
  thin,
}: {
  task: { state: string; done: number; total: number };
  thin?: boolean;
}) {
  const meta = STATE_TONE[task.state] ?? { bar: "" };
  // E5：total=0 → 0% 而非 NaN%
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
  if (!value && !live) return <span className="dl-pill">—</span>;
  return (
    <span className={`dl-pill dl-pill--${dir}`}>
      {live && value > 0 && <span className="dl-live-dot" />}
      <Icon name={dir === "down" ? "arrow-down" : "arrow-up"} size={11} />
      {fmtSpeed(value)}
    </span>
  );
}

export function EmptyState({
  icon,
  title,
  hint,
}: {
  icon: Parameters<typeof Icon>[0]["name"];
  title: string;
  hint?: string;
}) {
  return (
    <div className="dl-empty">
      <div className="dl-empty-orb">
        <Icon name={icon} size={22} />
      </div>
      <div style={{ fontWeight: 600 }}>{title}</div>
      {hint && <div style={{ fontSize: 12 }}>{hint}</div>}
    </div>
  );
}

export function SizeText({ task }: { task: { done: number; total: number } }) {
  return (
    <span style={{ fontVariantNumeric: "tabular-nums", color: "var(--fg-2)" }}>
      {fmtBytes(task.done)} / {fmtBytes(task.total)} · {fmtPct(task.done, task.total)}
    </span>
  );
}
