"use client";
/// 统计视图：KPI 卡 + 引擎/状态分布 + 速率曲线。
/// 交互打磨批：tone 变量名修正（--warning/--success 不存在 → --warn/--ok，
/// 旧值无效回落 currentColor）；状态分布用中文标签（与表格徽章口径一致）。

import type { GlobalLimits, ProviderStatusDto, Stats } from "@/lib/daemon/types";
import { fmtBytes, fmtSpeed } from "@/lib/daemon/format";
import SpeedChart, { type SpeedPoint } from "./SpeedChart";
import { Icon, type IconName } from "./Icon";
import { stateLabel } from "./Common";
import { client } from "@/lib/daemon/store";
import { useCallback, useEffect, useState } from "react";

function StatCard({
  icon,
  tone,
  label,
  value,
  sub,
  fill,
}: {
  icon: IconName;
  tone: string;
  label: string;
  value: string;
  sub?: string;
  fill?: number;
}) {
  return (
    <div className="dl-card" style={{ "--tone": tone } as React.CSSProperties}>
      <div style={{ display: "flex", gap: 12, alignItems: "center" }}>
        <div className="dl-stat-ico">
          <Icon name={icon} size={17} />
        </div>
        <div>
          <div style={{ fontSize: 11, color: "var(--fg-3)" }}>{label}</div>
          <div className="dl-stat-num">{value}</div>
          {sub && <div style={{ fontSize: 11, color: "var(--fg-3)" }}>{sub}</div>}
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
}: {
  stats: Stats | null;
  history: SpeedPoint[];
}) {
  // ---- #161 P2：全局限速总阀门 + Provider 状态（视图内自取，SecurityView 模式）----
  const [limits, setLimits] = useState<GlobalLimits | null>(null);
  const [limitsErr, setLimitsErr] = useState<string | null>(null);
  const [downInput, setDownInput] = useState("");
  const [upInput, setUpInput] = useState("");
  const [limitsBusy, setLimitsBusy] = useState(false);
  const [providers, setProviders] = useState<ProviderStatusDto[] | null>(null);

  const loadLimits = useCallback(async () => {
    try {
      setLimitsErr(null);
      setLimits(await client.globalLimits());
    } catch (e) {
      setLimitsErr(String(e));
    }
  }, []);

  useEffect(() => {
    loadLimits();
    client
      .providers()
      .then(setProviders)
      .catch(() => setProviders([]));
  }, [loadLimits]);

  /// 应用总阀门：空输入 = 不调整该方向；0 = 不限。响应即生效值。
  const applyLimits = async () => {
    setLimitsBusy(true);
    try {
      const g = await client.setGlobalLimits({
        max_download_kb_s: downInput === "" ? null : Number(downInput),
        max_upload_kb_s: upInput === "" ? null : Number(upInput),
      });
      setLimits(g);
      setLimitsErr(null);
    } catch (e) {
      setLimitsErr(String(e));
    } finally {
      setLimitsBusy(false);
    }
  };

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
    <div style={{ display: "grid", gap: 16 }}>
      <div className="dl-grid dl-grid--stats">
        <StatCard icon="cloud-download" tone="var(--accent)" label="总下载速度" value={fmtSpeed(st.down_bytes_s)} sub={`会话累计 ${fmtBytes(st.session_down_bytes)}`} />
        <StatCard icon="arrow-up" tone="var(--info)" label="总上传速度" value={fmtSpeed(st.up_bytes_s)} sub={`会话累计 ${fmtBytes(st.session_up_bytes)}`} />
        <StatCard icon="play" tone="var(--warn)" label="活跃任务" value={String(active)} sub={`共 ${st.total} 个任务`} fill={st.total ? (active / st.total) * 100 : 0} />
        <StatCard icon="layers" tone="var(--ok)" label="累计完成任务" value={String(doneCount)} sub={`共 ${st.total} 项任务`} fill={st.total ? (doneCount / st.total) * 100 : 0} />
      </div>

      <div className="dl-card">
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 10 }}>
          <Icon name="graph" size={14} className="dl-sec-ico" />
          <b>速率曲线</b>
          <span className="dl-badge">近 2 分钟</span>
          <div style={{ flex: 1 }} />
          <span className="dl-pill dl-pill--down">下行</span>
          <span className="dl-pill dl-pill--up">上行</span>
        </div>
        <SpeedChart data={history} />
      </div>

      {/* #161 P2：全局限速总阀门（运行时热改，不落盘）+ Provider 运行态可见 */}
      <div style={{ display: "grid", gap: 16, gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))" }}>
        <div className="dl-card">
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <Icon name="speed" size={14} className="dl-sec-ico" />
            <b>全局速率限制</b>
            <span className="dl-badge">运行时</span>
          </div>
          <div style={{ display: "flex", gap: 16, marginBottom: 10, fontSize: 12 }}>
            <span className="dl-pill dl-pill--down" title="所有引擎合计下行上限">
              下行 {limits ? (limits.max_download_kb_s ? `${limits.max_download_kb_s} KiB/s` : "不限") : "…"}
            </span>
            <span className="dl-pill dl-pill--up" title="BT 合计上行上限（HTTP/FTP 无上传）">
              上行 {limits ? (limits.max_upload_kb_s ? `${limits.max_upload_kb_s} KiB/s` : "不限") : "…"}
            </span>
          </div>
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 8 }}>
            <input
              className="dl-input"
              type="number"
              min={0}
              placeholder={`下行 KiB/s（0=不限）`}
              value={downInput}
              onChange={(e) => setDownInput(e.target.value)}
              style={{ width: 130 }}
            />
            <input
              className="dl-input"
              type="number"
              min={0}
              placeholder={`上行 KiB/s（0=不限）`}
              value={upInput}
              onChange={(e) => setUpInput(e.target.value)}
              style={{ width: 130 }}
            />
          </div>
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <button
              className="dl-btn dl-btn--primary"
              disabled={limitsBusy || (downInput === "" && upInput === "")}
              onClick={() => applyLimits()}
            >
              立即生效
            </button>
            <span style={{ fontSize: 11, color: "var(--fg-3)" }}>留空 = 不调整该方向；不落盘，重启回到设置基准</span>
          </div>
          {limitsErr && <div style={{ color: "var(--err)", fontSize: 12, marginTop: 8 }}>{limitsErr}</div>}
        </div>

        <div className="dl-card">
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <Icon name="radio-tower" size={14} className="dl-sec-ico" />
            <b>云盘 Provider</b>
          </div>
          {providers == null ? (
            <div style={{ color: "var(--fg-3)", fontSize: 12 }}>加载中…</div>
          ) : providers.length === 0 ? (
            <div style={{ color: "var(--fg-3)", fontSize: 12 }}>未注册 Provider</div>
          ) : (
            <div style={{ display: "grid", gap: 8 }}>
              {providers.map((p) => {
                const cooling = p.backoff_until != null && p.backoff_until * 1000 > Date.now();
                const state = !p.enabled
                  ? { cls: "dl-badge", txt: "已禁用" }
                  : cooling
                    ? { cls: "dl-badge", txt: `冷却至 ${new Date((p.backoff_until ?? 0) * 1000).toLocaleTimeString()}` }
                    : p.authenticated
                      ? { cls: "dl-badge dl-badge--success", txt: "可用" }
                      : { cls: "dl-badge", txt: "未认证" };
                return (
                  <div key={p.provider} style={{ display: "flex", gap: 8, alignItems: "center", fontSize: 12 }}>
                    <b style={{ minWidth: 64 }}>{p.provider}</b>
                    <span className={state.cls}>{state.txt}</span>
                    <span style={{ flex: 1 }} />
                    <span
                      title="剩余配额（供路由选择：quota>0 才可选）"
                      style={{ fontVariantNumeric: "tabular-nums" }}
                    >
                      配额 {p.quota_remaining > 1e12 ? "充足" : fmtBytes(p.quota_remaining)}
                    </span>
                    <span title={`当前占用并发 ${p.busy}`} style={{ fontVariantNumeric: "tabular-nums" }}>
                      并发 {p.busy}
                    </span>
                    {p.last_error && (
                      <span title={`最近错误：${p.last_error}`} style={{ cursor: "help" }}>
                        ⚠
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>

      <div style={{ display: "grid", gap: 16, gridTemplateColumns: "repeat(auto-fit, minmax(280px, 1fr))" }}>
        <div className="dl-card">
          <div style={{ display: "flex", gap: 8, alignItems: "center", marginBottom: 12 }}>
            <Icon name="server" size={14} className="dl-sec-ico" />
            <b>引擎分布</b>
          </div>
          {engineEntries.length === 0 ? (
            <div style={{ color: "var(--fg-3)", fontSize: 12 }}>暂无数据</div>
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
            <Icon name="pie-chart" size={14} className="dl-sec-ico" />
            <b>状态分布</b>
          </div>
          {stateEntries.length === 0 ? (
            <div style={{ color: "var(--fg-3)", fontSize: 12 }}>暂无数据</div>
          ) : (
            <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
              {stateEntries.map(([k, n]) => (
                <span key={k} className="dl-badge">
                  {stateLabel(k)}: {n}
                </span>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
