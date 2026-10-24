"use client";
/// 底部状态栏（Issue #115 Phase 1：28px）。
/// 本期内容：任务计数 · 选中数 · 会话流量。
/// #161 P3：计数改由 /stats 全量聚合派生（by_state 键同
/// TaskListItem.state 口径）——列表可能为服务端过滤子集，不能当
/// 计数源；stats 同拍轮询，新鲜度等同。
/// 注：/health /version /metrics 三条端点后端已就绪（http.rs），UI 侧尚未
/// 接入——是否展示属 #161 待办，而非等后端实现（2026-10-23 核实，防误导）。

import type { Stats } from "@/lib/daemon/types";
import { fmtBytes } from "@/lib/daemon/format";

export default function StatusBar({
  stats,
  selectedCount,
}: {
  stats: Stats | null;
  selectedCount: number;
}) {
  const bs = stats?.by_state ?? {};
  const total = stats?.total ?? 0;
  const seg = (ks: string[]) => ks.reduce((a, k) => a + (bs[k] ?? 0), 0);
  const active = seg(["Downloading", "Seeding", "Transferring", "FallbackProvider"]);
  const done = seg(["Completed", "Stopped", "Seeding"]);
  return (
    <footer className="dl-statusbar" role="contentinfo">
      <span>共 {total} 个任务 · {active} 进行中 · {done} 已完成</span>
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
