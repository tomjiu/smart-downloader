"use client";
/// 设置视图（S1 设置面前端）：qBittorrent 式分组 + 即时生效 + 落盘反馈。
/// 数据源：GET/PUT /settings；主题走 data-theme（与 qoder-ui 完全对齐）；
/// 后端连接（地址/token）存 localStorage。

import { useCallback, useEffect, useMemo, useState } from "react";
import type { Settings, ThemeKey } from "@/lib/daemon/types";
import { THEME_KEYS, THEME_META } from "@/lib/daemon/types";
import { client, getBase, getToken, setBase, setToken } from "@/lib/daemon/store";

const WEEKDAYS = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"];

function Effective({ kind }: { kind: "now" | "restart" | "new" }) {
  const map = {
    now: { cls: "dl-effective--now", txt: "立即生效" },
    new: { cls: "", txt: "新任务生效" },
    restart: { cls: "dl-effective--restart", txt: "重启生效" },
  } as const;
  const m = map[kind];
  return <span className={`dl-effective ${m.cls}`}>{m.txt}</span>;
}

function Switch({ on, onChange, label }: { on: boolean; onChange: (v: boolean) => void; label: string }) {
  return (
    <button
      type="button"
      className={`qoder-btn qoder-btn--ghost ${on ? "dl-nav-item--active" : ""}`}
      onClick={() => onChange(!on)}
      style={{ borderRadius: 99, padding: "4px 14px", minWidth: 92 }}
      aria-pressed={on}
      aria-label={label}
    >
      <span className={`qoder-icon qoder-icon--${on ? "check" : "circle-slash"}`} />
      {on ? "开" : "关"}
    </button>
  );
}

export default function SettingsView({ onToast }: { onToast: (t: { kind: string; text: string }) => void }) {
  const [s, setS] = useState<Settings | null>(null);
  const [theme, setTheme] = useState<ThemeKey>("forest-dark");
  const [dirty, setDirty] = useState<Record<string, unknown>>({});
  const [busy, setBusy] = useState(false);
  // 后端连接（本地生效，不入 /settings）
  const [base, setBaseState] = useState("");
  const [token, setTokenState] = useState("");

  const load = useCallback(async () => {
    try {
      setS(await client.settings());
      setDirty({});
    } catch (e) {
      onToast({ kind: "error", text: `加载设置失败：${e}` });
    }
  }, [onToast]);

  useEffect(() => {
    load();
    const savedTheme = (localStorage.getItem("dl.theme") as ThemeKey) || null;
    if (savedTheme && THEME_KEYS.includes(savedTheme)) setTheme(savedTheme);
    setBaseState(getBase());
    setTokenState(getToken());
  }, [load]);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("dl.theme", theme);
  }, [theme]);

  const patch = (domain: string, key: string, value: unknown) => {
    setS((cur) =>
      cur
        ? ({
            ...cur,
            [domain]: { ...(cur as unknown as Record<string, object>)[domain], [key]: value },
          } as Settings)
        : cur,
    );
    setDirty((d) => ({ ...d, [`${domain}.${key}`]: value }));
  };

  const num = (v: string) => (v === "" ? undefined : Number(v) || 0);

  const save = async () => {
    if (Object.keys(dirty).length === 0) return;
    setBusy(true);
    try {
      // 组装域级补丁（后端按域合并应用）
      const body: Record<string, Record<string, unknown>> = {};
      for (const [k, v] of Object.entries(dirty)) {
        const [d, f] = k.split(".");
        (body[d] ??= {})[f] = v;
      }
      const report = await client.putSettings(body, true);
      const parts: string[] = [];
      if (report.applied.length) parts.push(`已应用 ${report.applied.length} 项`);
      if (report.restart_required.length) parts.push(`重启生效 ${report.restart_required.length} 项`);
      parts.push(report.persisted ? "已落盘" : "未落盘（无 --config）");
      onToast({ kind: "success", text: `设置已保存：${parts.join("，")}` });
      await load();
    } catch (e) {
      onToast({ kind: "error", text: `保存失败：${e}` });
    } finally {
      setBusy(false);
    }
  };

  const dirtyCount = useMemo(() => Object.keys(dirty).length, [dirty]);

  if (!s) {
    return (
      <div className="dl-enter dl-card" style={{ color: "var(--text-tertiary)" }}>
        加载设置中…（若持续失败请检查「后端连接」）
      </div>
    );
  }

  return (
    <div className="dl-enter" style={{ display: "grid", gap: 16, maxWidth: 860 }}>
      {/* —— 带宽 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--speed" style={{ color: "var(--accent)" }} /> 带宽
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">
            全局下载限速 (KiB/s) <Effective kind="now" />
          </label>
          <input
            className="qoder-input"
            type="number"
            min={0}
            value={s.bandwidth.max_download_kb_s}
            onChange={(e) => patch("bandwidth", "max_download_kb_s", num(e.target.value) ?? 0)}
          />
          <div className="dl-field-help">所有引擎（HTTP/FTP/BT）合计下行上限；0 = 不限</div>

          <label className="dl-field-label">
            全局上传限速 (KiB/s) <Effective kind="now" />
          </label>
          <input
            className="qoder-input"
            type="number"
            min={0}
            value={s.bandwidth.max_upload_kb_s}
            onChange={(e) => patch("bandwidth", "max_upload_kb_s", num(e.target.value) ?? 0)}
          />
          <div className="dl-field-help">BT 上行合计上限；0 = 不限。当前引擎生效值：↓{s.bandwidth.effective_max_download_kb_s} / ↑{s.bandwidth.effective_max_upload_kb_s} KiB/s</div>
        </div>

        <div style={{ margin: "16px 0 10px", display: "flex", alignItems: "center", gap: 10 }}>
          <b>备用限速调度</b>
          <Switch
            label="备用限速"
            on={s.bandwidth.alt_enabled}
            onChange={(v) => patch("bandwidth", "alt_enabled", v)}
          />
          {s.bandwidth.alt_active_now && (
            <span className="qoder-badge qoder-badge--warning">备用窗口生效中</span>
          )}
        </div>
        {s.bandwidth.alt_enabled && (
          <div className="dl-form-grid dl-enter">
            <label className="dl-field-label">备用下载限速 (KiB/s)</label>
            <input
              className="qoder-input"
              type="number"
              min={0}
              value={s.bandwidth.alt_max_download_kb_s}
              onChange={(e) => patch("bandwidth", "alt_max_download_kb_s", num(e.target.value) ?? 0)}
            />
            <label className="dl-field-label">备用上传限速 (KiB/s)</label>
            <input
              className="qoder-input"
              type="number"
              min={0}
              value={s.bandwidth.alt_max_upload_kb_s}
              onChange={(e) => patch("bandwidth", "alt_max_upload_kb_s", num(e.target.value) ?? 0)}
            />
            <label className="dl-field-label">起止时间（本地时区）</label>
            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <input
                className="qoder-input"
                type="time"
                value={s.bandwidth.alt_from}
                onChange={(e) => patch("bandwidth", "alt_from", e.target.value)}
              />
              <span>→</span>
              <input
                className="qoder-input"
                type="time"
                value={s.bandwidth.alt_to}
                onChange={(e) => patch("bandwidth", "alt_to", e.target.value)}
              />
            </div>
            <div className="dl-field-help">支持跨零点（如 22:00 → 06:00）；窗口内自动切换到备用限速</div>
            <label className="dl-field-label">适用星期</label>
            <div style={{ display: "flex", gap: 4, flexWrap: "wrap" }}>
              {WEEKDAYS.map((w, i) => {
                const on = s.bandwidth.alt_days.includes(i);
                return (
                  <button
                    key={w}
                    type="button"
                    className={`qoder-btn qoder-btn--ghost ${on ? "dl-nav-item--active" : ""}`}
                    style={{ borderRadius: 99, padding: "3px 10px", fontSize: 12 }}
                    onClick={() => {
                      const next = on
                        ? s.bandwidth.alt_days.filter((x) => x !== i)
                        : [...s.bandwidth.alt_days, i].sort();
                      patch("bandwidth", "alt_days", next);
                    }}
                  >
                    {w}
                  </button>
                );
              })}
            </div>
            <div className="dl-field-help">不选任何星期 = 每天适用</div>
          </div>
        )}
      </section>

      {/* —— 连接 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--globe" style={{ color: "var(--accent)" }} /> 连接与代理
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">
            全局代理 <Effective kind="new" />
          </label>
          <input
            className="qoder-input"
            placeholder="http://host:port · socks5://user:pass@host:port · 留空 = 直连"
            value={s.connection.proxy}
            onChange={(e) => patch("connection", "proxy", e.target.value)}
          />
          <div className="dl-field-help">BT 会话立即重放生效；HTTP 逐任务 client 构建时合并（新任务生效，任务级代理优先）</div>

          <label className="dl-field-label">
            BT 监听端口 <Effective kind="now" />
          </label>
          <input
            className="qoder-input"
            type="number"
            min={0}
            max={65535}
            placeholder="0 = 内核默认"
            value={s.connection.bt_listen_port}
            onChange={(e) => patch("connection", "bt_listen_port", num(e.target.value) ?? 0)}
          />
          <div className="dl-field-help">0 = 内核默认（6881 系）；非 BT 构建下该项仅持久化</div>

          <label className="dl-field-label">
            BT 全局连接数上限 <Effective kind="now" />
          </label>
          <input
            className="qoder-input"
            type="number"
            min={0}
            placeholder="0 = 内核默认（200）"
            value={s.connection.bt_max_connections}
            onChange={(e) => patch("connection", "bt_max_connections", num(e.target.value) ?? 0)}
          />
        </div>
      </section>

      {/* —— BitTorrent —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--radio-tower" style={{ color: "var(--accent)" }} /> BitTorrent
          {!s.bittorrent.bt_available && (
            <span className="qoder-badge qoder-badge--warning">当前构建未启用 BT</span>
          )}
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">DHT（去中心化 peer 发现）<Effective kind="now" /></label>
          <Switch label="DHT" on={s.bittorrent.enable_dht} onChange={(v) => patch("bittorrent", "enable_dht", v)} />
          <label className="dl-field-label">LSD（本地网络发现）<Effective kind="now" /></label>
          <Switch label="LSD" on={s.bittorrent.enable_lsd} onChange={(v) => patch("bittorrent", "enable_lsd", v)} />
          <label className="dl-field-label">UPnP / NAT-PMP 端口映射<Effective kind="now" /></label>
          <Switch label="UPnP" on={s.bittorrent.enable_upnp} onChange={(v) => patch("bittorrent", "enable_upnp", v)} />
          <label className="dl-field-label">PEX（peer 交换）<Effective kind="now" /></label>
          <Switch label="PEX" on={s.bittorrent.enable_pex} onChange={(v) => patch("bittorrent", "enable_pex", v)} />
          <label className="dl-field-label">uTP 传输<Effective kind="now" /></label>
          <Switch label="uTP" on={s.bittorrent.enable_utp} onChange={(v) => patch("bittorrent", "enable_utp", v)} />
          <label className="dl-field-label">MSE 加密策略<Effective kind="now" /></label>
          <select
            className="qoder-input"
            value={s.bittorrent.encrypt}
            onChange={(e) => patch("bittorrent", "encrypt", e.target.value)}
          >
            <option value="disable">禁用（纯明文）</option>
            <option value="allow">允许（明文 + 加密）</option>
            <option value="require">强制加密</option>
          </select>
          <div className="dl-field-help">开关热改即时生效（新发现任务对 PEX 全量生效）；私有 tracker 建议关闭 DHT/PEX/LSD</div>
          <label className="dl-field-label">做种分享率上限<Effective kind="now" /></label>
          <input
            className="qoder-input"
            type="number"
            min={0}
            step="0.1"
            placeholder="0 = 不启用"
            value={s.bittorrent.max_share_ratio}
            onChange={(e) => patch("bittorrent", "max_share_ratio", num(e.target.value) ?? 0)}
          />
          <label className="dl-field-label">做种时长上限（分钟）<Effective kind="now" /></label>
          <input
            className="qoder-input"
            type="number"
            min={0}
            placeholder="0 = 不启用"
            value={s.bittorrent.max_seeding_time_min}
            onChange={(e) => patch("bittorrent", "max_seeding_time_min", num(e.target.value) ?? 0)}
          />
          <div className="dl-field-help">qBittorrent 同名能力：做种达到分享率或时长任一上限即自动暂停（可手动恢复）</div>
          <label className="dl-field-label">存储模式（预分配）<Effective kind="restart" /></label>
          <Switch
            label="预分配"
            on={s.bittorrent.storage_allocate}
            onChange={(v) => patch("bittorrent", "storage_allocate", v)}
          />
          <div className="dl-field-help">开 = 新 BT 任务预分配完整文件大小（磁盘占用即时到位、碎片更少）；关 = 稀疏按需增长；fastresume 恢复任务保留原模式</div>
          <label className="dl-field-label">新任务自动追加 tracker<Effective kind="now" /></label>
          <textarea
            className="qoder-input"
            rows={3}
            placeholder={"udp://tracker.example:6969/announce\nhttp://t2.example/announce"}
            value={(s.bittorrent.extra_trackers ?? []).join("\n")}
            onChange={(e) =>
              patch(
                "bittorrent",
                "extra_trackers",
                e.target.value.split("\n").map((x) => x.trim()).filter((x) => x.length > 0),
              )
            }
          />
          <div className="dl-field-help">每行一条；保存后仅对后续新建 BT 任务生效（存量任务在任务详情里管理 tracker）</div>
        </div>
      </section>

      {/* —— 队列 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--layers" style={{ color: "var(--accent)" }} /> 并发队列
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">BT 同时传输上限</label>
          <input className="qoder-input" type="number" min={0} value={s.queue.max_active_bt} onChange={(e) => patch("queue", "max_active_bt", num(e.target.value) ?? 0)} />
          <label className="dl-field-label">HTTP 同时传输上限</label>
          <input className="qoder-input" type="number" min={0} value={s.queue.max_active_http} onChange={(e) => patch("queue", "max_active_http", num(e.target.value) ?? 0)} />
          <label className="dl-field-label">FTP 同时传输上限</label>
          <input className="qoder-input" type="number" min={0} value={s.queue.max_active_ftp} onChange={(e) => patch("queue", "max_active_ftp", num(e.target.value) ?? 0)} />
          <div className="dl-field-help">0 = 不限（禁用排队）；超配额自动排队，槽位空闲按提交顺序递补；手动恢复 = 强制开始</div>
        </div>
      </section>

      {/* —— 下载与磁盘 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--file-directory" style={{ color: "var(--accent)" }} /> 下载
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">默认下载目录 <Effective kind="now" /></label>
          <input className="qoder-input" value={s.download.dest_root} onChange={(e) => patch("download", "dest_root", e.target.value)} />
          <div className="dl-field-help">目录不存在会自动创建；新任务默认落此目录</div>
          <label className="dl-field-label">磁盘预检严格模式 <Effective kind="restart" /></label>
          <Switch label="严格预检" on={s.download.disk_precheck_strict} onChange={(v) => patch("download", "disk_precheck_strict", v)} />
          <div className="dl-field-help">开 = 空间不可探测时拒绝入队（防预检被绕过）；关 = 告警放行</div>
          <label className="dl-field-label">HTTP 重定向最大跳数 <Effective kind="restart" /></label>
          <input
            className="qoder-input"
            type="number"
            min={1}
            max={100}
            value={s.download.max_redirects}
            onChange={(e) => patch("download", "max_redirects", num(e.target.value) ?? 10)}
          />
          <div className="dl-field-help">1..=100；默认 10（reqwest 默认值），仅启动时烘入 client</div>
          <label className="dl-field-label">错峰随机延迟（秒）<Effective kind="now" /></label>
          <input className="qoder-input" type="number" min={0} value={s.scheduler.start_jitter_seconds} onChange={(e) => patch("scheduler", "start_jitter_seconds", num(e.target.value) ?? 0)} />
          <div className="dl-field-help">新任务在 0..=N 秒内随机延迟启动；0 = 关闭</div>
        </div>
      </section>

      {/* —— 完成后与清理 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--check" style={{ color: "var(--accent)" }} /> 完成后与清理
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">完成 Webhook <Effective kind="now" /></label>
          <input className="qoder-input" placeholder="https://… 任务完成时 POST 通知；留空禁用" value={s.webhook.url} onChange={(e) => patch("webhook", "url", e.target.value)} />
          <label className="dl-field-label">完成后移动到 <Effective kind="now" /></label>
          <input className="qoder-input" placeholder="目录路径；留空禁用（BT 目录任务整体移动）" value={s.post_download.move_to} onChange={(e) => patch("post_download", "move_to", e.target.value)} />
          <label className="dl-field-label">完成后钩子程序 <Effective kind="now" /></label>
          <input className="qoder-input" placeholder="可执行程序路径；上下文经 SD_* 环境变量传入" value={s.post_download.hook} onChange={(e) => patch("post_download", "hook", e.target.value)} />
          <label className="dl-field-label">自动清理已完成任务（天）<Effective kind="now" /></label>
          <input className="qoder-input" type="number" min={0} value={s.cleanup.auto_remove_completed_days} onChange={(e) => patch("cleanup", "auto_remove_completed_days", num(e.target.value) ?? 0)} />
          <div className="dl-field-help">0 = 禁用；清扫间隔 10 分钟</div>
          <label className="dl-field-label">清理时保留文件</label>
          <Switch label="保留文件" on={s.cleanup.auto_remove_keep_data} onChange={(v) => patch("cleanup", "auto_remove_keep_data", v)} />
        </div>
      </section>

      {/* —— 保存条 —— */}
      <div className="dl-actions">
        <span style={{ fontSize: 12, color: "var(--text-tertiary)" }}>
          {s.meta.persist_path ? `配置文件：${s.meta.persist_path}` : "未指定 --config，保存仅运行时生效"}
        </span>
        <div style={{ flex: 1 }} />
        <button
          className="qoder-btn qoder-btn--ghost"
          aria-label="重置设置"
          disabled={busy || !dirtyCount}
          onClick={() => load()}
        >
          <span className="qoder-icon qoder-icon--sync" /> 重置
        </button>
        <button
          className="qoder-btn qoder-btn--primary"
          aria-label="保存设置"
          disabled={busy || !dirtyCount}
          onClick={save}
        >
          <span className="qoder-icon qoder-icon--save" />
          {dirtyCount ? `保存 ${dirtyCount} 项修改` : "保存"}
        </button>
      </div>

      {/* —— 外观 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--paintcan" style={{ color: "var(--accent)" }} /> 外观主题
        </h3>
        <div className="dl-theme-grid">
          {THEME_KEYS.map((t) => (
            <ThemeCard key={t} tk={t} active={theme === t} onPick={() => setTheme(t)} />
          ))}
        </div>
        <div className="dl-field-help" style={{ marginTop: 10 }}>
          主题经 <code>data-theme</code> 属性切换，与 qoder-ui 官方 8 主题逐变量一致
        </div>
      </section>

      {/* —— 后端连接 —— */}
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--server" style={{ color: "var(--accent)" }} /> 后端连接
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label">daemon 地址</label>
          <input
            className="qoder-input"
            placeholder="留空 = 同源（内嵌 UI / 桌面端）；如 http://127.0.0.1:8787"
            value={base}
            onChange={(e) => {
              setBaseState(e.target.value);
              setBase(e.target.value.trim());
            }}
          />
          <label className="dl-field-label">Bearer Token</label>
          <input
            className="qoder-input"
            type="password"
            placeholder="未配置 http_token 时留空"
            value={token}
            onChange={(e) => {
              setTokenState(e.target.value);
              setToken(e.target.value.trim());
            }}
          />
          <div className="dl-field-help">保存在本机浏览器存储；修改后自动生效（下次请求即走新地址）</div>
        </div>
      </section>
    </div>
  );
}

/// 主题预览卡：色板直接取自该主题的真实变量值（与 qoder-ui 对齐的关键）。
function ThemeCard({ tk, active, onPick }: { tk: ThemeKey; active: boolean; onPick: () => void }) {
  const meta = THEME_META[tk];
  const swatch = meta.dark
    ? ["#0b110e", "#151e1a", "#1a2420", meta.accent]
    : ["#f7faf8", "#ffffff", "#eef6f3", meta.accent];
  return (
    <button type="button" className={`dl-theme-card ${active ? "dl-theme-card--active" : ""}`} onClick={onPick}>
      <div className="dl-theme-swatch" style={{ background: swatch[0] }}>
        <i style={{ background: swatch[1], width: "38%", flex: "none" }} />
        <i style={{ background: swatch[2] }} />
        <i style={{ background: swatch[3], width: "14%", flex: "none" }} />
      </div>
      <div className="dl-theme-name">
        {active && <span className="qoder-icon qoder-icon--check" style={{ color: meta.accent }} />}
        {meta.label}
      </div>
    </button>
  );
}
