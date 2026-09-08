"use client";
/// 日志视图：事件流渲染（类型徽章 + 摘要）。

import { useMemo, useState } from "react";
import type { SchedulerEventEnvelope } from "@/lib/daemon/types";
import { EmptyState } from "./Common";

function summarize(env: SchedulerEventEnvelope): string {
  const e = env.event as Record<string, unknown>;
  switch (e.type) {
    case "task_created":
      return `任务创建 ${e.task_id ?? ""}`;
    case "state_changed":
      return `任务 ${e.task_id ?? ""}：${e.from} → ${e.to}`;
    case "progress":
      return `进度 ${e.task_id ?? ""}：${e.done_bytes ?? "?"} / ${e.total_bytes ?? "?"}`;
    case "speed":
      return `速率 ${e.task_id ?? ""}：↓${e.down_rate ?? 0} ↑${e.up_rate ?? 0} B/s`;
    case "error":
      return `错误 ${e.task_id ?? ""}：${e.message ?? ""}`;
    case "completed":
      return `完成 ${e.task_id ?? ""}`;
    case "failed":
      return `失败 ${e.task_id ?? ""}：${e.reason ?? e.message ?? ""}`;
    case "global_limits_changed":
      return `全局限速 → ↓${e.max_download_kb_s}KiB/s ↑${e.max_upload_kb_s}KiB/s`;
    case "settings_changed":
      return `设置更新：${Array.isArray(e.keys) ? e.keys.join(", ") : ""}`;
    case "task_activated":
      return `定时任务激活 ${e.task_id ?? ""}`;
    default:
      return JSON.stringify(e).slice(0, 120);
  }
}

export default function LogsView({ events }: { events: SchedulerEventEnvelope[] }) {
  const [type, setType] = useState("");
  const shown = useMemo(
    () => (type ? events.filter((e) => e.event.type === type) : events).slice().reverse(),
    [events, type],
  );
  const types = useMemo(() => Array.from(new Set(events.map((e) => e.event.type))), [events]);

  return (
    <div className="dl-enter">
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 12 }}>
        <button
          className={`qoder-btn qoder-btn--ghost ${!type ? "dl-nav-item--active" : ""}`}
          style={{ borderRadius: 99, padding: "4px 12px" }}
          onClick={() => setType("")}
        >
          全部
        </button>
        {types.map((t) => (
          <button
            key={t}
            className={`qoder-btn qoder-btn--ghost ${type === t ? "dl-nav-item--active" : ""}`}
            style={{ borderRadius: 99, padding: "4px 12px" }}
            onClick={() => setType(t)}
          >
            {t}
          </button>
        ))}
      </div>
      {shown.length === 0 ? (
        <EmptyState icon="output" title="暂无事件" hint="事件流经 SSE 实时推送" />
      ) : (
        <div className="dl-card" style={{ padding: 0, overflow: "hidden" }}>
          {shown.map((env, i) => (
            <div
              key={env.seq}
              style={{
                display: "flex",
                gap: 10,
                alignItems: "baseline",
                padding: "8px 14px",
                borderBottom: "1px solid var(--border-subtle)",
                fontSize: 13,
                background: i % 2 ? "var(--bg-surface)" : "transparent",
              }}
            >
              <span style={{ color: "var(--text-tertiary)", fontVariantNumeric: "tabular-nums", flex: "none" }}>
                #{env.seq}
              </span>
              <span className="qoder-badge qoder-badge--default" style={{ flex: "none" }}>
                {String(env.event.type)}
              </span>
              <span style={{ color: "var(--text-secondary)", overflow: "hidden", textOverflow: "ellipsis" }}>
                {summarize(env)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
