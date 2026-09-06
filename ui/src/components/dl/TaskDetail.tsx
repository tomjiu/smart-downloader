"use client";
/// 任务详情抽屉：进度/文件 + 任务级限速 / 顺序下载 / 代理 + 快捷操作。

import { useEffect, useState } from "react";
import type { TaskSnapshot } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { fmtBytes, fmtPct } from "@/lib/daemon/format";
import { StateBadge, TaskProgress } from "./Common";

export default function TaskDetail({
  id,
  onClose,
  onChanged,
}: {
  id: string;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [task, setTask] = useState<TaskSnapshot | null>(null);
  const [down, setDown] = useState("");
  const [up, setUp] = useState("");
  const [proxy, setProxy] = useState("");
  const [seq, setSeq] = useState(false);
  const [conn, setConn] = useState("");
  const [msg, setMsg] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const t = await client.getTask(id);
        if (!alive) return;
        setTask(t);
      } catch {
        /* 抽屉期间任务可能被移除 */
      }
    };
    load();
    const iv = setInterval(load, 1500);
    return () => {
      alive = false;
      clearInterval(iv);
    };
  }, [id]);

  const run = async (fn: () => Promise<unknown>, ok: string) => {
    try {
      await fn();
      setMsg(ok);
      onChanged();
    } catch (e) {
      setMsg(String(e));
    }
  };

  return (
    <>
      <div className="dl-overlay" onClick={onClose} />
      <div className="dl-drawer dl-enter" role="dialog" aria-label="任务详情">
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 14 }}>
          <span className="qoder-icon qoder-icon--file" style={{ color: "var(--accent)", fontSize: 18 }} />
          <b style={{ fontSize: 16 }}>任务详情</b>
          <div style={{ flex: 1 }} />
          <button className="qoder-btn qoder-btn--ghost" onClick={onClose} aria-label="关闭">
            <span className="qoder-icon qoder-icon--close" />
          </button>
        </div>

        {!task ? (
          <div style={{ color: "var(--text-tertiary)" }}>加载中…（任务可能已移除）</div>
        ) : (
          <div style={{ display: "grid", gap: 14 }}>
            <div>
              <div style={{ fontWeight: 700, marginBottom: 6, wordBreak: "break-all" }}>{task.name}</div>
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                <StateBadge state={task.state} />
                {task.engine && <span className="qoder-badge qoder-badge--default">{task.engine}</span>}
                <span className="qoder-badge qoder-badge--default">
                  {fmtPct(task.done, task.total)}
                </span>
              </div>
            </div>

            <TaskProgress task={task} />

            <div className="dl-card" style={{ fontSize: 13 }}>
              <Row k="已传输" v={`${fmtBytes(task.done)} / ${fmtBytes(task.total)}`} />
              <Row k="任务 ID" v={task.task_id} />
              {task.error && <Row k="错误" v={task.error} danger />}
            </div>

            <div className="dl-card">
              <b style={{ display: "block", marginBottom: 10 }}>任务级控制</b>
              <div className="dl-form-grid">
                <label className="dl-field-label">下载限速 (KiB/s)</label>
                <input
                  className="qoder-input"
                  aria-label="任务下载限速"
                  placeholder="0 = 不限"
                  value={down}
                  onChange={(e) => setDown(e.target.value)}
                />
                <label className="dl-field-label">上传限速 (KiB/s)</label>
                <input
                  className="qoder-input"
                  aria-label="任务上传限速"
                  placeholder="0 = 不限"
                  value={up}
                  onChange={(e) => setUp(e.target.value)}
                />
                <div />
                <button
                  className="qoder-btn qoder-btn--primary"
                  onClick={() =>
                    run(
                      () =>
                        client.taskLimit(
                          task.task_id,
                          down ? Number(down) || 0 : undefined,
                          up ? Number(up) || 0 : undefined,
                        ),
                      "限速已应用",
                    )
                  }
                >
                  应用限速
                </button>

                <label className="dl-field-label">顺序下载</label>
                <button
                  className={`qoder-btn qoder-btn--ghost ${seq ? "dl-nav-item--active" : ""}`}
                  onClick={() => {
                    const next = !seq;
                    setSeq(next);
                    run(() => client.taskSequential(task.task_id, next), next ? "已开启顺序下载" : "已关闭");
                  }}
                >
                  <span className={`qoder-icon qoder-icon--${seq ? "check" : "circle-slash"}`} />
                  {seq ? "开启中" : "已关闭"}
                </button>

                {task.engine === "bt" && (
                  <>
                    <label className="dl-field-label">连接数上限</label>
                    <input
                      className="qoder-input"
                      aria-label="任务连接数上限"
                      placeholder={task.max_connections != null ? String(task.max_connections) : "0 = 内核默认"}
                      value={conn}
                      onChange={(e) => setConn(e.target.value.replace(/[^0-9]/g, ""))}
                    />
                    <div />
                    <button
                      className="qoder-btn"
                      onClick={() => {
                        if (!conn) return;
                        run(() => client.taskConnections(task.task_id, Number(conn) || 0), conn === "0" ? "已复位内核默认" : "连接数上限已应用");
                        setConn("");
                      }}
                    >
                      应用连接数
                    </button>
                  </>
                )}

                <label className="dl-field-label">任务代理</label>
                <input
                  className="qoder-input"
                  aria-label="任务代理"
                  placeholder="http(s)/socks4/socks5://host:port，留空清除"
                  value={proxy}
                  onChange={(e) => setProxy(e.target.value)}
                />
                <div />
                <button
                  className="qoder-btn"
                  onClick={() => run(() => client.taskProxy(task.task_id, proxy || null), "代理已更新（新连接生效）")}
                >
                  更新代理
                </button>
              </div>
            </div>

            {task.files && task.files.length > 0 && (
              <div className="dl-card">
                <b style={{ display: "block", marginBottom: 10 }}>
                  子文件 <span className="qoder-badge qoder-badge--default">{task.files.length}</span>
                </b>
                <div style={{ display: "grid", gap: 8 }}>
                  {task.files.slice(0, 30).map((f) => (
                    <div key={f.path}>
                      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, gap: 8 }}>
                        <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                          {f.path}
                        </span>
                        <span style={{ flex: "none", fontVariantNumeric: "tabular-nums", color: "var(--text-tertiary)" }}>
                          {fmtBytes(f.done)} / {fmtBytes(f.size)}
                        </span>
                      </div>
                      <div className="dl-progress dl-progress--thin" style={{ marginTop: 3 }}>
                        <span
                          className="dl-bar dl-bar--run"
                          style={{ width: `${f.size ? (f.done / f.size) * 100 : 0}%` }}
                        />
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {msg && (
              <div className="qoder-alert qoder-alert--info">
                <div className="qoder-alert-content">{msg}</div>
              </div>
            )}
          </div>
        )}
      </div>
    </>
  );
}

function Row({ k, v, danger }: { k: string; v: string; danger?: boolean }) {
  return (
    <div style={{ display: "flex", justifyContent: "space-between", gap: 10, padding: "3px 0" }}>
      <span style={{ color: "var(--text-tertiary)" }}>{k}</span>
      <span style={{ color: danger ? "var(--error)" : undefined, wordBreak: "break-all", textAlign: "right" }}>
        {v}
      </span>
    </div>
  );
}
