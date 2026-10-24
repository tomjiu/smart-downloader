"use client";
/// 底部状态栏（Issue #115 Phase 1：28px）。
/// 本期内容：任务计数 · 选中数 · 会话流量。
/// 注：/health /version /metrics 三条端点后端已就绪（http.rs），UI 侧尚未
/// 接入——是否展示属 #161 待办，而非等后端实现（2026-10-23 核实，防误导）。

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
      {/* 版本号硬编码 v0.2：GET /version 已可用但未接（#161 P2 待办） */}
      <span className="dl-status-dim">v0.2</span>
    </footer>
  );
}
