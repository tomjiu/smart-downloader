"use client";
/// 统计视图：KPI 卡 + 引擎/状态分布 + 速率曲线。

import type { Stats, TaskListItem } from "@/lib/daemon/types";
import { fmtBytes, fmtSpeed } from "@/lib/daemon/format";
import SpeedChart, { type SpeedPoint } from "./SpeedChart";

function StatCard({
  icon,
  tone,
  label,
  value,
  sub,
  fill,
}: {
  icon: string;
  tone: string;
  label: string;
  value: string;
  sub?: string;
  fill?: number;
}) {
  return (
    <div className="dl-card dl-card--hover dl-enter" style={{ "--tone": tone } as React.CSSProperties}>
      <div style={{ display: "flex", gap: 12, alignItems: "center" }}>
        <div className="dl-stat-ico">
          <span className={`qoder-icon qoder-icon--${icon}`} />
        </div>
        <div>
          <div style={{ fontSize: 11, color: "var(--text-tertiary)" }}>{label}</div>
          <div className="dl-stat-num">{value}</div>
          {sub && <div style={{ fontSize: 11, color: "var(--text-tertiary)" }}>{sub}</div>}
        </div>
      </div>
      {fill != null && (
        <div className="dl-bar-track" style={{ marginTop: 12 }}>
          <span className="dl-bar-fill" style={{ width: `${Math.min(100, fill)}%` }} />
        </div>
      )}
    </div>
  );
}

export default function StatsView({
  stats,
  history,
  tasks,
}: {
  stats: Stats | null;
  history: SpeedPoint[];
  tasks: TaskListItem[];
}) {
  const st = stats ?? {
    total: 0,
    by_state: {},
    by_engine: {},
    down_bytes_s: 0,
    up_bytes_s: 0,
    session_down_bytes: 0,
    session_up_bytes: 0,
  };
  const active = ["Downloading", "Seeding", "Transferring", "FallbackProvider"].reduce(
    (a, k) => a + (st.by_state[k] ?? 0),
    0,
  );
  const doneCount = ["Completed", "Seeding"].reduce(
    (a, k) => a + (st.by_state[k] ?? 0),
    0,
  );
  const engineEntries = Object.entries(st.by_engine).filter(([, n]) => n > 0);
  const engineMax = Math.max(1, ...engineEntries.map(([, n]) => n));
  const stateEntries = Object.entries(st.by_state).filter(([, n]) => n > 0);

  return (
    <div className="dl-enter" style={{ display: "grid", gap: 16 }}>
      <div className="dl-grid dl-grid--stats">
        <StatCard icon="cloud-download" tone="var(--accent)" label="总下载速度" value={fmtSpeed(st.down_bytes_s)} sub={`会话累计 ${fmtBytes(st.session_down_bytes)}`} />
        <StatCard icon="arrow-up" tone="var(--info)" label="总上传速度" value={fmtSpeed(st.up_bytes_s)} sub={`会话累计 ${fmtBytes(st.session_up_bytes)}`} />
        <StatCard icon="play" tone="var(--warning)" label="活跃任务" value={String(active)} sub={`共 ${st.total} 个任务`} fill={st.total ? (active / st.total) * 100 : 0} />
        <StatCard icon="database" tone="var(--success)" label="累计完成任务" value={String(doneCount)} sub={`列表共 ${tasks.length} 项`} fill={tasks.length ? (doneCount / tasks.length) * 100 : 0} />
      </div>

      <div className="dl-card">
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 10 }}>
          <span className="qoder-icon qoder-icon--graph" style={{ color: "var(--accent)" }} />
          <b>速率曲线</b>
          <span className="qoder-badge qoder-badge--default">近 2 分钟</span>
          <div style={{ flex: 1 }} />
          <span className="dl-pill dl-pill--down">下行</span>
          <span className="dl-pill dl-pill--up">上行</span>
        </div>
        <SpeedChart data={history} />
      </div>

      <div style={{ display: "grid", gap: 16, gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))" }}>
        <div className="dl-card">
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <span className="qoder-icon qoder-icon--server" style={{ color: "var(--accent)" }} />
            <b>引擎分布</b>
          </div>
          {engineEntries.length === 0 ? (
            <div style={{ color: "var(--text-tertiary)", fontSize: 12 }}>暂无数据</div>
          ) : (
            engineEntries.map(([k, n]) => (
              <div key={k} style={{ marginBottom: 10 }}>
                <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, marginBottom: 4 }}>
                  <span>{k}</span>
                  <span style={{ fontVariantNumeric: "tabular-nums" }}>{n}</span>
                </div>
                <div className="dl-bar-track">
                  <span
                    className="dl-bar-fill"
                    style={{ width: `${(n / engineMax) * 100}%`, "--tone": "var(--accent)" } as React.CSSProperties}
                  />
                </div>
              </div>
            ))
          )}
        </div>
        <div className="dl-card">
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <span className="qoder-icon qoder-icon--pie-chart" style={{ color: "var(--info)" }} />
            <b>状态分布</b>
          </div>
          {stateEntries.length === 0 ? (
            <div style={{ color: "var(--text-tertiary)", fontSize: 12 }}>暂无数据</div>
          ) : (
            <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
              {stateEntries.map(([k, n]) => (
                <span key={k} className="qoder-badge qoder-badge--default">
                  {k}: {n}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
