"use client";
/// BT 任务种源管理面板（#143 第二梯队「种源管理」——BT 用户 80% 排障动作）。
/// 全部能力仅 BT 任务（其余引擎 409，面板不渲染）；magnet 元数据未就绪的
/// BT 任务部分操作可 409/失败——错误原样透出（daemon 文案已中文化）。
///
/// 交互细节（用户硬性要求「UI 实现要扎实」逐条落实）：
/// - tracker 表：tier 徽章 + URL rtl 截断（长 URL 保尾部可见——announce
///   key 在 query 尾，排障时尾部更有信息量）+ 逐行删除（无确认——可逆，
///   删错可再加回）；空表/加载/失败三态齐备
/// - 追加输入（tracker/peer/webseed 共用模式）：textarea 多行（一行一条），
///   Ctrl+Enter 提交（多行输入 Enter 换行是惯例，不劫持）；提交前 trim +
///   去空行 + 去重；空输入禁用提交
/// - peer 注入：部分成功语义——逐条回执（✓ addr / ✗ addr: error），
///   全失败也展示（后端 200/409 都带 results 体）
/// - 超级做种：引擎 set-only（快照无回读字段），用动作钮对（开启/关闭）
///   而非持久开关——不谎报状态
/// - 强制宣告：tracker / DHT 两钮独立（AnnounceReq 语义：至少一项 true）
/// - 重新校验：ConfirmDialog 二次确认（进度回零重走，长任务代价高）
/// - magnet：复制到剪贴板（clipboard API + execCommand 双保险——非安全
///   上下文 file:// / http:// 内网部署时 fallback）+ 只读输入框兜底手选
/// - 导出 .torrent：blob 下载，文件名从 Content-Disposition 解析（store 侧）
/// - 所有动作：busy 独立禁用 + 成功/失败反馈（ok/err 分色）+ 完成后
///   onChanged() 通知父级刷新快照

import { useCallback, useEffect, useRef, useState } from "react";
import type { PeerOutcome, TrackerEntry } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { Icon } from "./Icon";
import { ConfirmDialog } from "./Common";

/// textarea → 行数组（trim + 去空 + 保序去重）。
function parseLines(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || seen.has(line)) continue;
    seen.add(line);
    out.push(line);
  }
  return out;
}

/// 多行追加输入（tracker/peer/webseed 共用）：label + textarea + 提交钮。
/// Ctrl+Enter 提交；行计数实时提示（去空去重后）。
function LineInput({
  label,
  placeholder,
  value,
  onChange,
  onSubmit,
  busy,
  disabled,
  submitText,
  testId,
}: {
  label: string;
  placeholder: string;
  value: string;
  onChange: (v: string) => void;
  onSubmit: () => void;
  busy: boolean;
  disabled?: boolean;
  submitText: string;
  testId: string;
}) {
  const lines = parseLines(value);
  return (
    <div style={{ display: "grid", gap: 6 }}>
      <label className="dl-field-label" style={{ display: "flex", alignItems: "center", gap: 8 }}>
        {label}
        {value.trim() && lines.length > 0 && (
          <span className="dl-badge">{lines.length} 条</span>
        )}
      </label>
      <textarea
        className="dl-textarea"
        placeholder={placeholder}
        aria-label={label}
        data-testid={testId}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            if (!busy && lines.length > 0) onSubmit();
          }
        }}
      />
      <div>
        <button
          type="button"
          className="dl-btn"
          disabled={busy || disabled || lines.length === 0}
          data-testid={`${testId}-submit`}
          title={`追加（Ctrl+Enter）`}
          onClick={onSubmit}
        >
          <Icon name="plus" size={13} /> {submitText}
        </button>
      </div>
    </div>
  );
}

export default function TaskBtPanel({
  taskId,
  onChanged,
}: {
  taskId: string;
  onChanged: () => void;
}) {
  // ---- tracker 表 ----
  const [trackers, setTrackers] = useState<TrackerEntry[] | null>(null);
  const [trackErr, setTrackErr] = useState<string | null>(null);
  const [trackBusy, setTrackBusy] = useState(false);
  const [trackInput, setTrackInput] = useState("");
  const [removing, setRemoving] = useState<string | null>(null);

  // ---- 追加类输入 ----
  const [peerInput, setPeerInput] = useState("");
  const [peerResults, setPeerResults] = useState<PeerOutcome[] | null>(null);
  const [peerBusy, setPeerBusy] = useState(false);
  const [webseedInput, setWebseedInput] = useState("");
  const [webseedBusy, setWebseedBusy] = useState(false);

  // ---- 操作反馈 ----
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const [actBusy, setActBusy] = useState<string | null>(null);
  const [magnetText, setMagnetText] = useState<string | null>(null);
  const [confirmRecheck, setConfirmRecheck] = useState(false);
  const magnetInputRef = useRef<HTMLInputElement>(null);

  const loadTrackers = useCallback(async () => {
    setTrackBusy(true);
    setTrackErr(null);
    try {
      setTrackers(await client.listTrackers(taskId));
    } catch (e) {
      setTrackers(null);
      setTrackErr(String(e));
    } finally {
      setTrackBusy(false);
    }
  }, [taskId]);

  useEffect(() => {
    loadTrackers();
  }, [loadTrackers]);

  /// 统一动作执行：busy 标记（按动作键）+ 反馈 + 完成回调。
  const run = useCallback(
    async (key: string, fn: () => Promise<string>, opts?: { silent?: boolean }) => {
      setActBusy(key);
      try {
        const text = await fn();
        if (!opts?.silent) setMsg({ ok: true, text });
      } catch (e) {
        setMsg({ ok: false, text: String(e) });
      } finally {
        setActBusy(null);
        onChanged();
      }
    },
    [onChanged],
  );

  const addTrackers = async () => {
    const urls = parseLines(trackInput);
    if (urls.length === 0) return;
    setTrackBusy(true);
    try {
      const { added } = await client.addTrackers(taskId, urls);
      setTrackInput("");
      setMsg({
        ok: true,
        text: added === urls.length ? `已追加 ${added} 个 tracker` : `已追加 ${added}/${urls.length} 个 tracker（其余可能已存在）`,
      });
      await loadTrackers();
    } catch (e) {
      setMsg({ ok: false, text: `追加失败：${String(e)}` });
    } finally {
      setTrackBusy(false);
    }
  };

  const removeTracker = (url: string) => {
    setRemoving(url);
    run(
      `track-del:${url}`,
      async () => {
        await client.removeTracker(taskId, url);
        await loadTrackers();
        return "tracker 已移除";
      },
      { silent: true },
    ).finally(() => setRemoving(null));
  };

  const addPeers = async () => {
    const addrs = parseLines(peerInput);
    if (addrs.length === 0) return;
    setPeerBusy(true);
    setPeerResults(null);
    try {
      const { results } = await client.addPeers(taskId, addrs);
      setPeerResults(results);
      const ok = results.filter((r) => r.ok).length;
      if (ok > 0) setPeerInput("");
      setMsg({
        ok: ok > 0,
        text: ok === addrs.length ? `已注入 ${ok} 个 peer` : `注入 ${ok}/${addrs.length}（失败项见回执）`,
      });
      onChanged();
    } catch (e) {
      setMsg({ ok: false, text: `注入失败：${String(e)}` });
    } finally {
      setPeerBusy(false);
    }
  };

  const addWebseeds = async () => {
    const urls = parseLines(webseedInput);
    if (urls.length === 0) return;
    setWebseedBusy(true);
    try {
      const { added } = await client.addWebseeds(taskId, urls);
      if (added > 0) setWebseedInput("");
      setMsg({
        ok: added > 0,
        text: added > 0 ? `已注入 ${added} 个 web seed` : `注入 0 个（URL 可能已存在或不合法）`,
      });
      onChanged();
    } catch (e) {
      setMsg({ ok: false, text: `注入失败：${String(e)}` });
    } finally {
      setWebseedBusy(false);
    }
  };

  /// 剪贴板双保险：clipboard API 失败（非安全上下文）→ execCommand 兜底。
  const copyMagnet = async () => {
    const text = magnetText ?? (await client.taskMagnet(taskId)).magnet;
    setMagnetText(text);
    try {
      await navigator.clipboard.writeText(text);
      setMsg({ ok: true, text: "magnet 已复制到剪贴板" });
    } catch {
      const el = magnetInputRef.current;
      if (el) {
        el.select();
        if (document.execCommand("copy")) {
          setMsg({ ok: true, text: "magnet 已复制（兼容模式）" });
          return;
        }
      }
      setMsg({ ok: false, text: "复制失败——请手动选中下方链接复制" });
    }
  };

  return (
    <>
      <div className="dl-card">
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <Icon name="radio-tower" size={15} className="dl-drawer-ico" />
          <b>Tracker 管理</b>
          <span className="dl-badge">
            {trackers == null ? "…" : trackers.length}
          </span>
          <div style={{ flex: 1 }} />
          <button
            type="button"
            className="dl-btn dl-btn--ghost"
            aria-label="刷新 tracker 表"
            title="刷新 tracker 表"
            disabled={trackBusy}
            data-testid="trackers-refresh"
            onClick={loadTrackers}
          >
            <Icon name="sync" size={13} />
          </button>
        </div>

        {trackErr ? (
          <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
            加载失败：{trackErr}
          </div>
        ) : trackers == null ? (
          <div className="dl-field-help">加载中…</div>
        ) : trackers.length === 0 ? (
          <div className="dl-field-help" style={{ marginBottom: 10 }}>
            暂无 tracker（DHT 仅网络仍可工作；追加 below 可加回）
          </div>
        ) : (
          <div className="dl-track-list" role="list" aria-label="tracker 列表" style={{ marginBottom: 10 }}>
            {trackers.map((t) => (
              <div key={t.url} className="dl-track-row" role="listitem">
                <span className="dl-badge" title={`tier ${t.tier}（小者优先）`}>
                  T{t.tier}
                </span>
                <span className="dl-track-url" title={t.url}>
                  {t.url}
                </span>
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost dl-act-btn"
                  aria-label={`移除 tracker ${t.url}`}
                  title="移除（可重新追加，无需确认）"
                  disabled={removing === t.url || trackBusy}
                  data-testid="tracker-remove"
                  onClick={() => removeTracker(t.url)}
                >
                  <Icon name="close" size={12} />
                </button>
              </div>
            ))}
          </div>
        )}

        <LineInput
          label="追加 tracker"
          placeholder={"一行一个 announce URL（http/https/udp/wss）\nCtrl+Enter 提交"}
          value={trackInput}
          onChange={setTrackInput}
          onSubmit={addTrackers}
          busy={trackBusy}
          submitText="追加"
          testId="trackers-add"
        />
      </div>

      <div className="dl-card">
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <Icon name="server" size={15} className="dl-drawer-ico" />
          <b>Peer / Web seed 注入</b>
        </div>
        <div style={{ display: "grid", gap: 12 }}>
          <LineInput
            label="手动添加 peer"
            placeholder={"一行一个 ip:port（IPv6 形如 [::1]:6881）"}
            value={peerInput}
            onChange={setPeerInput}
            onSubmit={addPeers}
            busy={peerBusy}
            submitText="注入 peer"
            testId="peers-add"
          />
          {peerResults && peerResults.length > 0 && (
            <div className="dl-peer-results" aria-label="peer 注入回执">
              {peerResults.map((r) => (
                <div key={r.addr} className={r.ok ? "ok" : "err"}>
                  {r.ok ? "✓" : "✗"} {r.addr}
                  {!r.ok && r.error ? ` — ${r.error}` : ""}
                </div>
              ))}
            </div>
          )}
          <LineInput
            label="注入 web seed（云盘直链）"
            placeholder={"一行一个直链 URL（须原样携带 at= 签名参数）"}
            value={webseedInput}
            onChange={setWebseedInput}
            onSubmit={addWebseeds}
            busy={webseedBusy}
            submitText="注入 web seed"
            testId="webseeds-add"
          />
        </div>
      </div>

      <div className="dl-card">
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 10 }}>
          <Icon name="zap" size={15} className="dl-drawer-ico" />
          <b>BT 操作</b>
        </div>
        <div style={{ display: "grid", gap: 8 }}>
          <div style={{ display: "flex", gap: 6, flexWrap: "wrap", alignItems: "center" }}>
            <button
              type="button"
              className="dl-btn dl-btn--ghost"
              disabled={actBusy != null}
              title="做种态生效（超级种子模式：只向已完成的 peer 宣告零散块，加速冷门种子分发）"
              data-testid="superseeding-on"
              onClick={() => run("ss-on", async () => {
                await client.taskSuperSeeding(taskId, true);
                return "超级做种已开启";
              })}
            >
              <Icon name="check" size={13} /> 开启超级做种
            </button>
            <button
              type="button"
              className="dl-btn dl-btn--ghost"
              disabled={actBusy != null}
              title="关闭超级种子模式"
              data-testid="superseeding-off"
              onClick={() => run("ss-off", async () => {
                await client.taskSuperSeeding(taskId, false);
                return "超级做种已关闭";
              })}
            >
              <Icon name="close" size={13} /> 关闭
            </button>
          </div>

          <div style={{ display: "flex", gap: 6, flexWrap: "wrap", alignItems: "center" }}>
            <button
              type="button"
              className="dl-btn"
              disabled={actBusy != null}
              title="立即向全部 tracker 强制宣告（不等下一周期）"
              data-testid="announce-tracker"
              onClick={() => run("ann-t", async () => {
                await client.taskAnnounce(taskId, true, false);
                return "已强制向全部 tracker 宣告";
              })}
            >
              <Icon name="radio-tower" size={13} /> 强制宣告 tracker
            </button>
            <button
              type="button"
              className="dl-btn"
              disabled={actBusy != null}
              title="强制 DHT 宣告（DHT 未启用时内核 no-op）"
              data-testid="announce-dht"
              onClick={() => run("ann-d", async () => {
                await client.taskAnnounce(taskId, false, true);
                return "已强制 DHT 宣告";
              })}
            >
              <Icon name="globe" size={13} /> 宣告 DHT
            </button>
          </div>

          <div style={{ display: "flex", gap: 6, flexWrap: "wrap", alignItems: "center" }}>
            <button
              type="button"
              className="dl-btn"
              disabled={actBusy != null}
              title="全量重新校验已落盘数据（进度回零重走）"
              data-testid="recheck-btn"
              onClick={() => setConfirmRecheck(true)}
            >
              <Icon name="sync" size={13} /> 重新校验
            </button>
            <button
              type="button"
              className="dl-btn dl-btn--ghost"
              disabled={actBusy != null}
              title="生成 magnet 链接（含当前 tracker 表）并复制"
              data-testid="magnet-copy"
              onClick={() =>
                run(
                  "magnet",
                  async () => {
                    // copyMagnet 内部自理成功/剪贴板失败反馈；此处仅兜
                    // taskMagnet() 网络错误（run 的 catch 接住）。
                    await copyMagnet();
                    return "";
                  },
                  { silent: true },
                )
              }
            >
              <Icon name="magnet" size={13} /> 复制 magnet
            </button>
            <button
              type="button"
              className="dl-btn dl-btn--ghost"
              disabled={actBusy != null}
              title="导出 .torrent 文件"
              data-testid="export-torrent"
              onClick={() => run("export", async () => {
                const name = await client.exportTorrent(taskId);
                return `已导出 ${name}`;
              })}
            >
              <Icon name="cloud-download" size={13} /> 导出 .torrent
            </button>
          </div>

          {magnetText && (
            <input
              ref={magnetInputRef}
              className="dl-input"
              readOnly
              aria-label="magnet 链接（可手动选中复制）"
              value={magnetText}
              style={{ fontSize: "var(--fs-sm)", fontFamily: "var(--font-mono, ui-monospace, monospace)" }}
              onFocus={(e) => e.target.select()}
            />
          )}
        </div>

        {msg && (
          <div
            className={`dl-alert ${msg.ok ? "" : "dl-alert--error"}`}
            role="status"
            style={{ marginTop: 10 }}
          >
            {msg.text}
          </div>
        )}
      </div>

      {confirmRecheck && (
        <ConfirmDialog
          title="强制重新校验"
          body="将对已落盘数据做全量哈希重算：任务进度会回零重走（校验通过的部分会快速跳过），期间上传/下载暂停。确定继续吗？"
          confirmText="开始校验"
          onCancel={() => setConfirmRecheck(false)}
          onConfirm={() => {
            setConfirmRecheck(false);
            run("recheck", async () => {
              await client.taskRecheck(taskId);
              return "重新校验已启动";
            });
          }}
        />
      )}
    </>
  );
}
