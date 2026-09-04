//! 任务持久化：原子落盘（write_tasks_atomic）、autosave、从磁盘恢复（restore_from）。

use super::*;

/// 原子写任务文件（tmp + rename，防半写）。
/// 安全修复（V12，CWE-312/732）：PersistedTask 含完整 source（可能带凭据的 URL/headers），
/// 落盘必须 0600（rename 保留权限位）；存量宽松权限文件在下次写入时被收紧。
pub fn write_tasks_atomic(path: &Path, tasks: &[PersistedTask]) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(tasks).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(path) {
            let mode = md.permissions().mode() & 0o777;
            if mode != 0o600 {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    Ok(())
}

impl DaemonState {
    /// 序列化当前任务目录（持久化用）。`paused` 取自任务缓存态
    /// （pause/resume 处理器同步改写并 autosave，落盘时态准确）。
    pub(super) fn persisted_tasks(&self) -> Vec<PersistedTask> {
        self.tasks
            .lock()
            .values()
            .map(|r| PersistedTask {
                task: r.task.clone(),
                engine_kind: r.engine_kind,
                paused: matches!(r.task.state, TaskState::Paused),
            })
            .collect()
    }

    /// 自动落盘（启用 storage 时）。同步原子写：任务变更低频（add/remove/状态迁移），
    /// 必须保证顺序（异步并发写会竞态覆盖旧快照）；JSON 规模小，阻塞代价可忽略。
    pub(super) fn autosave(&self) {
        let Some(path) = self.persist_path.clone() else {
            return;
        };
        let data = self.persisted_tasks();
        if let Err(e) = write_tasks_atomic(&path, &data) {
            tracing::warn!("任务持久化失败 {path:?}: {e}");
        }
    }

    /// 从持久化文件恢复任务：逐条重新 add 到引擎（保留原 task_id，
    /// next_id 推进），add 失败的任务标 Failed 保留记录。返回恢复条数。
    pub async fn restore_from(&self, path: &Path) -> Result<usize, DaemonError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| DaemonError::Persist(format!("读取 {path:?} 失败: {e}")))?;
        let pts: Vec<PersistedTask> = serde_json::from_str(&text)
            .map_err(|e| DaemonError::Persist(format!("解析 {path:?} 失败: {e}")))?;
        let mut restored = 0usize;
        let mut failed = 0usize;
        for pt in pts {
            let mut t = pt.task.clone();
            let was_paused = pt.paused; // 用户暂停意图（P4 G5，旧文件无此字段 = false）
                                        // E23：定时任务未到期 → 不入引擎（engine_tid 空），到点由调度
                                        // 循环激活。paused 意图保留（用户在调度等待期暂停过）——恢复后
                                        // 仍 Paused，激活器只认 Queued 不会误触发；resume = 立即激活。
            if t.metadata.start_at_unix > now_unix() {
                t.state = if was_paused {
                    TaskState::Paused
                } else {
                    TaskState::Queued
                };
                let mut rec = TaskRecord {
                    task: t,
                    engine_tid: None,
                    engine_kind: pt.engine_kind,
                    engine_status: None,
                    events: vec![],
                };
                rec.push_event("restored", Some("scheduled_start".into()));
                self.tasks.lock().insert(rec.task.id.clone(), rec);
                restored += 1;
                continue;
            }
            t.state = TaskState::Queued; // 重启后重新入队
            let engine = match self.engine_for(pt.engine_kind) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!("恢复任务 {} 引擎不可用: {e}", t.id);
                    continue;
                }
            };
            match engine.add(&t).await {
                Ok(tid) => {
                    // 恢复期重放（best-effort）：持久化的任务级配置在恢复后原样
                    // 下发引擎，单项失败仅记事件不阻断恢复（任务可用性优先）。
                    let mut replay_details: Vec<String> = Vec::new();
                    // ① 限速重放：原样传合并配置（BT 引擎 None 方向=不限的
                    // 全量快照语义；HTTP 引擎 None up=no-op 不触发方向预拒）。
                    if let Some(l) = t.limits.clone().filter(|l| !l.is_empty()) {
                        if let Err(e) = engine.set_limits(&tid, l.down_kb_s, l.up_kb_s).await {
                            replay_details.push(format!("限速重放失败: {e}"));
                        }
                    }
                    // ② 子文件优先级重放（仅 BT 任务；非 BT 引擎 Unsupported →
                    // 记事件）。magnet 恢复时 metadata 未就绪（引擎 NotFound）→
                    // 挂 pending 集合，由重放循环在就绪后收敛；.torrent 任务
                    // add 时 metadata 已就绪，此处直接成功。
                    if pt.engine_kind == EngineKind::Bt {
                        if let Some(prios) = t.file_priorities.clone().filter(|p| !p.is_empty()) {
                            let pairs: Vec<(usize, u32)> =
                                prios.iter().enumerate().map(|(i, p)| (i, *p)).collect();
                            match engine.set_file_priorities(&tid, &pairs).await {
                                Ok(()) => {}
                                Err(smart_dl_core::types::EngineError::NotFound) => {
                                    self.pending_file_prio.lock().insert(t.id.clone());
                                    replay_details
                                        .push("子文件优先级待 metadata 就绪后重放".into());
                                }
                                Err(e) => {
                                    replay_details.push(format!("子文件优先级重放失败: {e}"));
                                }
                            }
                        }
                    }
                    // ③ 顺序下载重放：sequential=true 原样下发（BT=handle 级
                    // flag 即时；HTTP=字段改写，下一重下轮拾取；不支持引擎记
                    // 事件不阻断恢复）。flag 幂等，与 add 时下发叠加无副作用。
                    if t.sequential {
                        if let Err(e) = engine.set_sequential(&tid, true).await {
                            replay_details.push(format!("顺序下载重放失败: {e}"));
                        }
                    }
                    // ④ 暂停意图重放 + 运行态恢复（P4 G5）：
                    // - was_paused → engine.pause：BT（内核暂停 + 意图登记持续压制
                    //   + fastresume）；HTTP（暂停标志置位，循环段边界退出）。
                    //   记录态同步回写 Paused（否则缓存显示 Queued 与内核错位）。
                    // - 非 paused 且 BT → engine.resume：所有 add 路径内核侧强制
                    //   paused（lt_kernel 统一语义），不 resume 则恢复任务永不下载。
                    //   HTTP add 已自启下载循环（epoch 语义），不得重复 resume。
                    if was_paused {
                        if let Err(e) = engine.pause(&tid).await {
                            replay_details.push(format!("暂停意图重放失败: {e}"));
                        }
                    } else if pt.engine_kind == EngineKind::Bt {
                        if let Err(e) = engine.resume(&tid).await {
                            replay_details.push(format!("恢复运行重放失败: {e}"));
                        }
                    }
                    let mut rec = TaskRecord {
                        task: t,
                        engine_tid: Some(tid),
                        engine_kind: pt.engine_kind,
                        engine_status: None,
                        events: vec![],
                    };
                    if was_paused {
                        rec.task.state = TaskState::Paused;
                    }
                    if replay_details.is_empty() {
                        rec.push_event("restored", None);
                    } else {
                        rec.push_event("restored", Some(replay_details.join("; ")));
                    }
                    self.tasks.lock().insert(rec.task.id.clone(), rec);
                    restored += 1;
                }
                Err(e) => {
                    tracing::warn!("恢复任务 {} 引擎 add 失败（标 Failed）: {e}", t.id);
                    t.state = TaskState::Failed;
                    let mut rec = TaskRecord {
                        task: t,
                        engine_tid: None,
                        engine_kind: pt.engine_kind,
                        engine_status: None,
                        events: vec![],
                    };
                    rec.push_event("restored", Some(format!("引擎 add 失败: {e}")));
                    self.tasks.lock().insert(rec.task.id.clone(), rec);
                    failed += 1;
                }
            }
        }
        // next_id 推进到已用最大值之后（保留原 task_id 的关键）
        let max_id = self
            .tasks
            .lock()
            .keys()
            .filter_map(|k| k.strip_prefix('t').and_then(|s| s.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        self.next_id.fetch_max(max_id + 1, Ordering::SeqCst);
        tracing::info!("任务恢复完成: {restored} 恢复, {failed} 失败（引擎 add 错误）");
        Ok(restored)
    }
}
