//! 任务持久化：原子落盘（write_tasks_atomic）、autosave、从磁盘恢复（restore_from）。

use super::*;

/// 原子写任务文件（tmp + rename，防半写）。
/// 安全修复（V12，CWE-312/732）：PersistedTask 含完整 source（可能带凭据的 URL/headers），
/// 落盘必须 0600（rename 保留权限位）；存量宽松权限文件在下次写入时被收紧。
/// 审计修复（P1-1）：tmp 名唯一化（pid+纳秒）——固定 tmp 名在多调用方并发
/// 写入时交错破坏后 rename，产出损坏 JSON；唯一名保证 rename 原子性不被
/// 并发写破坏（autosave 串行化由 DaemonState.persist_lock 承担，直接调用方
/// 如 serve 退出路径亦受益）。
pub fn write_tasks_atomic(path: &Path, tasks: &[PersistedTask]) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(tasks).map_err(std::io::Error::other)?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut tmp_name = path.file_name().map_or_else(
        || format!("tasks-{unique}.json.tmp"),
        |f| format!("{}-{unique}.tmp", f.to_string_lossy()),
    );
    if tmp_name == format!("-{unique}.tmp") {
        tmp_name = format!("tasks-{unique}.json.tmp");
    }
    let tmp = path.with_file_name(tmp_name);
    let write_guard = TmpWriteGuard { path: tmp.clone() };
    let res = (|| {
        std::fs::write(&tmp, &json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    })();
    // rename 成功后 tmp 已不存在，drop guard 无操作；失败时清掉半写 tmp
    let renamed = res.is_ok();
    drop(write_guard);
    if !renamed {
        let _ = std::fs::remove_file(&tmp);
    }
    res?;
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

/// 临时文件 RAII 清理（仅在写入失败且未被 rename 消费时删半写文件）。
struct TmpWriteGuard {
    path: PathBuf,
}

impl Drop for TmpWriteGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// IP 封禁列表原子落盘（Task 46）：与 write_tasks_atomic 同配方（唯一 tmp +
/// 0600 + rename）。文件不含凭据，但保持 0600 口径一致（防面/深度防御）。
pub fn write_bans_atomic(path: &Path, bans: &[String]) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(bans).map_err(std::io::Error::other)?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!(
        "{}-{unique}.tmp",
        path.file_name()
            .map_or_else(|| "bans".to_string(), |f| f.to_string_lossy().to_string())
    );
    let tmp = path.with_file_name(tmp_name);
    let write_guard = TmpWriteGuard { path: tmp.clone() };
    let res = (|| {
        std::fs::write(&tmp, &json)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)
    })();
    let renamed = res.is_ok();
    drop(write_guard);
    if !renamed {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// IP 封禁列表回读（Task 46）：文件不存在 = 空列表（首次启动）；解析失败
/// 返回 Err（调用方 warn 后继续，封禁语义失效优于启动失败）。
pub fn read_bans(path: &Path) -> std::io::Result<Vec<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    serde_json::from_str(&text).map_err(std::io::Error::other)
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
    /// 审计修复（P1-1）：persist_lock 串行化——autosave 在 tasks 锁外被多调用方
    /// （HTTP handler/轮询回填/alert 循环）并发触发，唯一 tmp 名解决交错损坏，
    /// 该锁解决并发写导致的旧快照被新快照**重排覆盖**问题（后写者可能持旧快照）。
    pub(super) fn autosave(&self) {
        let Some(path) = self.persist_path.clone() else {
            return;
        };
        // 审查修复：快照必须落在 persist_lock 临界区内。原实现先快照后拿锁，
        // T2（新状态）先写完、T1（旧快照）后拿锁写入 → tasks.json 长期缺
        // T1 之后的任务/状态变更，重启即丢。锁内快照保证写入序 = 状态序。
        let _g = self.persist_lock.lock();
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
        for (idx, pt) in pts.into_iter().enumerate() {
            let mut t = pt.task.clone();
            let was_paused = pt.paused; // 用户暂停意图（P4 G5，旧文件无此字段 = false）
                                        // batch7（FIFO 保序）：旧档 added_at_ms 缺失（0 值）→ 按加载序
                                        // 回填 1..n——序号恒小于真实墙钟毫秒，恢复任务在递补 FIFO 中排
                                        // 在重启后新任务之前（语义正确：它们更早创建），且跨重启稳定。
            if t.metadata.added_at_ms == 0 {
                t.metadata.added_at_ms = idx as u64 + 1;
            }
            // 审计修复（P1-2）：终态任务（Completed/Stopped/Failed）不再重新入队——
            // 原实现一律 state=Queued 并 engine.add：HTTP 完成任务 .part/账本已清
            // （httpdl 只认 .part 续传）→ 重启后整文件重新下载并覆盖已落盘文件；
            // Failed 任务被静默复活重试；与 E20 完成龄清扫冲突（清扫条件永不满足）。
            // 终态任务仅重建记录（有记录无句柄），状态原样保留。
            if matches!(
                t.state,
                TaskState::Completed | TaskState::Stopped | TaskState::Failed
            ) {
                let mut rec = TaskRecord {
                    seeding_since: None,
                    task: t,
                    engine_tid: None,
                    engine_kind: pt.engine_kind,
                    engine_status: None,
                    events: vec![],
                };
                rec.push_event("restored", Some("terminal_state_kept".into()));
                self.tasks.lock().insert(rec.task.id.clone(), rec);
                restored += 1;
                continue;
            }
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
                    seeding_since: None,
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
            // batch6-P1：重启前处于 Seeding 的 BT 任务，恢复后直接回登记
            // 做种态（含计时重起）。旧实现强置 Queued：BT 轮询候选只认
            // Downloading|Seeding → 任务退出轮询管道（无速率/无名回填/做种
            // 限制执法失效）；若 lt 重发 finished alert 则 Queued→Seeding 会
            // 重发完成事件（webhook 重发 + finished_at 覆盖 + 计时清零）。
            let was_seeding = t.state == TaskState::Seeding;
            t.state = TaskState::Queued; // 重启后重新入队
            let engine = match self.engine_for(pt.engine_kind) {
                Ok(e) => e,
                Err(e) => {
                    // batch3-P1：引擎不可用（feature 未启用/BT 初始化失败等）时
                    // 不得静默丢弃记录——旧实现 continue 后首次 autosave 以内存
                    // 表整体重写 tasks.json，任务从持久化中无声消失。与 add 失败
                    // 路径同口径：插入记录 + Failed + 事件，用户可见可清理。
                    tracing::warn!("恢复任务 {} 引擎不可用: {e}", t.id);
                    let mut rec = TaskRecord {
                        seeding_since: None,
                        task: t,
                        engine_tid: None,
                        engine_kind: pt.engine_kind,
                        engine_status: None,
                        events: vec![],
                    };
                    rec.task.state = TaskState::Failed;
                    rec.push_event("restore_failed", Some(format!("engine 不可用: {e}")));
                    self.tasks.lock().insert(rec.task.id.clone(), rec);
                    restored += 1;
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
                    // ③b 连接数上限重放（S1-c，仅 BT）：Some 原样下发
                    // （>0 = 上限；0 = 复位会话级默认；handle 级参数，
                    // metadata 未就绪也可设，无 pending 场景）。
                    if pt.engine_kind == EngineKind::Bt {
                        if let Some(n) = t.max_connections {
                            if let Err(e) = engine.set_max_connections(&tid, n).await {
                                replay_details.push(format!("连接数上限重放失败: {e}"));
                            }
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
                        seeding_since: None,
                        task: t,
                        engine_tid: Some(tid),
                        engine_kind: pt.engine_kind,
                        engine_status: None,
                        events: vec![],
                    };
                    if was_paused {
                        rec.task.state = TaskState::Paused;
                    }
                    // batch6-P1：重启前 Seeding 的任务恢复后回登记做种态
                    //（计时重起；处于轮询管道内，执法/统计立即生效；后续
                    // 重发的 finished alert 因记录已是 Seeding 不再重发完成事件）
                    if was_seeding && !was_paused {
                        rec.task.state = TaskState::Seeding;
                        rec.seeding_since = Some(std::time::Instant::now());
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
                        seeding_since: None,
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
