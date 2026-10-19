"use client";
/// 顶部工具栏（Issue #115 Phase 1：44px，替代 208px 常驻侧栏）。
/// 布局：品牌 · 视图分段 · 过滤分段 · 搜索 · 速率 · 连接 · 新建 · 批量。
/// 后续批次槽位（UI-REWRITE-PLAN §6）：新建下拉多字段（Batch A）、
/// 状态栏健康/版本（Batch D）—— 本期只做形状 + 现有能力接线。

import type { Stats } from "@/lib/daemon/types";
import { fmtSpeed } from "@/lib/daemon/format";
import { Icon } from "./Icon";

export type ViewKey = "tasks" | "stats" | "rss" | "logs" | "settings";

const VIEWS: { key: ViewKey; label: string; icon: Parameters<typeof Icon>[0]["name"] }[] = [
  { key: "tasks", label: "任务", icon: "list-unordered" },
  { key: "stats", label: "统计", icon: "graph" },
  { key: "rss", label: "RSS", icon: "radio-tower" },
  { key: "logs", label: "日志", icon: "output" },
  { key: "settings", label: "设置", icon: "settings-gear" },
];

export const TASK_FILTERS = [
  { key: "all", label: "全部" },
  { key: "active", label: "进行中" },
  { key: "queued", label: "排队" },
  { key: "paused", label: "已暂停" },
  { key: "done", label: "已完成" },
  { key: "failed", label: "失败" },
] as const;

export type TaskFilterKey = (typeof TASK_FILTERS)[number]["key"];

export default function Toolbar({
  view,
  onView,
  filter,
  onFilter,
  query,
  onQuery,
  stats,
  connected,
  selectedCount,
  onBatch,
  onNew,
  busy,
}: {
  view: ViewKey;
  onView: (v: ViewKey) => void;
  filter: TaskFilterKey;
  onFilter: (f: TaskFilterKey) => void;
  query: string;
  onQuery: (q: string) => void;
  stats: Stats | null;
  connected: boolean;
  selectedCount: number;
  onBatch: (op: "pause" | "resume" | "remove") => void;
  onNew: () => void;
  busy: boolean;
}) {
  return (
    <header className="dl-toolbar" role="banner">
      <div className="dl-brand" aria-label="Smart Downloader">
        <i className="dl-brand-mark" />
        <span className="dl-brand-name">Smart Downloader</span>
      </div>

      {/* 视图分段（原侧栏导航移入工具栏） */}
      <nav className="dl-seg" aria-label="视图切换">
        {VIEWS.map((v) => (
          <button
            key={v.key}
            type="button"
            className={`dl-seg-item ${view === v.key ? "is-on" : ""}`}
            aria-pressed={view === v.key}
            onClick={() => onView(v.key)}
          >
            <Icon name={v.icon} size={13} />
            {v.label}
          </button>
        ))}
      </nav>

      {view === "tasks" && (
        <>
          {/* 过滤分段（仅任务视图） */}
          <div className="dl-seg" role="group" aria-label="状态过滤">
            {TASK_FILTERS.map((f) => (
              <button
                key={f.key}
                type="button"
                className={`dl-seg-item ${filter === f.key ? "is-on" : ""}`}
                aria-pressed={filter === f.key}
                onClick={() => onFilter(f.key)}
              >
                {f.label}
              </button>
            ))}
          </div>

          <div className="dl-toolbar-search">
            <Icon name="search" size={13} />
            <input
              className="dl-search-input"
              placeholder="搜索任务…"
              aria-label="搜索任务"
              value={query}
              onChange={(e) => onQuery(e.target.value)}
            />
          </div>
        </>
      )}

      <div className="dl-toolbar-spacer" />

      {/* 批量操作（Batch A 槽位：POST /tasks/batch 已接线） */}
      {view === "tasks" && selectedCount > 0 && (
        <div className="dl-seg" role="group" aria-label="批量操作">
          <button
            type="button"
            className="dl-seg-item"
            disabled={busy}
            onClick={() => onBatch("pause")}
          >
            批量暂停 ({selectedCount})
          </button>
          <button
            type="button"
            className="dl-seg-item"
            disabled={busy}
            onClick={() => onBatch("resume")}
          >
            批量恢复
          </button>
          <button
            type="button"
            className="dl-seg-item dl-seg-danger"
            disabled={busy}
            onClick={() => onBatch("remove")}
          >
            批量删除
          </button>
        </div>
      )}

      {/* 速率（E4：数值为 0 显示 —） */}
      <span className="dl-stat dl-stat--down" title="全局下载速率">
        <Icon name="arrow-down" size={12} />
        {stats && stats.down_bytes_s > 0 ? fmtSpeed(stats.down_bytes_s) : "—"}
      </span>
      <span className="dl-stat dl-stat--up" title="全局上传速率">
        <Icon name="arrow-up" size={12} />
        {stats && stats.up_bytes_s > 0 ? fmtSpeed(stats.up_bytes_s) : "—"}
      </span>

      <span
        className={`dl-conn ${connected ? "is-ok" : "is-err"}`}
        role="status"
        title={connected ? "已连接 daemon" : "未连接 daemon"}
      >
        <span className="dl-conn-dot" />
        {connected ? "已连接" : "未连接"}
      </span>

      <button type="button" className="dl-btn dl-btn--primary" onClick={onNew}>
        <Icon name="plus" size={13} /> 新建
      </button>
    </header>
  );
}
