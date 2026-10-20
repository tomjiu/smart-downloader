"use client";
/// 密集任务表格（Issue #115 Phase 1 核心，替代 TasksView 卡片网格）。
/// 规格：UI-DESIGN-SPEC.md §5.1 + §1.5 细节规范——
/// - 行高 --row-h: 32px，7 列（名称/状态/进度/速度/大小/剩余/操作）
/// - 无竖线（thead th 只有 border-bottom；tbody 用更弱的 --line-2）
/// - 数字列 tabular-nums；行选中 inset 3px accent 条；悬停只换背景
/// 内建 P0 修复：
/// - P0-2 name 可空兜底 displayName（搜索与渲染共用，绝不含 null）
/// 交互打磨批（本次加强）：
/// - 表头排序：三态（默认 → 升 → 降 → 默认），aria-sort + 方向指示
/// - 全选 checkbox indeterminate（部分选中显示横线态）
/// - 行三段选择语义：单击=唯一选中 / Ctrl+单击=切换 / Shift+单击=范围加选
/// - 键盘：↑/↓ 行间移动焦点；Enter 开详情；Space 选中
/// - 速率过期衰减：SSE 断连后 5s 无新事件的速率显示 —（不再残留旧值）

import { useMemo, useRef } from "react";
import type { TaskListItem, TaskRate, TaskSnapshot } from "@/lib/daemon/types";
import { fmtBytes, fmtEta, fmtSpeed } from "@/lib/daemon/format";
import { Icon } from "./Icon";
import { StateBadge, stateLabel } from "./Common";

export function displayName(t: { name?: string | null; source?: string | null; task_id?: string }): string {
  return t.name?.trim() || deriveNameFromSource(t.source) || t.task_id || "";
}

/// 来源串（脱敏 URL）末段派生名——仅展示兜底用（真实派生在引擎 CD 后回填）。
function deriveNameFromSource(source?: string | null): string {
  if (!source) return "";
  try {
    const s = source.trim();
    if (s.startsWith("magnet:")) {
      const m = /[?&]dn=([^&]+)/.exec(s);
      if (m) return decodeURIComponent(m[1]);
      return "磁力任务";
    }
    if (/^https?:\/\//.test(s) || /^ftp/i.test(s) || /^sftp/i.test(s)) {
      const u = new URL(s);
      const last = u.pathname.split("/").filter(Boolean).pop();
      if (last) return decodeURIComponent(last);
      return u.hostname;
    }
    return s.split("/").filter(Boolean).pop() || s;
  } catch {
    return source.slice(0, 48);
  }
}

const RESUMABLE = ["Paused", "Queued", "Evaluating", "Failed", "Stopped"];

/** 速率新鲜度窗口（ms）：超窗视为过期（SSE 断连残留清零）。 */
const RATE_TTL_MS = 5000;

export type TaskAction = "pause" | "resume" | "remove";

export type SortKey = "name" | "state" | "progress" | "speed" | "size" | "eta";
export type SortDir = "asc" | "desc";
export type TableSort = { key: SortKey; dir: SortDir } | null;

const SORTABLE: { key: SortKey; label: string; className?: string }[] = [
  { key: "name", label: "名称", className: "dl-th-name" },
  { key: "state", label: "状态" },
  { key: "progress", label: "进度", className: "dl-th-progress" },
  { key: "speed", label: "速度", className: "dl-th-num" },
  { key: "size", label: "大小", className: "dl-th-num" },
  { key: "eta", label: "剩余", className: "dl-th-num dl-th-eta" },
];

export default function TasksTable({
  tasks,
  snapshots,
  rates,
  selected,
  onToggleSelect,
  onToggleAll,
  onSelect,
  onSelectRange,
  onOpen,
  onAction,
  busy,
  query,
  sort,
  onSort,
}: {
  tasks: TaskListItem[];
  /** 详情快照（DlApp 轮询 /tasks/:id 合并；进度/大小/ETA 数据源） */
  snapshots: Record<string, TaskSnapshot>;
  rates: Record<string, TaskRate>;
  selected: Set<string>;
  onToggleSelect: (id: string) => void;
  onToggleAll: (on: boolean) => void;
  /** 单击行 = 唯一选中（Ctrl/Shift 语义见行 onKeyDown/onClick） */
  onSelect: (id: string) => void;
  /** Shift+单击 = 范围加选（不含剔除——精确剔除用 checkbox） */
  onSelectRange: (ids: string[]) => void;
  /** 双击行 / 名称 = 打开详情抽屉 */
  onOpen: (id: string) => void;
  onAction: (id: string, a: TaskAction) => void;
  busy: boolean;
  query: string;
  /** 当前列排序（null = 默认序：后端提交顺序） */
  sort: TableSort;
  onSort: (s: TableSort) => void;
}) {
  const allOn = tasks.length > 0 && tasks.every((t) => selected.has(t.task_id));
  const someOn = !allOn && tasks.some((t) => selected.has(t.task_id));
  const checkRef = useRef<HTMLInputElement>(null);
  /// Shift 范围选择锚点（最近一次普通/Ctrl 点击的行）。
  const anchorRef = useRef<{ id: string; idx: number } | null>(null);
  // 部分选中 → 原生 indeterminate（CSS 无法表达该态，必须 DOM 属性）
  if (checkRef.current) checkRef.current.indeterminate = someOn;

  /// 行渲染数据——速率双源合并：
  /// 1) SSE speed 事件（新鲜时优先；后端暂无生产者，见 types.ts 注释）
  /// 2) 快照 rates（/tasks/:id 2s 轮询，速率列的现役主数据源）
  /// SSE 源带 5s TTL 衰减（断连残留清零）；列表轮询 1s 一拍驱动重算。
  const now = Date.now();
  const rows = useMemo(
    () =>
      tasks.map((t) => {
        const snap = snapshots[t.task_id];
        const r = rates[t.task_id];
        const sseRate = r && now - r.ts < RATE_TTL_MS ? r : undefined;
        const rate =
          sseRate ??
          (snap?.rates
            ? { down: snap.rates.down_bytes_s, up: snap.rates.up_bytes_s, ts: now }
            : undefined);
        return { t, snap, rate };
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tasks, snapshots, rates],
  );

  const indexOf = (id: string) => rows.findIndex((r) => r.t.task_id === id);

  /** 行键盘：↑/↓ 移焦点（tbody 子序 = 渲染序）；Enter 开详情；Space 选中。 */
  const onRowKeyDown = (e: React.KeyboardEvent<HTMLTableRowElement>, idx: number, id: string) => {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const tbody = e.currentTarget.parentElement;
      if (!tbody) return;
      const next = tbody.children[idx + (e.key === "ArrowDown" ? 1 : -1)] as HTMLElement | undefined;
      next?.focus();
      return;
    }
    if (e.key === "Enter") {
      onOpen(id);
      return;
    }
    if (e.key === " ") {
      e.preventDefault();
      onSelect(id);
    }
  };

  if (tasks.length === 0) {
    // E1：0 任务空状态（表头不显示）
    return (
      <div className="dl-empty">
        <div className="dl-empty-orb">
          <Icon name="cloud-download" size={22} />
        </div>
        <div style={{ fontWeight: 600 }}>
          {query ? "无匹配任务" : "暂无任务"}
        </div>
        <div style={{ fontSize: 12 }}>
          {query ? "换个关键词试试" : "点击工具栏「新建」添加下载链接"}
        </div>
      </div>
    );
  }

  return (
    <div className="dl-table-wrap">
      <table className="dl-table" aria-label="任务列表">
        <thead>
          <tr>
            <th className="dl-th-check">
              <input
                ref={checkRef}
                type="checkbox"
                aria-label={someOn ? "全选（当前部分选中）" : "全选"}
                checked={allOn}
                onChange={(e) => onToggleAll(e.target.checked)}
              />
            </th>
            {SORTABLE.map((c) => {
              const active = sort?.key === c.key;
              const dir = active ? sort.dir : null;
              return (
                <th
                  key={c.key}
                  className={c.className ?? ""}
                  aria-sort={active ? (dir === "asc" ? "ascending" : "descending") : "none"}
                >
                  <button
                    type="button"
                    className="dl-th-sort"
                    title={
                      active
                        ? dir === "asc"
                          ? `按${c.label}升序（点击切换降序）`
                          : `按${c.label}降序（点击恢复默认序）`
                        : `按${c.label}排序`
                    }
                    onClick={() => {
                      if (!active) onSort({ key: c.key, dir: "asc" });
                      else if (dir === "asc") onSort({ key: c.key, dir: "desc" });
                      else onSort(null);
                    }}
                  >
                    {c.label}
                    {active && (
                      <Icon name={dir === "asc" ? "arrow-up" : "arrow-down"} size={10} className="dl-th-sort-ico" />
                    )}
                  </button>
                </th>
              );
            })}
            <th className="dl-th-act" aria-label="操作" />
          </tr>
        </thead>
        <tbody>
          {rows.map(({ t, snap, rate }, i) => {
            const isSel = selected.has(t.task_id);
            const done = snap?.done ?? 0;
            const total = snap?.total ?? 0;
            // E5：total=0（未知）→ 0% 而非 NaN%
            const pct = total > 0 ? (done / total) * 100 : 0;
            const down = rate?.down ?? 0;
            const up = rate?.up ?? 0;
            const resumable = RESUMABLE.includes(t.state);
            return (
              <tr
                key={t.task_id}
                className={isSel ? "is-selected" : ""}
                onClick={(e) => {
                  if (e.shiftKey) {
                    // Shift：以上次锚点为起点范围加选
                    const anchor = anchorRef.current;
                    const cur = indexOf(t.task_id);
                    if (anchor != null && anchor.idx >= 0 && cur >= 0) {
                      const [a, b] = anchor.idx <= cur ? [anchor.idx, cur] : [cur, anchor.idx];
                      onSelectRange(rows.slice(a, b + 1).map((r) => r.t.task_id));
                      return;
                    }
                  }
                  if (e.metaKey || e.ctrlKey) {
                    anchorRef.current = { id: t.task_id, idx: i };
                    onToggleSelect(t.task_id);
                    return;
                  }
                  anchorRef.current = { id: t.task_id, idx: i };
                  onSelect(t.task_id);
                }}
                onDoubleClick={() => onOpen(t.task_id)}
                tabIndex={0}
                onKeyDown={(e) => onRowKeyDown(e, i, t.task_id)}
              >
                <td
                  className="dl-td-check"
                  onClick={(e) => e.stopPropagation()}
                  onDoubleClick={(e) => e.stopPropagation()}
                >
                  <input
                    type="checkbox"
                    aria-label={`选择 ${displayName(t)}`}
                    checked={isSel}
                    onChange={() => {
                      anchorRef.current = { id: t.task_id, idx: i };
                      onToggleSelect(t.task_id);
                    }}
                  />
                </td>
                <td
                  className="dl-td-name"
                  title={displayName(t)}
                  onDoubleClick={(e) => {
                    e.stopPropagation();
                    onOpen(t.task_id);
                  }}
                >
                  {displayName(t)}
                </td>
                <td>
                  <StateBadge state={t.state} />
                </td>
                <td>
                  <div className="dl-td-progress">
                    <span
                      className={`dl-bar dl-bar--${barTone(t.state)}`}
                      style={{ width: `${pct}%` }}
                    />
                    <span className="dl-td-pct">
                      {total > 0 ? `${Math.round(pct)}%` : "—"}
                    </span>
                  </div>
                </td>
                <td className="dl-td-num">
                  {down > 0 ? (
                    <span className="dl-td-speed">
                      <Icon name="arrow-down" size={11} /> {fmtSpeed(down)}
                      {up > 0 && (
                        <>
                          {" "}
                          <Icon name="arrow-up" size={11} /> {fmtSpeed(up)}
                        </>
                      )}
                    </span>
                  ) : (
                    <span className="dl-td-dim">—</span>
                  )}
                </td>
                <td className="dl-td-num">
                  {snap && total > 0 ? `${fmtBytes(done)} / ${fmtBytes(total)}` : "—"}
                </td>
                <td className="dl-td-num dl-td-eta">
                  {total > 0 ? fmtEta(done, total, down) : "—"}
                </td>
                <td className="dl-td-act" onClick={(e) => e.stopPropagation()}>
                  <button
                    className="dl-btn dl-btn--ghost dl-act-btn"
                    disabled={busy}
                    aria-label={resumable ? "恢复" : "暂停"}
                    title={resumable ? "恢复" : "暂停"}
                    onClick={() => onAction(t.task_id, resumable ? "resume" : "pause")}
                  >
                    <Icon name={resumable ? "play" : "pause"} size={13} />
                  </button>
                  <button
                    className="dl-btn dl-btn--ghost dl-act-btn dl-act-del"
                    disabled={busy}
                    aria-label="删除"
                    title="删除（需确认）"
                    onClick={() => onAction(t.task_id, "remove")}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

/// 排序值提取（与表头 SORTABLE 对应）。
export function taskSortValue(t: TaskListItem, snap: TaskSnapshot | undefined, rate: TaskRate | undefined, key: SortKey): number | string {
  switch (key) {
    case "name":
      return displayName(t).toLocaleLowerCase("zh-Hans-CN");
    case "state":
      return stateLabel(t.state);
    case "progress":
      return snap && snap.total > 0 ? snap.done / snap.total : 0;
    case "speed":
      return rate?.down ?? 0;
    case "size":
      return snap?.total ?? 0;
    case "eta":
      // 剩余秒数（无速率/已完成 = Infinity 排最后）
      if (!snap || snap.total <= 0 || snap.done >= snap.total) return Infinity;
      return (snap.total - snap.done) / Math.max(1, rate?.down ?? 0);
  }
}

/// 状态 → 进度条色调（与 StateBadge 口径一致）。
function barTone(state: string): string {
  switch (state) {
    case "Seeding":
    case "Completed":
      return "done";
    case "Failed":
      return "error";
    case "Paused":
    case "Stopped":
      return "paused";
    case "Queued":
    case "Evaluating":
      return "queued";
    default:
      return "run";
  }
}
