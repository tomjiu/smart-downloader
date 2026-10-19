"use client";
/// 密集任务表格（Issue #115 Phase 1 核心，替代 TasksView 卡片网格）。
/// 规格：UI-DESIGN-SPEC.md §5.1 + §1.5 细节规范——
/// - 行高 --row-h: 32px，7 列（名称/状态/进度/速度/大小/剩余/操作）
/// - 无竖线（thead th 只有 border-bottom；tbody 用更弱的 --line-2）
/// - 数字列 tabular-nums；行选中 inset 3px accent 条；悬停只换背景
/// - 单击选中 / 双击打开详情
/// 内建 P0 修复：
/// - P0-2 name 可空兜底 displayName（搜索与渲染共用，绝不含 null）

import { useMemo } from "react";
import type { TaskListItem, TaskRate, TaskSnapshot } from "@/lib/daemon/types";
import { fmtBytes, fmtEta, fmtSpeed } from "@/lib/daemon/format";
import { Icon } from "./Icon";
import { StateBadge } from "./Common";

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

export type TaskAction = "pause" | "resume" | "remove";

export default function TasksTable({
  tasks,
  snapshots,
  rates,
  selected,
  onToggleSelect,
  onToggleAll,
  onSelect,
  onOpen,
  onAction,
  busy,
  query,
}: {
  tasks: TaskListItem[];
  /** 详情快照（DlApp 轮询 /tasks/:id 合并；进度/大小/ETA 数据源） */
  snapshots: Record<string, TaskSnapshot>;
  rates: Record<string, TaskRate>;
  selected: Set<string>;
  onToggleSelect: (id: string) => void;
  onToggleAll: (on: boolean) => void;
  /** 单击行 = 选中（不动已有选集语义：点行切换选中） */
  onSelect: (id: string) => void;
  /** 双击行 / 名称 = 打开详情抽屉 */
  onOpen: (id: string) => void;
  onAction: (id: string, a: TaskAction) => void;
  busy: boolean;
  query: string;
}) {
  const allOn = tasks.length > 0 && tasks.every((t) => selected.has(t.task_id));

  const rows = useMemo(
    () =>
      tasks.map((t) => {
        const snap = snapshots[t.task_id];
        const rate = rates[t.task_id];
        return { t, snap, rate };
      }),
    [tasks, snapshots, rates],
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
    <div className="dl-table-wrap">
      <table className="dl-table" aria-label="任务列表">
        <thead>
          <tr>
            <th className="dl-th-check">
              <input
                type="checkbox"
                aria-label="全选"
                checked={allOn}
                onChange={(e) => onToggleAll(e.target.checked)}
              />
            </th>
            <th className="dl-th-name">名称</th>
            <th>状态</th>
            <th className="dl-th-progress">进度</th>
            <th className="dl-th-num">速度</th>
            <th className="dl-th-num">大小</th>
            <th className="dl-th-num dl-th-eta">剩余</th>
            <th className="dl-th-act" aria-label="操作" />
          </tr>
        </thead>
        <tbody>
          {rows.map(({ t, snap, rate }) => {
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
                onClick={() => onSelect(t.task_id)}
                onDoubleClick={() => onOpen(t.task_id)}
                tabIndex={0}
                onKeyDown={(e) => {
                  if (e.key === "Enter") onOpen(t.task_id);
                  if (e.key === " ") {
                    e.preventDefault();
                    onSelect(t.task_id);
                  }
                }}
              >
                <td className="dl-td-check" onClick={(e) => e.stopPropagation()}>
                  <input
                    type="checkbox"
                    aria-label={`选择 ${displayName(t)}`}
                    checked={isSel}
                    onChange={() => onToggleSelect(t.task_id)}
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
                      {total > 0 ? `${Math.floor(pct)}%` : "—"}
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
                    title="删除"
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
