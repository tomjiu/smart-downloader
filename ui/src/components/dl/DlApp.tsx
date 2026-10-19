"use client";
/// 应用壳（Issue #115 Phase 1 重写）：工具栏(44px) + 内容(flex:1) + 状态栏(28px)。
/// - 数据轮询：列表 + stats（1s）+ 任务详情快照（1s 并发，表格进度/ETA 数据源）
/// - SSE 事件流：速率 / 完成 / 失败 toast
/// - 视图切换在工具栏分段（无侧栏）
/// 性能修复（审计 P1）：JSON.stringify 全量比较 → 浅层引用比较（长度 + 逐项
/// 引用相等即可判断「新数据」——fetch 每次返回新对象，引用比较足够触发更新，
/// 无需深比较语义；列表项字段级比较留给 React 渲染 diff）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type {
  SchedulerEventEnvelope,
  Stats,
  TaskListItem,
  TaskRate,
  TaskSnapshot,
} from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import TasksTable, { displayName, type TaskAction } from "./TasksTable";
import StatsView from "./StatsView";
import LogsView from "./LogsView";
import RssView from "./RssView";
import SettingsView from "./SettingsView";
import TaskDetail from "./TaskDetail";
import Toolbar, { type TaskFilterKey, type ViewKey } from "./Toolbar";
import StatusBar from "./StatusBar";

type SpeedPoint = { down: number; up: number };

const RESUMABLE = ["Paused", "Queued", "Evaluating", "Failed", "Stopped"];

function matchFilter(t: TaskListItem, f: TaskFilterKey): boolean {
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

export default function DlApp() {
  const [view, setView] = useState<ViewKey>("tasks");
  const [tasks, setTasks] = useState<TaskListItem[]>([]);
  const [snapshots, setSnapshots] = useState<Record<string, TaskSnapshot>>({});
  const [rates, setRates] = useState<Record<string, TaskRate>>({});
  const [stats, setStats] = useState<Stats | null>(null);
  const [events, setEvents] = useState<SchedulerEventEnvelope[]>([]);
  const [connected, setConnected] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  const [toasts, setToasts] = useState<{ id: number; kind: string; text: string }[]>([]);
  const [filter, setFilter] = useState<TaskFilterKey>("all");
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [adding, setAdding] = useState(false);
  const [newUrl, setNewUrl] = useState("");
  const historyRef = useRef<SpeedPoint[]>([]);
  const [historyTick, setHistoryTick] = useState(0);

  const toast = useCallback((t: { kind: string; text: string }) => {
    const id = Date.now() + Math.random();
    setToasts((cur) => [...cur, { id, ...t }]);
    setTimeout(() => setToasts((cur) => cur.filter((x) => x.id !== id)), 4200);
  }, []);

  const refresh = useCallback(async () => {
    // 审计修复（40-e P1-4）：失败保留旧数据——双请求都失败才判断连。
    const [t, s] = await Promise.all([
      client.listTasks().catch(() => null),
      client.stats().catch(() => null),
    ]);
    if (t !== null) setTasks(t);
    if (s) {
      setStats(s);
      const h = historyRef.current;
      const last = h[h.length - 1];
      if (!last || last.down !== s.down_bytes_s || last.up !== s.up_bytes_s) {
        h.push({ down: s.down_bytes_s, up: s.up_bytes_s });
        if (h.length > 180) h.splice(0, h.length - 180);
        setHistoryTick((x) => x + 1);
      }
    }
    setConnected(t !== null || s !== null);
  }, []);

  /// 详情快照轮询：仅对当前列表中的任务并发拉取（表格进度/大小/ETA 数据源）。
  /// 节拍 2s 独立于列表轮询（1s）：任务量大时减半请求压力；tasks 最新引用
  /// 经 ref 传递（避免每秒新数组引用重置 interval 计时）。
  const tasksRef = useRef<TaskListItem[]>([]);
  tasksRef.current = tasks;
  const refreshSnapshots = useCallback(async () => {
    const list = tasksRef.current;
    if (list.length === 0) {
      setSnapshots((cur) => (Object.keys(cur).length === 0 ? cur : {}));
      return;
    }
    const next: Record<string, TaskSnapshot> = {};
    await Promise.all(
      list.map(async (t) => {
        try {
          next[t.task_id] = await client.getTask(t.task_id);
        } catch {
          /* 任务可能已移除；保留缺失（表格显示 —） */
        }
      }),
    );
    if (Object.keys(next).length > 0) setSnapshots(next);
  }, []);

  useEffect(() => {
    refresh();
    const iv = setInterval(refresh, 1000);
    const unsub = client.subscribeEvents(
      (env) => {
        setEvents((cur) => {
          const next = [...cur, env];
          return next.length > 500 ? next.slice(next.length - 500) : next;
        });
        const type = env.event.type;
        if (type === "speed") {
          const tid = String(env.event.task_id ?? "");
          if (tid) {
            setRates((cur) => ({
              ...cur,
              [tid]: {
                down: Number(env.event.down_rate ?? 0),
                up: Number(env.event.up_rate ?? 0),
                ts: Date.now(),
              },
            }));
          }
        }
        if (type === "completed") toast({ kind: "success", text: "任务完成" });
        if (type === "failed") toast({ kind: "error", text: "任务失败" });
        // 审计修复（40-e P2-22）：终态事件清理速率表条目
        if (type === "completed" || type === "failed" || type === "removed") {
          const tid = String(env.event.task_id ?? "");
          if (tid) {
            setRates((cur) => {
              if (!(tid in cur)) return cur;
              const next = { ...cur };
              delete next[tid];
              return next;
            });
          }
        }
      },
      setConnected,
    );
    return () => {
      clearInterval(iv);
      unsub();
    };
  }, [refresh, toast]);

  /// 快照轮询独立节拍（2s）：挂载一次，读 ref 最新列表。
  useEffect(() => {
    refreshSnapshots();
    const iv = setInterval(refreshSnapshots, 2000);
    return () => clearInterval(iv);
  }, [refreshSnapshots]);

  /// 选集随任务列表收缩（已移除任务的选中项清除）。
  useEffect(() => {
    setSelected((cur) => {
      const alive = new Set(tasks.map((t) => t.task_id));
      const next = new Set([...cur].filter((id) => alive.has(id)));
      return next.size === cur.size ? cur : next;
    });
  }, [tasks]);

  const shown = useMemo(
    () =>
      tasks.filter(
        (t) =>
          matchFilter(t, filter) &&
          (!query ||
            displayName(t).toLowerCase().includes(query.toLowerCase()) ||
            t.source.toLowerCase().includes(query.toLowerCase())),
      ),
    [tasks, filter, query],
  );

  const act = useCallback(
    async (fn: () => Promise<unknown>) => {
      setBusy(true);
      try {
        await fn();
        await refresh();
      } catch (e) {
        toast({ kind: "error", text: `操作失败：${String(e)}` });
      } finally {
        setBusy(false);
      }
    },
    [refresh, toast],
  );

  const onAction = useCallback(
    (id: string, a: TaskAction) => {
      if (a === "remove") {
        act(() => client.remove(id));
        return;
      }
      act(() => (a === "resume" ? client.resume(id) : client.pause(id)));
    },
    [act],
  );

  const onBatch = useCallback(
    (op: "pause" | "resume" | "remove") => {
      const ids = [...selected];
      if (ids.length === 0) return;
      act(async () => {
        await client.batchTasks(ids, op);
        setSelected(new Set());
      });
    },
    [act, selected],
  );

  const add = async () => {
    if (busy || !newUrl.trim()) return;
    setBusy(true);
    try {
      await client.addTask({ url: newUrl.trim() });
      setNewUrl("");
      setAdding(false);
      await refresh();
    } catch (e) {
      toast({ kind: "error", text: `添加失败：${String(e)}` });
    } finally {
      setBusy(false);
    }
  };

  const toggleSelect = useCallback((id: string) => {
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);

  const toggleAll = useCallback(
    (on: boolean) => {
      setSelected(on ? new Set(shown.map((t) => t.task_id)) : new Set());
    },
    [shown],
  );

  const onSelectRow = useCallback(
    (id: string) => {
      setSelected((cur) => {
        // 单击行 = 唯一选中（Shift/Ctrl 语义留给 checkbox）
        return cur.size === 1 && cur.has(id) ? new Set() : new Set([id]);
      });
    },
    [],
  );

  return (
    <div className="dl-root">
      <Toolbar
        view={view}
        onView={setView}
        filter={filter}
        onFilter={setFilter}
        query={query}
        onQuery={setQuery}
        stats={stats}
        connected={connected}
        selectedCount={selected.size}
        onBatch={onBatch}
        onNew={() => setAdding((v) => !v)}
        busy={busy}
      />

      <div className="dl-body">
        <main className="dl-main">
          {view === "tasks" && (
            <>
              {adding && (
                <div className="dl-card dl-add-bar">
                  <input
                    className="dl-input dl-add-input"
                    placeholder="http(s):// · ftp(s):// · sftp:// · magnet:?xt=… · .m3u8 · .meta4"
                    aria-label="下载链接"
                    value={newUrl}
                    autoFocus
                    onChange={(e) => setNewUrl(e.target.value)}
                    onKeyDown={(e) => e.key === "Enter" && add()}
                  />
                  <button className="dl-btn dl-btn--primary" disabled={busy} onClick={add}>
                    添加
                  </button>
                  {/* Batch A 槽位：torrent 文件 / metalink / headers / sha256
                      （POST /tasks 全字段）—— 后续批次填 */}
                </div>
              )}
              <TasksTable
                tasks={shown}
                snapshots={snapshots}
                rates={rates}
                selected={selected}
                onToggleSelect={toggleSelect}
                onToggleAll={toggleAll}
                onSelect={onSelectRow}
                onOpen={setDetail}
                onAction={onAction}
                busy={busy}
                query={query}
              />
            </>
          )}
          {view === "stats" && (
            <StatsView stats={stats} history={historyRef.current} tasks={tasks} />
          )}
          {view === "rss" && <RssView onToast={toast} />}
          {view === "logs" && <LogsView events={events} />}
          {view === "settings" && <SettingsView onToast={toast} />}
          {/* historyTick 驱动曲线重绘 */}
          <span hidden>{historyTick}</span>
        </main>
      </div>

      <StatusBar tasks={tasks} stats={stats} selectedCount={selected.size} />

      {detail && (
        <TaskDetail id={detail} onClose={() => setDetail(null)} onChanged={refresh} />
      )}

      <div className="dl-toasts" role="status" aria-live="polite">
        {toasts.map((t) => (
          <div key={t.id} className={`dl-toast dl-toast--${t.kind}`}>
            {t.text}
          </div>
        ))}
      </div>
    </div>
  );
}
