"use client";
/// 应用壳：数据轮询 + SSE 事件 + 视图切换 + 主题装载 + toast。

import { useCallback, useEffect, useRef, useState } from "react";
import type {
  SchedulerEventEnvelope,
  Stats,
  TaskListItem,
  TaskRate,
} from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { fmtSpeed } from "@/lib/daemon/format";
import TasksView from "./TasksView";
import StatsView from "./StatsView";
import LogsView from "./LogsView";
import SettingsView from "./SettingsView";
import TaskDetail from "./TaskDetail";

type SpeedPoint = { down: number; up: number };

const NAV = [
  { key: "tasks", label: "任务", icon: "list-unordered" },
  { key: "stats", label: "统计", icon: "graph" },
  { key: "logs", label: "日志", icon: "output" },
  { key: "settings", label: "设置", icon: "settings-gear" },
] as const;

type ViewKey = (typeof NAV)[number]["key"];

export default function DlApp() {
  const [view, setView] = useState<ViewKey>("tasks");
  const [tasks, setTasks] = useState<TaskListItem[]>([]);
  const [rates, setRates] = useState<Record<string, TaskRate>>({});
  const [stats, setStats] = useState<Stats | null>(null);
  const [events, setEvents] = useState<SchedulerEventEnvelope[]>([]);
  const [connected, setConnected] = useState(false);
  const [detail, setDetail] = useState<string | null>(null);
  const [toasts, setToasts] = useState<{ id: number; kind: string; text: string }[]>([]);
  const historyRef = useRef<SpeedPoint[]>([]);
  const [historyTick, setHistoryTick] = useState(0);

  const toast = useCallback((t: { kind: string; text: string }) => {
    const id = Date.now() + Math.random();
    setToasts((cur) => [...cur, { id, ...t }]);
    setTimeout(() => setToasts((cur) => cur.filter((x) => x.id !== id)), 4200);
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [t, s] = await Promise.all([
        client.listTasks().catch(() => [] as TaskListItem[]),
        client.stats().catch(() => null),
      ]);
      // 内容不变不重渲染（轮询防抖：避免每秒替换子树，也提升自动化稳定性）
      setTasks((cur) => {
        const next = t;
        return JSON.stringify(cur) === JSON.stringify(next) ? cur : next;
      });
      if (s) {
        setStats((cur) => (JSON.stringify(cur) === JSON.stringify(s) ? cur : s));
        const h = historyRef.current;
        const last = h[h.length - 1];
        if (!last || last.down !== s.down_bytes_s || last.up !== s.up_bytes_s) {
          h.push({ down: s.down_bytes_s, up: s.up_bytes_s });
          if (h.length > 180) h.splice(0, h.length - 180);
          setHistoryTick((x) => x + 1);
        }
      }
      setConnected(true);
    } catch {
      setConnected(false);
    }
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
        if (type === "completed") toast({ kind: "success", text: `任务完成（#${env.seq}）` });
        if (type === "failed") toast({ kind: "error", text: `任务失败（#${env.seq}）` });
      },
      setConnected,
    );
    return () => {
      clearInterval(iv);
      unsub();
    };
  }, [refresh, toast]);

  const down = stats?.down_bytes_s ?? 0;
  const up = stats?.up_bytes_s ?? 0;

  return (
    <div className="dl-root">
      <header className="dl-header">
        <div className="dl-logo">
          <span className="qoder-icon qoder-icon--cloud-download" style={{ fontSize: 18 }} />
        </div>
        <div className="dl-title">Smart Downloader</div>
        <div className="dl-spacer" />
        <span className={`qoder-badge ${connected ? "qoder-badge--success" : "qoder-badge--error"}`}>
          {connected ? <span className="dl-live-dot" /> : null}
          {connected ? "已连接" : "未连接"}
        </span>
        <span className="dl-pill dl-pill--down">
          <span className="qoder-icon qoder-icon--arrow-down" />
          {fmtSpeed(down)}
        </span>
        <span className="dl-pill dl-pill--up">
          <span className="qoder-icon qoder-icon--arrow-up" />
          {fmtSpeed(up)}
        </span>
      </header>

      <div className="dl-body">
        <nav className="dl-sidebar" aria-label="主导航">
          {NAV.map((n) => (
            <button
              key={n.key}
              className={`dl-nav-item ${view === n.key ? "dl-nav-item--active" : ""}`}
              onClick={() => setView(n.key)}
            >
              <span className={`qoder-icon qoder-icon--${n.icon}`} />
              <span>{n.label}</span>
            </button>
          ))}
        </nav>

        <main className="dl-main">
          <div key={view} className="dl-enter">
            {view === "tasks" && (
              <TasksView tasks={tasks} rates={rates} onChanged={refresh} onOpen={setDetail} />
            )}
            {view === "stats" && <StatsView stats={stats} history={historyRef.current} tasks={tasks} />}
            {view === "logs" && <LogsView events={events} />}
            {view === "settings" && <SettingsView onToast={toast} />}
          </div>
          {/* historyTick 驱动曲线重绘 */}
          <span hidden>{historyTick}</span>
        </main>
      </div>

      {detail && <TaskDetail id={detail} onClose={() => setDetail(null)} onChanged={refresh} />}

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
