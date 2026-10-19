"use client";
/// 底部状态栏（Issue #115 Phase 1：28px）。
/// 本期内容：任务计数 · 选中数 · 会话流量。
/// Batch D 槽位预留：/health /version /metrics（后续批次填）。

import type { Stats, TaskListItem } from "@/lib/daemon/types";
import { fmtBytes } from "@/lib/daemon/format";

export default function StatusBar({
  tasks,
  stats,
  selectedCount,
}: {
  tasks: TaskListItem[];
  stats: Stats | null;
  selectedCount: number;
}) {
  const active = tasks.filter((t) =>
    ["Downloading", "Seeding", "Transferring", "FallbackProvider"].includes(t.state),
  ).length;
  const done = tasks.filter((t) =>
    ["Completed", "Stopped", "Seeding"].includes(t.state),
  ).length;
  return (
    <footer className="dl-statusbar" role="contentinfo">
      <span>共 {tasks.length} 个任务 · {active} 进行中 · {done} 已完成</span>
      {selectedCount > 0 && <span className="dl-status-sel">已选 {selectedCount} 项</span>}
      <span className="dl-statusbar-spacer" />
      {stats && (
        <span className="dl-status-num" title="本次会话累计流量（估算口径）">
          ↓ {fmtBytes(stats.session_down_bytes)} · ↑ {fmtBytes(stats.session_up_bytes)}
        </span>
      )}
      {/* Batch D 槽位：版本/健康/指标 —— 后续批次填 */}
      <span className="dl-status-dim">v0.2</span>
    </footer>
  );
}
