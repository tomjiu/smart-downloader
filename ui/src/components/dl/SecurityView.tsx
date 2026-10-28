"use client";
/// IP 封禁页（#143 第二梯队 Wave C——「IP 封禁页 /security/bans(4)，PT 用户」）。
///
/// 能力面（全部走 BT 引擎级黑名单；非 BT 引擎构建下封禁/解禁/导入会返回
/// 「BT 引擎不可用」逐条回执——UI 原样透出，不隐藏该边界）：
/// - 封禁表：列表加载 + 计数徽章 + 刷新；逐行解禁（幂等，可逆不加确认）
/// - 批量封禁：textarea 一行一条（IP / CIDR / 区间），Ctrl+Enter 提交，
///   trim/去空/去重；逐条回执（✓/✗ + 后端 error 原文——非法 IP 与
///   引擎不可用都在这里露出）
/// - 段导入：DAT（PeerGuardian）/ P2P 格式文本粘贴（`#` 注释行跳过）；
///   imported 计数 + 带行号的错误列表
/// - 全部解禁：ConfirmDialog 二次确认（一次性清空，影响面大）
///
/// 交互细节：与 TaskBtPanel 同一套多行输入模式（LineInput 语义）；空表
/// 空态 + 加载态 + 错误态三态齐备；操作后列表回填（响应自带 banned）。

import { useCallback, useEffect, useState } from "react";
import type { BanOutcome } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { Icon } from "./Icon";
import { ConfirmDialog, EmptyState } from "./Common";

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

export default function SecurityView({
  onToast,
}: {
  onToast: (t: { kind: string; text: string }) => void;
}) {
  const [bans, setBans] = useState<string[] | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const [banInput, setBanInput] = useState("");
  const [results, setResults] = useState<BanOutcome[] | null>(null);
  const [importInput, setImportInput] = useState("");
  const [importOut, setImportOut] = useState<{ imported: number; errors: string[] } | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);

  const load = useCallback(async () => {
    try {
      setLoadErr(null);
      const { banned } = await client.listBans();
      setBans(banned);
    } catch (e) {
      setLoadErr(String(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const banLines = parseLines(banInput);
  const importLines = importInput
    .split("\n")
    .filter((l) => l.trim() && !l.trim().startsWith("#")).length;

  const addBans = async () => {
    const ips = parseLines(banInput);
    if (ips.length === 0) return;
    setBusy(true);
    setResults(null);
    try {
      const out = await client.banIps(ips);
      setBans(out.banned);
      setResults(out.results);
      const ok = out.results.filter((r) => r.ok).length;
      if (ok > 0) setBanInput("");
      onToast({
        kind: ok > 0 ? "success" : "error",
        text: ok === ips.length ? `已封禁 ${ok} 项` : `封禁 ${ok}/${ips.length}（失败项见回执）`,
      });
    } catch (e) {
      onToast({ kind: "error", text: `封禁失败：${String(e)}` });
    } finally {
      setBusy(false);
    }
  };

  const unban = async (ips: string[]) => {
    if (ips.length === 0) return;
    setBusy(true);
    try {
      const out = await client.unbanIps(ips);
      setBans(out.banned);
      setResults(out.results);
      const ok = out.results.filter((r) => r.ok).length;
      onToast({
        kind: ok > 0 ? "success" : "error",
        text: ok === ips.length ? `已解禁 ${ok} 项` : `解禁 ${ok}/${ips.length}（失败项见回执）`,
      });
    } catch (e) {
      onToast({ kind: "error", text: `解禁失败：${String(e)}` });
    } finally {
      setBusy(false);
    }
  };

  const importBans = async () => {
    if (!importInput.trim()) return;
    setBusy(true);
    setImportOut(null);
    try {
      const out = await client.importBans(importInput);
      setBans(out.banned);
      setImportOut({ imported: out.imported, errors: out.errors });
      onToast({
        kind: out.imported > 0 ? "success" : "error",
        text: out.imported > 0 ? `已导入 ${out.imported} 个区间` : "导入 0 个区间（错误明细见下方）",
      });
    } catch (e) {
      onToast({ kind: "error", text: `导入失败：${String(e)}` });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="dl-page dl-page--narrow">
    <div className="dl-card" style={{ display: "grid", gap: 14 }}>
      <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
        <Icon name="shield" size={15} className="dl-drawer-ico" />
        <b>IP 封禁表</b>
        <span className="dl-badge">{bans == null ? "…" : bans.length}</span>
        <div style={{ flex: 1 }} />
        <button
          type="button"
          className="dl-btn dl-btn--ghost"
          aria-label="刷新封禁表"
          title="刷新封禁表"
          disabled={busy}
          data-testid="bans-refresh"
          onClick={load}
        >
          <Icon name="sync" size={13} />
        </button>
        {bans != null && bans.length > 0 && (
          <button
            type="button"
            className="dl-btn dl-btn--ghost dl-tool-btn--danger"
            disabled={busy}
            title="清空全部封禁（需确认）"
            data-testid="bans-clear"
            onClick={() => setConfirmClear(true)}
          >
            <Icon name="trash" size={13} /> 全部解禁
          </button>
        )}
      </div>

      {loadErr ? (
        <div className="dl-field-help" role="alert" style={{ color: "var(--err)" }}>
          加载失败：{loadErr}
        </div>
      ) : bans == null ? (
        <div className="dl-field-help">加载中…</div>
      ) : bans.length === 0 ? (
        <EmptyState
          icon="shield"
          title="暂无封禁项"
          hint="封禁的 IP / 区间将在 BT 引擎层拒绝对等连接（PT 屏蔽恶意 peer 常用）"
        />
      ) : (
        <div className="dl-track-list" role="list" aria-label="封禁列表" data-testid="bans-list">
          {bans.map((b) => (
            <div key={b} className="dl-track-row" role="listitem">
              <span className="dl-track-url" style={{ direction: "ltr" }} title={b}>
                {b}
              </span>
              <button
                type="button"
                className="dl-btn dl-btn--ghost dl-act-btn"
                aria-label={`解禁 ${b}`}
                title="解禁（幂等，可重新封禁）"
                disabled={busy}
                data-testid="ban-remove"
                onClick={() => unban([b])}
              >
                <Icon name="close" size={12} />
              </button>
            </div>
          ))}
        </div>
      )}

      <div style={{ display: "grid", gap: 6 }}>
        <label className="dl-field-label" style={{ display: "flex", alignItems: "center", gap: 8 }}>
          批量封禁
          {banInput.trim() && banLines.length > 0 && <span className="dl-badge">{banLines.length} 条</span>}
        </label>
        <textarea
          className="dl-textarea"
          placeholder={"一行一个：IP（1.2.3.4）/ CIDR（10.0.0.0/24）/ 区间（1.2.3.4-1.2.3.8）\nCtrl+Enter 提交"}
          aria-label="批量封禁输入"
          data-testid="bans-add-input"
          value={banInput}
          disabled={busy}
          onChange={(e) => setBanInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              if (!busy && banLines.length > 0) addBans();
            }
          }}
        />
        <div>
          <button
            type="button"
            className="dl-btn dl-btn--primary"
            disabled={busy || banLines.length === 0}
            data-testid="bans-add-submit"
            title="封禁（Ctrl+Enter）"
            onClick={addBans}
          >
            <Icon name="shield" size={13} /> 封禁
          </button>
        </div>
        {results && results.length > 0 && (
          <div className="dl-peer-results" aria-label="封禁回执" data-testid="bans-results">
            {results.map((r) => (
              <div key={r.ip} className={r.ok ? "ok" : "err"}>
                {r.ok ? "✓" : "✗"} {r.ip}
                {!r.ok && r.error ? ` — ${r.error}` : ""}
              </div>
            ))}
          </div>
        )}
      </div>

      <div style={{ display: "grid", gap: 6 }}>
        <label className="dl-field-label" style={{ display: "flex", alignItems: "center", gap: 8 }}>
          IP 段批量导入（DAT / P2P 格式）
          {importInput.trim() && importLines > 0 && <span className="dl-badge">{importLines} 行</span>}
        </label>
        <textarea
          className="dl-textarea"
          placeholder={"粘贴 PeerGuardian DAT 或 P2P 格式文本（逐行一个区间；# 注释与空行跳过）"}
          aria-label="IP 段导入输入"
          data-testid="bans-import-input"
          value={importInput}
          disabled={busy}
          onChange={(e) => setImportInput(e.target.value)}
        />
        <div>
          <button
            type="button"
            className="dl-btn"
            disabled={busy || !importInput.trim()}
            data-testid="bans-import-submit"
            onClick={importBans}
          >
            <Icon name="layers" size={13} /> 导入区间
          </button>
        </div>
        {importOut && (
          <div className="dl-peer-results" data-testid="bans-import-result">
            <div className={importOut.imported > 0 ? "ok" : "err"}>导入 {importOut.imported} 个区间</div>
            {importOut.errors.slice(0, 20).map((e, i) => (
              <div key={i} className="err">
                {e}
              </div>
            ))}
            {importOut.errors.length > 20 && (
              <div className="err">…另有 {importOut.errors.length - 20} 条错误未展示</div>
            )}
          </div>
        )}
      </div>

      {confirmClear && (
        <ConfirmDialog
          title={`清空全部 ${bans?.length ?? 0} 项封禁`}
          body="将解除全部 IP / 区间封禁（BT 引擎层即刻生效，重启持久化同步清除）。确定继续吗？"
          confirmText="全部解禁"
          danger
          onCancel={() => setConfirmClear(false)}
          onConfirm={() => {
            setConfirmClear(false);
            unban(bans ?? []);
          }}
        />
      )}
    </div>
    </div>
  );
}
