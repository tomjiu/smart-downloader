"use client";
/// 任务视图：过滤 + 任务卡（列表轻量形状 + 事件速率）+ 添加任务。

import { useMemo, useState } from "react";
import type { TaskListItem, TaskRate } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { EmptyState, SpeedPill, StateBadge } from "./Common";

const FILTERS = [
  { key: "all", label: "全部", icon: "list-unordered" },
  { key: "active", label: "进行中", icon: "play" },
  { key: "queued", label: "排队", icon: "kebab-horizontal" },
  { key: "paused", label: "已暂停", icon: "pause" },
  { key: "done", label: "已完成", icon: "pass" },
  { key: "failed", label: "失败", icon: "error" },
] as const;

function matchFilter(t: TaskListItem, f: string): boolean {
  switch (f) {
    case "active":
      return ["Downloading", "Seeding", "Transferring", "FallbackProvider"].includes(t.state);
    case "queued":
      return ["Queued", "Evaluating"].includes(t.state);
    case "paused":
      return t.state === "Paused";
    case "done":
      return ["Completed", "Stopped", "Seeding"].includes(t.state);
    case "failed":
      return t.state === "Failed";
    default:
      return true;
  }
}

export default function TasksView({
  tasks,
  rates,
  onChanged,
  onOpen,
}: {
  tasks: TaskListItem[];
  rates: Record<string, TaskRate>;
  onChanged: () => void;
  onOpen: (id: string) => void;
}) {
  const [filter, setFilter] = useState<string>("all");
  const [q, setQ] = useState("");
  const [adding, setAdding] = useState(false);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const shown = useMemo(
    () =>
      tasks.filter(
        (t) => matchFilter(t, filter) && (!q || t.name.toLowerCase().includes(q.toLowerCase())),
      ),
    [tasks, filter, q],
  );

  const act = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await fn();
      onChanged();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const add = async () => {
    if (!url.trim()) return;
    setBusy(true);
    setErr(null);
    try {
      await client.addTask({ url: url.trim() });
      setUrl("");
      setAdding(false);
      onChanged();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="dl-enter">
      <div style={{ display: "flex", gap: 10, flexWrap: "wrap", alignItems: "center", marginBottom: 14 }}>
        <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
          {FILTERS.map((f) => (
            <button
              key={f.key}
              className={`qoder-btn qoder-btn--ghost ${filter === f.key ? "dl-nav-item--active" : ""}`}
              onClick={() => setFilter(f.key)}
              style={{ borderRadius: 99, padding: "5px 12px" }}
            >
              <span className={`qoder-icon qoder-icon--${f.icon}`} /> {f.label}
            </button>
          ))}
        </div>
        <div style={{ flex: 1, minWidth: 120 }} />
        <input
          className="qoder-input"
          placeholder="搜索任务…"
          aria-label="搜索任务"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          style={{ maxWidth: 200 }}
        />
        <button className="qoder-btn qoder-btn--primary" onClick={() => setAdding((v) => !v)}>
          <span className="qoder-icon qoder-icon--plus" /> 新建任务
        </button>
      </div>

      {adding && (
        <div className="dl-card dl-enter" style={{ marginBottom: 14 }}>
          <div style={{ display: "flex", gap: 10, flexWrap: "wrap" }}>
            <input
              className="qoder-input"
              style={{ flex: 1, minWidth: 240 }}
              placeholder="http(s):// · ftp(s):// · sftp:// · magnet:?xt=… · .m3u8 · .meta4"
              aria-label="下载链接"
              value={url}
              autoFocus
              onChange={(e) => setUrl(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && add()}
            />
            <button className="qoder-btn qoder-btn--primary" disabled={busy} onClick={add}>
              添加
            </button>
          </div>
          {err && <div style={{ color: "var(--error)", marginTop: 8, fontSize: 12 }}>{err}</div>}
        </div>
      )}

      {shown.length === 0 ? (
        <EmptyState icon="cloud-download" title="暂无任务" hint="点击「新建任务」添加下载链接" />
      ) : (
        <div className="dl-grid dl-grid--tasks">
          {shown.map((t, i) => {
            const rate = rates[t.task_id];
            const live = ["Downloading", "Transferring"].includes(t.state);
            return (
              <div
                key={t.task_id}
                className="dl-card dl-card--hover dl-enter"
                style={{ "--i": i } as React.CSSProperties}
                onClick={() => onOpen(t.task_id)}
                role="button"
                tabIndex={0}
              >
                <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
                  <StateBadge state={t.state} />
                  <div style={{ fontWeight: 600, flex: 1, minWidth: 160, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {t.name}
                  </div>
                  <span className="qoder-badge qoder-badge--default">{t.engine ?? "—"}</span>
                </div>
                {rate && rate.down > 0 && (
                  <div style={{ marginTop: 8, display: "flex", gap: 8 }}>
                    <SpeedPill dir="down" value={rate.down} live={live} />
                    {rate.up > 0 && <SpeedPill dir="up" value={rate.up} />}
                  </div>
                )}
                <div style={{ display: "flex", gap: 8, marginTop: 8 }}>
                  <div style={{ flex: 1 }}>
                    {["Paused", "Queued", "Evaluating", "Failed", "Stopped"].includes(t.state) ? (
                      <button
                        className="qoder-btn qoder-btn--ghost"
                        disabled={busy}
                        onClick={(e) => { e.stopPropagation(); act(() => client.resume(t.task_id)); }}
                      >
                        <span className="qoder-icon qoder-icon--play" /> 恢复
                      </button>
                    ) : (
                      <button
                        className="qoder-btn qoder-btn--ghost"
                        disabled={busy}
                        onClick={(e) => { e.stopPropagation(); act(() => client.pause(t.task_id)); }}
                      >
                        <span className="qoder-icon qoder-icon--pause" /> 暂停
                      </button>
                    )}
                  </div>
                  <button
                    className="qoder-btn qoder-btn--ghost"
                    disabled={busy}
                    onClick={(e) => { e.stopPropagation(); act(() => client.remove(t.task_id)); }}
                  >
                    <span className="qoder-icon qoder-icon--trash" />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
