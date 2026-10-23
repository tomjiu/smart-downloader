//! 任务创建域：链接/HTTP/FTP/SFTP/BT 种子/迅雷导入/metalink 添加、槽位管理、定时激活、
//! 目标路径与空间预检。

use super::*;
impl DaemonState {
    /// 添加任务入口：支持 http/https/thunder:///qqdl:// 链接（归一化后走 HTTP 引擎）；
    /// magnet（feature `bt` 时走 libtorrent 引擎）；ed2k/无法识别 → InvalidSource。
    pub async fn add_link_task(
        &self,
        link: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_link_task_opts(link, dest_root, AddHttpOpts::default())
            .await
    }

    // ===== 定时/错峰下载（E23）=====
    //
    // 语义：start_at 在未来的任务**不接入引擎**（engine_tid 空、停留 Queued），
    // 到点由调度循环 `activate_due_tasks` 调引擎 add 激活（与普通 add 同链路，
    // 查重/预检/目录创建已在 add 路径完成）。两个入口：
    // - 显式定时：`AddTaskReq.start_at_unix`（unix 秒；过去时刻 = 立即，宽容不 400）
    // - 错峰：`[scheduler] start_jitter_seconds` > 0 时，未显式指定的任务在
    //   0..=N 秒内随机延迟启动（批量入队不被同时压向引擎/带宽）。

    /// 解析任务定时启动时刻（E23）：显式值直传（0/过去 = 立即）；未显式且
    /// 配置了错峰抖动 → now + 0..=jitter 秒（亚秒纳秒 ^ next_id 混熵，错峰
    /// 无需密码学随机）；否则 0（立即）。
    pub(super) fn resolve_start_at(&self, explicit: Option<u64>) -> u64 {
        if let Some(t) = explicit {
            return t;
        }
        let jitter = self
            .start_jitter_secs
            .load(std::sync::atomic::Ordering::Relaxed) as u64;
        if jitter == 0 {
            return 0;
        }
        let nano = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        let mix = nano
            ^ self
                .next_id
                .load(std::sync::atomic::Ordering::Relaxed)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15);
        now_unix() + (mix % (jitter + 1))
    }

    /// 落一条调度等待任务记录（E23）：不入引擎（engine_tid 空），到点由
    /// 调度循环激活。与 E21 conflict_skip 同款「有记录无句柄」形态。
    pub(super) fn insert_scheduled_task(&self, task: DownloadTask, kind: EngineKind) -> String {
        let task_id = task.id.clone();
        let start_at = task.metadata.start_at_unix;
        let mut rec = TaskRecord {
            seeding_since: None,
            task,
            engine_tid: None,
            engine_kind: kind,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", Some(format!("scheduled_start@{start_at}")));
        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        task_id
    }

    // ==================== S1-b 并发队列门控 ====================
    // [queue] max_active_{bt,http,ftp}（0 = 不限）：add 入口配额满 → 任务落
    // Queued 排队（无句柄），由 activate_due_tasks（serve 1s tick）在槽位空闲
    // 后按 FIFO（added_at_ms 升序，重启后保序）递补激活。槽位释放 = 任务进入非传输态
    // （Completed/Failed/Stopped/Paused/Seeding/移除）。手动 resume = 强制
    // 开始（无视配额，对齐 qbit「强制继续」语义）；恢复路径重放不设闸
    // （保持停机前在飞集合，仅新增激活受闸）。

    /// 引擎槽位下标（[bt, http, ftp]；Provider/XunleiNas/Sftp 无独立配额，
    /// 归 ftp 桶但 Provider/XunleiNas 实际不会走到——门控仅在 add 路径调用，
    /// kind 恒为 Bt/Http/Ftp/Sftp；Sftp 复用 ftp 桶（同为文件传输协议，
    /// 独立配额键后续按需拆分））。
    fn slot_index(kind: EngineKind) -> usize {
        match kind {
            EngineKind::Bt => 0,
            EngineKind::Http => 1,
            EngineKind::Ftp | EngineKind::Sftp | EngineKind::Provider | EngineKind::XunleiNas => 2,
        }
    }

    /// 引擎并发配额（S1-b）：None = 不限（配置 0 或引擎无独立配额）。
    /// 读 live queue_cfg（设置热改跟随）。
    pub(super) fn queue_limit_of(&self, kind: EngineKind) -> Option<u32> {
        let q = self.queue_cfg.lock().clone();
        let v = match kind {
            EngineKind::Bt => q.max_active_bt,
            EngineKind::Http => q.max_active_http,
            _ => q.max_active_ftp,
        };
        (v > 0).then_some(v)
    }

    /// 当前占用引擎槽位的任务数（S1-b）：有句柄且不在释放态（Paused = 用户
    /// 挂起 / Seeding = 做种不占下载槽 / 终态）。add 后记录态短暂停留 Queued
    /// （首轮引擎状态同步前）——有句柄即占位。batch7-P1：slot_reservations
    /// 集内 id（add 在途窗口的占位记录，Queued 无句柄）一并计入——关闭
    /// 「记录未插入前并发 add 双双过闸」的超卖窗口（TOCTOU）。
    pub(super) fn active_slot_counts(&self) -> [u32; 3] {
        let tasks = self.tasks.lock();
        Self::active_slot_counts_locked(&tasks, &self.slot_reservations.lock())
    }

    /// [`Self::active_slot_counts`] 的已持锁变体（gate_or_enqueue 在同一
    /// tasks 临界区内完成「计数检查 + 记录插入 + 预留登记」，消除检查与
    /// 登记之间的并发窗口）。锁序与公开版一致：tasks → slot_reservations。
    fn active_slot_counts_locked(
        tasks: &std::collections::HashMap<TaskId, TaskRecord>,
        reservations: &std::collections::HashSet<TaskId>,
    ) -> [u32; 3] {
        let mut counts = [0u32; 3];
        for rec in tasks.values() {
            if rec.engine_tid.is_none() && !reservations.contains(&rec.task.id) {
                continue;
            }
            if matches!(
                rec.task.state,
                TaskState::Paused
                    | TaskState::Seeding
                    | TaskState::Completed
                    | TaskState::Stopped
                    | TaskState::Failed
            ) {
                continue;
            }
            counts[Self::slot_index(rec.engine_kind)] += 1;
        }
        counts
    }

    /// add 路径配额闸门（S1-b + batch7-P1 预留槽位改造）：
    /// - true = 有槽位，已同步插入【预留占位记录】（Queued + 无句柄 +
    ///   slot_reservations 登记 + TaskCreated 发布），调用方继续 engine.add：
    ///   成功 → [`Self::attach_reserved_handle`]，失败 →
    ///   [`Self::rollback_slot_reservation`]。占位记录使 add await 期间任务
    ///   对并发 add/槽位计数可见，根治在途窗口超卖（TOCTOU）。
    /// - false = 配额已满，任务已落排队记录（Queued + 无句柄 + queue_wait 事件
    ///   + TaskCreated 发布），调用方直接返回 task_id。
    ///
    /// 调用时机：定时判定之后（定时任务不受此闸——到点由调度循环按当时配额激活）；
    /// engine_for 解析之后（引擎不可用直接报错，不占槽不入队）。
    pub(super) fn gate_or_enqueue(&self, task: &DownloadTask, kind: EngineKind) -> bool {
        // 并发正确性修复（第三轮审查后 flaky 实证）：计数检查与记录插入/
        // 预留登记必须在同一 tasks 临界区内完成。原实现三步分离：
        //   ① active_slot_counts()（检查） ② tasks.insert（插入）
        //   ③ slot_reservations.insert（登记）
        // ①与②③之间存在窗口：并发 add B 在 A 已执行②未执行③时计数——A 的
        // 占位记录 engine_tid=None 且不在预留集 → 被漏计 → B 过闸 → 超卖
        // （slot_reservation_closes_inflight_oversell 5 次 2 次失败即此竞态）。
        // 修复：持 tasks 锁完成「计数 + 插入 + 登记」三步原子化；
        // autosave/publish 保持在锁外（Bug B：autosave 重入 tasks 锁自死锁）。
        let task_id = task.id.clone();
        let limit = self.queue_limit_of(kind);
        let reserved = {
            let mut tasks = self.tasks.lock();
            let full = limit
                .map(|l| {
                    Self::active_slot_counts_locked(&tasks, &self.slot_reservations.lock())
                        [Self::slot_index(kind)]
                        >= l
                })
                .unwrap_or(false);
            if full {
                let mut rec = TaskRecord {
                    seeding_since: None,
                    task: task.clone(),
                    engine_tid: None,
                    engine_kind: kind,
                    engine_status: None,
                    events: vec![],
                };
                rec.push_event(
                    "add",
                    Some(format!(
                        "queue_wait: {kind:?} 并发配额已满（{}），槽位空闲后自动递补",
                        limit.unwrap_or(0)
                    )),
                );
                tasks.insert(task_id.clone(), rec);
                false
            } else {
                // batch7-P1：有槽位或不限配额 → 同步预留（占位记录 + 预留集
                // 登记）。不限配额同样占位：attach 的 double-check 依赖占位
                // 记录存在（缺失 = 用户已移除，正确回滚），不可省略。
                let mut rec = TaskRecord {
                    seeding_since: None,
                    task: task.clone(),
                    engine_tid: None,
                    engine_kind: kind,
                    engine_status: None,
                    events: vec![],
                };
                rec.push_event("add", Some("slot_reserve: 引擎接入中".into()));
                tasks.insert(task_id.clone(), rec);
                self.slot_reservations.lock().insert(task_id.clone());
                true
            }
        };
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        reserved
    }

    /// engine.add 成功后句柄落位（batch7-P1）：预留记录 attach 句柄 + 状态
    /// Downloading。double-check 竞态间隙的用户意图（同 activate_one 口径）：
    /// - 记录缺失（用户在窗口内移除）/ 状态已变（用户暂停）/ 句柄已存在
    ///   （resume 强制开始抢先激活）→ 尽力回滚本方引擎任务（不留孤儿），
    ///   不覆盖用户意图；
    /// - 正常 → attach + StateChanged(Queued→Downloading)。
    ///
    /// 预留集摘除与句柄落位在同一 tasks 临界区内完成（PR #114 催修 P0-2：
    /// 统一全局锁序 tasks → slot_reservations，见 docs/LOCK_MODEL.md）。
    /// 原写法「先单独锁 slot_res 摘除、再锁 tasks」语义上即反向锁序——
    /// 与 gate_or_enqueue/递补筛选（持 tasks 取 slot_res）方向相反；
    /// 且存在外部可见瞬态（预留已摘 + 句柄未落位），并发 gate 计数会把
    /// 该任务漏计 → 短暂超卖窗口。合并进同一临界区后：锁序统一 +
    /// 「摘除+落位」原子化，瞬态对外不可见。
    pub(super) async fn attach_reserved_handle(
        &self,
        task_id: &str,
        engine_tid: EngineTaskId,
        kind: EngineKind,
        expected_state: &TaskState,
    ) {
        let attached = {
            let mut tasks = self.tasks.lock();
            self.slot_reservations.lock().remove(task_id);
            match tasks.get_mut(task_id) {
                Some(rec) if rec.engine_tid.is_none() && rec.task.state == *expected_state => {
                    rec.engine_tid = Some(engine_tid.clone());
                    // batch3-P1 同口径：BT 无轮询纠偏 → 激活即 Downloading；
                    // HTTP/FTP/SFTP 留 Queued，首轮轮询迁移（poll_engine_states
                    // 既有语义：迁移事件/速率接入依赖该次转移，不可提前吞掉）。
                    if kind == EngineKind::Bt {
                        rec.task.state = TaskState::Downloading(kind);
                    }
                    rec.push_event("add", None);
                    true
                }
                Some(rec) => {
                    rec.push_event(
                        "add",
                        Some("引擎接入间隙意图变更（暂停/抢先激活/移除），引擎任务已回滚".into()),
                    );
                    false
                }
                None => false,
            }
        };
        if attached {
            self.autosave();
            self.hub.publish(SchedulerEvent::StateChanged {
                task_id: task_id.to_string(),
                from: expected_state.clone(),
                to: TaskState::Downloading(kind),
            });
        } else {
            // 竞态间隙用户意图变更：尽力移除刚接入的引擎任务（不留孤儿）
            if let Ok(e) = self.engine_for(kind) {
                let _ = e.remove(&engine_tid, false).await;
            }
            self.autosave();
        }
    }

    /// engine.add 失败回滚（batch7-P1）：摘预留标记 + 删除占位记录（对齐
    /// 旧语义：add 失败 = 无记录，API 返回 Err）。句柄已存在（竞态抢先）
    /// 则仅摘标记保留记录。
    /// 锁序同 attach_reserved_handle：tasks → slot_reservations（同一临界区，
    /// PR #114 催修 P0-2 统一锁序）。
    pub(super) fn rollback_slot_reservation(&self, task_id: &str) {
        {
            let mut tasks = self.tasks.lock();
            self.slot_reservations.lock().remove(task_id);
            let orphan = tasks.get(task_id).is_some_and(|r| r.engine_tid.is_none());
            if orphan {
                tasks.remove(task_id);
            }
        }
        self.autosave();
    }

    /// 激活单个定时任务（E23）：调引擎 add 接入 + 记录句柄 + 事件。
    /// add 失败/引擎不可用 → E30 重试拦截（预算未用尽安排退避重试）否则置
    /// Failed（对齐 restore add 失败语义）。激活成功时消费重试安排
    /// （next_retry_at 清零，快照不再显示过期时间）。
    /// 返回是否激活成功。调用方需保证任务处于 Queued（调度等待态）。
    pub(super) async fn activate_one(
        &self,
        id: &str,
        task: DownloadTask,
        kind: EngineKind,
    ) -> bool {
        let engine = match self.engine_for(kind) {
            Ok(e) => e,
            Err(e) => {
                let retrying = {
                    let mut tasks = self.tasks.lock();
                    match tasks.get_mut(id) {
                        Some(rec) => {
                            rec.push_event("scheduled_start", Some(format!("引擎不可用: {e}")));
                            let to = rec.fail_or_schedule_retry(Some(&format!("引擎不可用: {e}")));
                            to == TaskState::Queued
                        }
                        None => false,
                    }
                };
                self.autosave();
                if !retrying {
                    self.hub.publish(SchedulerEvent::Failed {
                        task_id: id.to_string(),
                        reason: format!("定时激活引擎不可用: {e}"),
                    });
                }
                return false;
            }
        };
        match engine.add(&task).await {
            Ok(tid) => {
                {
                    let mut tasks = self.tasks.lock();
                    match tasks.get_mut(id) {
                        // 双检：激活间隙任务可能已被 resume 路径抢先激活/被移除/
                        // 被用户暂停（batch6-P2：候选快照后用户 pause →
                        // Queued→Paused，engine_tid 仍 None——不校验状态会把
                        // 任务覆盖回 Downloading，违背用户暂停意图）。比对
                        // 调用方快照态：resume 激活的快照态本来就是
                        // Paused/Failed（合法），调度激活为 Queued；快照后
                        // 状态被改写 = 间隙有并发意图 → 放弃本次激活。激活
                        // 成功即消费重试安排（E30：next_retry_at 清零）。
                        Some(rec) if rec.engine_tid.is_none() && rec.task.state == task.state => {
                            rec.engine_tid = Some(tid);
                            rec.task.metadata.next_retry_at_unix = 0;
                            // batch3-P1：激活即 Downloading（BT 任务无轮询纠偏，
                            // 与 add 路径同口径；HTTP 任务轮询稍后幂等确认）
                            rec.task.state = TaskState::Downloading(kind);
                            rec.push_event("scheduled_start", None);
                        }
                        _ => return false,
                    }
                }
                self.autosave();
                self.hub.publish(SchedulerEvent::TaskActivated {
                    task_id: id.to_string(),
                });
                true
            }
            Err(e) => {
                let retrying = {
                    let mut tasks = self.tasks.lock();
                    match tasks.get_mut(id) {
                        Some(rec) => {
                            rec.push_event("scheduled_start", Some(format!("引擎 add 失败: {e}")));
                            let to =
                                rec.fail_or_schedule_retry(Some(&format!("引擎 add 失败: {e}")));
                            to == TaskState::Queued
                        }
                        None => false,
                    }
                };
                self.autosave();
                if !retrying {
                    self.hub.publish(SchedulerEvent::Failed {
                        task_id: id.to_string(),
                        reason: format!("定时激活失败: {e}"),
                    });
                }
                false
            }
        }
    }

    /// 调度激活循环驱动点（E23+E30+S1-b）：把等待中的任务（未接入引擎、
    /// Queued）逐个接入引擎。候选与到期判定：
    /// - 带重试安排（next_retry_at > 0）→ 按 next_retry_at 判定（重试安排
    ///   优先，避免定时任务首次激活间隙被误读）
    /// - 否则 start_at > 0 → 按 E23 start_at 判定
    /// - 否则 → S1-b queue_wait（add 时配额满落队），视为随时到期
    ///
    /// 激活统一过 S1-b 配额闸门：槽位不足则本轮跳过（队首优先）。递补排序 =
    /// (queue_priority 升序, added_at_ms 升序)——Task 46 qbit 队列位置对标：
    /// 值小者先补位，同值保持 FIFO（create 序，Instant 纳秒粒度同刻碰撞实际
    /// 不可能）。serve 以 1s 周期驱动（终态/暂停/移除释放槽位 → 下轮递补）；
    /// 测试可直接调用。返回激活成功的 task_id 列表（激活序）。
    pub async fn activate_due_tasks(&self) -> Vec<String> {
        let now = now_unix();
        let due: Vec<(String, DownloadTask, EngineKind)> = {
            let tasks = self.tasks.lock();
            let mut v: Vec<(
                String,
                DownloadTask,
                EngineKind,
                i32,
                u64,
                std::time::Instant,
            )> = tasks
                .iter()
                .filter(|(_, rec)| {
                    if rec.engine_tid.is_some()
                        || rec.task.state != TaskState::Queued
                        // batch7-P1：add 在途占位记录不是递补候选（正在接入）
                        || self.slot_reservations.lock().contains(&rec.task.id)
                    {
                        return false;
                    }
                    let m = &rec.task.metadata;
                    if m.next_retry_at_unix > 0 {
                        // E30：重试等待中——到期才激活
                        m.next_retry_at_unix <= now
                    } else if m.start_at_unix > 0 {
                        // E23：定时启动等待中——到期才激活
                        m.start_at_unix <= now
                    } else {
                        // S1-b：queue_wait 等槽位——随时可激活（下方配额闸门把关）
                        true
                    }
                })
                .map(|(id, rec)| {
                    (
                        id.clone(),
                        rec.task.clone(),
                        rec.engine_kind,
                        rec.task.queue_priority,
                        rec.task.metadata.added_at_ms,
                        rec.task.created_at,
                    )
                })
                .collect();
            // S1-b 递补序：优先级小者先，同值 FIFO（batch7：可持久化的
            // added_at_ms 为主键——重启后恢复任务保有原始入队序；created_at
            // 仅作运行期同毫秒 tie-break）
            v.sort_by_key(|(_, _, _, prio, ms, created)| (*prio, *ms, *created));
            v.into_iter()
                .map(|(id, t, k, _, _, _)| (id, t, k))
                .collect()
        };
        let mut counts = self.active_slot_counts();
        let mut activated = Vec::new();
        for (id, task, kind) in due {
            // S1-b 配额闸门：槽位不足本轮跳过（下一 tick 再试）
            if let Some(limit) = self.queue_limit_of(kind) {
                if counts[Self::slot_index(kind)] >= limit {
                    continue;
                }
            }
            if self.activate_one(&id, task, kind).await {
                counts[Self::slot_index(kind)] += 1;
                activated.push(id);
            }
        }
        activated
    }

    /// 顺序下载变体：`sequential` 写入任务（HTTP=在飞窗口；BT=sequential
    /// flag；其余引擎忽略）。引擎 add 后对 BT 任务立即下发（handle 级 flag，
    /// metadata 未就绪也可设）。
    /// 任务级代理（E5）：仅 HTTP 任务生效；magnet/ed2k 等任务忽略该字段。
    /// 链接任务创建（E6 opts 收口）：HTTP 分支整体透传 `AddHttpOpts`；
    /// magnet 分支仅取 `sequential`（其余字段对 BT 无语义，静默忽略）；
    /// ed2k/ftp/xunlei 分支不受影响。
    /// 文件冲突改名候选（E21）：`a.bin` → `a(1).bin`（无扩展名 → `a(1)`）；
    /// 首个磁盘不存在的候选。上限 1000（防极端目录全占满时死循环）。
    pub(super) fn bump_conflict_name(dir: &Path, name: &str) -> Option<String> {
        let (stem, ext) = match name.rsplit_once('.') {
            // 无扩展名或纯点：整体当 stem（对齐常见下载器行为）
            Some((st, e)) if !st.is_empty() && !e.is_empty() => (st, Some(e)),
            _ => (name, None),
        };
        for k in 1..1000 {
            let cand = match ext {
                Some(e) => format!("{stem}({k}).{e}"),
                None => format!("{stem}({k})"),
            };
            if !dir.join(&cand).exists() {
                return Some(cand);
            }
        }
        None
    }

    /// 完成文件移动（E27）：目标目录自动创建；同名冲突自动改名
    /// （`bump_conflict_name`）；同盘 rename 直达，跨盘（EXDEV 等错误）
    /// copy+remove 回退。返回最终落位路径。
    pub(super) fn move_completed_file(
        src: &Path,
        dst_dir: &Path,
        name: &str,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(dst_dir).map_err(|e| format!("目标目录创建失败 {dst_dir:?}: {e}"))?;
        let target_name = if dst_dir.join(name).exists() {
            DaemonState::bump_conflict_name(dst_dir, name)
                .ok_or_else(|| format!("目标目录同名冲突且改名候选耗尽: {dst_dir:?}/{name}"))?
        } else {
            name.to_string()
        };
        let target = dst_dir.join(target_name);
        if let Err(e) = fs::rename(src, &target) {
            // 跨设备 rename 失败（EXDEV）→ copy + remove 回退
            fs::copy(src, &target).map_err(|e2| {
                let _ = fs::remove_file(&target); // 半份拷贝不留垃圾
                format!("rename 失败（{e}）且 copy 回退也失败: {e2}")
            })?;
            fs::remove_file(src)
                .map_err(|e| format!("copy 成功但源文件删除失败（存在重复副本）: {e}"))?;
        }
        Ok(target)
    }

    /// 完成后目录整体移动（batch5，BT 多文件任务；与 move_completed_file
    /// 同语义）：同盘 rename；跨盘 copy_dir_all + remove_dir_all 回退
    ///（目录无原子跨盘移动；回退中途失败会留部分副本，错误信息如实报告）。
    /// 同名冲突自动改名 `name(1)`（复用 bump_conflict_name）。
    pub(super) fn move_completed_dir(
        src: &Path,
        dst_dir: &Path,
        name: &str,
    ) -> Result<PathBuf, String> {
        fs::create_dir_all(dst_dir).map_err(|e| format!("目标目录创建失败 {dst_dir:?}: {e}"))?;
        let target_name = if dst_dir.join(name).exists() {
            DaemonState::bump_conflict_name(dst_dir, name)
                .ok_or_else(|| format!("目标目录同名冲突且改名候选耗尽: {dst_dir:?}/{name}"))?
        } else {
            name.to_string()
        };
        let target = dst_dir.join(target_name);
        if let Err(e) = fs::rename(src, &target) {
            // 跨设备 rename 失败（EXDEV）→ 递归 copy + remove 回退
            copy_dir_recursive(src, &target).map_err(|e2| {
                let _ = fs::remove_dir_all(&target); // 半份拷贝不留垃圾
                format!("rename 失败（{e}）且递归 copy 回退也失败: {e2}")
            })?;
            fs::remove_dir_all(src)
                .map_err(|e| format!("copy 成功但源目录删除失败（存在重复副本）: {e}"))?;
        }
        Ok(target)
    }

    pub async fn add_link_task_opts(
        &self,
        link: String,
        dest_root: Option<String>,
        opts: AddHttpOpts,
    ) -> Result<TaskId, DaemonError> {
        match normalize_user_link(&link) {
            NormalizedSource::Http(real) => self.add_http_task_opts(real, dest_root, opts).await,
            NormalizedSource::Magnet(m) => {
                #[cfg(feature = "bt")]
                {
                    // batch5（E30 贯通 BT）：magnet 入口同 consuming opts.auto_retry
                    return self
                        .add_bt_task_opts(
                            m,
                            dest_root,
                            opts.sequential,
                            opts.start_at_unix,
                            opts.auto_retry,
                        )
                        .await;
                }
                #[cfg(not(feature = "bt"))]
                {
                    let _ = opts.sequential;
                    let _ = opts.start_at_unix;
                    let _ = opts.auto_retry;
                    Err(DaemonError::InvalidSource(format!(
                        "magnet 需 BT 引擎（编译时启用 --features daemon/bt）: {m}"
                    )))
                }
            }
            NormalizedSource::Ed2k(e) => {
                Err(DaemonError::InvalidSource(format!("ed2k 不支持: {e}")))
            }
            NormalizedSource::Ftp(u) => {
                #[cfg(feature = "ftp")]
                {
                    return self
                        .add_ftp_task_opts(u, dest_root, opts.start_at_unix, opts.auto_retry)
                        .await;
                }
                #[cfg(not(feature = "ftp"))]
                {
                    let _ = opts.start_at_unix;
                    let _ = opts.auto_retry;
                    Err(DaemonError::InvalidSource(format!(
                        "ftp 需 FTP 引擎（编译时启用 --features ftp）: {u}"
                    )))
                }
            }
            NormalizedSource::Sftp(u) => {
                #[cfg(feature = "sftp")]
                {
                    return self
                        .add_sftp_task_opts(u, dest_root, opts.start_at_unix)
                        .await;
                }
                #[cfg(not(feature = "sftp"))]
                {
                    let _ = opts.start_at_unix;
                    Err(DaemonError::InvalidSource(format!(
                        "sftp 需 SFTP 引擎（编译时启用 --features sftp）: {u}"
                    )))
                }
            }
            NormalizedSource::XunleiShare(u) => Err(DaemonError::InvalidSource(format!(
                "迅雷网盘分享暂不支持直接导入: {u}"
            ))),
            NormalizedSource::Unsupported(orig) => Err(DaemonError::InvalidSource(format!(
                "无法识别的链接: {orig}"
            ))),
        }
    }

    /// 添加 BT 任务（feature `bt`，顺序下载 opts 直通入口）：btih canonical 查重 → 引擎 add → TaskCreated 事件。
    /// `start_at_unix`（E23）：Some(未来) = 延迟入引擎（不调 engine.add），
    /// 到点由调度循环激活。`auto_retry`（batch5，E30 贯通 BT）：失败自动
    /// 重试次数上限（拦截器 bt_alerts 已接线，指数退避；0 = 不重试）。
    #[cfg(feature = "bt")]
    pub(super) async fn add_bt_task_opts(
        &self,
        magnet: String,
        dest_root: Option<String>,
        sequential: bool,
        start_at_unix: Option<u64>,
        auto_retry: u32,
    ) -> Result<TaskId, DaemonError> {
        // B10：目标目录预检（创建/可写）；magnet 总大小元数据前未知 → 空间预检跳过
        // dest 未指定 → 默认落盘目录（与 HTTP 一致：default_dest_root 配置）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root =
            ensure_dest_root(non_empty_dest(dest_root).or(Some(def)), &self.dest_roots())?;
        let canonical = CanonicalId {
            kind: CanonicalKind::Bt,
            identity: btih_of(&magnet).unwrap_or_else(|| magnet.clone()),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Magnet(magnet.clone()),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: RetryState {
                retries: 0,
                max_retries: auto_retry,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                added_at_ms: now_unix_ms(),
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎（记录 Queued + 无句柄），
        // 到点由调度循环接入（engine.add 与查重/预检后置同链路）。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Bt));
        }
        // S1-b 队列门控（batch7 预留槽位，同 HTTP 口径）：engine.add 成功
        // attach / 失败回滚；顺序 flag 在 attach 前下发（句柄仍存活）。
        let engine = self.engine_for(EngineKind::Bt)?;
        if !self.gate_or_enqueue(&task, EngineKind::Bt) {
            return Ok(task_id);
        }
        let engine_tid = match engine.add(&task).await {
            Ok(tid) => tid,
            Err(e) => {
                self.rollback_slot_reservation(&task_id);
                return Err(DaemonError::Engine(e.to_string()));
            }
        };
        // 顺序下载立即下发（handle 级 flag，metadata 未就绪也可设；
        // 失败不回滚任务，恢复重放 + set_sequential 端点可补）。
        if sequential {
            if let Err(e) = engine.set_sequential(&engine_tid, true).await {
                tracing::warn!("BT 任务 {task_id} 顺序下载 flag 下发失败: {e}");
            }
        }
        self.attach_reserved_handle(&task_id, engine_tid, EngineKind::Bt, &task.state)
            .await;
        Ok(task_id)
    }

    /// 添加 .torrent 文件任务（feature `bt`）：infohash canonical 查重 → 引擎
    /// add_torrent_file → TaskCreated 事件。torrent 字节来自 API base64 解码。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_torrent_task_opts(torrent_bytes, dest_root, false, None, 0)
            .await
    }

    /// 顺序下载变体：`sequential` 写入任务 + 引擎 add 后立即下发 flag。
    /// `start_at_unix`（E23）：Some(未来) = 延迟入引擎，到点由调度循环激活。
    /// `auto_retry`（batch5，E30 贯通 BT）：同 magnet 入口。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task_opts(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
        sequential: bool,
        start_at_unix: Option<u64>,
        auto_retry: u32,
    ) -> Result<TaskId, DaemonError> {
        // B10：目标目录预检（创建/可写）；dest 未指定 → 默认落盘目录（与 HTTP/BT-magnet 一致）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root =
            ensure_dest_root(non_empty_dest(dest_root).or(Some(def)), &self.dest_roots())?;
        let Some(ih) = torrent_infohash(&torrent_bytes) else {
            return Err(DaemonError::InvalidSource(
                ".torrent 解析失败：无法定位 info dict".into(),
            ));
        };
        // B10：torrent 总大小已知 → 空间预检（多文件按 files 各项求和；解析失败
        // 回退单文件最小解析；均拿不到才跳过）
        if let Some(total) = torrent_precheck_total(&torrent_bytes) {
            precheck_space(&dest_root, total, self.disk_precheck_strict)?;
        }
        let canonical = CanonicalId {
            kind: CanonicalKind::Bt,
            identity: ih.clone(),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::TorrentFile(torrent_bytes),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: RetryState {
                retries: 0,
                max_retries: auto_retry,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                added_at_ms: now_unix_ms(),
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Bt));
        }
        // S1-b 队列门控（batch7 预留槽位，同 magnet 口径）：engine.add 成功
        // attach / 失败回滚；顺序 flag 在 attach 前下发（句柄仍存活）。
        let engine = self.engine_for(EngineKind::Bt)?;
        if !self.gate_or_enqueue(&task, EngineKind::Bt) {
            return Ok(task_id);
        }
        let engine_tid = match engine.add(&task).await {
            Ok(tid) => tid,
            Err(e) => {
                self.rollback_slot_reservation(&task_id);
                return Err(DaemonError::Engine(e.to_string()));
            }
        };
        // 顺序下载立即下发（同 magnet 路径：handle 级 flag，失败不回滚）。
        if sequential {
            if let Err(e) = engine.set_sequential(&engine_tid, true).await {
                tracing::warn!("BT 任务 {task_id} 顺序下载 flag 下发失败: {e}");
            }
        }
        self.attach_reserved_handle(&task_id, engine_tid, EngineKind::Bt, &task.state)
            .await;
        Ok(task_id)
    }

    /// 迅雷任务导入（M9）：xlbt.cfg + 一组 .bt.xltd + .torrent → xunlei-convert fastresume
    /// → btcore.add_xunlei_resume → TaskCreated 事件。
    ///
    /// 单文件 torrent：`xltds` 应包含恰好 1 个 `.bt.xltd`（对应唯一文件）。
    /// 多文件 torrent：`xltds` 按 `meta.files` 顺序，每个文件对应一个 `.bt.xltd`。
    #[cfg(feature = "xunlei-import")]
    pub async fn add_xunlei_import_task(
        &self,
        torrent: Vec<u8>,
        cfg: Vec<u8>,
        xltds: Vec<Vec<u8>>,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        use xunlei_convert::{build_bitfield_from_indices, FastresumeConverter, XlbtCfg};

        // 1. 解析 torrent
        let meta = TorrentMeta::parse(&torrent)?;

        // 单文件/多文件统一归一化为文件列表
        let files: Vec<FileMeta> = if meta.files.is_empty() {
            vec![FileMeta {
                path: meta.name.clone(),
                size: meta.file_size,
                piece_offset: 0,
                piece_count: meta.pieces_hash.len(),
            }]
        } else {
            meta.files.clone()
        };
        let total_size: u64 = files.iter().map(|f| f.size).sum();

        // xltd 数量须与文件数一致
        if xltds.len() != files.len() {
            return Err(DaemonError::InvalidSource(format!(
                "xltd 数量 {} 与 torrent 文件数 {} 不匹配",
                xltds.len(),
                files.len()
            )));
        }

        // 2. 确保目标目录存在
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root =
            ensure_dest_root(non_empty_dest(dest_root).or(Some(def)), &self.dest_roots())?;

        // 3. 空间预检（总大小已知）
        precheck_space(&dest_root, total_size, self.disk_precheck_strict)?;

        // 4. 查重
        let canonical = CanonicalId {
            kind: CanonicalKind::Bt,
            identity: meta.info_hash.clone(),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        // 5. 转换：逐文件分析 xltd，合并全局 bitfield
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_dir = std::env::temp_dir().join(format!("xunlei-import-{}-{}", task_id, unique));
        std::fs::create_dir_all(&tmp_dir)
            .map_err(|e| DaemonError::Engine(format!("创建临时目录失败: {e}")))?;
        let torrent_path = tmp_dir.join("source.torrent");
        let cfg_path = tmp_dir.join("source.xlbt.cfg");
        std::fs::write(&torrent_path, &torrent)
            .map_err(|e| DaemonError::Engine(format!("写临时 torrent 失败: {e}")))?;
        std::fs::write(&cfg_path, &cfg)
            .map_err(|e| DaemonError::Engine(format!("写临时 cfg 失败: {e}")))?;

        // 全局完成位图（初始全 0）
        let mut bitfield = vec![0u8; (meta.pieces_hash.len() + 7) / 8];
        // 审计修复（P0-2）：按真实索引合并全局完成集。原实现把各文件完成数
        // 累加后当全局前缀长度置位 0..N——多文件种子跨文件进度不均衡（迅雷
        // 子文件选择下载）或非顺序完成时，前置未下载 piece 被误标完成 →
        // libtorrent 信任 fastresume → 落盘文件混入全零块（静默损坏）。
        let mut completed_global: std::collections::BTreeSet<usize> =
            std::collections::BTreeSet::new();
        let mut partial_infos: Vec<xunlei_convert::PartialPieceInfo> = Vec::new();

        let mut converter = FastresumeConverter::new();
        for (file_idx, file) in files.iter().enumerate() {
            let xltd_path = tmp_dir.join(format!("source.{}.bt.xltd", file_idx));
            std::fs::write(&xltd_path, &xltds[file_idx])
                .map_err(|e| DaemonError::Engine(format!("写临时 xltd[{}] 失败: {e}", file_idx)))?;

            // 该文件对应的 pieces 子集（局部索引从 0 起，xltd 是文件镜像）
            let file_pieces =
                &meta.pieces_hash[file.piece_offset..file.piece_offset + file.piece_count];

            let report = converter.analyze(
                &torrent_path,
                &cfg_path,
                &xltd_path,
                meta.piece_length,
                file_pieces,
                0, // file_offset：xltd 是文件镜像，局部偏移固定 0
                file.size,
            )?;

            // 把局部 partial piece 索引映射回全局索引
            for &(local_idx, nonzero, total) in &report.xltd.partial_details {
                let global_idx = file.piece_offset + local_idx;
                partial_infos.push(xunlei_convert::PartialPieceInfo {
                    index: global_idx,
                    nonzero_bytes: nonzero,
                    total_bytes: total,
                });
            }
            // 局部完成索引映射回全局索引（file.piece_offset 为该文件首 piece）
            for &local_idx in &report.completed_piece_indices {
                completed_global.insert(file.piece_offset + local_idx);
            }
        }

        // 用 lenient 策略构建全局 bitfield（按真实索引 + partial 阈值合并）
        let completed_vec: Vec<usize> = completed_global.into_iter().collect();
        bitfield = build_bitfield_from_indices(
            meta.pieces_hash.len(),
            &completed_vec,
            &partial_infos,
            0.5,
        );

        // fastresume file_sizes：[[size, pad], ...]，pad = piece 边界填充
        let file_sizes: Vec<[u64; 2]> = files
            .iter()
            .map(|f| {
                let plen = meta.piece_length as u64;
                let pad = (plen - (f.size % plen)) % plen;
                [f.size, pad]
            })
            .collect();

        let fr = converter.build_fastresume(
            &meta.info_hash,
            &bitfield,
            &meta.name,
            dest_root.to_str().unwrap_or("./"),
            &file_sizes,
        )?;
        let fastresume_bytes = xunlei_convert::fastresume::bencode_fastresume(&fr)
            .map_err(|e| DaemonError::Engine(format!("fastresume bencode 失败: {e}")))?;

        // 清理临时文件（best-effort）
        let _ = std::fs::remove_dir_all(&tmp_dir);

        // 6. 通过 btcore 导入（能力询问：BT 扩展面）
        let engine = self.engine_for(EngineKind::Bt)?;
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::Engine("BT 引擎未提供扩展面（add_xunlei_resume）".into())
        })?;
        let engine_tid = bt
            .add_xunlei_resume(fastresume_bytes)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;

        // 7. 创建任务记录
        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::TorrentFile(torrent),
            // 审计修复（batch10）：补齐 ContentIdentity 新增的 md5/sha1 字段——
            // xunlei-import 路径自 E25 扩展后未跟上（该 feature 组合编译失败，
            // 上游预存问题）；导入场景无主源摘要（bitfield 进 fastresume）。
            identity: ContentIdentity::SingleFile {
                size: total_size,
                etag: None,
                sha256: None,
                md5: None,
                sha1: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: files
                .iter()
                .map(|f| TaskFile {
                    rel_path: f.path.clone(),
                    size: f.size,
                    done: 0,
                    state: FileState::Pending,
                    source_urls: vec![],
                    identity: None,
                    etag: None,
                    engine: EngineKind::Bt,
                })
                .collect(),
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: Some(meta.name.clone()),
                added_at_unix: 0,
                added_at_ms: now_unix_ms(),
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };
        let mut rec = TaskRecord {
            seeding_since: None,
            task,
            engine_tid: Some(engine_tid.clone()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("xunlei-import", None);
        // batch3-P1：记录态同步 Downloading（与前两处 BT add 同口径）
        rec.task.state = TaskState::Downloading(EngineKind::Bt);

        // 8. peer 注入（best-effort）：把 cfg 里的 bt:// 地址注入引擎
        if let Ok(cfg_obj) = XlbtCfg::parse(&cfg) {
            let engine = self.engine_for(EngineKind::Bt)?;
            if let Some(bt) = engine.bt() {
                for peer_str in cfg_obj.peers {
                    if let Some((ip, port)) = parse_bt_peer(&peer_str) {
                        let addr = format!("{}:{}", ip, port);
                        if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
                            let _ = bt.add_peer(&engine_tid, addr).await;
                        }
                    }
                }
            }
        }

        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Bt),
        });
        Ok(task_id)
    }

    /// 添加 HTTP 任务：canonical 查重 → HttpEngine.add → TaskCreated 事件。
    pub async fn add_http_task(
        &self,
        url: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_http_task_opts(url, dest_root, AddHttpOpts::default())
            .await
    }

    /// 创建 HTTP 任务（E6 opts 收口）：sequential/proxy（E5）+ headers/auth/
    /// sha256/backup_url+backup_md5/name（E6 新暴露）。入参校验（E6 validate +
    /// E5 代理构建试水）在探测/建任务之前——远端不可达 ≠ 入参非法，分开定性。
    pub async fn add_http_task_opts(
        &self,
        url: String,
        dest_root: Option<String>,
        opts: AddHttpOpts,
    ) -> Result<TaskId, DaemonError> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(DaemonError::InvalidSource(url));
        }
        opts.validate().map_err(DaemonError::InvalidSource)?;
        let AddHttpOpts {
            sequential,
            proxy,
            headers,
            basic_auth,
            sha256,
            sha1,
            md5,
            backup_url,
            backup_md5,
            name,
            conflict,
            start_at_unix,
            auto_retry,
        } = opts;
        // E5：任务级代理 URL 校验（构建一次 client 试水，成功即合法）。
        if let Some(p) = &proxy {
            if p.is_empty() {
                return Err(DaemonError::InvalidSource("proxy 不能为空字符串".into()));
            }
            smart_dl_httpdl::build_proxied_client(p)
                .map_err(|e| DaemonError::InvalidSource(format!("proxy 非法 {p:?}: {e}")))?;
        }
        // 校验和归一（引擎端摘要为小写 hex；trim 防复制粘贴带空白）
        let sha256 = sha256.map(|s| normalize_digest(&s));
        let sha1 = sha1.map(|s| normalize_digest(&s));
        let md5 = md5.map(|s| normalize_digest(&s));
        let backup_md5 = backup_md5.map(|s| normalize_digest(&s));
        // B10：目标目录预检（创建/可写）；HTTP 大小在响应头才知 → 空间预检跳过
        // dest 未指定 → 默认落盘目录（serve 配置 dest_root；未注入时为 daemon cwd）
        // batch5-P2：与其余 5 处调用点同构——non_empty_dest 包 dest_root 本身，
        // Some("") 时回落默认根（原包在 or 之后，默认根本身为空串时会落 cwd）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root =
            ensure_dest_root(non_empty_dest(dest_root).or(Some(def)), &self.dest_roots())?;
        let canonical = CanonicalId {
            kind: CanonicalKind::Http,
            identity: canonical_http_url(&url), // D34：剥 token 参数后的 canonical 身份
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        // E21 文件冲突策略：仅显式名任务可预判目标路径（派生名任务最终名在
        // 引擎侧 CD 才确定，v1 保持引擎默认覆盖）。`.part` 存在不属冲突
        //（那是续传现场），只看最终落盘名。
        let mut skip_download = false;
        let name = match (name, conflict) {
            (Some(n), Some(ConflictPolicy::Rename)) => {
                if dest_root.join(&n).exists() {
                    let bumped = Self::bump_conflict_name(&dest_root, &n).ok_or_else(|| {
                        DaemonError::InvalidSource("改名冲突：连续 1000 个候选名均被占用".into())
                    })?;
                    tracing::info!("冲突策略 rename: {n:?} → {bumped:?}");
                    Some(bumped)
                } else {
                    Some(n)
                }
            }
            (Some(n), Some(ConflictPolicy::Skip)) if dest_root.join(&n).exists() => {
                skip_download = true;
                Some(n)
            }
            (n, _) => n, // overwrite（默认）或目标不存在：原样
        };

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Http {
                url: url.clone(),
                headers,
                auth: basic_auth.map(|(u, p)| Auth::Basic(u, p)),
                backup_url,
                proxy: proxy.clone(),
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256,
                sha1,
                md5,
                backup_md5,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: RetryState {
                retries: 0,
                max_retries: auto_retry,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name,
                added_at_unix: 0,
                added_at_ms: now_unix_ms(),
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };

        // E21 skip：目标文件已在 → 不入引擎，任务直接落 Completed
        //（既有文件保持原样；完成事件/Webhook 照常——publish_task_completed
        // 一并写 finished_at）
        if skip_download {
            let mut rec = TaskRecord {
                seeding_since: None,
                task,
                engine_tid: None,
                engine_kind: EngineKind::Http,
                engine_status: None,
                events: vec![],
            };
            rec.push_event("add", Some("conflict_skip".into()));
            rec.task.state = TaskState::Completed;
            rec.task.identity = ContentIdentity::SingleFile {
                size: rec
                    .task
                    .metadata
                    .name
                    .as_deref()
                    .and_then(|n| dest_root.join(n).metadata().ok())
                    .map(|m| m.len())
                    .unwrap_or(0),
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            };
            rec.task.metadata.finished_at_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.tasks.lock().insert(task_id.clone(), rec);
            self.autosave();
            self.hub.publish(SchedulerEvent::TaskCreated {
                task_id: task_id.clone(),
            });
            self.publish_task_completed(&task_id);
            return Ok(task_id);
        }
        // E23 定时启动：start_at 未来 → 延迟入引擎（记录 Queued + 无句柄），
        // 到点由调度循环接入。置于 conflict_skip 之后：文件已在即完成，
        // 调度无意义（两开关同时给出时 skip 优先）。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Http));
        }
        // S1-b 队列门控（batch7 预留槽位）：配额满 → 落排队记录；有槽位 →
        // 同步预留占位（关闭 add 在途窗口超卖），engine.add 成功 attach /
        // 失败回滚。
        let engine = self.engine_for(EngineKind::Http)?;
        if !self.gate_or_enqueue(&task, EngineKind::Http) {
            return Ok(task_id);
        }
        let engine_tid = match engine.add(&task).await {
            Ok(tid) => tid,
            Err(e) => {
                self.rollback_slot_reservation(&task_id);
                return Err(DaemonError::Engine(e.to_string()));
            }
        };
        self.attach_reserved_handle(&task_id, engine_tid, EngineKind::Http, &task.state)
            .await;
        Ok(task_id)
    }

    /// 添加 FTP 任务（feature `ftp`）：校验 `ftp://` 前缀 → ensure_dest_root →
    /// `parse_ftp_auth` 提取 user/pass → 归一化 URL 作 canonical 查重 → 路由 `EngineKind::Ftp`
    /// 引擎 → add → TaskCreated/StateChanged 事件与持久化（完全仿照 add_http_task）。
    ///
    /// 目录任务（url 以 `/` 结尾）：引擎 `add` 时已同步 LIST 出文件清单，此处做【有限次数的
    /// files 同步】——轮询 `engine.status(tid)` 数次直到 `files` 非空，按 TaskFile 结构映射写入
    /// `task.files`；始终为空（目录瞬时无文件/解析延迟）则静默跳过，文件级进度后续经既有轮询
    /// 链路从 EngineStatus 透出，不做强制阻塞。
    #[cfg(feature = "ftp")]
    pub async fn add_ftp_task(
        &self,
        url: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_ftp_task_opts(url, dest_root, None, 0).await
    }

    /// 定时变体（E23）：`start_at_unix` Some(未来) = 延迟入引擎，到点由
    /// 调度循环激活。
    #[cfg(feature = "ftp")]
    pub async fn add_ftp_task_opts(
        &self,
        url: String,
        dest_root: Option<String>,
        start_at_unix: Option<u64>,
        auto_retry: u32,
    ) -> Result<TaskId, DaemonError> {
        if !url.starts_with("ftp://") && !url.starts_with("ftps://") {
            return Err(DaemonError::InvalidSource(url));
        }
        // B10：目标目录预检；目录总大小需 LIST 才可知 → 空间预检跳过（同 HTTP 逻辑）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root =
            ensure_dest_root(non_empty_dest(dest_root).or(Some(def)), &self.dest_roots())?;
        let (user, pass) = smart_dl_core::source_parse::ftp::parse_ftp_auth(&url);
        // D34 复用 canonical 归一化（url 无 query 时基本原样）：FTP 身份键 = 归一化 URL
        let canonical = CanonicalId {
            kind: CanonicalKind::Ftp,
            identity: canonical_http_url(&url),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        let is_dir = url.ends_with('/');
        // 单文件任务：落盘名取 URL 最后一段（引擎 `add` 用作 dest 相对文件名）；目录任务由引擎自理
        let name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Ftp {
                url: url.clone(),
                user,
                pass,
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: RetryState {
                retries: 0,
                max_retries: auto_retry,
            },
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: if is_dir { None } else { name },
                added_at_unix: 0,
                added_at_ms: now_unix_ms(),
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Ftp));
        }
        // S1-b 队列门控（batch7 预留槽位，同 HTTP 口径）：engine.add 成功
        // attach / 失败回滚；目录 files 同步改为就位后直接改写占位记录。
        // batch8（P2）：ftp:// 明文传输显式告警（凭据与数据均不加密）；
        // 敏感内容建议 ftps://（RFC 4217 显式 TLS，全链强制校验）。
        if let DownloadSource::Ftp { url, .. } = &task.source {
            if url.starts_with("ftp://") {
                tracing::warn!(
                    "FTP 任务 {task_id} 使用明文 ftp://（凭据与数据均不加密传输）；如源站支持建议改用 ftps://"
                );
            }
        }
        let engine = self.engine_for(EngineKind::Ftp)?;
        if !self.gate_or_enqueue(&task, EngineKind::Ftp) {
            return Ok(task_id);
        }
        let engine_tid = match engine.add(&task).await {
            Ok(tid) => tid,
            Err(e) => {
                self.rollback_slot_reservation(&task_id);
                return Err(DaemonError::Engine(e.to_string()));
            }
        };

        // 目录任务：有限次 files 同步（FtpEngine::add 已同步 LIST，首轮通常即可命中）
        if is_dir {
            for _ in 0..8 {
                if let Ok(st) = engine.status(&engine_tid).await {
                    if !st.files.is_empty() {
                        let files = st
                            .files
                            .into_iter()
                            .map(|f| TaskFile {
                                rel_path: f.rel_path,
                                size: f.size,
                                done: f.done,
                                state: FileState::Active,
                                source_urls: vec![url.clone()],
                                identity: None,
                                etag: None,
                                engine: EngineKind::Ftp,
                            })
                            .collect();
                        if let Some(rec) = self.tasks.lock().get_mut(&task_id) {
                            rec.task.files = files;
                        }
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            }
        }

        self.attach_reserved_handle(&task_id, engine_tid, EngineKind::Ftp, &task.state)
            .await;
        Ok(task_id)
    }

    /// 添加 SFTP 任务（feature `sftp`，C-S1）：校验 `sftp://` 前缀 + user 必填
    /// （SSH 无匿名惯例）→ ensure_dest_root → `parse_sftp_auth` 提取 user/pass →
    /// 归一化 URL 作 canonical 查重（CanonicalKind::Sftp，与 ftp:// 键不相撞）→
    /// 路由 `EngineKind::Sftp` 引擎 → add → TaskCreated/StateChanged 事件与
    /// 持久化（完全仿照 add_ftp_task；v1 仅单文件——目录任务路由层报错）。
    #[cfg(feature = "sftp")]
    pub async fn add_sftp_task(
        &self,
        url: String,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_sftp_task_opts(url, dest_root, None).await
    }

    /// 定时变体（E23）：`start_at_unix` Some(未来) = 延迟入引擎，到点由
    /// 调度循环激活。
    #[cfg(feature = "sftp")]
    pub async fn add_sftp_task_opts(
        &self,
        url: String,
        dest_root: Option<String>,
        start_at_unix: Option<u64>,
    ) -> Result<TaskId, DaemonError> {
        if !url.starts_with("sftp://") {
            return Err(DaemonError::InvalidSource(url));
        }
        let (user, _pass) = smart_dl_core::source_parse::sftp::parse_sftp_auth(&url);
        if user.is_empty() {
            return Err(DaemonError::InvalidSource(format!(
                "sftp 需显式 user（SSH 无匿名惯例）: {url}"
            )));
        }
        // B10：目标目录预检（单文件 size 由引擎 add 时 stat，预检略过同 FTP 单文件）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root =
            ensure_dest_root(non_empty_dest(dest_root).or(Some(def)), &self.dest_roots())?;
        // canonical 归一化同 FTP 键构方式；CanonicalKind::Sftp 与 ftp:// 键不相撞
        let canonical = CanonicalId {
            kind: CanonicalKind::Sftp,
            identity: canonical_http_url(&url),
            validator: None,
            token_sensitive: false,
        };
        let task_id = format!("t{}", self.next_id.fetch_add(1, Ordering::SeqCst));

        // 查重（canonical 一致 → DuplicateRejected）
        {
            let tasks = self.tasks.lock();
            for (existing, rec) in tasks.iter() {
                if rec.task.canonical_id == canonical {
                    self.hub.publish(SchedulerEvent::DuplicateRejected {
                        task_id: task_id.clone(),
                        existing: existing.clone(),
                    });
                    return Err(DaemonError::Duplicate(existing.clone()));
                }
            }
        }

        // 单文件：落盘名取 URL 最后一段（引擎 `add` 用作 dest 相对文件名）
        let name = url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::Sftp {
                url: url.clone(),
                user,
                pass: _pass,
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: dest_root.clone(),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name,
                added_at_unix: 0,
                added_at_ms: now_unix_ms(),
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Sftp));
        }
        // S1-b 队列门控（batch7 预留槽位，同 FTP 口径；Sftp 与 Ftp 共用 ftp 桶
        // 配额，见 slot_index 注释）：engine.add 成功 attach / 失败回滚。
        let engine = self.engine_for(EngineKind::Sftp)?;
        if !self.gate_or_enqueue(&task, EngineKind::Sftp) {
            return Ok(task_id);
        }
        let engine_tid = match engine.add(&task).await {
            Ok(tid) => tid,
            Err(e) => {
                self.rollback_slot_reservation(&task_id);
                return Err(DaemonError::Engine(e.to_string()));
            }
        };
        self.attach_reserved_handle(&task_id, engine_tid, EngineKind::Sftp, &task.state)
            .await;
        Ok(task_id)
    }

    /// Metalink 引导 XML 拉取（B1，`.meta4`/`.metalink` URL 入口）：bootstrap
    /// client（serve 注入 = 引擎全局 client 克隆，代理/cookie/超时同源）；
    /// 未注入时按需新建裸 client 兜底（部分测试/嵌入式调用）。2xx 外状态
    /// → InvalidSource；响应体不裁剪（大小护栏交给后续 parse 的 XML 错误
    /// 上限——quick-xml 流式解析，超大输入天然可处理）。
    pub(crate) async fn fetch_metalink_xml(&self, url: &str) -> Result<String, DaemonError> {
        let client = match &self.bootstrap_client {
            Some(c) => c.clone(),
            None => reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .read_timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|e| {
                    DaemonError::InvalidSource(format!("bootstrap client 构建失败: {e}"))
                })?,
        };
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| DaemonError::InvalidSource(format!("metalink 引导拉取失败: {e}")))?;
        let resp = resp
            .error_for_status()
            .map_err(|e| DaemonError::InvalidSource(format!("metalink 引导拉取失败: {e}")))?;
        // batch3-P2：限长读取（16MB 封顶）——旧 resp.text() 不限量，已认证
        // 客户端指向高带宽/无限流响应可在 read_timeout 窗口内注入数 GB 字符串
        // 打爆 daemon 内存（OOM）。
        const METALINK_MAX: u64 = 16 * 1024 * 1024;
        if let Some(len) = resp.content_length() {
            if len > METALINK_MAX {
                return Err(DaemonError::InvalidSource(format!(
                    "metalink 引导响应过大: {len} 字节（上限 {METALINK_MAX}）"
                )));
            }
        }
        let mut xml: Vec<u8> = Vec::new();
        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| DaemonError::InvalidSource(format!("metalink 读取失败: {e}")))?
        {
            if xml.len() as u64 + chunk.len() as u64 > METALINK_MAX {
                return Err(DaemonError::InvalidSource(
                    "metalink 引导响应超过 16MB 上限".into(),
                ));
            }
            xml.extend_from_slice(&chunk);
        }
        String::from_utf8(xml)
            .map_err(|e| DaemonError::InvalidSource(format!("metalink 编码非法: {e}")))
    }

    /// 创建 Metalink4 任务集（B1）：解析 RFC 5854 XML → 逐 `<file>` 展开为
    /// HTTP 任务，全链复用既有能力——主 URL = priority 最高（数值最小）者，
    /// 次高者作 backup_url（E2/E3 mirror failover 直通）；内建哈希择强
    /// （sha256 > sha1 > md5）直通 E3 校验链；文件名取 name 属性末段
    /// （`MetalinkFile::display_name`，V3 终审）。
    ///
    /// 语义边界：
    /// - 任务级字段（sequential/proxy/headers/basic_auth/conflict/start_at/
    ///   auto_retry）逐文件继承 opts；哈希/名字/URL 每文件各异由 XML 覆写。
    /// - 仅展开 http(s) URL（`http_sorted_urls`）；ftp:// 等混合协议 URL
    ///   过滤不参与。文件无可用 URL → InvalidSource（含文件名定位）。
    /// - 展开失败即中止（已创建任务保留，不回滚——引擎句柄回收代价大，
    ///   错误信息带已创建计数）；metalink 来自同一发布者，部分失败通常
    ///   意味着整份描述文件不可用。
    /// - `<size>` 空间预检：add_http_task_opts 的 HTTP 大小在响应头才知
    ///   （B10 同口径），XML size 未直接参与预检，仅随探测链由引擎校验。
    pub(crate) async fn add_metalink_tasks(
        &self,
        xml: &str,
        dest_root: Option<String>,
        opts: AddHttpOpts,
    ) -> Result<Vec<TaskId>, DaemonError> {
        let files = crate::metalink::parse_metalink4(xml).map_err(DaemonError::InvalidSource)?;
        let mut ids: Vec<TaskId> = Vec::with_capacity(files.len());
        for f in &files {
            let name = f.display_name().map_err(DaemonError::InvalidSource)?;
            let urls = f.http_sorted_urls();
            let primary = urls.first().map(|u| u.url.clone()).ok_or_else(|| {
                DaemonError::InvalidSource(format!(
                    "metalink <file name={:?}> 无可用 http(s) <url>",
                    f.name
                ))
            })?;
            let backup_url = urls.get(1).map(|u| u.url.clone());
            // 哈希择强填单槽位（AddHttpOpts 主源互斥）；backup_md5 恒不设
            // ——metalink 哈希是内容级（主备同内容），备用源校验由引擎既有
            // 身份切换逻辑接管。
            let (sha256, sha1, md5) = match f.best_hash() {
                Some(("sha256", h)) => (Some(h.to_string()), None, None),
                Some(("sha1", h)) => (None, Some(h.to_string()), None),
                Some(("md5", h)) => (None, None, Some(h.to_string())),
                // best_hash 契约仅返回以上三档；wildcard 为类型完备性兜底
                _ => (None, None, None),
            };
            let id = self
                .add_http_task_opts(
                    primary,
                    dest_root.clone(),
                    AddHttpOpts {
                        backup_url,
                        sha256,
                        sha1,
                        md5,
                        name: Some(name),
                        backup_md5: None,
                        ..opts.clone()
                    },
                )
                .await
                .map_err(|e| match e {
                    DaemonError::Duplicate(existing) => DaemonError::InvalidSource(format!(
                        "metalink 展开中断：<file name={:?}> 与已有任务 {existing} 重复",
                        f.name
                    )),
                    other => DaemonError::InvalidSource(format!(
                        "metalink 展开中断（已创建 {} 个任务，不回滚）: {other}",
                        ids.len()
                    )),
                })?;
            ids.push(id);
        }
        Ok(ids)
    }
}
/// 解析 `bt://ip:port` 为 `(ip, port)`。
#[cfg(feature = "xunlei-import")]
fn parse_bt_peer(s: &str) -> Option<(String, u16)> {
    let s = s.strip_prefix("bt://")?;
    let mut parts = s.rsplitn(2, ':');
    let port_str = parts.next()?;
    let ip = parts.next()?;
    let port = port_str.parse::<u16>().ok()?;
    Some((ip.to_string(), port))
}

/// B10（§12 D36）：dest_root 预检——缺失目录自动创建 + 可写探测（探针文件）。
/// 空间充足性由 `precheck_space` 在总大小已知时另行检查。
///
/// 安全修复（V2，CWE-22 变体）：`allowed_roots` 白名单——dest 规范化后必须落在
/// 某个白名单根内（拒 symlink 逃逸）；原始输入含 `..` 分量直接拒绝。
/// `allowed_roots` 传空切片 = 不校验（仅测试/serve 初始化自身使用；
/// 生产路径必须传非空，DaemonState 内部兜底 default_dest_root）。
/// 空串 dest 归一为 None（回落默认根）。审查修复：`dest:""` 传入为 Some("")
/// 绕过调用方 `or(Some(def))` 回落，ensure_dest_root 内空路径必然白名单越界。
pub fn non_empty_dest(dest: Option<String>) -> Option<String> {
    dest.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

pub fn ensure_dest_root(
    dest: Option<String>,
    allowed_roots: &[PathBuf],
) -> Result<PathBuf, DaemonError> {
    let raw = dest.unwrap_or_else(|| ".".to_string());
    let p = PathBuf::from(&raw);
    // 1) 原始输入拒绝 `..`（canonicalize 前快速拒绝，语义清晰）
    for comp in p.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(DaemonError::InvalidSource(format!(
                "dest 含 `..` 分量已拒绝: {raw}"
            )));
        }
    }
    // 2) 白名单校验：canonicalize 后比对前缀（同时拦截 symlink 指向白名单外）。
    //    审查修复（P2）：白名单判定必须先于 create_dir_all——原顺序在白名单外
    //    预创建任意目录后才拒绝（已认证调用方可借此预建目录结构）。
    if !allowed_roots.is_empty() {
        // dest 未创建时 canonicalize 会失败 → 以 dest_root 视角校验父链：
        // 逐级向上找第一个已存在祖先做前缀比对，再统一建目录。
        let mut probe = p.clone();
        let mut anchor = None;
        loop {
            if probe.exists() {
                anchor = Some(probe.clone());
                break;
            }
            match probe.parent() {
                Some(parent) if probe != parent => probe = parent.to_path_buf(),
                _ => break,
            }
        }
        let inside = match anchor {
            Some(a) => a
                .canonicalize()
                .ok()
                .map(|ca| {
                    // p 与 anchor 的相对尾链无 `..`（第 1 步已拒），拼接比对
                    let tail = p.strip_prefix(&a).unwrap_or(Path::new(""));
                    ca.join(tail)
                })
                .map(|full| {
                    allowed_roots.iter().any(|r| {
                        let _ = fs::create_dir_all(r);
                        match r.canonicalize() {
                            Ok(cr) => full.starts_with(&cr),
                            Err(_) => false,
                        }
                    })
                })
                .unwrap_or(false),
            None => false,
        };
        if !inside {
            return Err(DaemonError::InvalidSource(format!(
                "dest 越界（不在允许的下载根目录内）: {raw}"
            )));
        }
    }
    fs::create_dir_all(&p)
        .map_err(|e| DaemonError::InvalidSource(format!("目标目录不可创建: {e}")))?;
    // 3) 可写探针：随机后缀防可预测竞态（V10-3）
    let probe = p.join(format!(
        ".write_probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&probe, b"ok")
        .map_err(|e| DaemonError::InvalidSource(format!("目标目录不可写: {e}")))?;
    let _ = fs::remove_file(&probe);
    Ok(p)
}

/// B10：空间预检（总大小已知时调用）——`evaluate_disk` 判定不足 → 拒绝入队。
/// 安全/健壮性修复（V10-2）：磁盘可用空间取不到（fs2 失败）时不再静默放行——
/// 非严格模式（默认）告警日志 + 放行（保留旧行为）；`strict=true` 时拒绝入队，
/// 防止预检被绕过后续盘写满。由配置 `[download] disk_precheck_strict` 控制。
pub fn precheck_space(p: &Path, total: u64, strict: bool) -> Result<(), DaemonError> {
    let Ok(avail) = fs2::free_space(p) else {
        if strict {
            return Err(DaemonError::InvalidSource(format!(
                "磁盘可用空间不可探测且 disk_precheck_strict=true，拒绝入队: {}",
                p.display()
            )));
        }
        tracing::warn!(
            "磁盘可用空间不可探测，空间预检已跳过（可配置 [download] disk_precheck_strict=true 强制拒绝）: {}",
            p.display()
        );
        return Ok(());
    };
    use smart_dl_storage::{evaluate_disk, DiskCheck};
    if let DiskCheck::Insufficient {
        required,
        available,
    } = evaluate_disk(avail, total)
    {
        return Err(DaemonError::InvalidSource(format!(
            "磁盘空间不足: 需要 {} 字节, 可用 {} 字节",
            required, available
        )));
    }
    Ok(())
}

/// D34：canonical URL —— 剥离签名/token 参数后作为去重身份，使同一资源的
/// 带签名链接（token 过期/轮换）仍能识别为同一任务。
/// 黑名单（设计文档 §7 D34）：`token|sig|signature|expires|auth|X-Amz-*|X-Goog-*|X-Tencent-*|X-QiNiu-*`
pub fn canonical_http_url(raw: &str) -> String {
    let Ok(mut u) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let mut kept: Vec<(String, String)> = Vec::new();
    for (k, v) in u.query_pairs() {
        if !is_token_param(&k) {
            kept.push((k.into_owned(), v.into_owned()));
        }
    }
    if kept.is_empty() {
        u.set_query(None);
    } else {
        let qs: Vec<String> = kept.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        u.set_query(Some(&qs.join("&")));
    }
    u.to_string()
}

/// 参数名是否命中 D34 token 黑名单（大小写敏感匹配，前缀通配 X-* 云签名族）。
fn is_token_param(name: &str) -> bool {
    matches!(name, "token" | "sig" | "signature" | "expires" | "auth")
        || name.starts_with("X-Amz-")
        || name.starts_with("X-Goog-")
        || name.starts_with("X-Tencent-")
        || name.starts_with("X-QiNiu-")
}

/// 递归目录拷贝（batch5；std::fs 无 copy_dir_all）：目录逐层创建 + 文件
/// 逐个 copy（保留权限位 via fs::copy）。符号链接按目标内容展开（v1 从简：
/// BT 数据目录内不应有 symlink）。
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod copy_dir_tests {
    use super::*;

    #[test]
    fn copy_dir_recursive_copies_tree() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        fs::create_dir_all(src.join("nested/deep")).unwrap();
        fs::write(src.join("a.bin"), b"AAA").unwrap();
        fs::write(src.join("nested/b.bin"), b"BB").unwrap();
        fs::write(src.join("nested/deep/c.txt"), b"C").unwrap();
        let dst = td.path().join("dst");
        copy_dir_recursive(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("a.bin")).unwrap(), b"AAA");
        assert_eq!(fs::read(dst.join("nested/b.bin")).unwrap(), b"BB");
        assert_eq!(fs::read(dst.join("nested/deep/c.txt")).unwrap(), b"C");
    }
}
