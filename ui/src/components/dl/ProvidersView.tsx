"use client";
/// 插件页（#213-4，协调方确认范围：全部 Provider——迅雷登录/账号、夸克
/// Cookie/配置、Provider 运行态）。
///
/// 数据源（全部既有端点，零后端改动）：
/// - `GET /providers`：已注册 provider 的运行态（enabled/authenticated/
///   quota/busy/backoff/last_error）——5s 自动轮询（busy/冷却是活数据）
/// - `GET /config` 精简快照：插件启停态 + 迅雷档位（区分「总开关关」/
///   「单个插件关」/「启用未认证」三种空态语义）
///
/// 信息架构：
/// - 插件体系说明卡（云兜底是什么、何时被使用）
/// - 已知插件目录（迅雷/夸克恒显示——未注册时展示启用引导而非隐藏，
///   这是「迅雷无入口」问题（#213-4.3）的核心修复）
/// - 其余已注册 provider（mock 等）通用卡
///
/// 登录引导（#213 §5.5 的取舍）：迅雷登录当前是 CLI 流程（打开本地
/// App 同款登录页/浏览器授权/终端二维码），UI v1 以「可复制命令 + 参数
/// 说明」引导；HTTP 化登录是独立后续特性（涉及异步流程/进度事件/取消
/// 语义，不塞进本页首版）。夸克同理（cookie JSON 由示例/CLI 写入）。

import { useCallback, useEffect, useState } from "react";
import type { ConfigSnapshot, ProviderStatusDto } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { fmtBytes } from "@/lib/daemon/format";
import { Icon } from "./Icon";

type ToastFn = (t: { kind: string; text: string }) => void;

/** 运行态徽章（与统计页 Provider 卡同口径的判定，独立实现避免跨视图耦合）。 */
function runtimeBadge(p: ProviderStatusDto) {
  const cooling = p.backoff_until != null && p.backoff_until * 1000 > Date.now();
  if (!p.enabled) return { cls: "dl-badge", txt: "已禁用" };
  if (cooling)
    return {
      cls: "dl-badge",
      txt: `冷却至 ${new Date((p.backoff_until ?? 0) * 1000).toLocaleTimeString()}`,
    };
  if (p.authenticated) return { cls: "dl-badge dl-badge--success", txt: "可用" };
  return { cls: "dl-badge", txt: "未认证" };
}

/// 已知插件目录（恒显示；run 为 null = 未注册——展示启用/登录引导）。
function PluginCard({
  name,
  title,
  icon,
  capabilities,
  run,
  enabled,
  extra,
  guide,
}: {
  name: string;
  title: string;
  icon: Parameters<typeof Icon>[0]["name"];
  capabilities: string;
  run: ProviderStatusDto | null;
  enabled: boolean;
  extra?: React.ReactNode;
  guide: React.ReactNode;
}) {
  const badge = run ? runtimeBadge(run) : { cls: "dl-badge", txt: "未注册" };
  return (
    <section className="dl-card" data-testid={`plugin-card-${name}`}>
      <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
        <Icon name={icon} size={15} className="dl-sec-ico" />
        <b>{title}</b>
        <span className={badge.cls}>{badge.txt}</span>
        <div style={{ flex: 1 }} />
        {extra}
      </div>
      <div style={{ fontSize: 12, color: "var(--fg-3)", marginBottom: 10 }}>
        能力：{capabilities}
      </div>
      {run && (
        <div
          style={{
            display: "flex",
            gap: 14,
            flexWrap: "wrap",
            fontSize: 12,
            marginBottom: 10,
            fontVariantNumeric: "tabular-nums",
          }}
        >
          <span title="剩余配额（供路由选择：quota>0 才可选）">
            配额 {run.quota_remaining > 1e12 ? "充足" : fmtBytes(run.quota_remaining)}
          </span>
          <span title="当前占用并发">并发 {run.busy}</span>
          {run.last_error && (
            <span title={`最近错误：${run.last_error}`} style={{ cursor: "help", color: "var(--warn)" }}>
              ⚠ 最近有错误
            </span>
          )}
        </div>
      )}
      {guide}
    </section>
  );
}

export default function ProvidersView({ onToast }: { onToast: ToastFn }) {
  const [providers, setProviders] = useState<ProviderStatusDto[] | null>(null);
  const [cfg, setCfg] = useState<ConfigSnapshot | null>(null);
  const [copied, setCopied] = useState(false);

  const load = useCallback(() => {
    client
      .providers()
      .then(setProviders)
      .catch(() => {
        /* 断连保持 null（等待态）；connected 翻转后由外层 1s 轮询驱动重拉 */
      });
    client
      .configSnapshot()
      .then(setCfg)
      .catch(() => {});
  }, []);

  // R29 同款连接态语义：挂载即拉 + 5s 轮询（busy/冷却是活数据）。
  useEffect(() => {
    load();
    const iv = setInterval(load, 5000);
    return () => clearInterval(iv);
  }, [load]);

  const copyLoginCmd = async () => {
    try {
      await navigator.clipboard.writeText("smart-dl-daemon xunlei-login --page");
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      onToast({ kind: "error", text: "复制失败（浏览器剪贴板权限拒绝）" });
    }
  };

  const xl = providers?.find((p) => p.provider.startsWith("xunlei")) ?? null;
  const qk = providers?.find((p) => p.provider.startsWith("quark")) ?? null;
  // 其余已注册（mock / 未来新增）：已知目录之外才进通用卡
  const others = (providers ?? []).filter(
    (p) => !p.provider.startsWith("xunlei") && !p.provider.startsWith("quark"),
  );

  const loginGuide = (
    <div style={{ display: "grid", gap: 6 }}>
      <div style={{ fontSize: 12, color: "var(--fg-3)" }}>
        登录当前经 CLI 完成（本地渲染 App 同款登录页，默认；也可
        <code> --browser</code> 系统浏览器授权或<code> --qr</code> 终端二维码）。
        登录态写入 <code>xunlei_auth.json</code>（daemon 工作目录；桌面端 =
        应用数据目录），可用 <code>--token</code> 指定路径。身份档位
        <code>--tier web|nas</code>（当前：
        <b> {cfg?.provider_xunlei_tier ?? "web"} </b>），档位分文件防互踢。
      </div>
      <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <code className="dl-code">smart-dl-daemon xunlei-login --page</code>
        <button
          type="button"
          className="dl-btn dl-btn--ghost"
          aria-label="复制登录命令"
          onClick={copyLoginCmd}
        >
          <Icon name={copied ? "check" : "copy"} size={13} />
          {copied ? "已复制" : "复制"}
        </button>
      </div>
    </div>
  );

  return (
    <div className="dl-page dl-page--narrow" data-testid="plugins-view">
      {/* —— 插件体系说明 —— */}
      <section className="dl-card">
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <Icon name="zap" size={15} className="dl-sec-ico" />
          <b>插件（云盘 Provider）</b>
          <span className={cfg ? (cfg.provider_enabled ? "dl-badge dl-badge--success" : "dl-badge") : "dl-badge"}>
            {cfg == null ? "…" : cfg.provider_enabled ? "总开关已启用" : "总开关未启用"}
          </span>
          <div style={{ flex: 1 }} />
          <button type="button" className="dl-btn dl-btn--ghost" aria-label="刷新插件状态" onClick={load}>
            <Icon name="sync" size={13} />
          </button>
        </div>
        <div style={{ fontSize: 12, color: "var(--fg-3)" }}>
          云兜底：直链下载失败（死链/限速）时自动转投云盘离线下载并回源拉取。
          未启用时 daemon 不注册任何 Provider，任务仅在本地引擎间调度。
          启用需在 daemon.toml 配置
          <code> [provider] enabled=true </code>
          （默认关——不自动烧云盘配额），随后按插件单独开启。
        </div>
      </section>

      {/* —— 已知插件目录（恒显示）—— */}
      <PluginCard
        name="xunlei"
        title="迅雷云盘"
        icon="zap"
        capabilities="离线下载 · 链接刷新"
        run={xl}
        enabled={cfg?.provider_xunlei_enabled ?? false}
        guide={loginGuide}
      />
      <PluginCard
        name="quark"
        title="夸克网盘"
        icon="globe"
        capabilities="离线转存 · 链接刷新"
        run={qk}
        enabled={cfg?.provider_quark_enabled ?? false}
        guide={
          <div style={{ fontSize: 12, color: "var(--fg-3)" }}>
            Cookie 登录态由示例/CLI 侧写入 <code>quark_auth.json</code>（daemon
            只读加载 + 刷新回写；路径可经 daemon.toml
            <code> [provider_quark] cookie_path </code>指定）。UI 内登录入口为
            后续特性。
          </div>
        }
      />

      {/* —— 其余已注册 provider（mock / 未来新增）—— */}
      {others.map((p) => (
        <PluginCard
          key={p.provider}
          name={p.provider}
          title={p.provider}
          icon="layers"
          capabilities="（由插件声明）"
          run={p}
          enabled={p.enabled}
          guide={<div style={{ fontSize: 12, color: "var(--fg-3)" }}>通用 Provider：无专属引导。</div>}
        />
      ))}

      {providers != null && providers.length === 0 && (
        <div style={{ fontSize: 12, color: "var(--fg-3)", padding: "4px 2px" }}>
          当前未注册任何 Provider——上方目录里的插件启用后（daemon.toml）重启
          daemon 即出现在此页并开始上报运行态。
        </div>
      )}
    </div>
  );
}
