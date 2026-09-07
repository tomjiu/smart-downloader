"use client";
/// RSS 订阅视图（batch5，qbit RSS Downloader 对标）：订阅管理 + 规则
/// 管理（含 use_regex / episode_filter / 每 feed 间隔）+ 条目命中情况。

import { useCallback, useEffect, useState } from "react";
import type { RssFeed, RssItem, RssRule } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";

function Section({
  title,
  count,
  children,
}: {
  title: string;
  count?: number;
  children: React.ReactNode;
}) {
  return (
    <section className="dl-card">
      <h3 style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 14 }}>
        {title}
        {count != null && <span className="qoder-badge qoder-badge--default">{count}</span>}
      </h3>
      {children}
    </section>
  );
}

export default function RssView({ onToast }: { onToast: (t: { kind: string; text: string }) => void }) {
  const [feeds, setFeeds] = useState<RssFeed[]>([]);
  const [rules, setRules] = useState<RssRule[]>([]);
  const [items, setItems] = useState<RssItem[]>([]);
  const [feedUrl, setFeedUrl] = useState("");
  const [feedFilter, setFeedFilter] = useState<number | null>(null);
  // 规则表单
  const [rName, setRName] = useState("");
  const [rMust, setRMust] = useState("");
  const [rMustNot, setRMustNot] = useState("");
  const [rFeed, setRFeed] = useState<string>("");
  const [rDest, setRDest] = useState("");
  const [rRegex, setRRegex] = useState(false);
  const [rEpisode, setREpisode] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const [f, r, i] = await Promise.all([
        client.rssFeeds(),
        client.rssRules(),
        client.rssItems(feedFilter ?? undefined).catch(() => ({ items: [] as RssItem[] })),
      ]);
      setFeeds(f.feeds ?? []);
      setRules(r.rules ?? []);
      setItems(i.items ?? []);
    } catch (e) {
      onToast({ kind: "error", text: `RSS 加载失败：${e}` });
    }
  }, [feedFilter, onToast]);

  useEffect(() => {
    load();
    const iv = setInterval(load, 5000);
    return () => clearInterval(iv);
  }, [load]);

  const run = async (fn: () => Promise<unknown>, ok: string) => {
    setBusy(true);
    try {
      await fn();
      onToast({ kind: "success", text: ok });
      await load();
    } catch (e) {
      onToast({ kind: "error", text: String(e) });
    } finally {
      setBusy(false);
    }
  };

  const addRule = () => {
    const must = rMust.split("\n").map((x) => x.trim()).filter(Boolean);
    const mustNot = rMustNot.split("\n").map((x) => x.trim()).filter(Boolean);
    if (!rName.trim() || (must.length === 0 && mustNot.length === 0)) {
      onToast({ kind: "error", text: "规则名与至少一个关键词必填" });
      return;
    }
    run(
      () =>
        client.rssAddRule({
          name: rName.trim(),
          enabled: true,
          must_contain: must,
          must_not_contain: mustNot,
          feed_id: rFeed ? Number(rFeed) : null,
          tags: [],
          dest: rDest.trim() || null,
          use_regex: rRegex,
          episode_filter: rEpisode.trim() || null,
        }),
      "规则已创建",
    ).then(() => {
      setRName("");
      setRMust("");
      setRMustNot("");
      setRFeed("");
      setRDest("");
      setRRegex(false);
      setREpisode("");
    });
  };

  return (
    <div className="dl-enter" style={{ display: "grid", gap: 16, maxWidth: 860 }}>
      {/* —— 订阅源 —— */}
      <Section title="订阅源" count={feeds.length}>
        <div style={{ display: "flex", gap: 8, marginBottom: 12 }}>
          <input
            className="qoder-input"
            style={{ flex: 1 }}
            placeholder="RSS/Atom 订阅 URL（https://…）"
            value={feedUrl}
            onChange={(e) => setFeedUrl(e.target.value)}
          />
          <button
            className="qoder-btn qoder-btn--primary"
            disabled={busy || !feedUrl.trim()}
            onClick={() =>
              run(() => client.rssAddFeed(feedUrl.trim()), "订阅已添加").then(() => setFeedUrl(""))
            }
          >
            添加订阅
          </button>
          <button
            className="qoder-btn qoder-btn--ghost"
            disabled={busy}
            onClick={() => run(() => client.rssRefresh(), "已刷新全部订阅（规则自动建任务）")}
          >
            <span className="qoder-icon qoder-icon--sync" /> 全部刷新
          </button>
        </div>
        {feeds.length === 0 ? (
          <div style={{ color: "var(--text-tertiary)", fontSize: 12 }}>暂无订阅；daemon 需启用 [rss] auto_refresh 才会自动刷新</div>
        ) : (
          feeds.map((f) => (
            <div
              key={f.id}
              style={{ display: "flex", alignItems: "center", gap: 10, padding: "6px 0", borderBottom: "1px solid var(--border)" }}
            >
              <b style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", maxWidth: 320 }}>{f.title}</b>
              <span className="qoder-badge qoder-badge--default">{f.item_count} 条</span>
              <span className="qoder-badge qoder-badge--warning">{f.pending_count} 待匹配</span>
              {f.interval_override_secs ? (
                <span className="qoder-badge qoder-badge--default">间隔 {f.interval_override_secs}s</span>
              ) : null}
              <div style={{ flex: 1 }} />
              <button
                className="qoder-btn qoder-btn--ghost"
                aria-label={`删除订阅 ${f.title}`}
                onClick={() => run(() => client.rssRemoveFeed(f.id), "订阅已删除")}
              >
                <span className="qoder-icon qoder-icon--close" />
              </button>
            </div>
          ))
        )}
      </Section>

      {/* —— 自动下载规则 —— */}
      <Section title="自动下载规则" count={rules.length}>
        <div className="dl-form-grid">
          <label className="dl-field-label">规则名 *</label>
          <input className="qoder-input" value={rName} onChange={(e) => setRName(e.target.value)} />
          <label className="dl-field-label">必须包含（每行一条{rRegex ? "，正则" : ""}）*</label>
          <textarea className="qoder-input" rows={2} value={rMust} onChange={(e) => setRMust(e.target.value)} />
          <label className="dl-field-label">不得包含（每行一条）</label>
          <textarea className="qoder-input" rows={2} value={rMustNot} onChange={(e) => setRMustNot(e.target.value)} />
          <label className="dl-field-label">限定订阅</label>
          <select className="qoder-input" value={rFeed} onChange={(e) => setRFeed(e.target.value)}>
            <option value="">全部订阅</option>
            {feeds.map((f) => (
              <option key={f.id} value={f.id}>
                {f.title}
              </option>
            ))}
          </select>
          <label className="dl-field-label">集数过滤</label>
          <input
            className="qoder-input"
            placeholder="如 1x02;1x04-1x06 或 S02E01（留空不过滤）"
            value={rEpisode}
            onChange={(e) => setREpisode(e.target.value)}
          />
          <label className="dl-field-label">落盘目录</label>
          <input className="qoder-input" placeholder="留空 = 默认目录" value={rDest} onChange={(e) => setRDest(e.target.value)} />
          <label className="dl-field-label">关键词按正则解释</label>
          <button
            type="button"
            className={`qoder-btn qoder-btn--ghost ${rRegex ? "dl-nav-item--active" : ""}`}
            style={{ borderRadius: 99, padding: "4px 14px", minWidth: 92 }}
            onClick={() => setRRegex(!rRegex)}
            aria-pressed={rRegex}
            aria-label="使用正则表达式"
          >
            <span className={`qoder-icon qoder-icon--${rRegex ? "check" : "circle-slash"}`} />
            {rRegex ? "正则" : "子串"}
          </button>
        </div>
        <div style={{ display: "flex", justifyContent: "flex-end", marginTop: 12 }}>
          <button className="qoder-btn qoder-btn--primary" disabled={busy} onClick={addRule}>
            创建规则
          </button>
        </div>
        {rules.length > 0 && (
          <div style={{ marginTop: 14 }}>
            {rules.map((r) => (
              <div
                key={r.id}
                style={{ display: "flex", alignItems: "center", gap: 8, padding: "6px 0", borderBottom: "1px solid var(--border)", flexWrap: "wrap" }}
              >
                <b>{r.name}</b>
                {r.use_regex && <span className="qoder-badge qoder-badge--default">正则</span>}
                {r.episode_filter && <span className="qoder-badge qoder-badge--default">集数 {r.episode_filter}</span>}
                <span style={{ fontSize: 12, color: "var(--text-tertiary)", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {(r.must_contain.length ? `含 [${r.must_contain.join(", ")}]` : "") +
                    (r.must_not_contain.length ? ` 斥 [${r.must_not_contain.join(", ")}]` : "")}
                </span>
                <div style={{ flex: 1 }} />
                <button
                  className="qoder-btn qoder-btn--ghost"
                  aria-label={`删除规则 ${r.name}`}
                  onClick={() => run(() => client.rssRemoveRule(r.id), "规则已删除")}
                >
                  <span className="qoder-icon qoder-icon--close" />
                </button>
              </div>
            ))}
          </div>
        )}
      </Section>

      {/* —— 条目 —— */}
      <Section title="条目" count={items.length}>
        <div style={{ display: "flex", gap: 8, marginBottom: 10 }}>
          <select
            className="qoder-input"
            style={{ maxWidth: 260 }}
            value={feedFilter ?? ""}
            onChange={(e) => setFeedFilter(e.target.value ? Number(e.target.value) : null)}
            aria-label="按订阅过滤条目"
          >
            <option value="">全部订阅的条目</option>
            {feeds.map((f) => (
              <option key={f.id} value={f.id}>
                {f.title}
              </option>
            ))}
          </select>
        </div>
        {items.length === 0 ? (
          <div style={{ color: "var(--text-tertiary)", fontSize: 12 }}>暂无条目</div>
        ) : (
          items.slice(0, 50).map((i) => (
            <div
              key={i.guid}
              style={{ display: "flex", alignItems: "center", gap: 10, padding: "5px 0", borderBottom: "1px solid var(--border)" }}
            >
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 13 }}>{i.title}</span>
              <div style={{ flex: 1 }} />
              {i.task_id ? (
                <span className="qoder-badge qoder-badge--success">已建任务 {i.task_id}</span>
              ) : (
                <span className="qoder-badge qoder-badge--default">未匹配</span>
              )}
            </div>
          ))
        )}
      </Section>
    </div>
  );
}
