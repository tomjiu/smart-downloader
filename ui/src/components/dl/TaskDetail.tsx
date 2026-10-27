"use client";
/// 任务详情抽屉：进度/文件 + 任务级限速 / 顺序下载 / 代理 + 快捷操作。
/// Issue #115 Phase 1：旧库 class 清零；name 可空兜底（P0-2）。
/// 交互打磨批：
/// - Esc 关闭（document 级监听，仅抽屉挂载期间存在）
/// - 打开时初始聚焦「关闭」按钮（键盘用户不被丢在页尾）
/// - 操作反馈分色：成功 info / 失败 err（旧实现同色误导）
/// - 限速输入 placeholder 回显当前值（snapshot.limits；旧实现看不出已设限速）
/// - 子文件 >30 截断时明示总数（不再静默截断）
/// 第二梯队（#143 种源管理 / 队列优先级 / 文件优先级 / 重命名 / 标签）：
/// - Esc 分层：焦点在输入件内 → 第一次 Esc 仅失焦（GitHub 惯例——多行
///   textarea 里直接关抽屉会误伤输入）；失焦后再 Esc 才关抽屉
/// - 重命名：行内编辑（Enter 保存 / Esc 取消 / 空值与未改名禁用提交）
/// - 标签：逗号或空白分隔 → 数组（后端 E18 归一化）；清空 = null 语义
/// - 队列优先级：置顶/上移/下移/置底（PriorityReq action 语义），
///   操作后回显 queue_priority 数值
/// - 子文件优先级选择器（BT）：0 不下载 / 1 低 / 4 正常 / 7 高，
///   初值来自快照 file_priorities（下标与 files 对齐）
/// - BT 任务追加 TaskBtPanel（tracker/peer/webseed/超级做种/宣告/recheck/
///   magnet/导出）

import { useEffect, useRef, useState } from "react";
import type { TaskLogs, TaskSnapshot } from "@/lib/daemon/types";
import { client } from "@/lib/daemon/store";
import { fmtBytes, fmtPct, fmtUnixShort } from "@/lib/daemon/format";
import { StateBadge, TaskProgress } from "./Common";
import { Icon } from "./Icon";
import { displayName } from "./TasksTable";
import TaskBtPanel from "./TaskBtPanel";

const FILES_SHOWN = 30;

/// libtorrent 子文件优先级档位（http.rs FilePriorityReq 注释口径）。
const FILE_PRIORITIES = [
  { value: 0, label: "不下载" },
  { value: 1, label: "低" },
  { value: 4, label: "正常" },
  { value: 7, label: "高" },
] as const;

/// 标签输入 → 数组（逗号/空白分隔，trim + 去重去空）。
function parseTags(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of text.split(/[,，\s]+/)) {
    const t = raw.trim();
    if (!t || seen.has(t)) continue;
    seen.add(t);
    out.push(t);
  }
  return out;
}

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
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  /// 重命名（行内编辑态）。
  const [editingName, setEditingName] = useState(false);
  const [nameDraft, setNameDraft] = useState("");
  const nameInputRef = useRef<HTMLInputElement>(null);

  /// 标签编辑。
  const [tagsDraft, setTagsDraft] = useState("");
  const [editingTags, setEditingTags] = useState(false);
  const tagsInputRef = useRef<HTMLInputElement>(null);

  /// 队列优先级（操作后回显；快照无该字段——set-only 契约）。
  const [queuePrio, setQueuePrio] = useState<number | null>(null);
  /// 操作日志（体检 #182 P2：/tasks/:id/logs 端点接入——原仅全局事件流，
  /// 任务自身审计轨迹无 UI 入口）。与快照同拍轮询（低频变化，成本低）。
  const [logs, setLogs] = useState<TaskLogs | null>(null);
  const [prioBusy, setPrioBusy] = useState(false);

  /// 文件优先级：本地乐观值（下标 → 档位；快照轮询 1.5s 会再对齐）。
  const [filePrioOverride, setFilePrioOverride] = useState<Record<number, number>>({});
  const [filePrioBusy, setFilePrioBusy] = useState<number | null>(null);

  useEffect(() => {
    let alive = true;
    const load = async () => {
      try {
        const t = await client.getTask(id);
        if (!alive) return;
        setTask(t);
        // 审查修复（P2）：顺序下载开关回读快照字段——原实现恒初始化 false，
        // 已开启的任务显示"已关闭"且首点误发 true。
        setSeq(Boolean(t.sequential));
        // 体检 #182 C-1：队列优先级从快照回读（原仅 set 响应回显——抽屉
        // 重开/页面刷新后丢显）。快照 0 = 默认（序列化省略 → undefined）。
        setQueuePrio(t.queue_priority ?? 0);
        // 重置乐观覆盖（快照是权威值；任务切换时也须清）。
        setFilePrioOverride({});
        // 操作日志（体检 #182 P2）：与快照同拍轮询（低频变化，成本低）。
        const lg = await client.getTaskLogs(id).catch(() => null);
        if (alive && lg) setLogs(lg);
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

  /// Esc 关闭 + 初始聚焦（键盘可达性）。
  /// Esc 分层（第二梯队）：焦点在输入件 → 第一次失焦，再 Esc 才关。
  useEffect(() => {
    closeRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const t = e.target as HTMLElement | null;
      if (
        t instanceof HTMLElement &&
        (t.tagName === "TEXTAREA" ||
          t.tagName === "INPUT" ||
          t.tagName === "SELECT" ||
          t.isContentEditable)
      ) {
        // 不 stopPropagation——让输入件自身的 React onKeyDown（如重命名
        // 取消编辑）继续收到该事件；此处只消费「关抽屉」语义。
        t.blur();
        return;
      }
      e.stopPropagation();
      onClose();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const run = async (fn: () => Promise<unknown>, ok: string) => {
    try {
      await fn();
      setMsg({ ok: true, text: ok });
      onChanged();
    } catch (e) {
      setMsg({ ok: false, text: String(e) });
    }
  };

  /// 队列优先级动作（top/up/down/bottom）。busy 独立禁用四钮。
  const movePrio = (action: "top" | "up" | "down" | "bottom") => {
    setPrioBusy(true);
    client
      .taskPriority(id, { action })
      .then(({ queue_priority }) => {
        setQueuePrio(queue_priority);
        setMsg({ ok: true, text: `队列优先级已调整（当前 ${queue_priority}）` });
        onChanged();
      })
      .catch((e) => setMsg({ ok: false, text: String(e) }))
      .finally(() => setPrioBusy(false));
  };

  /// 保存重命名（空值/未修改禁用；失败回显错误）。
  const saveName = () => {
    if (!task) return;
    const next = nameDraft.trim();
    if (!next || next === (task.name ?? displayName(task))) {
      setEditingName(false);
      return;
    }
    client
      .taskRename(id, next)
      .then((snap) => {
        setTask(snap);
        setEditingName(false);
        setMsg({ ok: true, text: "任务名已更新" });
        onChanged();
      })
      .catch((e) => setMsg({ ok: false, text: `重命名失败：${String(e)}` }));
  };

  /// 保存标签（空 = 清空 null 语义；后端归一化后回显）。
  const saveTags = () => {
    const tags = parseTags(tagsDraft);
    client
      .taskTags(id, tags.length > 0 ? tags : null)
      .then(({ tags: normalized }) => {
        setTask((cur) => (cur ? { ...cur, tags: normalized } : cur));
        setEditingTags(false);
        setMsg({ ok: true, text: normalized.length > 0 ? "标签已保存" : "标签已清空" });
        onChanged();
      })
      .catch((e) => setMsg({ ok: false, text: `保存标签失败：${String(e)}` }));
  };

  /// 子文件优先级变更：乐观更新 + 单文件级 busy。
  const setFilePrio = (index: number, priority: number) => {
    if (!task) return;
    setFilePrioBusy(index);
    setFilePrioOverride((cur) => ({ ...cur, [index]: priority }));
    client
      .taskFilePriority(id, [{ index, priority }])
      .then(({ priorities }) => {
        // 响应 = 全量优先级表（与 files 下标对齐）；null 项保留乐观值。
        setFilePrioOverride((cur) => {
          const next = { ...cur };
          priorities.forEach((p, i) => {
            if (p != null) next[i] = p;
          });
          return next;
        });
        onChanged();
      })
      .catch((e) => {
        setMsg({ ok: false, text: `优先级设置失败：${String(e)}` });
        // 失败回滚乐观值（下一轮快照轮询会覆盖，显式清理更即时）。
        setFilePrioOverride((cur) => {
          const next = { ...cur };
          delete next[index];
          return next;
        });
      })
      .finally(() => setFilePrioBusy(null));
  };

  return (
    <>
      <div className="dl-overlay" onClick={onClose} />
      <div className="dl-drawer" role="dialog" aria-modal="true" aria-label="任务详情">
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 14 }}>
          <Icon name="file" size={16} className="dl-drawer-ico" />
          <b style={{ fontSize: 15 }}>任务详情</b>
          <div style={{ flex: 1 }} />
          <button ref={closeRef} className="dl-btn dl-btn--ghost" onClick={onClose} aria-label="关闭（Esc）" title="关闭（Esc）">
            <Icon name="close" size={13} />
          </button>
        </div>

        {!task ? (
          <div style={{ color: "var(--fg-3)" }}>加载中…（任务可能已移除）</div>
        ) : (
          <div style={{ display: "grid", gap: 14 }}>
            <div>
              {/* 重命名（第二梯队）：标题行内编辑——铅笔进入，Enter 保存，Esc 取消 */}
              {editingName ? (
                <div style={{ display: "flex", gap: 6, marginBottom: 6 }}>
                  <input
                    ref={nameInputRef}
                    className="dl-input"
                    aria-label="任务名"
                    data-testid="rename-input"
                    value={nameDraft}
                    autoFocus
                    onChange={(e) => setNameDraft(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") {
                        e.preventDefault();
                        saveName();
                      }
                      if (e.key === "Escape") {
                        e.stopPropagation();
                        setEditingName(false);
                      }
                    }}
                  />
                  <button
                    type="button"
                    className="dl-btn dl-btn--primary"
                    aria-label="保存任务名"
                    data-testid="rename-save"
                    disabled={!nameDraft.trim() || nameDraft.trim() === (task.name ?? displayName(task))}
                    onClick={saveName}
                  >
                    <Icon name="check" size={13} />
                  </button>
                </div>
              ) : (
                <div
                  style={{ fontWeight: 600, marginBottom: 6, wordBreak: "break-all", display: "flex", alignItems: "center", gap: 6 }}
                >
                  {/* P0-2 修复：name 可空兜底（绝不渲染 null） */}
                  <span style={{ minWidth: 0, wordBreak: "break-all" }}>{displayName(task)}</span>
                  <button
                    type="button"
                    className="dl-btn dl-btn--ghost dl-act-btn"
                    aria-label="重命名"
                    title="重命名"
                    data-testid="rename-start"
                    onClick={() => {
                      setNameDraft(task.name ?? displayName(task));
                      setEditingName(true);
                      requestAnimationFrame(() => nameInputRef.current?.focus());
                    }}
                  >
                    <Icon name="pencil" size={12} />
                  </button>
                </div>
              )}
              <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
                <StateBadge state={task.state} />
                {task.engine && <span className="dl-badge">{task.engine}</span>}
                <span className="dl-badge">{fmtPct(task.done, task.total)}</span>
                {/* 标签（E18 透出）：展示 + 行内编辑 */}
                {(task.tags ?? []).map((t) => (
                  <span key={t} className="dl-badge dl-badge--info">
                    <Icon name="tag" size={10} /> {t}
                  </span>
                ))}
                {editingTags ? (
                  <span style={{ display: "inline-flex", gap: 4, alignItems: "center" }}>
                    <input
                      ref={tagsInputRef}
                      className="dl-input"
                      style={{ minHeight: 24, width: 200 }}
                      aria-label="标签（逗号分隔）"
                      data-testid="tags-input"
                      placeholder="逗号或空格分隔"
                      value={tagsDraft}
                      autoFocus
                      onChange={(e) => setTagsDraft(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          saveTags();
                        }
                        if (e.key === "Escape") {
                          e.stopPropagation();
                          setEditingTags(false);
                        }
                      }}
                    />
                    <button
                      type="button"
                      className="dl-btn dl-btn--primary"
                      aria-label="保存标签"
                      data-testid="tags-save"
                      onClick={saveTags}
                    >
                      <Icon name="check" size={12} />
                    </button>
                  </span>
                ) : (
                  <button
                    type="button"
                    className="dl-btn dl-btn--ghost"
                    style={{ minHeight: 22, padding: "1px 8px" }}
                    aria-label={(task.tags ?? []).length > 0 ? "编辑标签" : "添加标签"}
                    title={(task.tags ?? []).length > 0 ? "编辑标签" : "添加标签"}
                    data-testid="tags-edit"
                    onClick={() => {
                      setTagsDraft((task.tags ?? []).join(", "));
                      setEditingTags(true);
                      requestAnimationFrame(() => tagsInputRef.current?.focus());
                    }}
                  >
                    <Icon name="tag" size={11} /> {(task.tags ?? []).length > 0 ? "编辑标签" : "标签"}
                  </button>
                )}
              </div>
            </div>

            <TaskProgress task={task} />

            <div className="dl-card" style={{ fontSize: 13 }}>
              <Row k="已传输" v={`${fmtBytes(task.done)} / ${fmtBytes(task.total)}`} />
              <Row k="任务 ID" v={task.task_id} />
              {/* A3（契约审计 2026-10-21）：E33 做种统计透出——后端 TaskSnapshot
                  的 total_downloaded/total_uploaded/share_ratio 仅 BT 任务携带
                  （条件序列化），此前未建模 → 做种任务看不到分享率与累计流量。 */}
              {(task.total_downloaded != null || task.total_uploaded != null) && (
                <>
                  <Row k="累计下载" v={fmtBytes(task.total_downloaded ?? 0)} />
                  <Row k="累计上传" v={fmtBytes(task.total_uploaded ?? 0)} />
                  {task.share_ratio != null && (
                    <Row k="分享率" v={task.share_ratio.toFixed(2)} />
                  )}
                </>
              )}
              {/* A3：E30 重试预算透出——失败任务详情可见重试进度与下次到期。 */}
              {task.max_retries != null && task.max_retries > 0 && (
                <Row
                  k="自动重试"
                  v={
                    task.next_retry_at_unix
                      ? `${task.retries ?? 0}/${task.max_retries}（下次 ${fmtUnixShort(Number(task.next_retry_at_unix))}）`
                      : `${task.retries ?? 0}/${task.max_retries}`
                  }
                />
              )}
              {queuePrio != null && <Row k="队列优先级" v={String(queuePrio)} />}
              {task.error && <Row k="错误" v={task.error} danger />}
            </div>

            {/* 队列优先级（第二梯队；全引擎可用——排队调度语义与引擎无关） */}
            <div className="dl-card">
              <b style={{ display: "block", marginBottom: 10 }}>队列优先级</b>
              <div className="dl-prio-group" role="group" aria-label="队列优先级调整">
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost"
                  disabled={prioBusy}
                  title="移到队列最前（下一个开始）"
                  data-testid="prio-top"
                  onClick={() => movePrio("top")}
                >
                  <Icon name="chevrons-top" size={13} /> 置顶
                </button>
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost"
                  disabled={prioBusy}
                  title="上移一位"
                  data-testid="prio-up"
                  onClick={() => movePrio("up")}
                >
                  <Icon name="chevron-up" size={13} /> 上移
                </button>
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost"
                  disabled={prioBusy}
                  title="下移一位"
                  data-testid="prio-down"
                  onClick={() => movePrio("down")}
                >
                  <Icon name="chevron-down" size={13} /> 下移
                </button>
                <button
                  type="button"
                  className="dl-btn dl-btn--ghost"
                  disabled={prioBusy}
                  title="移到队列最后"
                  data-testid="prio-bottom"
                  onClick={() => movePrio("bottom")}
                >
                  <Icon name="chevrons-bottom" size={13} /> 置底
                </button>
              </div>
              <div className="dl-field-help" style={{ marginTop: 8 }}>
                排队任务的执行顺序；进行中任务不受影响。
                {queuePrio != null ? ` 当前值 ${queuePrio}（数值大者优先）。` : ""}
              </div>
            </div>

            <div className="dl-card">
              <b style={{ display: "block", marginBottom: 10 }}>任务级控制</b>
              <div className="dl-form-grid">
                <label className="dl-field-label">下载限速 (KiB/s)</label>
                <input
                  className="dl-input"
                  aria-label="任务下载限速"
                  data-testid="limit-down"
                  placeholder={task.limits?.down_kb_s ? `当前 ${task.limits.down_kb_s}` : "0 = 不限"}
                  value={down}
                  onChange={(e) => setDown(e.target.value)}
                />
                <label className="dl-field-label">上传限速 (KiB/s)</label>
                <input
                  className="dl-input"
                  aria-label="任务上传限速"
                  placeholder={task.limits?.up_kb_s ? `当前 ${task.limits.up_kb_s}` : "0 = 不限"}
                  value={up}
                  onChange={(e) => setUp(e.target.value)}
                />
                <div />
                <button
                  className="dl-btn dl-btn--primary"
                  data-testid="apply-limit"
                  onClick={() =>
                    run(
                      () => {
                        // 审查修复（P2）：非数字/负数输入不再静默变 0（=不限速），
                        // 非法输入报错不发请求。
                        const parse = (v: string) => {
                          if (!v.trim()) return undefined;
                          const n = Number(v);
                          if (!Number.isFinite(n) || n < 0) {
                            throw new Error("限速必须为非负数字");
                          }
                          return Math.floor(n);
                        };
                        const d = parse(down);
                        const u = parse(up);
                        if (d === undefined && u === undefined) {
                          throw new Error("请至少填写一个方向的限速值");
                        }
                        return client.taskLimit(task.task_id, d, u);
                      },
                      "限速已应用",
                    )
                  }
                >
                  应用限速
                </button>

                <label className="dl-field-label">顺序下载</label>
                <button
                  className={`dl-btn dl-btn--ghost ${seq ? "dl-btn--primary" : ""}`}
                  aria-pressed={seq}
                  onClick={() => {
                    const next = !seq;
                    setSeq(next);
                    run(() => client.taskSequential(task.task_id, next), next ? "已开启顺序下载" : "已关闭");
                  }}
                >
                  <Icon name={seq ? "check" : "close"} size={13} />
                  {seq ? "开启中" : "已关闭"}
                </button>

                {task.engine === "bt" && (
                  <>
                    <label className="dl-field-label">首尾块优先（边下边播）</label>
                    <div style={{ display: "flex", gap: 6, flexWrap: "wrap" }}>
                      <button
                        className="dl-btn dl-btn--ghost"
                        aria-label="开启首尾块优先"
                        onClick={() => run(() => client.taskPieceFirstLast(task.task_id, 7), "已提升首尾块优先级")}
                      >
                        <Icon name="check" size={13} /> 开启
                      </button>
                      <button
                        className="dl-btn dl-btn--ghost"
                        aria-label="恢复默认块优先级"
                        onClick={() => run(() => client.taskPieceFirstLast(task.task_id, 0), "已恢复默认块优先级")}
                      >
                        恢复
                      </button>
                    </div>
                    <label className="dl-field-label">连接数上限</label>
                    <input
                      className="dl-input"
                      aria-label="任务连接数上限"
                      placeholder={task.max_connections != null ? `当前 ${task.max_connections}` : "0 = 内核默认"}
                      value={conn}
                      onChange={(e) => setConn(e.target.value.replace(/[^0-9]/g, ""))}
                    />
                    <div />
                    <button
                      className="dl-btn"
                      disabled={!conn}
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
                  className="dl-input"
                  aria-label="任务代理"
                  placeholder="http(s)/socks4/socks5://host:port，留空清除"
                  value={proxy}
                  onChange={(e) => setProxy(e.target.value)}
                />
                <div />
                <button
                  className="dl-btn"
                  onClick={() => run(() => client.taskProxy(task.task_id, proxy || null), "代理已更新（新连接生效）")}
                >
                  更新代理
                </button>
              </div>
            </div>

            {/* BT 种源管理（第二梯队核心）——仅 BT 引擎渲染 */}
            {task.engine === "bt" && <TaskBtPanel taskId={task.task_id} onChanged={onChanged} />}

            {task.files && task.files.length > 0 && (
              <div className="dl-card">
                <b style={{ display: "block", marginBottom: 10 }}>
                  子文件 <span className="dl-badge">{task.files.length}</span>
                  {task.engine === "bt" && (
                    <span className="dl-field-help" style={{ display: "inline", marginLeft: 8 }}>
                      优先级：不下载 / 低 / 正常 / 高
                    </span>
                  )}
                </b>
                <div style={{ display: "grid", gap: 8 }}>
                  {task.files.slice(0, FILES_SHOWN).map((f, i) => {
                    const prio =
                      filePrioOverride[i] ?? task.file_priorities?.[i] ?? 4;
                    return (
                      <div key={f.rel_path}>
                        <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, gap: 8, alignItems: "center" }}>
                          <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={f.rel_path}>
                            {f.rel_path}
                          </span>
                          <span style={{ flex: "none", display: "inline-flex", gap: 8, alignItems: "center", fontVariantNumeric: "tabular-nums", color: "var(--fg-3)" }}>
                            {fmtBytes(f.done)} / {fmtBytes(f.size)}
                            {task.engine === "bt" && (
                              <select
                                className="dl-prio-select"
                                aria-label={`文件 ${f.rel_path} 优先级`}
                                data-testid={`file-prio-${i}`}
                                disabled={filePrioBusy === i}
                                value={prio}
                                onChange={(e) => setFilePrio(i, Number(e.target.value))}
                              >
                                {FILE_PRIORITIES.map((p) => (
                                  <option key={p.value} value={p.value}>
                                    {p.label}
                                  </option>
                                ))}
                              </select>
                            )}
                          </span>
                        </div>
                        <div className="dl-progress dl-progress--thin" style={{ marginTop: 3 }}>
                          <span
                            className="dl-bar dl-bar--run"
                            style={{ width: `${f.size ? (f.done / f.size) * 100 : 0}%` }}
                          />
                        </div>
                      </div>
                    );
                  })}
                </div>
                {task.files.length > FILES_SHOWN && (
                  <div className="dl-field-help" style={{ marginTop: 10 }}>
                    共 {task.files.length} 个子文件，仅显示前 {FILES_SHOWN} 项（优先级设置同理仅前 {FILES_SHOWN} 项）
                  </div>
                )}
              </div>
            )}

            {/* 操作日志（体检 #182 P2）：任务自身审计轨迹（add/pause/resume/
                remove/restored + detail），区别于日志视图的全局事件流 */}
            {logs && logs.events.length > 0 && (
              <div>
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 6,
                    margin: "4px 0 8px",
                    color: "var(--fg-2)",
                    fontSize: 13,
                    fontWeight: 600,
                  }}
                >
                  <Icon name="output" size={13} /> 操作日志
                  <span className="dl-badge">{logs.events.length}</span>
                </div>
                <div
                  className="dl-card"
                  style={{ padding: "6px 10px", maxHeight: 180, overflowY: "auto" }}
                  aria-label="任务操作日志"
                >
                  {logs.events.map((e, i) => (
                    <div
                      key={i}
                      style={{
                        display: "flex",
                        gap: 8,
                        padding: "2px 0",
                        fontSize: 12,
                        color: "var(--fg-2)",
                      }}
                    >
                      <span style={{ color: "var(--fg-3)", flex: "none" }}>
                        {new Date(e.at_ms).toLocaleTimeString()}
                      </span>
                      <span
                        className="dl-badge"
                        style={{ flex: "none" }}
                        data-testid="task-log-op"
                      >
                        {e.op}
                      </span>
                      <span style={{ wordBreak: "break-all", color: "var(--fg-3)" }}>
                        {e.detail ?? ""}
                      </span>
                    </div>
                  ))}
                </div>
              </div>
            )}

            {msg && (
              <div className={`dl-alert ${msg.ok ? "" : "dl-alert--error"}`} role="status">
                {msg.text}
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
      <span style={{ color: "var(--fg-3)" }}>{k}</span>
      <span style={{ color: danger ? "var(--err)" : undefined, wordBreak: "break-all", textAlign: "right" }}>
        {v}
      </span>
    </div>
  );
}
