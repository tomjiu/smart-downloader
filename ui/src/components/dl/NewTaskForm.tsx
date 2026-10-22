"use client";
/// 新建任务全字段表单（#143 第二梯队 Wave B——「现在只能贴裸 URL」补全）。
///
/// 源三模式（AddTaskReq 优先级 torrent > metalink > url 的 UI 投影）：
/// - 链接：URL 直贴（http/ftp/sftp/magnet/m3u8）+ **探测预览**（E31：不建
///   任务取大小/建议落盘名/Range 支持；headers/proxy/auth 随探测下发——
///   预览即验证反防盗链配置是否真的有效，qBittorrent 无此能力）
/// - .torrent 文件：本地选文件 → base64（torrent_b64）
/// - Metalink 文件：本地选文件 → base64（metalink_b64，逐 file 展开，
///   响应 count 多任务）
///
/// 交互细节（「UI 扎实」逐条）：
/// - 表单常驻挂载 + hidden 切换（收起不丢已填内容——十几字段误触丢稿
///   不可接受；展开时自动聚焦 URL 输入）
/// - Esc 不收表单（与详情抽屉 Esc 分层不同：这里全是用户输入，误关代价
///   高；收起走「取消」钮/工具栏开关，title 同步勘误）
/// - 校验和三选一下拉（sha256/sha1/md5 互斥——后端同给多个 400，UI 单选
///   天然满足）+ 长度即时校验（64/40/32 hex，红框 + 提示，禁提交）
/// - auto_retry 0..=10（后端越界 400；UI min/max 钳制 + 说明）
/// - 定时启动 datetime-local → unix 秒；过去时刻提示「立即开始」（后端
///   宽容语义透出，不拦）
/// - 请求头键值行动态增删（空键行提交时跳过）
/// - backup_md5 仅在 backup_url 已填时可编辑（成对语义的 UI 投影）
/// - 成功才收起 + toast（metalink 多任务展示 count）；失败留在表单内
///   联播后端错误原文（不 toast 一闪而过——用户要对照修改）
/// - 文件读取 FileReader.readAsDataURL（大文件安全——不用 fromCharCode
///   展开栈溢出的路径）；读后显示文件名 + 体积徽章，可移除重选

import { useEffect, useRef, useState } from "react";
import type { AddTaskReq, ProbeOut } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { fmtBytes } from "@/lib/daemon/format";
import { Icon } from "./Icon";

type SourceMode = "url" | "torrent" | "metalink";

type HeaderRow = { k: string; v: string };

/// 校验和档位（互斥；长度 = hex 位数）。
const CHECKSUM_KINDS = [
  { key: "sha256", label: "SHA-256", len: 64 },
  { key: "sha1", label: "SHA-1", len: 40 },
  { key: "md5", label: "MD5", len: 32 },
] as const;
type ChecksumKind = (typeof CHECKSUM_KINDS)[number]["key"];

const CONFLICT_POLICIES = [
  { value: "overwrite", label: "覆盖（默认）" },
  { value: "rename", label: "自动改名 name(1).ext" },
  { value: "skip", label: "已存在则跳过" },
] as const;

/// datetime-local 值 → unix 秒（本地时区；空/无效 → undefined）。
function localToUnix(v: string): number | undefined {
  if (!v) return undefined;
  const ms = new Date(v).getTime();
  return Number.isFinite(ms) ? Math.floor(ms / 1000) : undefined;
}

/// 文件 → 标准 base64（readAsDataURL 取逗号后段；大文件安全）。
function fileToB64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const r = new FileReader();
    r.onload = () => {
      const s = String(r.result ?? "");
      const i = s.indexOf(",");
      if (i < 0) reject(new Error("文件读取异常"));
      else resolve(s.slice(i + 1));
    };
    r.onerror = () => reject(new Error("文件读取失败"));
    r.readAsDataURL(file);
  });
}

export default function NewTaskForm({
  open,
  onOpen,
  onAdded,
}: {
  /** 展开态（DlApp adding；aria-expanded 数据源） */
  open: boolean;
  onOpen: (v: boolean) => void;
  /** 添加成功回调（DlApp：refresh + toast 由本组件自理后通知） */
  onAdded: (info: { count: number }) => void;
}) {
  // ---- 源 ----
  const [mode, setMode] = useState<SourceMode>("url");
  const [url, setUrl] = useState("");
  const [file, setFile] = useState<{ name: string; size: number; b64: string } | null>(null);
  const [fileErr, setFileErr] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const urlInputRef = useRef<HTMLInputElement>(null);

  // ---- 通用 ----
  const [name, setName] = useState("");
  const [dest, setDest] = useState("");

  // ---- 高级 ----
  const [down, setDown] = useState("");
  const [up, setUp] = useState("");
  const [sequential, setSequential] = useState(false);
  const [proxy, setProxy] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [headerRows, setHeaderRows] = useState<HeaderRow[]>([]);
  const [checksumKind, setChecksumKind] = useState<ChecksumKind>("sha256");
  const [checksumVal, setChecksumVal] = useState("");
  const [backupUrl, setBackupUrl] = useState("");
  const [backupMd5, setBackupMd5] = useState("");
  const [conflict, setConflict] = useState("");
  const [startAt, setStartAt] = useState("");
  const [autoRetry, setAutoRetry] = useState("");

  // ---- 状态 ----
  const [submitting, setSubmitting] = useState(false);
  const [probing, setProbing] = useState(false);
  const [probeOut, setProbeOut] = useState<ProbeOut | null>(null);
  const [probeErr, setProbeErr] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  /// 展开时聚焦 URL（常驻挂载下 autoFocus 只在首挂载生效——用 effect 驱动）。
  useEffect(() => {
    if (open) requestAnimationFrame(() => urlInputRef.current?.focus());
  }, [open]);

  /// 校验和长度校验（当前档位）。
  const checksumLen = CHECKSUM_KINDS.find((k) => k.key === checksumKind)!.len;
  const checksumInvalid =
    checksumVal.trim().length > 0 &&
    !new RegExp(`^[0-9a-fA-F]{${checksumLen}}$`).test(checksumVal.trim());
  const backupMd5Invalid =
    backupMd5.trim().length > 0 && !/^[0-9a-fA-F]{32}$/.test(backupMd5.trim());
  const backupPairErr =
    backupMd5.trim().length > 0 && !backupUrl.trim() ? "backup_md5 须与 backup_url 成对提供" : null;
  const retryVal = autoRetry.trim() === "" ? null : Number(autoRetry);
  const retryInvalid = retryVal != null && (!Number.isInteger(retryVal) || retryVal < 0 || retryVal > 10);

  const hasSource = mode === "url" ? url.trim().length > 0 : file != null;
  const canSubmit =
    !submitting && hasSource && !checksumInvalid && !backupMd5Invalid && !backupPairErr && !retryInvalid && !fileErr;

  const pickFile = async (f: File | null | undefined) => {
    if (!f) return;
    setFileErr(null);
    try {
      const b64 = await fileToB64(f);
      setFile({ name: f.name, size: f.size, b64 });
    } catch (e) {
      setFile(null);
      setFileErr(String(e));
    }
  };

  const resetAfterSuccess = () => {
    setUrl("");
    setFile(null);
    setName("");
    setDest("");
    setDown("");
    setUp("");
    setSequential(false);
    setProxy("");
    setUsername("");
    setPassword("");
    setHeaderRows([]);
    setChecksumVal("");
    setBackupUrl("");
    setBackupMd5("");
    setConflict("");
    setStartAt("");
    setAutoRetry("");
    setProbeOut(null);
    setProbeErr(null);
  };

  /// 探测预览（仅 URL 模式 http(s)；headers/proxy/auth 随发）。
  const runProbe = async () => {
    const u = url.trim();
    if (!u) return;
    setProbing(true);
    setProbeOut(null);
    setProbeErr(null);
    try {
      const headers: Record<string, string> = {};
      for (const h of headerRows) {
        if (h.k.trim()) headers[h.k.trim()] = h.v;
      }
      const out = await client.probe({
        url: u,
        proxy: proxy.trim() || undefined,
        username: username.trim() || undefined,
        password: username.trim() ? password : undefined,
        headers: Object.keys(headers).length > 0 ? headers : undefined,
      });
      setProbeOut(out);
    } catch (e) {
      setProbeErr(String(e));
    } finally {
      setProbing(false);
    }
  };

  const submit = async () => {
    if (!canSubmit) return;
    setSubmitting(true);
    setErr(null);
    try {
      const body: Partial<AddTaskReq> = {};
      // 源（三选一）
      if (mode === "url") body.url = url.trim();
      else if (mode === "torrent") body.torrent_b64 = file!.b64;
      else body.metalink_b64 = file!.b64;

      // 通用
      if (name.trim()) body.name = name.trim();
      if (dest.trim()) body.dest = dest.trim();

      // 高级（仅含有效值——空串不发，走后端缺省）
      if (down.trim()) {
        const n = Number(down);
        if (!Number.isFinite(n) || n < 0) throw new Error("下载限速必须为非负数字");
        body.down_kb_s = Math.floor(n);
      }
      if (up.trim()) {
        const n = Number(up);
        if (!Number.isFinite(n) || n < 0) throw new Error("上传限速必须为非负数字");
        body.up_kb_s = Math.floor(n);
      }
      if (sequential) body.sequential = true;
      if (proxy.trim()) body.proxy = proxy.trim();
      if (username.trim()) {
        body.username = username.trim();
        body.password = password;
      }
      const headers: Record<string, string> = {};
      for (const h of headerRows) {
        if (h.k.trim()) headers[h.k.trim()] = h.v;
      }
      if (Object.keys(headers).length > 0) body.headers = headers;
      if (checksumVal.trim()) {
        if (checksumKind === "sha256") body.sha256 = checksumVal.trim().toLowerCase();
        else if (checksumKind === "sha1") body.sha1 = checksumVal.trim().toLowerCase();
        else body.md5 = checksumVal.trim().toLowerCase();
      }
      if (backupUrl.trim()) {
        body.backup_url = backupUrl.trim();
        if (backupMd5.trim()) body.backup_md5 = backupMd5.trim().toLowerCase();
      }
      if (conflict) body.conflict_policy = conflict;
      const start = localToUnix(startAt);
      if (start != null && start > 0) body.start_at_unix = start;
      if (retryVal != null) body.auto_retry = retryVal;

      const res = await client.addTask(body);
      const count = res.count ?? res.task_ids.length ?? 1;
      resetAfterSuccess();
      onOpen(false);
      onAdded({ count });
    } catch (e) {
      setErr(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  const startHint =
    startAt && (localToUnix(startAt) ?? 0) <= Math.floor(Date.now() / 1000)
      ? "所选时刻已过去——将立即开始"
      : null;

  return (
    <div
      className="dl-card dl-add-bar"
      id="dl-add-bar"
      hidden={!open}
      aria-label="新建任务"
    >
      {/* 源模式切换（AddTaskReq 三源优先级的 UI 投影） */}
      <div role="tablist" aria-label="任务来源" style={{ display: "flex", gap: 4, marginBottom: 10 }}>
        {(
          [
            { key: "url", label: "链接" },
            { key: "torrent", label: ".torrent 文件" },
            { key: "metalink", label: "Metalink 文件" },
          ] as const
        ).map((m) => (
          <button
            key={m.key}
            type="button"
            role="tab"
            aria-selected={mode === m.key}
            className={`dl-btn dl-btn--ghost ${mode === m.key ? "dl-btn--primary" : ""}`}
            data-testid={`src-mode-${m.key}`}
            onClick={() => setMode(m.key)}
          >
            {m.label}
          </button>
        ))}
      </div>

      {mode === "url" && (
        <div style={{ display: "grid", gap: 8 }}>
          <div style={{ display: "flex", gap: 6 }}>
            <input
              ref={urlInputRef}
              className="dl-input dl-add-input"
              placeholder="http(s):// · ftp(s):// · magnet:?xt=… · .m3u8 · .meta4"
              aria-label="下载链接"
              data-testid="add-url-input"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
              }}
            />
            <button
              type="button"
              className="dl-btn"
              disabled={probing || !url.trim() || !/^https?:\/\//.test(url.trim())}
              title={
                /^https?:\/\//.test(url.trim())
                  ? "探测大小/落盘名/Range 支持（不建任务；headers/代理/认证随探测下发）"
                  : "仅 http(s) 直链可预览"
              }
              data-testid="probe-btn"
              onClick={runProbe}
            >
              {probing ? "探测中…" : "预览"}
            </button>
          </div>
          {probeOut && (
            <div className="dl-field-help" role="status" data-testid="probe-result" style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
              <span>落盘名 <b>{probeOut.suggest_name}</b></span>
              <span>大小 <b>{probeOut.total != null ? fmtBytes(probeOut.total) : "未知"}</b></span>
              <span>断点续传 <b>{probeOut.range_supported ? "支持" : "不支持"}</b></span>
              {probeOut.content_type && <span>类型 {probeOut.content_type}</span>}
            </div>
          )}
          {probeErr && (
            <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
              探测失败：{probeErr}
            </div>
          )}
        </div>
      )}

      {(mode === "torrent" || mode === "metalink") && (
        <div style={{ display: "grid", gap: 8 }}>
          <input
            ref={fileInputRef}
            type="file"
            className="dl-input"
            style={{ display: "none" }}
            accept={mode === "torrent" ? ".torrent" : ".metalink,.meta4,.xml"}
            aria-label={mode === "torrent" ? "选择 .torrent 文件" : "选择 Metalink 文件"}
            data-testid="file-input"
            onChange={(e) => pickFile(e.target.files?.[0])}
          />
          <div style={{ display: "flex", gap: 6, alignItems: "center" }}>
            <button
              type="button"
              className="dl-btn"
              data-testid="file-pick"
              onClick={() => fileInputRef.current?.click()}
            >
              <Icon name="file" size={13} /> 选择{mode === "torrent" ? " .torrent" : " Metalink"}文件
            </button>
            {file && (
              <>
                <span className="dl-badge" title={file.name}>
                  {file.name}
                </span>
                <span className="dl-badge">{fmtBytes(file.size)}</span>
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost dl-act-btn"
                  aria-label="移除已选文件"
                  title="移除重选"
                  onClick={() => {
                    setFile(null);
                    if (fileInputRef.current) fileInputRef.current.value = "";
                  }}
                >
                  <Icon name="close" size={12} />
                </button>
              </>
            )}
          </div>
          <div className="dl-field-help">
            {mode === "torrent"
              ? "文件内容以 base64 提交（torrent_b64）；添加后进入元数据获取阶段。"
              : "RFC 5854 XML，逐 <file> 展开为多任务（响应含数量）；内建哈希直通校验链。"}
          </div>
          {fileErr && (
            <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
              {fileErr}
            </div>
          )}
        </div>
      )}

      {/* 通用选项 */}
      <div className="dl-form-grid" style={{ marginTop: 10 }}>
        <label className="dl-field-label" htmlFor="ntf-name">
          保存名{mode === "url" ? "" : "（HTTP 任务有效；本模式忽略）"}
        </label>
        <input
          id="ntf-name"
          className="dl-input"
          placeholder="留空 = 引擎派生（Content-Disposition → URL 末段）"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <label className="dl-field-label" htmlFor="ntf-dest">
          目标目录
        </label>
        <input
          id="ntf-dest"
          className="dl-input"
          placeholder="留空 = [download] dest_root（相对路径）"
          value={dest}
          onChange={(e) => setDest(e.target.value)}
        />
      </div>

      {/* 高级选项（默认折叠——高频路径是「贴链接回车」） */}
      <details style={{ marginTop: 10 }} data-testid="advanced-details">
        <summary className="dl-field-label" style={{ cursor: "pointer", userSelect: "none" }} data-testid="advanced-toggle">
          高级选项（限速 / 认证 / 请求头 / 校验 / 备源 / 定时 / 重试）
        </summary>
        <div className="dl-form-grid" style={{ marginTop: 8 }}>
          <label className="dl-field-label" htmlFor="ntf-down">下载限速 (KiB/s)</label>
          <input id="ntf-down" className="dl-input" inputMode="numeric" placeholder="0 = 不限；留空 = 走全局" value={down} onChange={(e) => setDown(e.target.value)} />
          <label className="dl-field-label" htmlFor="ntf-up">上传限速 (KiB/s)</label>
          <input id="ntf-up" className="dl-input" inputMode="numeric" placeholder="仅 BT；0 = 不限" value={up} onChange={(e) => setUp(e.target.value)} />
          <label className="dl-field-label">顺序下载</label>
          <button
            type="button"
            className={`dl-btn dl-btn--ghost ${sequential ? "dl-btn--primary" : ""}`}
            aria-pressed={sequential}
            onClick={() => setSequential((v) => !v)}
          >
            <Icon name={sequential ? "check" : "close"} size={13} /> {sequential ? "开启（边下边播）" : "关闭"}
          </button>

          <label className="dl-field-label" htmlFor="ntf-proxy">代理</label>
          <input id="ntf-proxy" className="dl-input" placeholder="http(s)/socks4/socks5://host:port（仅 HTTP 任务）" value={proxy} onChange={(e) => setProxy(e.target.value)} />

          <label className="dl-field-label" htmlFor="ntf-user">HTTP Basic 认证</label>
          <div style={{ display: "flex", gap: 6 }}>
            <input id="ntf-user" className="dl-input" placeholder="用户名" autoComplete="off" value={username} onChange={(e) => setUsername(e.target.value)} />
            <input className="dl-input" type="password" placeholder="密码" autoComplete="new-password" aria-label="密码" value={password} onChange={(e) => setPassword(e.target.value)} />
          </div>

          <label className="dl-field-label">自定义请求头</label>
          <div style={{ display: "grid", gap: 4 }}>
            {headerRows.map((h, i) => (
              <div key={i} style={{ display: "flex", gap: 6 }}>
                <input
                  className="dl-input"
                  style={{ flex: 1 }}
                  placeholder="Header（如 Referer）"
                  aria-label={`请求头 ${i + 1} 名`}
                  value={h.k}
                  onChange={(e) =>
                    setHeaderRows((cur) => cur.map((x, j) => (j === i ? { ...x, k: e.target.value } : x)))
                  }
                />
                <input
                  className="dl-input"
                  style={{ flex: 1.4 }}
                  placeholder="值"
                  aria-label={`请求头 ${i + 1} 值`}
                  value={h.v}
                  onChange={(e) =>
                    setHeaderRows((cur) => cur.map((x, j) => (j === i ? { ...x, v: e.target.value } : x)))
                  }
                />
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost dl-act-btn"
                  aria-label={`移除请求头 ${i + 1}`}
                  onClick={() => setHeaderRows((cur) => cur.filter((_, j) => j !== i))}
                >
                  <Icon name="close" size={12} />
                </button>
              </div>
            ))}
            <div>
              <button
                type="button"
                className="dl-btn dl-btn--ghost"
                data-testid="header-add"
                onClick={() => setHeaderRows((cur) => [...cur, { k: "", v: "" }])}
              >
                <Icon name="plus" size={12} /> 加一行
              </button>
            </div>
          </div>

          <label className="dl-field-label" htmlFor="ntf-checksum">内容校验（三选一）</label>
          <div style={{ display: "flex", gap: 6 }}>
            <select
              id="ntf-checksum"
              className="dl-prio-select"
              style={{ minWidth: 96 }}
              aria-label="校验算法"
              value={checksumKind}
              onChange={(e) => setChecksumKind(e.target.value as ChecksumKind)}
            >
              {CHECKSUM_KINDS.map((k) => (
                <option key={k.key} value={k.key}>{k.label}</option>
              ))}
            </select>
            <input
              className="dl-input"
              style={{ flex: 1 }}
              placeholder={`${checksumLen} 位十六进制（仅 HTTP 任务）`}
              aria-label="校验值"
              data-testid="checksum-input"
              value={checksumVal}
              onChange={(e) => setChecksumVal(e.target.value)}
            />
          </div>
          {checksumInvalid && (
            <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
              {CHECKSUM_KINDS.find((k) => k.key === checksumKind)!.label} 须为 {checksumLen} 位十六进制（当前 {checksumVal.trim().length} 位）
            </div>
          )}

          <label className="dl-field-label" htmlFor="ntf-backup">备用源 URL</label>
          <input id="ntf-backup" className="dl-input" placeholder="主源失败自动切换（仅 HTTP）" value={backupUrl} onChange={(e) => setBackupUrl(e.target.value)} />
          <label className="dl-field-label" htmlFor="ntf-backup-md5">备用源 MD5</label>
          <input
            id="ntf-backup-md5"
            className="dl-input"
            placeholder="32 位十六进制；须与备用源成对"
            aria-label="备用源 MD5"
            value={backupMd5}
            disabled={!backupUrl.trim()}
            onChange={(e) => setBackupMd5(e.target.value)}
          />
          {backupMd5Invalid && (
            <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
              MD5 须为 32 位十六进制
            </div>
          )}
          {backupPairErr && (
            <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
              {backupPairErr}
            </div>
          )}

          <label className="dl-field-label" htmlFor="ntf-conflict">文件冲突策略</label>
          <select
            id="ntf-conflict"
            className="dl-prio-select"
            style={{ width: "100%" }}
            aria-label="文件冲突策略"
            value={conflict}
            onChange={(e) => setConflict(e.target.value)}
          >
            <option value="">默认（覆盖）</option>
            {CONFLICT_POLICIES.map((p) => (
              <option key={p.value} value={p.value}>{p.label}</option>
            ))}
          </select>

          <label className="dl-field-label" htmlFor="ntf-start">定时启动</label>
          <input
            id="ntf-start"
            className="dl-input"
            type="datetime-local"
            aria-label="定时启动时刻"
            data-testid="start-at-input"
            value={startAt}
            onChange={(e) => setStartAt(e.target.value)}
          />
          {startHint && <div className="dl-field-help">{startHint}</div>}

          <label className="dl-field-label" htmlFor="ntf-retry">自动重试</label>
          <input
            id="ntf-retry"
            className="dl-input"
            inputMode="numeric"
            placeholder="0..=10；留空 = 不重试（仅 HTTP/FTP）"
            aria-label="失败自动重试次数上限"
            data-testid="auto-retry-input"
            value={autoRetry}
            onChange={(e) => setAutoRetry(e.target.value.replace(/[^0-9]/g, ""))}
          />
          {retryInvalid && (
            <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
              自动重试须为 0..=10 的整数
            </div>
          )}
        </div>
      </details>

      {/* 动作行 */}
      <div style={{ display: "flex", gap: 8, marginTop: 12, alignItems: "center" }}>
        <button
          type="button"
          className="dl-btn dl-btn--primary"
          disabled={!canSubmit}
          data-testid="add-submit"
          title={hasSource ? "添加任务（Enter）" : mode === "url" ? "先填链接" : "先选择文件"}
          onClick={submit}
        >
          {submitting ? "添加中…" : "添加"}
        </button>
        <button
          type="button"
          className="dl-btn"
          disabled={submitting}
          onClick={() => onOpen(false)}
        >
          取消
        </button>
        <span className="dl-field-help" style={{ flex: 1 }}>
          Esc 不收起表单（防误触丢稿）；收起用「取消」。
        </span>
      </div>

      {err && (
        <div className="dl-alert dl-alert--error" role="alert" style={{ marginTop: 10 }}>
          {err}
        </div>
      )}
    </div>
  );
}
