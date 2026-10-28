"use client";
/// 设置视图（S1 设置面前端）：qBittorrent 式分组 + 即时生效 + 落盘反馈。
/// 数据源：GET/PUT /settings；主题走 data-theme（tokens.css 12 主题）；
/// 后端连接（地址/token）存 localStorage。
/// 交互打磨批：
/// - 移除死 class dl-enter（旧库残留，规范 G0-8 同族清理）
/// - 主题预览卡色板改用目标主题真实 token（swatch 容器挂 data-theme，
///   旧实现硬编码色与注释声明不符）
/// - 保存条移至页尾 + sticky（长页滚动时始终可达）
/// - Enter 保存（input 内，textarea 换行不受影响）；未保存离开页面前
///   beforeunload 提醒

import { Icon } from "./Icon";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { Settings, SettingsReq, ThemeKey } from "@/lib/daemon/types";
import { DEFAULT_THEME, THEME_KEYS, THEME_META } from "@/lib/daemon/types";
import { client, getBase, getToken, setBase, setToken } from "@/lib/daemon/store";

/// #161 P0 同构收口：设置域字段名编译期校验。
/// 后端 PUT /settings 各域全 Option + serde(default)——拼错字段名 = 静默
/// 空操作（当初 max_download_kb_s/down_kb_s P0 的原始现场）。此组类型让
/// patch/patchMany/DEFAULTS 的 [domain, key] 组合对生成的 SettingsReq 做
/// mapped-type 联动校验：任一侧改名，此处编译期报错。
type SettingsDomain = keyof SettingsReq;
type SettingsField<D extends SettingsDomain> = keyof NonNullable<SettingsReq[D]>;
/// 二元组首列与次列联动：["bandwidth", "max_download_kb_s"] 合法、
/// ["bandwidth", "typo"] 编译错；跨域组合（download 视图组含 scheduler 项）
/// 每项独立校验，不限制同组同域（DEFAULTS 按视图分组而非后端域）。
type PatchEntry = {
  [D in SettingsDomain]: [D, SettingsField<D>, unknown];
}[SettingsDomain];

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
      className={`dl-btn dl-btn--ghost ${on ? "dl-btn--primary" : ""}`}
      onClick={() => onChange(!on)}
      style={{ borderRadius: 99, padding: "4px 14px", minWidth: 92 }}
      aria-pressed={on}
      aria-label={label}
    >
      <Icon name={on ? "check" : "close"} size={13} />
      {on ? "开" : "关"}
    </button>
  );
}

/// P1-6（Issue #127）：预设方案——一键套用的配置组合（写入表单标记脏，
/// 用户审阅后统一保存；不碰代理/目录等用户特定项）。域字段与
/// Settings 接口对应；语义与 tomjiu 建议（PR116-UI-FEEDBACK §B）一致。
const PRESETS: Array<{
  key: string;
  name: string;
  desc: string;
  icon: string;
  /// PatchEntry 联动校验（#161）：预设里的 [域, 字段] 组合对生成的
  /// SettingsReq 编译期校验，防止预设静默失效。
  entries: PatchEntry[];
}> = [
  {
    key: "eco",
    name: "省电 / 低负载",
    desc: "并发 1、下载 1 MiB/s、上传 256 KiB/s、做种快速止盈",
    icon: "battery",
    entries: [
      ["queue", "max_active_http", 1],
      ["queue", "max_active_bt", 1],
      ["queue", "max_active_ftp", 1],
      ["bandwidth", "max_download_kb_s", 1024],
      ["bandwidth", "max_upload_kb_s", 256],
      ["bittorrent", "max_share_ratio", 0.5],
      ["bittorrent", "max_seeding_time_min", 30],
    ],
  },
  {
    key: "turbo",
    name: "极速下载",
    desc: "不限速不限并发、启用全部 P2P 扩展（DHT/PEX/LSD/μTP）",
    icon: "zap",
    entries: [
      ["bandwidth", "max_download_kb_s", 0],
      ["bandwidth", "max_upload_kb_s", 0],
      ["queue", "max_active_http", 0],
      ["queue", "max_active_bt", 0],
      ["queue", "max_active_ftp", 0],
      ["bittorrent", "enable_dht", true],
      ["bittorrent", "enable_lsd", true],
      ["bittorrent", "enable_pex", true],
      ["bittorrent", "enable_utp", true],
      ["bittorrent", "encrypt", "allow"],
    ],
  },
  {
    key: "stealth",
    name: "隐身 / 最安全",
    desc: "强制加密、禁 DHT/PEX/LSD/μTP、小并发（代理需自配）",
    icon: "shield",
    entries: [
      ["bittorrent", "encrypt", "require"],
      ["bittorrent", "enable_dht", false],
      ["bittorrent", "enable_lsd", false],
      ["bittorrent", "enable_pex", false],
      ["bittorrent", "enable_utp", false],
      ["bittorrent", "enable_upnp", false],
      ["queue", "max_active_http", 2],
      ["queue", "max_active_bt", 1],
    ],
  },
  {
    key: "pt",
    name: "PT 站",
    desc: "强制加密、禁 DHT/PEX/LSD（私有 tracker 语义）、上传不限、比率 2.0",
    icon: "radio-tower",
    entries: [
      ["bittorrent", "encrypt", "require"],
      ["bittorrent", "enable_dht", false],
      ["bittorrent", "enable_lsd", false],
      ["bittorrent", "enable_pex", false],
      ["bittorrent", "enable_utp", true],
      ["bittorrent", "enable_upnp", true],
      ["bandwidth", "max_upload_kb_s", 0],
      ["bittorrent", "max_share_ratio", 2.0],
      ["bittorrent", "max_seeding_time_min", 0],
    ],
  },
];

/// P1-6（Issue #127）：后端出厂默认（与 config.rs 各 Default impl 单源对齐——
/// 后续后端加字段需同步此处）。恢复默认 = 写入表单标记脏（可审阅可放弃），
/// 不直接落盘。
const DEFAULTS: Record<string, PatchEntry[]> = {
  bandwidth: [
    ["bandwidth", "max_download_kb_s", 0],
    ["bandwidth", "max_upload_kb_s", 0],
    ["bandwidth", "alt_enabled", false],
    ["bandwidth", "alt_max_download_kb_s", 0],
    ["bandwidth", "alt_max_upload_kb_s", 0],
  ],
  connection: [
    ["connection", "proxy", ""],
    ["connection", "bt_listen_port", 0],
    ["connection", "bt_max_connections", 0],
  ],
  bittorrent: [
    ["bittorrent", "enable_dht", false],
    ["bittorrent", "enable_lsd", false],
    ["bittorrent", "enable_upnp", false],
    ["bittorrent", "enable_pex", false],
    ["bittorrent", "enable_utp", false],
    ["bittorrent", "encrypt", "allow"],
    ["bittorrent", "max_share_ratio", 0],
    ["bittorrent", "max_seeding_time_min", 0],
    ["bittorrent", "storage_allocate", false],
  ],
  download: [
    ["download", "disk_precheck_strict", false],
    ["download", "max_redirects", 10],
    ["scheduler", "start_jitter_seconds", 0],
    ["scheduler", "completion_action", "none"],
  ],
  queue: [
    ["queue", "max_active_bt", 0],
    ["queue", "max_active_http", 0],
    ["queue", "max_active_ftp", 0],
  ],
  cleanup: [
    ["cleanup", "auto_remove_completed_days", 0],
    ["cleanup", "auto_remove_keep_data", false],
    ["post_download", "move_to", ""],
    ["post_download", "hook", ""],
    ["webhook", "url", ""],
  ],
};

export default function SettingsView({ onToast }: { onToast: (t: { kind: string; text: string }) => void }) {
  const [s, setS] = useState<Settings | null>(null);
  // P0-1 修复（Issue #127）：初始值读当前 DOM 实际 data-theme，而非硬编码
  // DEFAULT_THEME——旧实现挂载即写回 nord-dark，系统浅色用户点开设置瞬间
  // 变暗且 localStorage 被污染（layout 启动脚本的 prefers-color-scheme
  // 判定被覆盖）。layout 启动脚本已保证 DOM 上是生效主题（savedTheme 或
  // 系统偏好），此处以 DOM 为唯一真源；SSR 无 document 时退回默认值。
  const [theme, setTheme] = useState<ThemeKey>(() => {
    if (typeof document === "undefined") return DEFAULT_THEME;
    const cur = document.documentElement.getAttribute("data-theme") as ThemeKey | null;
    return cur && THEME_KEYS.includes(cur) ? cur : DEFAULT_THEME;
  });
  const [dirty, setDirty] = useState<Record<string, unknown>>({});
  const [busy, setBusy] = useState(false);
  // P1-6（Issue #127）：设置项搜索（分组/字段名关键词过滤）
  const [search, setSearch] = useState("");
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
    // 主题不再从 localStorage 二次读取（DOM 已是真源；双源会重现
    // 「挂载写回旧值」的时序窗）。持久化键仅供 layout 启动脚本冷启动用。
    setBaseState(getBase());
    setTokenState(getToken());
  }, [load]);

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    localStorage.setItem("dl.theme", theme);
  }, [theme]);

  const patch = <D extends SettingsDomain>(
    domain: D,
    key: SettingsField<D>,
    value: unknown,
  ) => {
    setS((cur) =>
      cur
        ? ({
            ...cur,
            [domain]: { ...(cur as unknown as Record<string, object>)[domain], [key]: value },
          } as Settings)
        : cur,
    );
    setDirty((d) => ({ ...d, [`${domain}.${String(key)}`]: value }));
  };

  const num = (v: string) => (v === "" ? undefined : Number(v) || 0);

  const save = async () => {
    if (Object.keys(dirty).length === 0) return;
    setBusy(true);
    try {
      // 组装域级补丁（后端按域合并应用）。dirty 的键在写入时已经过
      // patch/patchMany 的 mapped-type 编译期校验，此处动态重组后断言安全。
      const body = {} as SettingsReq;
      const dom = body as unknown as Record<string, Record<string, unknown>>;
      for (const [k, v] of Object.entries(dirty)) {
        const [d, f] = k.split(".");
        (dom[d] ??= {})[f] = v;
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

  /// 搜索命中判定（P1-6）：关键词对 section 标题/别名/字段名做包含匹配，
  /// 空搜索恒命中。中文/英文别名都收录（如「限速 bandwidth speed」）。
  const hit = (...keywords: string[]) =>
    !search.trim() || keywords.some((k) => k.toLowerCase().includes(search.trim().toLowerCase()));

  /// 一次 patch 多项（P1-6 preset / 恢复默认共用）：批量写入域字段并标记脏。
  /// 入参 PatchEntry 联动校验 [域, 字段] 组合（#161）。
  const patchMany = (entries: PatchEntry[]) => {
    setS((cur) => {
      if (!cur) return cur;
      const next: Record<string, unknown> = { ...(cur as unknown as Record<string, unknown>) };
      for (const [d, k, v] of entries) {
        next[d] = { ...(next[d] as object), [k]: v };
      }
      return next as unknown as Settings;
    });
    setDirty((cur) => {
      const next = { ...cur };
      for (const [d, k, v] of entries) next[`${d}.${k}`] = v;
      return next;
    });
  };

  /// 未保存修改离开页面提醒（beforeunload 标准行为；切视图不阻断——
  /// dirty 状态在保存条上已可见）。
  useEffect(() => {
    if (dirtyCount === 0) return;
    const h = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = "";
    };
    window.addEventListener("beforeunload", h);
    return () => window.removeEventListener("beforeunload", h);
  }, [dirtyCount]);

  /// 后端连接卡片（R25 提升为独立块）：纯本地 localStorage 读写，不依赖
  /// GET /settings——令牌配错（401）时 /settings 永远拉不到，若此卡片
  /// 跟随 s 渲染，用户将无法在 UI 内修正令牌（排障死锁：唯一修复入口
  /// 恰在需要它的时刻不可达）。s 未就绪也渲染。
  const connCard = (
    <section className="dl-card">
      <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
        <Icon name="server" size={14} className="dl-sec-ico" /> 后端连接

      </h3>
      <div className="dl-form-grid">
        <label className="dl-field-label" htmlFor="set-a11y-24">daemon 地址</label>
        <input id="set-a11y-24"
          className="dl-input"
          placeholder="留空 = 同源（内嵌 UI / 桌面端）；如 http://127.0.0.1:8787"
          value={base}
          onChange={(e) => {
            setBaseState(e.target.value);
            setBase(e.target.value.trim());
          }}
        />
        <label className="dl-field-label" htmlFor="set-a11y-25">Bearer Token</label>
        <input id="set-a11y-25"
          className="dl-input"
          type="password"
          placeholder="未配置 http_token 时留空"
          value={token}
          onChange={(e) => {
            setTokenState(e.target.value);
            setToken(e.target.value.trim());
          }}
        />
        <div className="dl-field-help">保存在本机浏览器存储；修改后自动生效（下次请求即走新地址/新令牌）</div>
      </div>
    </section>
  );

  if (!s) {
    return (
      <>
        <div className="dl-card" style={{ color: "var(--fg-3)" }}>
          加载设置中…（若持续失败请检查下方「后端连接」——令牌/地址）
        </div>
        {connCard}
      </>
    );
  }

  return (
    <div
      className="dl-page dl-page--narrow"
      onKeyDown={(e) => {
        // Enter 保存（仅单行 input；textarea 的 Enter 保留换行）
        if (e.key === "Enter" && e.target instanceof HTMLInputElement && !busy && dirtyCount) {
          e.preventDefault();
          save();
        }
      }}
    >
      {/* —— 搜索 + 预设方案（P1-6，Issue #127） —— */}
      <section className="dl-card">
        <div className="dl-toolbar-search" style={{ maxWidth: 420 }}>
          <Icon name="search" size={13} />
          <input
            className="dl-search-input"
            placeholder="搜索设置项…（如：限速 / 加密 / 关机）"
            aria-label="搜索设置项"
            data-testid="settings-search"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && search) {
                e.stopPropagation();
                setSearch("");
              }
            }}
          />
          {search && (
            <button type="button" className="dl-tool-btn" aria-label="清空搜索" title="清空搜索" onClick={() => setSearch("")}>
              <Icon name="close" size={12} />
            </button>
          )}
        </div>
        <div style={{ margin: "12px 0 4px", display: "flex", alignItems: "center", gap: 8 }}>
          <b>预设方案</b>
          <span style={{ fontSize: 12, color: "var(--fg-3)" }}>一键套用配置组合（填入表单，审阅后统一保存）</span>
        </div>
        <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(210px, 1fr))", gap: 10 }}>
          {PRESETS.map((p) => (
            <button
              key={p.key}
              type="button"
              className="dl-btn dl-btn--ghost"
              style={{ flexDirection: "column", alignItems: "flex-start", gap: 4, padding: "10px 12px", textAlign: "left" }}
              aria-label={`套用预设：${p.name}`}
              disabled={busy}
              onClick={() => {
                patchMany(p.entries);
                onToast({ kind: "success", text: `预设「${p.name}」已填入表单（${p.entries.length} 项，待保存）` });
              }}
            >
              <span style={{ display: "flex", alignItems: "center", gap: 6, fontWeight: 600 }}>
                <Icon name={p.icon as never} size={13} /> {p.name}
              </span>
              <span style={{ fontSize: 11.5, color: "var(--fg-3)", lineHeight: 1.4 }}>{p.desc}</span>
            </button>
          ))}
        </div>
      </section>

      {search.trim() && (
        <div className="dl-field-help" style={{ marginTop: -6 }}>
          {`按「${search.trim()}」过滤——未命中的分组已隐藏`}
        </div>
      )}

      {hit("带宽 bandwidth 限速 speed 备用 alt 上传 upload 调度") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="speed" size={14} className="dl-sec-ico" /> 带宽
          <span style={{ flex: 1 }} />
          <button type="button" className="dl-btn dl-btn--ghost" style={{ fontSize: 11.5 }} disabled={busy}
            aria-label="恢复带宽默认值" title="把本组字段填回出厂默认（待保存）"
            onClick={() => patchMany(DEFAULTS.bandwidth)}>
            恢复默认
          </button>
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label" htmlFor="set-a11y-1">
            全局下载限速 (KiB/s) <Effective kind="now" />
          </label>
          <input id="set-a11y-1"
            className="dl-input"
            type="number"
            min={0}
            value={s.bandwidth.max_download_kb_s}
            onChange={(e) => patch("bandwidth", "max_download_kb_s", num(e.target.value) ?? 0)}
          />
          <div className="dl-field-help">所有引擎（HTTP/FTP/BT）合计下行上限；0 = 不限</div>

          <label className="dl-field-label" htmlFor="set-a11y-2">
            全局上传限速 (KiB/s) <Effective kind="now" />
          </label>
          <input id="set-a11y-2"
            className="dl-input"
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
            <span className="dl-badge dl-badge--warning">备用窗口生效中</span>
          )}
        </div>
        {s.bandwidth.alt_enabled && (
          <div className="dl-form-grid">
            <label className="dl-field-label" htmlFor="set-a11y-3">备用下载限速 (KiB/s)</label>
            <input id="set-a11y-3"
              className="dl-input"
              type="number"
              min={0}
              value={s.bandwidth.alt_max_download_kb_s}
              onChange={(e) => patch("bandwidth", "alt_max_download_kb_s", num(e.target.value) ?? 0)}
            />
            <label className="dl-field-label" htmlFor="set-a11y-4">备用上传限速 (KiB/s)</label>
            <input id="set-a11y-4"
              className="dl-input"
              type="number"
              min={0}
              value={s.bandwidth.alt_max_upload_kb_s}
              onChange={(e) => patch("bandwidth", "alt_max_upload_kb_s", num(e.target.value) ?? 0)}
            />
            <label className="dl-field-label" htmlFor="set-a11y-5">起止时间（本地时区）</label>
            <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
              <input id="set-a11y-5"
                className="dl-input"
                type="time"
                value={s.bandwidth.alt_from}
                onChange={(e) => patch("bandwidth", "alt_from", e.target.value)}
              />
              <span>→</span>
              <input
                className="dl-input"
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
                    className={`dl-btn dl-btn--ghost ${on ? "dl-btn--primary" : ""}`}
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
      )}

      {hit("连接 connection 代理 proxy 端口 port listen") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="globe" size={14} className="dl-sec-ico" /> 连接与代理
          <span style={{ flex: 1 }} />
          <button type="button" className="dl-btn dl-btn--ghost" style={{ fontSize: 11.5 }} disabled={busy}
            aria-label="恢复连接默认值" title="把本组字段填回出厂默认（待保存）"
            onClick={() => patchMany(DEFAULTS.connection)}>
            恢复默认
          </button>
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label" htmlFor="set-a11y-6">
            全局代理 <Effective kind="new" />
          </label>
          <input id="set-a11y-6"
            className="dl-input"
            placeholder="http://host:port · socks5://user:pass@host:port · 留空 = 直连"
            value={s.connection.proxy}
            onChange={(e) => patch("connection", "proxy", e.target.value)}
          />
          <div className="dl-field-help">BT 会话立即重放生效；HTTP 逐任务 client 构建时合并（新任务生效，任务级代理优先）</div>

          <label className="dl-field-label" htmlFor="set-a11y-7">
            BT 监听端口 <Effective kind="now" />
          </label>
          <input id="set-a11y-7"
            className="dl-input"
            type="number"
            min={0}
            max={65535}
            placeholder="0 = 内核默认"
            value={s.connection.bt_listen_port}
            onChange={(e) => patch("connection", "bt_listen_port", num(e.target.value) ?? 0)}
          />
          <div className="dl-field-help">0 = 内核默认（6881 系）；非 BT 构建下该项仅持久化</div>

          <label className="dl-field-label" htmlFor="set-a11y-8">
            BT 全局连接数上限 <Effective kind="now" />
          </label>
          <input id="set-a11y-8"
            className="dl-input"
            type="number"
            min={0}
            placeholder="0 = 内核默认（200）"
            value={s.connection.bt_max_connections}
            onChange={(e) => patch("connection", "bt_max_connections", num(e.target.value) ?? 0)}
          />
        </div>
      </section>
      )}

      {/* —— BitTorrent —— */}
      {hit("BitTorrent BT DHT PEX LSD uTP 加密 encrypt 做种 share ratio tracker upnp") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="radio-tower" size={14} className="dl-sec-ico" /> BitTorrent
          {!s.bittorrent.bt_available && (
            <span className="dl-badge dl-badge--warning">当前构建未启用 BT</span>
          )}
        
        <span style={{ flex: 1 }} />
        <button type="button" className="dl-btn dl-btn--ghost" style={{ fontSize: 11.5 }} disabled={busy}
          aria-label="恢复BitTorrent默认值" title="把本组字段填回出厂默认（待保存）"
          onClick={() => patchMany(DEFAULTS.bittorrent)}>
          恢复默认
        </button>
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
          <label className="dl-field-label" htmlFor="set-a11y-9">MSE 加密策略<Effective kind="now" /></label>
          <select id="set-a11y-9"
            className="dl-input"
            value={s.bittorrent.encrypt}
            onChange={(e) => patch("bittorrent", "encrypt", e.target.value)}
          >
            <option value="disable">禁用（纯明文）</option>
            <option value="allow">允许（明文 + 加密）</option>
            <option value="require">强制加密</option>
          </select>
          <div className="dl-field-help">开关热改即时生效（新发现任务对 PEX 全量生效）；私有 tracker 建议关闭 DHT/PEX/LSD</div>
          <label className="dl-field-label" htmlFor="set-a11y-10">做种分享率上限<Effective kind="now" /></label>
          <input id="set-a11y-10"
            className="dl-input"
            type="number"
            min={0}
            step="0.1"
            placeholder="0 = 不启用"
            value={s.bittorrent.max_share_ratio}
            onChange={(e) => patch("bittorrent", "max_share_ratio", num(e.target.value) ?? 0)}
          />
          <label className="dl-field-label" htmlFor="set-a11y-11">做种时长上限（分钟）<Effective kind="now" /></label>
          <input id="set-a11y-11"
            className="dl-input"
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
          <label className="dl-field-label" htmlFor="set-a11y-12">新任务自动追加 tracker<Effective kind="now" /></label>
          <textarea id="set-a11y-12"
            className="dl-input"
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
      )}

      {/* —— 队列 —— */}
      {hit("队列 queue 并发 active 并行") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="layers" size={14} className="dl-sec-ico" /> 并发队列
        
        <span style={{ flex: 1 }} />
        <button type="button" className="dl-btn dl-btn--ghost" style={{ fontSize: 11.5 }} disabled={busy}
          aria-label="恢复队列默认值" title="把本组字段填回出厂默认（待保存）"
          onClick={() => patchMany(DEFAULTS.queue)}>
          恢复默认
        </button>
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label" htmlFor="set-a11y-13">BT 同时传输上限</label>
          <input id="set-a11y-13" className="dl-input" type="number" min={0} value={s.queue.max_active_bt} onChange={(e) => patch("queue", "max_active_bt", num(e.target.value) ?? 0)} />
          <label className="dl-field-label" htmlFor="set-a11y-14">HTTP 同时传输上限</label>
          <input id="set-a11y-14" className="dl-input" type="number" min={0} value={s.queue.max_active_http} onChange={(e) => patch("queue", "max_active_http", num(e.target.value) ?? 0)} />
          <label className="dl-field-label" htmlFor="set-a11y-15">FTP 同时传输上限</label>
          <input id="set-a11y-15" className="dl-input" type="number" min={0} value={s.queue.max_active_ftp} onChange={(e) => patch("queue", "max_active_ftp", num(e.target.value) ?? 0)} />
          <div className="dl-field-help">0 = 不限（禁用排队）；超配额自动排队，槽位空闲按提交顺序递补；手动恢复 = 强制开始</div>
        </div>
      </section>
      )}

      {/* —— 下载与磁盘 —— */}
      {hit("下载 download 磁盘 disk 目录 dest 预检 重定向 redirect 错峰 jitter 定时 关机 completion") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="file-directory" size={14} className="dl-sec-ico" /> 下载
        
        <span style={{ flex: 1 }} />
        <button type="button" className="dl-btn dl-btn--ghost" style={{ fontSize: 11.5 }} disabled={busy}
          aria-label="恢复下载与磁盘默认值" title="把本组字段填回出厂默认（待保存）"
          onClick={() => patchMany(DEFAULTS.download)}>
          恢复默认
        </button>
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label" htmlFor="set-a11y-16">默认下载目录 <Effective kind="now" /></label>
          <input id="set-a11y-16" className="dl-input" value={s.download.dest_root} onChange={(e) => patch("download", "dest_root", e.target.value)} />
          <div className="dl-field-help">目录不存在会自动创建；新任务默认落此目录</div>
          <label className="dl-field-label">磁盘预检严格模式 <Effective kind="restart" /></label>
          <Switch label="严格预检" on={s.download.disk_precheck_strict} onChange={(v) => patch("download", "disk_precheck_strict", v)} />
          <div className="dl-field-help">开 = 空间不可探测时拒绝入队（防预检被绕过）；关 = 告警放行</div>
          <label className="dl-field-label" htmlFor="set-a11y-17">HTTP 重定向最大跳数 <Effective kind="restart" /></label>
          <input id="set-a11y-17"
            className="dl-input"
            type="number"
            min={1}
            max={100}
            value={s.download.max_redirects}
            onChange={(e) => patch("download", "max_redirects", num(e.target.value) ?? 10)}
          />
          <div className="dl-field-help">1..=100；默认 10（reqwest 默认值），仅启动时烘入 client</div>
          <label className="dl-field-label" htmlFor="set-a11y-18">错峰随机延迟（秒）<Effective kind="now" /></label>
          <input id="set-a11y-18" className="dl-input" type="number" min={0} value={s.scheduler.start_jitter_seconds} onChange={(e) => patch("scheduler", "start_jitter_seconds", num(e.target.value) ?? 0)} />
          <div className="dl-field-help">新任务在 0..=N 秒内随机延迟启动；0 = 关闭</div>

          {/* P1-6（Issue #127）：完成后动作——后端 Task 46 已支持（PUT /settings
              scheduler.completion_action，热更），UI 此前未暴露 = 「下载完成后
              关机」无入口。 */}
          <label className="dl-field-label" htmlFor="set-a11y-19">全部任务完成后 <Effective kind="now" /></label>
          <select id="set-a11y-19"
            className="dl-input"
            aria-label="全部任务完成后动作"
            data-testid="completion-action"
            value={s.scheduler.completion_action || "none"}
            onChange={(e) => patch("scheduler", "completion_action", e.target.value)}
          >
            <option value="none">不执行任何动作（默认）</option>
            <option value="exit">退出 daemon 进程</option>
            <option value="shutdown">关闭系统（关机）</option>
            <option value="sleep">系统休眠</option>
            <option value="hibernate">系统冬眠</option>
          </select>
          <div className="dl-field-help">
            全部任务到达终态（含暂停/做种）后触发，每进程会话最多一次；空任务表不触发
          </div>
        </div>
      </section>
      )}

      {/* —— 完成后与清理 —— */}
      {hit("完成后 清理 cleanup webhook hook 移动 move 自动") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="check" size={14} className="dl-sec-ico" /> 完成后与清理
        
        <span style={{ flex: 1 }} />
        <button type="button" className="dl-btn dl-btn--ghost" style={{ fontSize: 11.5 }} disabled={busy}
          aria-label="恢复完成后与清理默认值" title="把本组字段填回出厂默认（待保存）"
          onClick={() => patchMany(DEFAULTS.cleanup)}>
          恢复默认
        </button>
        </h3>
        <div className="dl-form-grid">
          <label className="dl-field-label" htmlFor="set-a11y-20">完成 Webhook <Effective kind="now" /></label>
          <input id="set-a11y-20" className="dl-input" placeholder="https://… 任务完成时 POST 通知；留空禁用" value={s.webhook.url} onChange={(e) => patch("webhook", "url", e.target.value)} />
          <label className="dl-field-label" htmlFor="set-a11y-21">完成后移动到 <Effective kind="now" /></label>
          <input id="set-a11y-21" className="dl-input" placeholder="目录路径；留空禁用（BT 目录任务整体移动）" value={s.post_download.move_to} onChange={(e) => patch("post_download", "move_to", e.target.value)} />
          <label className="dl-field-label" htmlFor="set-a11y-22">完成后钩子程序 <Effective kind="now" /></label>
          <input id="set-a11y-22" className="dl-input" placeholder="可执行程序路径；上下文经 SD_* 环境变量传入" value={s.post_download.hook} onChange={(e) => patch("post_download", "hook", e.target.value)} />
          <label className="dl-field-label" htmlFor="set-a11y-23">自动清理已完成任务（天）<Effective kind="now" /></label>
          <input id="set-a11y-23" className="dl-input" type="number" min={0} value={s.cleanup.auto_remove_completed_days} onChange={(e) => patch("cleanup", "auto_remove_completed_days", num(e.target.value) ?? 0)} />
          <div className="dl-field-help">0 = 禁用；清扫间隔 10 分钟</div>
          <label className="dl-field-label">清理时保留文件</label>
          <Switch label="保留文件" on={s.cleanup.auto_remove_keep_data} onChange={(v) => patch("cleanup", "auto_remove_keep_data", v)} />
        </div>
      </section>
      )}

      {/* —— 外观 —— */}
      {hit("外观 主题 theme 色板") && (
      <section className="dl-card">
        <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
          <Icon name="paintcan" size={14} className="dl-sec-ico" /> 外观主题
        
        </h3>
        <div className="dl-theme-grid">
          {THEME_KEYS.map((t) => (
            <ThemeCard key={t} tk={t} active={theme === t} onPick={() => setTheme(t)} />
          ))}
        </div>
        <div className="dl-field-help" style={{ marginTop: 10 }}>
          主题经 <code>data-theme</code> 属性切换，色板与设计规范 12 主题逐变量一致
        </div>
      </section>
      )}

      {/* —— 后端连接（connCard：s 未就绪也渲染，见定义处注释） —— */}
      {hit("后端 连接 server 地址 token base") && connCard}

      {/* —— 保存条（页尾 + sticky：长页滚动始终可达） —— */}
      <div className="dl-actions dl-actions--sticky">
        <span style={{ fontSize: 12, color: "var(--fg-3)" }}>
          {s.meta.persist_path ? `配置文件：${s.meta.persist_path}` : "未指定 --config，保存仅运行时生效"}
        </span>
        <div style={{ flex: 1 }} />
        <button
          className="dl-btn dl-btn--ghost"
          aria-label="放弃修改并重新加载"
          disabled={busy || !dirtyCount}
          onClick={() => load()}
        >
          <Icon name="sync" size={13} /> 重置
        </button>
        <button
          className="dl-btn dl-btn--primary"
          aria-label="保存设置"
          disabled={busy || !dirtyCount}
          onClick={save}
        >
          <Icon name="save" size={13} />
          {dirtyCount ? `保存 ${dirtyCount} 项修改` : "保存"}
        </button>
      </div>
    </div>
  );
}

/// 主题预览卡：swatch 容器挂 data-theme={tk}，渲染目标主题的**迷你窗口
/// 预览**（标题栏/侧栏 --bg-2 → 画布 --bg → 按钮 --bg-3+--line-3 边框 →
/// 强调块 --accent → 正文行 --fg-2/--fg-3）——全部取该主题真实 token，
/// 预览内部自洽（每个颜色都落在它所属主题的画布上），修复
/// 「四色块条像一坨分不清的灰、配色与背景不适配」。几何见 globals.css。
function ThemeCard({ tk, active, onPick }: { tk: ThemeKey; active: boolean; onPick: () => void }) {
  const meta = THEME_META[tk];
  return (
    <button
      type="button"
      className={`dl-theme-card ${active ? "dl-theme-card--active" : ""}`}
      aria-pressed={active}
      onClick={onPick}
    >
      <div className="dl-theme-swatch" data-theme={tk} aria-hidden="true">
        <i className="dl-ts-bar" />
        <i className="dl-ts-side" />
        <i className="dl-ts-btn" />
        <i className="dl-ts-acc" />
        <i className="dl-ts-l1" />
        <i className="dl-ts-l2" />
      </div>
      <div className="dl-theme-name">
        {active && <Icon name="check" size={12} style={{ color: meta.accent }} />}
        {meta.label}
      </div>
    </button>
  );
}
