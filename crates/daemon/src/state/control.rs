//! 生命周期控制域：批量操作、暂停/恢复/限速、移除（含删数据）、provider 兜底传输、
//! 完成清理、HTTP/FTP 引擎状态轮询（含 BT 做种限制执行）。

use super::query::engine_state_to_task;
use super::*;
impl DaemonState {
    /// 批量操作（E7）：按入参顺序逐任务执行；重复 id 静默去重（保留首次出现序，
    /// 避免同一任务被 pause 两次产生假失败）；单项失败（NotFound/引擎错误）
    /// 记入该项结果后继续，绝不短路。永远返回 BatchOutcome（HTTP 层恒 200）。
    pub async fn batch(&self, ids: &[String], action: BatchAction) -> BatchOutcome {
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();
        let (mut ok, mut bad) = (0usize, 0usize);
        for id in ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            let r = match action {
                BatchAction::Pause => self.pause(id).await,
                BatchAction::Resume => self.resume(id).await,
                BatchAction::Remove { delete_data } => self.remove_with(id, delete_data).await,
            };
            match r {
                Ok(()) => {
                    ok += 1;
                    results.push(BatchItemResult {
                        id: id.clone(),
                        ok: true,
                        error: None,
                    });
                }
                Err(e) => {
                    bad += 1;
                    results.push(BatchItemResult {
                        id: id.clone(),
                        ok: false,
                        error: Some(e.to_string()),
                    });
                }
            }
        }
        BatchOutcome {
            results,
            succeeded: ok,
            failed: bad,
        }
    }

    /// 按条件批量操作（E19）：`ListQuery` 选择器（states/engines/tags/search
    /// 复用列表过滤口径，无分页 → 全量命中集）解析命中任务后复用 `batch`。
    ///
    /// - **只开放非破坏性动作**（pause/resume）：按过滤条件选择后 remove 属
    ///   危险操作（误配过滤 = 批量误删），批量删除仍走显式 id 路径（E7）
    /// - 命中集上限 `batch_select` 内 1000（防御性；显式 id 批量另有 100 上限
    ///   语义，选择器面向"一键重试全部失败"这类可能超 100 的运维场景）
    /// - 命中集为空 → 空 BatchOutcome（幂等便利，不报错）
    pub async fn batch_select(
        &self,
        q: &ListQuery,
        action: BatchAction,
    ) -> Result<BatchOutcome, DaemonError> {
        if matches!(action, BatchAction::Remove { .. }) {
            return Err(DaemonError::InvalidSource(
                "按条件选择不支持 remove（批量删除请走显式 ids 路径）".into(),
            ));
        }
        // 无分页取全量命中（limit=None offset=0），按创建序稳定执行
        let (rows, _) = self.list_filtered(&ListQuery {
            states: q.states.clone(),
            engines: q.engines.clone(),
            limit: None,
            offset: 0,
            search: q.search.clone(),
            tags: q.tags.clone(),
        });
        const SELECT_CAP: usize = 1000;
        if rows.len() > SELECT_CAP {
            return Err(DaemonError::InvalidSource(format!(
                "条件命中数量超上限（{} > {SELECT_CAP}），请收窄选择条件",
                rows.len()
            )));
        }
        let ids: Vec<String> = rows.into_iter().map(|r| r.task_id).collect();
        Ok(self.batch(&ids, action).await)
    }

    pub async fn pause(&self, id: &str) -> Result<(), DaemonError> {
        // E23：调度等待中任务（engine_tid 空 + Queued）无引擎句柄可暂停——
        // 语义 = 取消自动启动（记录置 Paused；start_at 保留供展示，激活器
        // 只认 Queued 不会误触发）。resume 回该任务 = 立即激活。
        // 其余无句柄任务（E21 skip Completed / restore add 失败 Failed）→
        // 落到引擎侧逻辑按 404 口径拒绝，与现行为一致。
        // 结构约束：锁作用域内不得出现 await（guard 非 Send，会污染整个
        // handler future 的 Send 判定）——锁内纯决策，await 全部在锁外。
        let decision = {
            let mut tasks = self.tasks.lock();
            match tasks.get_mut(id) {
                Some(rec) if rec.engine_tid.is_some() => Some(true), // 已被激活 → 引擎侧暂停
                Some(rec) => {
                    if rec.task.state != TaskState::Queued {
                        return Err(DaemonError::NotFound(id.to_string()));
                    }
                    rec.push_event("pause", Some("scheduled".into()));
                    rec.task.state = TaskState::Paused;
                    if let Some(es) = rec.engine_status.as_mut() {
                        es.down_rate = 0;
                        es.up_rate = 0;
                    }
                    Some(false) // 调度中 → 记录级暂停已完成
                }
                None => return Err(DaemonError::NotFound(id.to_string())),
            }
        };
        match decision {
            Some(true) => self.pause_engine_task(id).await,
            _ => {
                self.autosave();
                self.hub.publish(SchedulerEvent::StateChanged {
                    task_id: id.to_string(),
                    from: TaskState::Queued,
                    to: TaskState::Paused,
                });
                Ok(())
            }
        }
    }

    /// 引擎侧暂停（原 pause 主体，E23 拆出：调度中任务走记录级暂停分支）。
    pub(super) async fn pause_engine_task(&self, id: &str) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        // batch6-P2：广播 from = 记录态实际值（旧实现硬编码 Downloading，
        // 执法暂停发生在 Seeding 态时事件失真）
        let from = rec.task.state.clone();
        self.engine_for(rec.engine_kind)?
            .pause(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("pause", None);
            rec.task.state = TaskState::Paused; // 记录缓存同步（alert 流不迁移 pause）
                                                // E11：暂停即清零缓存速率——轮询器不再光顾暂停任务，
                                                // 不清则 /stats 恒把最后窗口速率计入聚合（恢复后下一轮刷新）。
            if let Some(es) = rec.engine_status.as_mut() {
                es.down_rate = 0;
                es.up_rate = 0;
            }
            rec.seeding_since = None; // Task 39：离开做种态清计时
        }
        // 暂停意图必须立刻持久化（P4 G5）：否则重启后暂停任务被当作运行任务恢复
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from,
            to: TaskState::Paused,
        });
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<(), DaemonError> {
        // E23：未接入引擎的任务（调度等待 Queued / 调度等待期被暂停 Paused）
        // → resume = 立即激活（消费定时，直接开始）。激活后记录态置
        // Downloading（对齐引擎侧 resume 语义；HTTP add 自启下载循环，BT 内
        // 核由 add 后正常下载链路接管）。
        // E32：终态 Failed（无句柄——激活失败/E30 激活失败路径）→ resume =
        // 手动重试：重新接入引擎。auto_retry 预算【不重置】——耗尽后手动
        // 重试仅再给一次机会，任务再败时 fail_or_schedule_retry 依既有计数
        // 直接终态（防预算白给循环）；有句柄 Failed 走下方引擎侧 resume
        // （httpdl epoch 重入 + 段账本续传，BT handle 恢复）语义不变。
        // 其余无句柄终态（E21 skip Completed / Stopped）→ 404 口径拒绝。
        let pending = {
            let tasks = self.tasks.lock();
            match tasks.get(id) {
                Some(rec) => {
                    if rec.engine_tid.is_none() {
                        Some((rec.task.clone(), rec.engine_kind))
                    } else {
                        None
                    }
                }
                None => return Err(DaemonError::NotFound(id.to_string())),
            }
        };
        if let Some((task, kind)) = pending {
            // batch7-P1：add 在途占位（引擎正在接入）→ resume 无需也无法重复
            // 激活，幂等成功返回（否则 activate_one 会二次 engine.add 产生
            // 双引擎任务）。
            if self.slot_reservations.lock().contains(id) {
                return Ok(());
            }
            if !matches!(
                task.state,
                TaskState::Queued | TaskState::Paused | TaskState::Failed
            ) {
                return Err(DaemonError::NotFound(id.to_string()));
            }
            let from = task.state.clone();
            if !self.activate_one(id, task, kind).await {
                return Err(DaemonError::Engine(format!(
                    "任务 {id} 调度激活失败（任务已标 Failed，详情见任务事件）"
                )));
            }
            {
                let mut tasks = self.tasks.lock();
                if let Some(rec) = tasks.get_mut(id) {
                    if from == TaskState::Failed {
                        rec.push_event("retry", Some("手动重试（resume）".into()));
                    } else {
                        rec.push_event("resume", None);
                    }
                    rec.task.state = TaskState::Downloading(kind);
                }
            }
            self.autosave();
            self.hub.publish(SchedulerEvent::StateChanged {
                task_id: id.to_string(),
                from,
                to: TaskState::Downloading(kind),
            });
            return Ok(());
        }
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        self.engine_for(rec.engine_kind)?
            .resume(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // batch6-P1：做种态任务 resume 后记录态/计时收敛引擎实况——内核对
        // 已完成种子 pause→resume 不再产生 finished alert，旧实现恒写
        // Downloading → UI 恒显"下载中 100%"、seeding_since 永不重登记
        // （max_seeding_time 对该任务从此失效，与执法注释承诺
        // "resume 后再达标再次触发"相悖）。resume 后查询引擎实况：
        // Seeding → 记录态 Seeding + 计时重起。
        let engine = self.engine_for(rec.engine_kind)?;
        let engine_seeding = if rec.engine_kind == EngineKind::Bt {
            matches!(
                engine.status(&tid).await.map(|s| s.state),
                Ok(smart_dl_core::types::EngineState::Seeding)
            )
        } else {
            false
        };
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("resume", None);
            if engine_seeding {
                rec.task.state = TaskState::Seeding;
                rec.seeding_since = Some(std::time::Instant::now());
            } else {
                rec.task.state = TaskState::Downloading(rec.engine_kind);
            }
        }
        // 恢复态同步持久化（P4 G5：与 pause 对称）
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: rec.task.state.clone(),
            to: if engine_seeding {
                TaskState::Seeding
            } else {
                TaskState::Downloading(rec.engine_kind)
            },
        });
        Ok(())
    }

    /// 任务级限速（P1 能力增强）。合并口径：请求中 `None` 的方向沿用既有值
    /// （首设即不限）；引擎调用总拿到全量两方向（BT 引擎 None 方向按不限下发，
    /// 避免 lt_set_limits 全量语义把已设方向清零）。
    ///
    /// - `Some(0)` = 该方向不限速；`Some(n)` = 上限 n KiB/s
    /// - HTTP/FTP：仅 down 方向有意义（up → 引擎报错，HTTP 层映射 409/422）
    /// - 合并结果持久化（tasks.json）并在恢复时重放；内存中即时生效
    ///   （HTTP 引擎热调速率；BT 引擎 libtorrent per-torrent limit）
    pub async fn set_task_limits(
        &self,
        id: &str,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<smart_dl_core::task::TaskLimits, DaemonError> {
        let (engine, tid, merged) = {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            // up 方向仅 BT 引擎有意义；其余引擎在此预拒（HTTP 层映射 409，
            // 避免引擎层 Engine 错误被当成服务端 500）
            if up_kb_s.is_some() && rec.engine_kind != EngineKind::Bt {
                return Err(DaemonError::UnsupportedOp(format!(
                    "任务 {id}（{:?}）无上传方向，up_kb_s 仅对 BT 任务有意义",
                    rec.engine_kind
                )));
            }
            let old = rec.task.limits.take().unwrap_or_default();
            let merged = smart_dl_core::task::TaskLimits {
                down_kb_s: down_kb_s.or(old.down_kb_s),
                up_kb_s: up_kb_s.or(old.up_kb_s),
            };
            // 两方向均为空（从未设置且请求未带）→ 维持 None（快照不出噪声字段）
            rec.task.limits = if merged.is_empty() {
                None
            } else {
                Some(merged.clone())
            };
            rec.push_event(
                "limits_changed",
                Some(format!(
                    "down={:?} up={:?}",
                    merged.down_kb_s, merged.up_kb_s
                )),
            );
            let engine = self.engine_for(rec.engine_kind)?;
            (engine, tid, merged)
        };
        engine
            .set_limits(&tid, merged.down_kb_s, merged.up_kb_s)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        self.autosave();
        Ok(merged)
    }

    /// 删除任务（E7 前 semantics：保留已下载数据）。
    pub async fn remove(&self, id: &str) -> Result<(), DaemonError> {
        self.remove_with(id, false).await
    }

    /// 删除任务（Issue #120 P0 修复：先停引擎、后删记录，错误传播）。
    ///
    /// 旧实现三连静默：①先删记录（UI 立即消失，此后失败无迹可寻）→
    /// ②`engine_for` Err 静默跳过（引擎未装配时引擎侧任务继续跑）→
    /// ③`engine.remove` Err 静默吞掉（无日志无事件）→ 恒返回 `Ok`。
    /// 用户侧表现即「删了还在下」：`.part` 持续增长、磁盘不释放、重启后
    /// 记录已删无法恢复但文件残留。
    ///
    /// 新顺序（记录保留到引擎确认停止之后，失败可重试）：
    /// 1. 锁内取记录快照（不删）——失败时任务仍在列表；
    /// 2. `engine_for` 失败 → Err（引擎不可用 ≠ 放弃停止责任）；
    /// 3. `engine.remove` 失败 → Err 传播；**NotFound 例外**：引擎侧已无
    ///    此任务（会话重启/已退役）= 停止目标已达成，放行记录清理。
    ///    HTTP 引擎的 remove 内部先置 pause 旗标（在飞段尽快收尾退出），
    ///    无需额外 pause 调用；
    /// 4. 引擎确认停止后才删记录 + autosave。
    ///
    /// 并发幂等：锁外 await 期间同 id 的并发 remove_with 各自走引擎
    /// remove（引擎侧幂等/NotFound），末尾记录删除无条件执行（二次删
    /// no-op）——与自动清扫（Err 分支跳过）与批量删除（逐项 Err 收集）
    /// 两个既有 `Result` 消费方契约一致。
    pub async fn remove_with(&self, id: &str, delete_data: bool) -> Result<(), DaemonError> {
        enum StopPlan {
            /// 无引擎句柄（provider 兜底完成 / 恢复失败记录）——直接清记录。
            NoEngine,
            /// 停引擎（engine 句柄 + 引擎侧任务 id）。
            Stop {
                engine: std::sync::Arc<dyn DownloadEngine>,
                tid: crate::state::EngineTaskId,
            },
        }
        let plan = {
            let tasks = self.tasks.lock();
            let Some(rec) = tasks.get(id) else {
                return Err(DaemonError::NotFound(id.to_string()));
            };
            match rec.engine_tid.clone() {
                Some(tid) => {
                    let engine = self.engine_for(rec.engine_kind).map_err(|e| {
                        DaemonError::Engine(format!("引擎不可用，无法停止任务 {id}: {e}"))
                    })?;
                    StopPlan::Stop { engine, tid }
                }
                None => StopPlan::NoEngine,
            }
        }; // ← tasks 锁在此释放（引擎 await 不跨锁）
        if let StopPlan::Stop { engine, tid } = plan {
            match engine.remove(&tid, delete_data).await {
                Ok(()) => {}
                Err(smart_dl_core::types::EngineError::NotFound) => {
                    tracing::warn!("任务 {id} 引擎侧已不存在（NotFound），继续清理记录");
                }
                Err(e) => {
                    return Err(DaemonError::Engine(format!("停止引擎任务失败 {id}: {e}")));
                }
            }
        }
        self.tasks.lock().remove(id);
        self.autosave();
        Ok(())
    }

    /// Q-B9 手动兜底（M6 接线）：BT 任务 → 云 Provider → 直链 → HTTP 引擎传输。
    /// 前置（FallbackPolicy 默认冻结）：任务须为 BT 且已暂停；BT 进度 < 50%。
    /// 成功 → 任务置 Completed + 事件广播 + 落盘。
    pub async fn fallback(&self, id: &str) -> Result<FallbackOutcome, DaemonError> {
        // 1. 任务存在性 + 必须是 BT
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::Fallback(format!(
                "仅 BT 任务支持云兜底（{} 为 {:?}）",
                id, rec.engine_kind
            )));
        }
        // 2. 串行策略（默认禁双份占盘）→ 必须先暂停
        if rec.task.state != TaskState::Paused {
            return Err(DaemonError::Fallback(format!(
                "需先暂停 BT 任务 {id}（串行兜底策略：禁 BT/直链双份占盘）"
            )));
        }
        // 3. BT 进度（metadata 未到 → total=0 → 进度 0，允许兜底）；≥50% 拒绝
        let bt_progress = match (&rec.engine_tid, self.engine_for(EngineKind::Bt).ok()) {
            (Some(tid), Some(engine)) => engine
                .status(tid)
                .await
                .ok()
                .map(|s| {
                    if s.total == 0 {
                        0.0
                    } else {
                        s.total_done as f64 / s.total as f64
                    }
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        // 4. 协商器 + 传输 sink → 执行兜底
        if self.providers.is_empty() {
            return Err(DaemonError::Fallback(
                "无可用 provider（未配置或全部不可用）".into(),
            ));
        }
        let coord = FallbackCoordinator::new(
            self.providers.clone(),
            smart_dl_core::ownership::FallbackPolicy::default(),
        );
        let http = self
            .engine_for(EngineKind::Http)
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let sink = FallbackSink { http };
        let outcome = coord
            .begin_fallback(&rec.task, bt_progress, true, &sink)
            .await
            .map_err(map_provider_err)?;
        // 4b. BT 引擎任务退役（直链已替代 BT 传输，keep data）：
        // 快照不再读引擎实时下载态 → 回落到记录态 Completed
        // 审计修复（P1-4）：engine_tid 必须一并清空——残留的 Some(tid) 让
        // pause/resume/set_limits/set_sequential/set_max_connections/peers/
        // trackers/webseeds 继续对已退役句柄操作（EngineError），与记录态
        // Completed 彻底错位；恢复链路也会对不存在引擎任务 resume。
        // （锁纪律：tasks 锁内不得 await——先取句柄快照，remove 在锁外执行。）
        let retired_tid = {
            let tasks = self.tasks.lock();
            tasks.get(id).and_then(|r| r.engine_tid.clone())
        };
        if let Some(tid) = retired_tid {
            if let Ok(bt) = self.engine_for(EngineKind::Bt) {
                let _ = bt.remove(&tid, false).await;
            }
        }
        {
            let mut tasks = self.tasks.lock();
            if let Some(r) = tasks.get_mut(id) {
                r.engine_tid = None;
            }
        }
        // 5. 成功：置 Completed + 事件 + 落盘
        {
            let mut tasks = self.tasks.lock();
            if let Some(r) = tasks.get_mut(id) {
                r.push_event("fallback", Some(format!("provider={}", outcome.provider)));
                r.task.state = TaskState::Completed;
            }
        }
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: TaskState::Downloading(EngineKind::Bt),
            to: TaskState::Completed,
        });
        // E17：完成事件统一出口（广播 + Webhook）
        self.publish_task_completed(id);
        Ok(outcome)
    }

    /// 已完成任务自动清扫（E20）：扫描 Completed 且完成龄期 ≥
    /// `auto_remove_completed_days` 的任务，逐个按 `auto_remove_keep_data`
    /// 处置（默认保留文件）。返回本次清扫的任务 id（测试断言用）。
    /// days=0 禁用；记录无完成时刻（旧档未记）→ 跳过（不猜测龄期）。
    pub async fn sweep_completed_cleanup(&self) -> Vec<String> {
        let (days, keep_data) = {
            let c = self.cleanup.lock();
            (c.auto_remove_completed_days, c.auto_remove_keep_data)
        };
        if days == 0 {
            return Vec::new(); // 禁用
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let deadline = now.saturating_sub(days as u64 * 86_400);
        let due: Vec<String> = {
            let tasks = self.tasks.lock();
            tasks
                .values()
                .filter(|r| {
                    r.task.state == TaskState::Completed
                        && r.task.metadata.finished_at_unix > 0
                        && r.task.metadata.finished_at_unix <= deadline
                })
                .map(|r| r.task.id.clone())
                .collect()
        };
        let mut swept = Vec::new();
        for id in due {
            // remove_with 处置记录 + 引擎退役 + 落盘；保持清扫尽力而为不短路
            match self.remove_with(&id, !keep_data).await {
                Ok(()) => {
                    tracing::info!("自动清扫已完成任务: {id}（保留数据={keep_data}）");
                    swept.push(id);
                }
                Err(e) => tracing::warn!("自动清扫 {id} 失败（跳过）: {e}"),
            }
        }
        if !swept.is_empty() {
            tracing::info!("本次自动清扫 {} 个已完成任务", swept.len());
        }
        swept
    }

    /// 引擎状态轮询：HTTP/FTP 任务状态推进（记录权威=引擎实时态）+ 全引擎速率缓存。
    /// 每轮对候选项调用 `engine.status()`：
    /// - 缓存：`EngineStatus` 整体写入 `engine_status`（速率/错误供 `/stats`、
    ///   `task_logs` 读取；运行态字段不落盘，无 autosave 负担）；
    /// - HTTP/FTP：引擎终态（Completed/Error）→ 记录推进 Completed/Failed + 落盘；
    ///   引擎活跃（Downloading/MetadataPending）→ Queued 记录顺带推进 Downloading；
    /// - BT：仅缓存（状态权威 = alert 流，轮询不得双头迁移）。
    ///
    /// 返回本批 HTTP/FTP 迁移效果供事件广播；无变化的任务跳过。
    pub async fn poll_engine_states(&self) -> Vec<HttpPollEffect> {
        // 先收集候选（锁外做引擎调用；避免长持锁）。HTTP/FTP 引擎无 alert 回调，
        // 状态推进依赖轮询；BT 活跃任务仅做速率缓存（Downloading/Seeding——
        // 做种中 up_rate 对 /stats 有意义）。
        let candidates: Vec<(String, EngineTaskId, EngineKind)> = {
            let tasks = self.tasks.lock();
            tasks
                .iter()
                .filter(|(_, rec)| match rec.engine_kind {
                    // SFTP 与 HTTP/FTP 同为轮询推进引擎（无 alert 回调）
                    EngineKind::Http | EngineKind::Ftp | EngineKind::Sftp => matches!(
                        rec.task.state,
                        TaskState::Queued | TaskState::Downloading(_)
                    ),
                    EngineKind::Bt => matches!(
                        rec.task.state,
                        TaskState::Downloading(_) | TaskState::Seeding
                    ),
                    // provider/xunlei-nas 暂无轮询路径
                    EngineKind::Provider | EngineKind::XunleiNas => false,
                })
                .filter_map(|(id, rec)| {
                    rec.engine_tid
                        .clone()
                        .map(|t| (id.clone(), t, rec.engine_kind))
                })
                .collect()
        };
        let mut effects = Vec::new();
        for (id, tid, kind) in candidates {
            let Ok(engine) = self.engine_for(kind) else {
                continue;
            };
            // 引擎侧已移除/不可用 → 跳过（任务移除后轮询器自然停）
            let Ok(st) = engine.status(&tid).await else {
                continue;
            };
            if matches!(kind, EngineKind::Bt) {
                // BT 缓存分支：状态权威 = alert 流（状态不迁移、不落盘）。
                // E28：任务名回填在此放行——torrent metadata name 就绪 +
                // metadata.name 空缺 → 幂等回填 + 事件（E9 同语义：一次成功
                // 后 name 非 None 自然停）。快照缓存照旧整体入缓存。
                let seeding_since = self.cache_bt_poll(&id, &st);
                // D36 补线（BT）：速率/进度事件——alert 流只管状态迁移，
                // 速率快照此前仅存缓存不出事件。做种只发 Speed（up_rate 对
                // UI/统计有意义；进度恒满无信息量）。置于执法前：达标暂停
                // 轮次不追发（Paused 后的 Speed 是噪声）。
                self.hub.publish(SchedulerEvent::Speed {
                    task_id: id.clone(),
                    down_rate: st.down_rate,
                    up_rate: st.up_rate,
                });
                if st.state != EngineState::Seeding {
                    self.hub.publish(SchedulerEvent::Progress {
                        task_id: id.clone(),
                        done: st.total_done,
                        total: st.total,
                    });
                }
                // F3/Task 39 执法：share_ratio（E33 all-time 口径，与快照字段
                // 同源）+ 做种时长（seeding_since 起算）。仅 Seeding 态触发；
                // 达阈值 → 完整 pause 语义（记录同步 Paused + autosave + 广播，
                // 与手动暂停同口径，用户可 resume，resume 后再达标再次触发）。
                // 独立 async fn：锁与 await 的跨点隔离在内部生成器（Send 门禁）。
                self.enforce_seeding_limit(&id, engine.clone(), &st, seeding_since)
                    .await;
                continue;
            }
            // Bug B 根因修复：autosave 移到锁外（persisted_tasks 重入同一把非重入锁
            // 会同线程自死锁——与 apply_bt_alert 同源缺陷）。
            let mut backfilled = false;
            let advanced: Option<(TaskState, TaskState)> = {
                let mut tasks = self.tasks.lock();
                let Some(rec) = tasks.get_mut(&id) else {
                    continue;
                };
                // 双检：轮询间隙状态可能已被别处推进（remove/pause/恢复）
                if !matches!(
                    rec.task.state,
                    TaskState::Queued | TaskState::Downloading(_)
                ) {
                    continue;
                }
                // E11 速率缓存：引擎快照整体入缓存（含速率/错误；运行态不落盘，
                // 不 autosave）。置于回填/迁移之前——to==from 轮次缓存仍刷新。
                rec.engine_status = Some(st.clone());
                // E9 名字回填（幂等）：metadata.name 空缺 + 引擎报了最终落盘名
                // → 回填 + 事件。置于状态迁移判断之前：下载中任务 to==from
                // 不迁移，但回填仍需进行（回填一次成功后 name 非 None 自然停）。
                if rec.task.metadata.name.is_none() {
                    if let Some(n) = &st.name {
                        rec.task.metadata.name = Some(n.clone());
                        rec.push_event("name_backfilled", Some(n.clone()));
                        backfilled = true;
                    }
                }
                let from = rec.task.state.clone();
                let raw_to = engine_state_to_task(&st.state, kind);
                // E30：失败拦截——重试预算未用尽 → 清句柄回 Queued 安排退避
                // 重激活（调度循环到期重接入引擎）；用尽 → Failed 终态。
                let to = if raw_to == TaskState::Failed {
                    rec.fail_or_schedule_retry(st.error.as_deref())
                } else {
                    raw_to.clone()
                };
                if to == from {
                    // 已在目标态（活跃→活跃）：不迁移，但本轮回填/缓存仍生效
                    None
                } else {
                    // 错误随快照整体入缓存（st.error），无需单独写点
                    rec.task.state = to.clone();
                    Some((from, to))
                }
            }; // ← tasks 锁在此释放
            if backfilled {
                self.autosave(); // 锁外落盘：名字回填持久化（P4 G5 同口径）
            }
            if advanced.is_some() {
                self.autosave(); // 锁外落盘：终态/推进落盘（修复 Bug B 重入自死锁）
            }
            // D36 补线：Speed/Progress 事件生产者（原为全仓库零生产者的设计缺口
            // ——Throttler 1s 合并、UI SSE 路由、快照跳号补拉均已就位，唯独
            // 没人发事件；UI 速率列只能依赖 2s 快照轮询）。引擎报 Downloading
            // 的活跃轮次发双事件（终态/失败轮次不发：避免 Completed 之后
            // 还追发 Speed{0}）。锁外发布：hub 自带锁，不与 tasks 锁嵌套。
            if st.state == EngineState::Downloading {
                self.hub.publish(SchedulerEvent::Speed {
                    task_id: id.clone(),
                    down_rate: st.down_rate,
                    up_rate: st.up_rate,
                });
                self.hub.publish(SchedulerEvent::Progress {
                    task_id: id.clone(),
                    done: st.total_done,
                    total: st.total,
                });
            }
            // 仅真迁移产生 effect（to==from 的纯回填轮次不广播）；
            // E30：to 取拦截后的实际目标（重试安排 = Queued，非引擎报的 Failed）
            if let Some((from, to)) = advanced {
                effects.push(HttpPollEffect {
                    task_id: id,
                    from,
                    to,
                    message: st.error.clone().unwrap_or_default(),
                });
            }
        }
        effects
    }

    /// BT 轮询缓存 + E28 名回填（同步、锁内无 await）：快照整体入
    /// `engine_status` 缓存；metadata name 就绪且记录空缺 → 幂等回填 + 事件。
    /// 返回该记录的 `seeding_since`（做种时长执法入参）。
    fn cache_bt_poll(
        &self,
        id: &str,
        st: &smart_dl_core::types::EngineStatus,
    ) -> Option<std::time::Instant> {
        let mut tasks = self.tasks.lock();
        if let Some(rec) = tasks.get_mut(id) {
            // 双检：轮询间隙状态可能已被 alert 推进至终态
            //（终态不缓存——与 apply_bt_alert 的终态清零同口径）
            if matches!(
                rec.task.state,
                TaskState::Downloading(_) | TaskState::Seeding
            ) {
                if rec.task.metadata.name.is_none() {
                    if let Some(n) = &st.name {
                        rec.task.metadata.name = Some(n.clone());
                        rec.push_event("name_backfilled", Some(n.clone()));
                    }
                }
                rec.engine_status = Some(st.clone());
                return rec.seeding_since;
            }
        }
        None
    }

    /// F3/Task 39 做种限制执法（qbit Share Ratio Limit + 做种时间限制）：
    /// Seeding 态 + （share_ratio ≥ 阈值 或 做种时长 ≥ 上限）→ 完整 pause
    /// 语义（复用 `pause`：引擎暂停 + 记录同步 Paused + autosave + 状态广播；
    /// 与手动暂停同口径，用户可 resume，resume 后再达标会再次触发）+
    /// `seeding_limit_reached` 事件（原因明细）。
    /// 独立 async fn —— 内部锁不跨自身 await（外层轮询生成器的 Send 门禁）。
    async fn enforce_seeding_limit(
        &self,
        id: &str,
        engine: std::sync::Arc<dyn DownloadEngine>,
        st: &smart_dl_core::types::EngineStatus,
        seeding_since: Option<std::time::Instant>,
    ) {
        if !matches!(st.state, smart_dl_core::types::EngineState::Seeding) {
            return;
        }
        let ratio_limit = engine.seeding_ratio_limit();
        let time_limit_min = engine.seeding_time_limit();
        if ratio_limit.is_none() && time_limit_min.is_none() {
            return;
        }
        let ratio = crate::state::share_ratio(st.total_uploaded, st.total_downloaded);
        let elapsed_min = seeding_since
            .map(|t| t.elapsed().as_secs() / 60)
            .unwrap_or(0);
        // batch6-P2：downloaded==0 且 uploaded>0 视作 ratio=∞（qbit 同口径）
        // ——种子导入/已完整数据直接做种时 all-time downloaded=0，旧实现
        // share_ratio 返回 None → 限制永不触发。
        let ratio_infinite = st.total_downloaded == 0 && st.total_uploaded > 0;
        let ratio_hit = match (ratio_limit, ratio) {
            (Some(l), Some(r)) => r >= l,
            (Some(_), None) => ratio_infinite,
            _ => false,
        };
        let time_hit = matches!((time_limit_min, elapsed_min), (Some(l), m) if m >= l as u64);
        if !ratio_hit && !time_hit {
            return;
        }
        let reason = match (ratio_hit, time_hit) {
            (true, true) => format!(
                "ratio={:.2}>=?{:?} time={elapsed_min}min>=?{:?}min（双达）",
                ratio.unwrap_or(f64::INFINITY),
                ratio_limit,
                time_limit_min
            ),
            (true, false) => format!(
                "ratio={:.2} >= limit={:?}",
                ratio.unwrap_or(f64::INFINITY),
                ratio_limit
            ),
            (false, true) => format!(
                "seeding time={elapsed_min}min >= limit={:?}min",
                time_limit_min
            ),
            (false, false) => unreachable!("已由 hit 判定"),
        };
        // 完整 pause 语义（记录同步/事件/持久化/广播都在里面）
        if self.pause(id).await.is_ok() {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.seeding_since = None;
                rec.push_event("seeding_limit_reached", Some(reason));
            }
        }
    }
}
/// 兜底传输 sink：HTTP 引擎承接 provider 直链下载（M5 直链 → HttpEngine）。
/// 每个文件建引擎任务 → 轮询到终态 → 移除引擎任务（不留记录，属于父 BT 任务流程）。
struct FallbackSink {
    http: Arc<dyn DownloadEngine>,
}

#[async_trait::async_trait]
impl HttpSink for FallbackSink {
    async fn transfer(
        &self,
        task_id: &str,
        url: &str,
        dest_root: std::path::PathBuf,
        name: Option<String>,
    ) -> Result<(), SinkError> {
        // 目标父目录（rel_path 可能含子目录）。
        // batch8（P2）：云端文件名（provider 原样透传）先 sanitize_rel 再
        // join——绝对路径名此前可整体替换 base，在 dest_root 外任意建目录
        // （引擎落盘前的最后一道 mkdir 同源防线）。
        if let Some(rel) = &name {
            match smart_dl_storage::sanitize_rel(rel) {
                Ok(safe) => {
                    if let Some(parent) = dest_root.join(&safe).parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
                Err(e) => {
                    return Err(SinkError::Failed(format!(
                        "云端文件名非法（sanitize 拒绝）: {e}"
                    )))
                }
            }
        }
        let task = DownloadTask {
            id: task_id.to_string(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: url.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: url.to_string(),
                headers: vec![],
                auth: None,
                backup_url: None,
                proxy: None,
            },
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root,
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
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
            max_connections: None,
            queue_priority: 0,
        };
        let tid = self
            .http
            .add(&task)
            .await
            .map_err(|e| SinkError::Failed(e.to_string()))?;
        // 轮询到终态（直链传输上限 600s：免费档聚合 ~1MB/s，须容纳数百 MB 文件）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let started = std::time::Instant::now();
        let mut last_beat = started;
        let mut last_done = u64::MAX;
        let result = loop {
            let st = self
                .http
                .status(&tid)
                .await
                .map_err(|e| SinkError::Failed(e.to_string()))?;
            if last_done == u64::MAX {
                last_done = st.total_done;
            }
            if st.total_done != last_done {
                last_done = st.total_done;
            }
            if last_beat.elapsed() >= std::time::Duration::from_secs(5) {
                last_beat = std::time::Instant::now();
            }
            match st.state {
                EngineState::Completed => break Ok(()),
                EngineState::Error => {
                    break Err(SinkError::Failed(
                        st.error.unwrap_or_else(|| "engine error".into()),
                    ))
                }
                _ => {
                    if std::time::Instant::now() >= deadline {
                        break Err(SinkError::Failed("直链传输超时(60s)".into()));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        };
        let _ = self.http.remove(&tid, false).await;
        result
    }

    async fn update_sources(&self, _task_id: &str, _urls: Vec<String>) -> Result<(), SinkError> {
        // v1：直链不续期（真实 provider 的 refresh_links 接入后实现）
        Ok(())
    }
}

/// ProviderError → DaemonError 的人类可读映射。
fn map_provider_err(e: ProviderError) -> DaemonError {
    use ProviderError as P;
    let msg = match e {
        P::ManualOnly => "BT 进度 ≥50%，按兜底策略不允许（仅进度 <50% 可兜底）".to_string(),
        P::RequiresPause => "需先暂停 BT 任务（串行兜底策略）".to_string(),
        P::NoProvider => "无可用 provider（未配置/未认证/配额耗尽/冷却中/并发满）".to_string(),
        P::Expired => "直链已过期且刷新/重提交均失败".to_string(),
        P::RetriesExhausted => "直链过期恢复次数超限（update_sources≤3 + resubmit≤2）".to_string(),
        other => other.to_string(),
    };
    DaemonError::Fallback(msg)
}
