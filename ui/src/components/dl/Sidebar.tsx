"use client";
/// 左侧导航栏（2026-10-19 布局重设计，对标 qBittorrent / Linear / VS Code）。
/// 信息架构：导航（结构性，"我在哪"）归侧栏；动作（上下文性，"我能做什么"）
/// 归视图工具栏——上一版把两者混进 44px 顶栏是布局混乱的根因。
/// - 结构：品牌区(48px) → 「任务」状态过滤组（qBittorrent 式计数）
///   → 「数据」组（统计/RSS/日志）→ 底部固定区（速率 · 连接 · 设置）
/// - 折叠：216px ↔ 56px 图标栏，localStorage["dl.side"] 持久化；
///   ≤900px 视口初始自动折叠（一次性判断，不覆写用户已存选择）
/// - 交互：点击过滤项 = 切到任务视图 + 应用该过滤（跨视图直达，
///   在 RSS 页点「进行中」也能一步到位）；aria-current 标记当前位置；
///   图标栏模式靠 title 提供完整文案（计数入 tooltip）
/// 六态覆盖：default/hover/active/focus-visible/disabled(设置无禁用态)/selected(is-on)

import type { Stats } from "@/lib/daemon/types";
import { fmtSpeed } from "@/lib/daemon/format";
import { Icon, type IconName } from "./Icon";
import { TASK_FILTERS, type TaskFilterKey, type ViewKey } from "./Toolbar";

const FILTER_ICONS: Record<TaskFilterKey, IconName> = {
  all: "list-unordered",
  active: "cloud-download",
  queued: "layers",
  paused: "pause",
  done: "check",
  failed: "error",
};

const DATA_VIEWS: { key: Exclude<ViewKey, "tasks" | "settings">; label: string; icon: IconName }[] = [
  { key: "stats", label: "统计", icon: "graph" },
  { key: "rss", label: "RSS", icon: "radio-tower" },
  { key: "logs", label: "日志", icon: "output" },
  { key: "plugins", label: "插件", icon: "zap" },
  { key: "security", label: "安全", icon: "shield" },
];

export default function Sidebar({
  view,
  onView,
  filter,
  onFilter,
  counts,
  stats,
  connected,
  collapsed,
  onToggleCollapsed,
}: {
  view: ViewKey;
  onView: (v: ViewKey) => void;
  filter: TaskFilterKey;
  onFilter: (f: TaskFilterKey) => void;
  /** 过滤分段计数（DlApp 按 tasks 实时计算） */
  counts: Record<TaskFilterKey, number>;
  stats: Stats | null;
  connected: boolean;
  collapsed: boolean;
  onToggleCollapsed: () => void;
}) {
  return (
    <aside className={`dl-side ${collapsed ? "is-rail" : ""}`} id="dl-side" aria-label="主导航">
      {/* 品牌区：图标栏模式下仅留品牌点 + 折叠钮 */}
      <div className="dl-side-brand">
        <i className="dl-brand-mark" />
        {!collapsed && <span className="dl-brand-name">Smart Downloader</span>}
        <button
          type="button"
          className="dl-side-toggle"
          aria-expanded={!collapsed}
          aria-controls="dl-side"
          aria-label={collapsed ? "展开侧栏" : "收起侧栏"}
          title={collapsed ? "展开侧栏" : "收起侧栏"}
          onClick={onToggleCollapsed}
        >
          <Icon name="panel-left" size={14} />
        </button>
      </div>

      <nav className="dl-side-nav" aria-label="视图与过滤">
        {/* 任务状态过滤（qBittorrent 模式：过滤即导航目标） */}
        {!collapsed && <div className="dl-side-group" aria-hidden>任务</div>}
        <ul className="dl-side-list">
          {TASK_FILTERS.map((f) => {
            const on = view === "tasks" && filter === f.key;
            return (
              <li key={f.key}>
                <button
                  type="button"
                  className={`dl-side-item ${on ? "is-on" : ""}`}
                  aria-current={on ? "page" : undefined}
                  title={collapsed ? `${f.label}（${counts[f.key] ?? 0}）` : undefined}
                  onClick={() => {
                    onView("tasks");
                    onFilter(f.key);
                  }}
                >
                  <Icon name={FILTER_ICONS[f.key]} size={15} />
                  {!collapsed && <span className="dl-side-label">{f.label}</span>}
                  {!collapsed && (
                    <span className={`dl-side-count ${on ? "is-on" : ""}`}>{counts[f.key] ?? 0}</span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>

        {/* 数据视图组 */}
        {!collapsed && <div className="dl-side-group" aria-hidden>数据</div>}
        <ul className="dl-side-list">
          {DATA_VIEWS.map((v) => (
            <li key={v.key}>
              <button
                type="button"
                className={`dl-side-item ${view === v.key ? "is-on" : ""}`}
                aria-current={view === v.key ? "page" : undefined}
                title={collapsed ? v.label : undefined}
                onClick={() => onView(v.key)}
              >
                <Icon name={v.icon} size={15} />
                {!collapsed && <span className="dl-side-label">{v.label}</span>}
              </button>
            </li>
          ))}
        </ul>
      </nav>

      {/* 底部固定区：速率（E4：0 显示 —）· 连接 · 设置 */}
      <div className="dl-side-foot">
        <div
          className="dl-side-rates"
          title={collapsed ? `↓ ${fmtRate(stats?.down_bytes_s)} · ↑ ${fmtRate(stats?.up_bytes_s)}` : "全局实时速率"}
        >
          <span className="dl-side-rate dl-side-rate--down" title="全局下载速率">
            <Icon name="arrow-down" size={12} />
            {!collapsed && fmtRate(stats?.down_bytes_s)}
          </span>
          <span className="dl-side-rate dl-side-rate--up" title="全局上传速率">
            <Icon name="arrow-up" size={12} />
            {!collapsed && fmtRate(stats?.up_bytes_s)}
          </span>
        </div>
        <span className={`dl-conn ${connected ? "is-ok" : "is-err"}`} role="status" title={connected ? "已连接 daemon" : "未连接 daemon"}>
          <span className="dl-conn-dot" />
          {!collapsed && (connected ? "已连接" : "未连接")}
        </span>
        <button
          type="button"
          className={`dl-side-item ${view === "settings" ? "is-on" : ""}`}
          aria-current={view === "settings" ? "page" : undefined}
          title={collapsed ? "设置" : undefined}
          onClick={() => onView("settings")}
        >
          <Icon name="settings-gear" size={15} />
          {!collapsed && <span className="dl-side-label">设置</span>}
        </button>
      </div>
    </aside>
  );
}

function fmtRate(v: number | undefined): string {
  return v && v > 0 ? fmtSpeed(v) : "—";
}
