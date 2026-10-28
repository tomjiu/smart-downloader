"use client";
/// 日志视图：事件流渲染（类型徽章 + 摘要）。
/// 交互打磨批：类型 tab 补 aria-pressed；头部计数（总数/过滤后）；
/// 过滤无匹配时给出明确空态文案。

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
    <div className="dl-page">
      <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginBottom: 12, alignItems: "center" }}>
        <button
          className={`dl-tab ${!type ? "dl-tab--active" : ""}`}
          aria-pressed={!type}
          onClick={() => setType("")}
        >
          全部
          <span className="dl-tab-count">{events.length}</span>
        </button>
        {types.map((t) => (
          <button
            key={t}
            className={`dl-tab ${type === t ? "dl-tab--active" : ""}`}
            aria-pressed={type === t}
            onClick={() => setType(t)}
          >
            {t}
          </button>
        ))}
        <div style={{ flex: 1 }} />
        <span style={{ fontSize: 11, color: "var(--fg-3)", whiteSpace: "nowrap" }}>
          显示 {shown.length} / {events.length} 条（保留最近 500 条）
        </span>
      </div>
      {shown.length === 0 ? (
        <EmptyState
          icon="output"
          title={type ? "该类型暂无事件" : "暂无事件"}
          hint={type ? "切换到其他类型试试" : "事件流经 SSE 实时推送"}
        />
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
                borderBottom: "1px solid var(--line-2)",
                fontSize: 13,
                background: i % 2 ? "var(--bg-3)" : "transparent",
              }}
            >
              <span style={{ color: "var(--fg-3)", fontVariantNumeric: "tabular-nums", flex: "none" }}>
                #{env.seq}
              </span>
              <span className="dl-badge" style={{ flex: "none" }}>
                {String(env.event.type)}
              </span>
              <span style={{ color: "var(--fg-2)", overflow: "hidden", textOverflow: "ellipsis" }}>
                {summarize(env)}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
