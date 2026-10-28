"use client";
/// 通用小件：状态徽章 / 进度条 / 速率胶囊 / 空态 / 确认对话框 / 表格骨架 / daemon 不可达面板。
/// Issue #115 Phase 1：全部改用 dl-* token 体系（旧库 class 已清零）。
/// 交互打磨批：ConfirmDialog（危险操作确认，alertdialog 语义 + Esc +
/// 初始焦点取消按钮）；TableSkeleton（E8：首帧数据未到时不再误报「暂无任务」）。
/// R24：DaemonDown——首帧未到且轮询全败时替换骨架（区分「加载中」与
/// 「daemon 未启动」，首跑时序 UI 先于 daemon 是常见场景）。

import { useEffect, useRef } from "react";
import { fmtBytes, fmtPct, fmtSpeed, fmtUnixShort } from "@/lib/daemon/format";
import { Icon } from "./Icon";

export const STATE_TONE: Record<string, { label: string; badge: string; bar: string }> = {
  Downloading: { label: "下载中", badge: "dl-badge--primary", bar: "run" },
  Seeding: { label: "做种中", badge: "dl-badge--info", bar: "run" },
  Transferring: { label: "传输中", badge: "dl-badge--primary", bar: "run" },
  FallbackProvider: { label: "云兜底", badge: "dl-badge--info", bar: "run" },
  Evaluating: { label: "评估中", badge: "dl-badge--info", bar: "queued" },
  Queued: { label: "排队中", badge: "", bar: "queued" },
  Paused: { label: "已暂停", badge: "dl-badge--warning", bar: "paused" },
  Completed: { label: "已完成", badge: "dl-badge--success", bar: "done" },
  Stopped: { label: "已停止", badge: "", bar: "paused" },
  Failed: { label: "失败", badge: "dl-badge--error", bar: "error" },
};

export function stateLabel(state: string): string {
  return STATE_TONE[state]?.label ?? state;
}

export function StateBadge({ state }: { state: string }) {
  const meta = STATE_TONE[state] ?? { label: state, badge: "", bar: "" };
  const live = ["Downloading", "Seeding", "Transferring", "FallbackProvider"].includes(state);
  return (
    <span className={`dl-badge ${meta.badge}`}>
      {live && <span className="dl-live-dot" />}
      {meta.label}
    </span>
  );
}

/// A2（契约审计 2026-10-21）：Queued 态限定徽章——后端 TaskSummary 透出
/// start_at_unix（E23 定时）与 next_retry_at_unix（E30 重试）专供列表区分，
/// 不建模则定时/重试等待与普通排队无法区分（用户看到「卡在排队」）。
/// 渲染规则：Queued 态按字段追加「定时 hh:mm」/「重试 x/y」小徽章；
/// 非 Queued 不渲染（字段语义即 Queued 附注）。定时优先于重试展示
/// （同一任务两者并存理论上不存在：定时任务失败后 next_retry 才出现，
/// 而 start_at 已消费清零——并存时以定时为准防误导）。
export function QueuedNote({
  t,
}: {
  t: {
    state: string;
    start_at_unix?: number;
    retries?: number;
    max_retries?: number;
    next_retry_at_unix?: number;
  };
}) {
  if (t.state !== "Queued") return null;
  if (t.start_at_unix && t.start_at_unix > 0) {
    return (
      <span className="dl-badge dl-badge--info dl-queued-note" title={`定时启动：${fmtUnixShort(t.start_at_unix)}`}>
        <Icon name="clock" size={11} /> 定时 {fmtUnixShort(t.start_at_unix)}
      </span>
    );
  }
  if (t.next_retry_at_unix && t.next_retry_at_unix > 0) {
    const retries = t.retries ?? 0;
    const max = t.max_retries;
    const label = max ? `重试 ${retries}/${max}` : `重试 ${retries}`;
    return (
      <span className="dl-badge dl-badge--warning dl-queued-note" title={`下次重试：${fmtUnixShort(t.next_retry_at_unix)}`}>
        <Icon name="sync" size={11} /> {label}
      </span>
    );
  }
  return null;
}

export function TaskProgress({
  task,
  thin,
}: {
  task: { state: string; done: number; total: number };
  thin?: boolean;
}) {
  const meta = STATE_TONE[task.state] ?? { bar: "" };
  // E5：total=0 → 0% 而非 NaN%
  const pct = task.total > 0 ? (task.done / task.total) * 100 : 0;
  return (
    <div
      className={`dl-progress ${thin ? "dl-progress--thin" : ""}`}
      role="progressbar"
      aria-valuenow={Math.round(pct)}
      aria-valuemin={0}
      aria-valuemax={100}
    >
      <span className={`dl-bar dl-bar--${meta.bar || "run"}`} style={{ width: `${pct}%` }} />
    </div>
  );
}

export function SpeedPill({
  dir,
  value,
  live,
}: {
  dir: "down" | "up";
  value: number;
  live?: boolean;
}) {
  if (!value && !live) return <span className="dl-pill">—</span>;
  return (
    <span className={`dl-pill dl-pill--${dir}`}>
      {live && value > 0 && <span className="dl-live-dot" />}
      <Icon name={dir === "down" ? "arrow-down" : "arrow-up"} size={11} />
      {fmtSpeed(value)}
    </span>
  );
}

export function EmptyState({
  icon,
  title,
  hint,
}: {
  icon: Parameters<typeof Icon>[0]["name"];
  title: string;
  hint?: string;
}) {
  return (
    <div className="dl-empty">
      <div className="dl-empty-orb">
        <Icon name={icon} size={22} />
      </div>
      <div style={{ fontWeight: 600 }}>{title}</div>
      {hint && <div style={{ fontSize: 12 }}>{hint}</div>}
    </div>
  );
}

export function SizeText({ task }: { task: { done: number; total: number } }) {
  return (
    <span style={{ fontVariantNumeric: "tabular-nums", color: "var(--fg-2)" }}>
      {fmtBytes(task.done)} / {fmtBytes(task.total)} · {fmtPct(task.done, task.total)}
    </span>
  );
}

/// E8：表格加载骨架（首帧数据未到）。静态灰条（克制：无 shimmer 动画）。
export function TableSkeleton({ rows = 8 }: { rows?: number }) {
  return (
    <div className="dl-table-wrap" aria-busy="true" aria-live="polite">
      <div className="dl-skel-colgroup" aria-hidden="true">
        <span style={{ width: "4%" }} />
        <span style={{ width: "34%" }} />
        <span style={{ width: "10%" }} />
        <span style={{ width: "22%" }} />
        <span style={{ width: "10%" }} />
        <span style={{ width: "10%" }} />
        <span style={{ width: "8%" }} />
      </div>
      <span className="dl-skel-hint" role="status">正在加载任务列表…</span>
      {Array.from({ length: rows }, (_, i) => (
        <div className="dl-skel-row" key={i} aria-hidden="true">
          <span style={{ width: "4%" }} />
          <span style={{ width: "34%", height: 10 }} />
          <span style={{ width: "10%", height: 10 }} />
          <span style={{ width: "22%", height: 4 }} />
          <span style={{ width: "10%", height: 10 }} />
          <span style={{ width: "10%", height: 10 }} />
          <span style={{ width: "8%", height: 10 }} />
        </div>
      ))}
    </div>
  );
}

/// R24/R25：daemon 连接失败面板（双变体）。触发条件 = 首帧未到
/// （loaded=false）且最近一轮双请求（/tasks + /stats）全败——与
/// TableSkeleton（daemon 可达、数据在途）严格互斥。旧体验：骨架条无限
/// 渲染，用户无法分辨「慢加载」与「daemon 未启动」；且令牌配错（HTTP
/// 401/403，daemon 活着）与网络不可达（拒绝/超时）同脸——排障方向完全
/// 错误。变体（R25，HttpError 分类驱动）：
/// - unreachable：网络层失败——指引检查 daemon 启动/地址可达；
/// - auth：HTTP 401/403——daemon 已应答但拒绝，指引检查令牌/白名单。
/// 恢复路径零交互：轮询每秒自动重试，daemon 起来/凭据修正即自愈
/// （connected 翻转驱动，e2e 有全链护栏）。
export function DaemonDown({
  base,
  hasToken,
  mode = "unreachable",
}: {
  base: string;
  hasToken: boolean;
  mode?: "unreachable" | "auth";
}) {
  const auth = mode === "auth";
  return (
    <div
      className={`dl-daemon-down${auth ? " is-auth" : ""}`}
      role="alert"
      data-testid="daemon-down"
      data-mode={mode}
    >
      <div className="dl-empty-orb">
        <Icon name={auth ? "shield" : "server"} size={22} />
      </div>
      <div className="dl-dd-title">
        {auth ? "认证失败（daemon 拒绝了请求）" : "无法连接 daemon"}
      </div>
      <div className="dl-dd-meta">
        <span className="dl-dd-k">地址</span>
        <code className="dl-dd-v" data-testid="daemon-down-base">
          {base || "同源（当前页面地址）"}
        </code>
      </div>
      <div className="dl-dd-meta">
        <span className="dl-dd-k">令牌</span>
        <span className="dl-dd-v">{hasToken ? "已设置（Bearer）" : "未设置"}</span>
      </div>
      <div className="dl-dd-hint">
        {auth ? (
          <>
            daemon 已连接但返回 401/403——请检查 Bearer 令牌（设置 → 连接 →
            Bearer Token）是否与服务端 <code>http_token</code> 一致；若走反向
            代理/跨源，还需服务端 <code>extra_allowed_hosts</code> /
            <code>extra_allowed_origins</code> 白名单放行。
          </>
        ) : (
          <>
            请确认 daemon 已启动（<code>smart-dl-daemon serve</code>）且地址可达。
            页面每秒自动重试，无需刷新——连接恢复后自动进入任务列表。
          </>
        )}
      </div>
    </div>
  );
}

/// 危险/不可逆操作确认（删除任务、删除订阅、批量删除等）。
/// - role="alertdialog" + aria-modal，Esc 取消
/// - 初始焦点落在「取消」（安全默认；Tab 一格到确认）
/// - danger = 确认按钮红态；overlay 纯色半透明（规范 §1.5.7）
export function ConfirmDialog({
  title,
  body,
  confirmText,
  danger,
  onConfirm,
  onCancel,
}: {
  title: string;
  body: string;
  confirmText: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onCancel();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [onCancel]);
  return (
    <>
      <div className="dl-overlay" onClick={onCancel} />
      <div
        className="dl-confirm"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="dl-confirm-title"
        aria-describedby="dl-confirm-body"
      >
        <div className="dl-confirm-title" id="dl-confirm-title">
          <Icon name={danger ? "error" : "pass"} size={15} className={`dl-confirm-ico ${danger ? "is-danger" : ""}`} />
          {title}
        </div>
        <div className="dl-confirm-body" id="dl-confirm-body">
          {body}
        </div>
        <div className="dl-confirm-actions">
          <button
            ref={cancelRef}
            type="button"
            className="dl-btn"
            data-testid="confirm-cancel"
            onClick={onCancel}
          >
            取消
          </button>
          <button
            type="button"
            className={`dl-btn ${danger ? "dl-btn--danger-solid" : "dl-btn--primary"}`}
            data-testid="confirm-ok"
            onClick={onConfirm}
          >
            {confirmText}
          </button>
        </div>
      </div>
    </>
  );
}
