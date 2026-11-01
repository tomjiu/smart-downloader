"use client";
/// 视图工具栏（2026-10-19 布局重设计）：只承载当前视图的**动作**，
/// 不再承担导航/品牌/速率/连接（那些归侧栏——导航与动作分离是
/// qBittorrent / Figma / Linear 的通用信息架构）。
/// - 任务视图：新建任务（主按钮）+ 批量 暂停/恢复/删除（空选集禁用，
///   常驻不条件渲染——避免布局跳动，且 disabled 态可被六态检查覆盖）
///   + 搜索（/ 快捷键聚焦，输入态显示清除钮）
/// - 非任务视图：视图标题 + 副标题（保持 48px 行高一致，内容区不跳）
/// 高度 48px、背景同画布 + 底边框（Figma 模式：工具栏属于内容区）。

import type { RefObject } from "react";
import { Icon } from "./Icon";

export type ViewKey = "tasks" | "stats" | "rss" | "logs" | "plugins" | "settings" | "security";

export const TASK_FILTERS = [
  { key: "all", label: "全部" },
  { key: "active", label: "进行中" },
  { key: "queued", label: "排队" },
  { key: "paused", label: "已暂停" },
  { key: "done", label: "已完成" },
  { key: "failed", label: "失败" },
] as const;

export type TaskFilterKey = (typeof TASK_FILTERS)[number]["key"];

const VIEW_TITLES: Record<Exclude<ViewKey, "tasks">, { title: string; sub: string }> = {
  stats: { title: "统计", sub: "全局速率、任务分布与会话累计" },
  rss: { title: "RSS", sub: "订阅源与自动抓取规则" },
  logs: { title: "日志", sub: "daemon 事件流（实时）" },
  plugins: { title: "插件", sub: "云盘 Provider：迅雷/夸克运行态与登录配置" },
  settings: { title: "设置", sub: "主题、连接与引擎参数" },
  security: { title: "安全", sub: "BT 对等连接 IP 封禁表（引擎级黑名单）" },
};

export default function Toolbar({
  view,
  query,
  onQuery,
  selectedCount,
  onBatch,
  onNew,
  adding,
  busy,
  searchInputRef,
}: {
  view: ViewKey;
  query: string;
  onQuery: (q: string) => void;
  selectedCount: number;
  onBatch: (op: "pause" | "resume" | "remove") => void;
  onNew: () => void;
  /** 新建任务条展开态（aria-expanded 数据源） */
  adding: boolean;
  busy: boolean;
  /** 搜索框引用（DlApp 的 "/" 快捷键聚焦目标） */
  searchInputRef: RefObject<HTMLInputElement | null>;
}) {
  return (
    <header className="dl-toolbar" role="banner">
      {view === "tasks" ? (
        <>
          <button
            type="button"
            className="dl-btn dl-btn--primary"
            aria-expanded={adding}
            aria-controls="dl-add-bar"
            title={adding ? "收起新建任务栏" : "展开新建任务栏"}
            data-testid="new-task-btn"
            onClick={onNew}
          >
            <Icon name="plus" size={13} /> 新建任务
          </button>

          <div className="dl-tool-sep" aria-hidden />

          {/* 批量操作（常驻 + 空选集禁用；POST /tasks/batch 已接线） */}
          <div className="dl-tool-group" role="group" aria-label="批量操作">
            <button
              type="button"
              className="dl-tool-btn"
              disabled={busy || selectedCount === 0}
              title={selectedCount === 0 ? "先勾选任务（表格首列）" : `暂停选中的 ${selectedCount} 个任务`}
              onClick={() => onBatch("pause")}
            >
              <Icon name="pause" size={13} />
              <span className="dl-tool-btn-label">暂停</span>
              {selectedCount > 0 && <span className="dl-tool-count">{selectedCount}</span>}
            </button>
            <button
              type="button"
              className="dl-tool-btn"
              disabled={busy || selectedCount === 0}
              title={selectedCount === 0 ? "先勾选任务（表格首列）" : `恢复选中的 ${selectedCount} 个任务`}
              onClick={() => onBatch("resume")}
            >
              <Icon name="play" size={13} />
              <span className="dl-tool-btn-label">恢复</span>
              {selectedCount > 0 && <span className="dl-tool-count">{selectedCount}</span>}
            </button>
            <button
              type="button"
              className="dl-tool-btn dl-tool-btn--danger"
              disabled={busy || selectedCount === 0}
              title={selectedCount === 0 ? "先勾选任务（表格首列）" : `删除选中的 ${selectedCount} 个任务（需确认）`}
              onClick={() => onBatch("remove")}
            >
              <Icon name="trash" size={13} />
              <span className="dl-tool-btn-label">删除</span>
              {selectedCount > 0 && <span className="dl-tool-count">{selectedCount}</span>}
            </button>
          </div>

          <div className="dl-toolbar-spacer" />

          <div className="dl-toolbar-search">
            <Icon name="search" size={13} />
            <input
              ref={searchInputRef}
              className="dl-search-input"
              placeholder="搜索任务… （按 / 聚焦）"
              aria-label="搜索任务"
              data-testid="search-input"
              value={query}
              onChange={(e) => onQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Escape" && query) {
                  e.stopPropagation();
                  onQuery("");
                }
              }}
            />
            {query && (
              <button
                type="button"
                className="dl-search-clear"
                aria-label="清空搜索"
                title="清空搜索"
                onClick={() => onQuery("")}
              >
                <Icon name="close" size={12} />
              </button>
            )}
          </div>
        </>
      ) : (
        <>
          <h1 className="dl-view-title">{VIEW_TITLES[view].title}</h1>
          <span className="dl-view-sub">{VIEW_TITLES[view].sub}</span>
          <div className="dl-toolbar-spacer" />
        </>
      )}
    </header>
  );
}
