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
/// - 行三段选择语义：单击=唯一选中 / Ctrl=单击=切换 / Shift+单击=范围加选
/// - 键盘：↑/↓ 行间移动焦点；Enter 开详情；Space 选中
/// - 速率过期衰减：SSE 断连后 5s 无新事件的速率显示 —（不再残留旧值）
/// P2（Issue #115 实施计划 §8）：虚拟滚动——固定行高窗口化，零依赖。
/// 1000+ 任务只渲染可视区 ± overscan（F4）；键盘导航跨窗口时先滚后聚焦。

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { TaskListItem, TaskRate, TaskSnapshot } from "@/lib/daemon/types";
import { fmtBytes, fmtEta, fmtSpeed } from "@/lib/daemon/format";
import { Icon } from "./Icon";
import { StateBadge, stateLabel, QueuedNote } from "./Common";

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

/// P2 虚拟滚动常量：行高与 globals.css 的 --row-h: 32px 同步（固定行高
/// 是窗口化数学的前提——行高变化时两处必须一起改）；OVERSCAN 平衡滚动
/// 快速拖动时的白屏与渲染量。
const ROW_H = 32;
const OVERSCAN = 6;

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
  /// 1) SSE speed 事件（新鲜时优先）
  /// 2) 快照 rates（/tasks/:id 2s 轮询）
  /// SSE 源带 5s TTL 衰减（断连残留清零；DlApp 的过期清理 interval 驱动
  /// rates 变化 → 本 memo 重算）。回落合并不在此处做（旧实现为回落路径
  /// 每次新建对象，击穿行级 memo）——行内计算（P1-5-B）。
  const now = Date.now();
  const rows = useMemo(
    () =>
      tasks.map((t) => {
        const r = rates[t.task_id];
        const sse = r && now - r.ts < RATE_TTL_MS ? r : undefined;
        return { t, snap: snapshots[t.task_id], sse };
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tasks, snapshots, rates],
  );

  const indexOf = (id: string) => rows.findIndex((r) => r.t.task_id === id);
  void indexOf; // 保留给排序/调试用途（行点击已改用 idx 直传）

  /// P2 虚拟滚动：可视窗口 [start, end)。滚动时 rAF 合并更新（高频
  /// scroll 事件不直接 setState）；rows 数量变化时钳回有效区。
  const wrapRef = useRef<HTMLDivElement>(null);
  const [range, setRange] = useState({ start: 0, end: 40 });
  const rafRef = useRef(0);
  const measure = useCallback(() => {
    const el = wrapRef.current;
    if (!el) return;
    const start = Math.max(0, Math.floor(el.scrollTop / ROW_H) - OVERSCAN);
    const vis = Math.ceil(el.clientHeight / ROW_H);
    const end = Math.min(rowsRef.current.length, start + vis + OVERSCAN * 2);
    setRange((r) => (r.start === start && r.end === end ? r : { start, end }));
  }, []);
  const onScroll = useCallback(() => {
    if (rafRef.current) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = 0;
      measure();
    });
  }, [measure]);
  // 初始/尺寸变化/行数变化时重测（含列表缩短后 end 越界回收）
  useEffect(() => {
    measure();
  }, [measure, rows.length]);
  useEffect(() => {
    const el = wrapRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => measure());
    ro.observe(el);
    return () => ro.disconnect();
  }, [measure]);
  const start = Math.min(range.start, Math.max(0, rows.length - 1));
  const end = Math.min(range.end, rows.length);
  const padTop = start * ROW_H;
  const padBottom = Math.max(0, (rows.length - end) * ROW_H);

  /// 键盘导航跨窗口：目标行未渲染时先滚动使其进入窗口，range 更新后
  /// 由下方 effect 重试聚焦（React 渲染完成前 querySelector 拿不到行）。
  const [focusIdx, setFocusIdx] = useState<number | null>(null);
  useEffect(() => {
    if (focusIdx == null) return;
    const row = wrapRef.current?.querySelector<HTMLTableRowElement>(
      `tr[data-idx="${focusIdx}"]`,
    );
    if (row) {
      row.focus();
      setFocusIdx(null);
    }
  }, [focusIdx, range]);

  /// P1-5-B（Issue #127）：行级交互 handlers 稳定化——经 ref 间接层读取
  /// 最新回调（DlApp 传入的内联箭头函数每秒变身份，直接透传会击穿行级
  /// memo）。包装后身份恒定，TaskRow 的浅比较只对数据 props 生效。
  const cbRef = useRef({
    toggle: onToggleSelect,
    select: onSelect,
    range: onSelectRange,
    open: onOpen,
    action: onAction,
  });
  cbRef.current = { toggle: onToggleSelect, select: onSelect, range: onSelectRange, open: onOpen, action: onAction };
  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const rowHandlers = useMemo(
    () => ({
      /** 行点击：Shift=范围加选 / Ctrl=切换 / 单击=唯一选中 */
      click: (e: React.MouseEvent, idx: number, id: string) => {
        if (e.shiftKey) {
          const anchor = anchorRef.current;
          if (anchor != null && anchor.idx >= 0 && idx >= 0) {
            const [a, b] = anchor.idx <= idx ? [anchor.idx, idx] : [idx, anchor.idx];
            cbRef.current.range(rowsRef.current.slice(a, b + 1).map((r) => r.t.task_id));
            return;
          }
        }
        anchorRef.current = { id, idx };
        if (e.metaKey || e.ctrlKey) cbRef.current.toggle(id);
        else cbRef.current.select(id);
      },
      /** checkbox 切换（同步锚点） */
      check: (idx: number, id: string) => {
        anchorRef.current = { id, idx };
        cbRef.current.toggle(id);
      },
      open: (id: string) => cbRef.current.open(id),
      action: (id: string, a: TaskAction) => cbRef.current.action(id, a),
      /** 行键盘：↑/↓ 移焦点（虚拟窗口下先滚后聚，见 focusIdx effect）；
       * Enter 开详情；Space 选中 */
      key: (e: React.KeyboardEvent<HTMLTableRowElement>, idx: number, id: string) => {
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          const t = idx + (e.key === "ArrowDown" ? 1 : -1);
          const total = rowsRef.current.length;
          if (t < 0 || t >= total) return;
          // 目标行不在当前窗口 → 先滚动使其渲染（滚动量对齐窗口中央）
          const el = wrapRef.current;
          if (el) {
            const rowTop = t * ROW_H;
            const rowBottom = rowTop + ROW_H;
            if (rowTop < el.scrollTop) el.scrollTop = rowTop;
            else if (rowBottom > el.scrollTop + el.clientHeight)
              el.scrollTop = rowBottom - el.clientHeight;
          }
          // 窗口内直接聚焦；窗口刚扩展的下一帧由 effect 接力
          const next = wrapRef.current?.querySelector<HTMLTableRowElement>(
            `tr[data-idx="${t}"]`,
          );
          if (next) next.focus();
          else setFocusIdx(t);
          return;
        }
        if (e.key === "Enter") {
          cbRef.current.open(id);
          return;
        }
        if (e.key === " ") {
          e.preventDefault();
          cbRef.current.select(id);
        }
      },
    }),
    [],
  );

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
    <div className="dl-table-wrap" ref={wrapRef} onScroll={onScroll}>
      <table className="dl-table" aria-label="任务列表" aria-rowcount={rows.length}>
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
          {/* P2 虚拟滚动：上下占位行撑出完整滚动高度；窗口内行才渲染 */}
          {padTop > 0 && (
            <tr className="dl-vpad" style={{ height: padTop }} aria-hidden>
              <td colSpan={8} />
            </tr>
          )}
          {rows.slice(start, end).map(({ t, snap, sse }, vi) => {
            const i = start + vi;
            return (
              <TaskRow
                key={t.task_id}
                t={t}
                snap={snap}
                sse={sse}
                isSel={selected.has(t.task_id)}
                idx={i}
                busy={busy}
                h={rowHandlers}
              />
            );
          })}
          {padBottom > 0 && (
            <tr className="dl-vpad" style={{ height: padBottom }} aria-hidden>
              <td colSpan={8} />
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

/// P1-5-B（Issue #127）：行组件 memo——数据 props 全部引用稳定
/// （DlApp 侧任务/快照引用复用；SSE 速率对象按事件存在），内容未变的行
/// 在整树重渲染时被浅比较跳过（1000 任务 × 每秒轮询 → 仅变更行重渲染）。
/// 速率合并移入行内：SSE 新鲜值优先，否则回落快照 rates——旧实现在外层
/// useMemo 里为回落路径每次新建对象，会击穿 memo。
type RowHandlers = {
  click: (e: React.MouseEvent, idx: number, id: string) => void;
  check: (idx: number, id: string) => void;
  open: (id: string) => void;
  action: (id: string, a: TaskAction) => void;
  key: (e: React.KeyboardEvent<HTMLTableRowElement>, idx: number, id: string) => void;
};

const TaskRow = memo(function TaskRow({
  t, snap, sse, isSel, idx, busy, h,
}: {
  t: TaskListItem;
  snap?: TaskSnapshot;
  sse?: TaskRate;
  isSel: boolean;
  idx: number;
  busy: boolean;
  h: RowHandlers;
}) {
  const done = snap?.done ?? 0;
  const total = snap?.total ?? 0;
  // E5：total=0（未知）→ 0% 而非 NaN%
  const pct = total > 0 ? (done / total) * 100 : 0;
  const down = sse?.down ?? snap?.rates?.down_bytes_s ?? 0;
  const up = sse?.up ?? snap?.rates?.up_bytes_s ?? 0;
  const resumable = RESUMABLE.includes(t.state);
  const id = t.task_id;
  return (
    <tr
      className={isSel ? "is-selected" : ""}
      data-testid="task-row"
      data-idx={idx}
      data-task-state={t.state}
      onClick={(e) => h.click(e, idx, id)}
      onDoubleClick={() => h.open(id)}
      tabIndex={0}
      onKeyDown={(e) => h.key(e, idx, id)}
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
          onChange={() => h.check(idx, id)}
        />
      </td>
      <td
        className="dl-td-name"
        data-testid="task-name"
        title={displayName(t)}
        onDoubleClick={(e) => {
          e.stopPropagation();
          h.open(id);
        }}
      >
        {displayName(t)}
      </td>
      <td data-testid="task-state">
        <StateBadge state={t.state} />
        <QueuedNote t={t} />
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
      <td className="dl-td-num" data-testid="task-speed">
        {/* #147 §一核查补修：旧门 down>0 会把「仅上传」的做种任务速率列
            恒置为 ———数据双路（SSE 事件 + 快照回落）都健康时仍不可见，
            #146 只修了数据侧（快照引用比较），渲染门是另一半根因。
            现改为 down>0 || up>0：下载段仅 down>0 时渲染（保持原样），
            做种任务只显 ↑ 上传速率。 */}
        {down > 0 || up > 0 ? (
          <span className="dl-td-speed">
            {down > 0 && (
              <>
                <Icon name="arrow-down" size={11} /> {fmtSpeed(down)}
                {up > 0 && " "}
              </>
            )}
            {up > 0 && (
              <>
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
          data-testid="pause-resume-btn"
          disabled={busy}
          aria-label={resumable ? "恢复" : "暂停"}
          title={resumable ? "恢复" : "暂停"}
          onClick={() => h.action(id, resumable ? "resume" : "pause")}
        >
          <Icon name={resumable ? "play" : "pause"} size={13} />
        </button>
        <button
          className="dl-btn dl-btn--ghost dl-act-btn dl-act-del"
          data-testid="delete-btn"
          disabled={busy}
          aria-label="删除"
          title="删除（需确认）"
          onClick={() => h.action(id, "remove")}
        >
          <Icon name="trash" size={13} />
        </button>
      </td>
    </tr>
  );
});

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
