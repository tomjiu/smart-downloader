//! BT 操作域：文件/队列优先级、顺序/超级做种、tracker/webseed/peer 管理、
//! 通告/recheck/首尾块/magnet 导出、代理与连接数。

use super::*;
impl DaemonState {
    /// 任务级子文件优先级（P1 能力增强，BT 多文件）。设置后返回当前各文件
    /// 优先级快照（下标 = 文件序，与 TaskSnapshot.files 对齐）。
    ///
    /// - 仅 BT 任务（其余 → UnsupportedOp，HTTP 层映射 409）
    /// - 文件数锚定与 metadata 就绪性探测合一：先 readback 当前优先级表
    ///   （engine 侧真实文件数），metadata 未就绪/句柄缺失 → UnsupportedOp（409）
    /// - 下标越界 / 优先级 >7 → InvalidSource（400）；内核侧两段式校验兜底
    /// - 持久化 + 恢复重放：成功后把全量快照（readback None 视为默认 4）写入
    ///   `task.file_priorities` 并落盘；恢复时原样重放（magnet 未就绪场景由
    ///   重放循环延迟收敛，见 `replay_pending_file_priorities`）
    pub async fn set_task_file_priorities(
        &self,
        id: &str,
        priorities: &[(usize, u32)],
    ) -> Result<Vec<Option<u32>>, DaemonError> {
        let (engine, tid, kind) = {
            let rec = self
                .tasks
                .lock()
                .get(id)
                .cloned()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            // kind 预拒先于引擎解析（错误形态保真）：非 BT 任务即使其引擎
            // 未加载也应拿 409 而非「引擎未加载」（启用该 feature 也无济于事）。
            if rec.engine_kind != EngineKind::Bt {
                return Err(DaemonError::UnsupportedOp(format!(
                    "仅 BT 任务支持子文件优先级（{id} 为 {:?}）",
                    rec.engine_kind
                )));
            }
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            (self.engine_for(rec.engine_kind)?, tid, rec.engine_kind)
        };
        // 能力询问（TD-2 Step 3）：BT 语义方法只在 BtEngineExt 上存在——
        // 结构 enforcement（Bt 槽引擎必挂扩展面，理论不可达的防御分支）。
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!("仅 BT 任务支持子文件优先级（{id} 为 {kind:?}）"))
        })?;
        // metadata 就绪探测 + 文件数锚定（当前优先级表长度 = 引擎侧文件数）。
        // 引擎 NotFound（torrent/metadata 缺失）→ 409「metadata 未就绪」，
        // 与任务记录级 404（tasks 表无此 id）语义分离。
        let current = bt.file_priorities(&tid).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::NotFound => DaemonError::UnsupportedOp(
                "BT 任务 metadata 未就绪（或引擎句柄不存在），无法设置子文件优先级".into(),
            ),
            other => DaemonError::Engine(other.to_string()),
        })?;
        let nf = current.len();
        for (idx, prio) in priorities {
            if *idx >= nf {
                return Err(DaemonError::InvalidSource(format!(
                    "文件下标 {idx} 越界（任务 {id} 引擎侧共 {nf} 个文件）"
                )));
            }
            if *prio > 7 {
                return Err(DaemonError::InvalidSource(format!(
                    "优先级 {prio} 越界（0..=7：0=不下载 1=低 4=默认 7=最高）"
                )));
            }
        }
        bt.set_file_priorities(&tid, priorities)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // ── 收敛循环（R5 实测缺陷修复，2026-10 多文件本地 swarm）────────────
        // 两个真实故障模式：
        //  ① libtorrent 异步记账：设后立即查可能读到旧值（lt_kernel.cpp
        //     lt_set_file_priorities 注释明示，以 file_prio_alert 为准）——旧
        //     实现把首次 readback 直接进持久化记录，恢复重放时反噬用户 skip；
        //  ② 任务启动初期设置非确定性丢失：同脚本两跑 total_wanted 16MB/24MB
        //     分叉（skip 的 b.bin 一跑未下、一跑全量下完）。
        // 策略：有界 verify-and-reapply——每轮 readback 与「请求合并后的期望值」
        // 比对，漂移即全量重设；连续两轮收敛（≈800ms 无漂移）提前离场。
        // 更晚的回退（如大种子 checking 完成时）由轮询推进路径的
        // 「进入 Downloading 重入 pending」兜底重放（control.rs）。
        // 持久化与响应均取期望值（意图优先），绝不持久化与请求矛盾的旧 readback。
        const PRIO_VERIFY_ROUNDS: usize = 6;
        const PRIO_VERIFY_INTERVAL_MS: u64 = 400;
        let mut snapshot: Vec<Option<u32>> = Vec::new();
        let mut drift_count: usize = 0;
        let mut converged_streak: usize = 0;
        for round in 0..PRIO_VERIFY_ROUNDS {
            if round > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(PRIO_VERIFY_INTERVAL_MS)).await;
            }
            let cur = bt
                .file_priorities(&tid)
                .await
                .map_err(|e| DaemonError::Engine(e.to_string()))?;
            // 期望值 = readback ∪ 请求（请求覆盖对应下标；越界下标已被上面
            // 文件数锚定拒绝，不会出现 *i >= expect.len()，防御性跳过）
            let mut expect = cur.clone();
            for (i, p) in priorities {
                if *i < expect.len() {
                    expect[*i] = Some(*p);
                }
            }
            if cur.is_empty() || cur != expect {
                drift_count += 1;
                converged_streak = 0;
                // 漂移：全量重设（readback 基 + 请求覆盖），下轮复验
                let full: Vec<(usize, u32)> = expect
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| p.map(|p| (i, p)))
                    .collect();
                if !full.is_empty() {
                    bt.set_file_priorities(&tid, &full)
                        .await
                        .map_err(|e| DaemonError::Engine(e.to_string()))?;
                }
            } else {
                converged_streak += 1;
                // 连续两轮收敛（≈800ms 无漂移）→ 可信收敛提前离场；
                // 更晚的回退由轮询推进的「进入 Downloading 重入 pending」
                // 兜底重放（control.rs）。
                if converged_streak >= 2 {
                    snapshot = expect;
                    break;
                }
            }
            snapshot = expect;
        }
        if drift_count > 0 {
            tracing::warn!(
                "任务 {id} 子文件优先级经历 {drift_count} 轮漂移重设（libtorrent 异步记账/启动竞态），已收敛"
            );
        }
        // 持久化全量快照：期望值的 None（内核未定值）按 libtorrent 默认
        // 优先级 4 归一，保证重放值与引擎语义一致。
        let persisted: Vec<u32> = snapshot.iter().map(|p| p.unwrap_or(4)).collect();
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.task.file_priorities = Some(persisted);
                rec.push_event(
                    "file_priorities_changed",
                    Some(
                        priorities
                            .iter()
                            .map(|(i, p)| format!("{i}={p}"))
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                );
            }
        }
        self.pending_file_prio.lock().remove(id);
        self.autosave();
        Ok(snapshot)
    }

    /// 任务级顺序下载开关（边下边播）：引擎即时生效（HTTP=字段改写下轮拾取；
    /// BT=sequential flag 即时）+ 任务持久化 + TaskSequentialChanged 事件。
    /// FTP 引擎不支持（Unsupported → 400）。
    pub async fn set_task_sequential(&self, id: &str, on: bool) -> Result<(), DaemonError> {
        let (engine, tid) = {
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
            (self.engine_for(rec.engine_kind)?, tid)
        };
        engine.set_sequential(&tid, on).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::Unsupported => {
                DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持顺序下载"))
            }
            other => DaemonError::Engine(other.to_string()),
        })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.task.sequential = on;
                rec.push_event("sequential_changed", Some(on.to_string()));
            }
        }
        self.autosave();
        Ok(())
    }

    /// 超级种子开关（qbit 任务右键同名能力）：仅 BT 任务（其余 kind →
    /// `UnsupportedOp` 409，同 set_max_connections 预拒惯例）。仅下发引擎
    /// flag（做种态生效、下载中无效果），不改记录状态、不落盘。
    pub async fn set_task_super_seeding(&self, id: &str, on: bool) -> Result<(), DaemonError> {
        let (engine, tid) = {
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
            (self.engine_for(rec.engine_kind)?, tid)
        };
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持超级种子（仅 BT 任务）"))
        })?;
        bt.set_super_seeding(&tid, on).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::Unsupported => {
                DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持超级种子"))
            }
            // batch6-P2：句柄已不存在 → NotFound 语义而非 500
            smart_dl_core::types::EngineError::NotFound => DaemonError::NotFound(id.to_string()),
            other => DaemonError::Engine(other.to_string()),
        })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.push_event("super_seeding_changed", Some(on.to_string()));
            }
        }
        Ok(())
    }

    /// 强制宣告/重新校验/导出（Task 46）：公共前置——BT 能力询问 + 引擎句柄
    /// 解析（返回 `BtEngineExt` 借用，直接承载 BT 语义调用；错误消息与
    /// 旧 kind 预拒守卫逐字一致）。
    async fn bt_op_engine(
        &self,
        id: &str,
        op_name: &str,
    ) -> Result<(&'_ dyn smart_dl_core::types::BtEngineExt, String), DaemonError> {
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
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "任务 {id} 的引擎不支持 {op_name}（仅 BT 任务）"
            )));
        }
        // 从注册表直接借用（返回引用无法由 engine_for 的克隆构造）。
        let engine = self.engines.get(&EngineKind::Bt).ok_or_else(|| {
            DaemonError::InvalidSource(format!(
                "引擎未加载: {:?}（编译时启用对应 feature）",
                EngineKind::Bt
            ))
        })?;
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持 {op_name}（仅 BT 任务）"))
        })?;
        Ok((bt, tid))
    }

    /// 强制宣告（Task 46，qbit/BitComet 任务右键对标）：tracker 宣告与
    /// DHT 宣告可组合（`tracker`/`dht` 均真 = 两者都执行）。全部请求失败
    /// 才返回 Err（单路失败聚合进错误文本）。
    pub async fn announce_task(
        &self,
        id: &str,
        tracker: bool,
        dht: bool,
    ) -> Result<(), DaemonError> {
        if !tracker && !dht {
            return Err(DaemonError::InvalidSource(
                "announce 请求无效：tracker 与 dht 至少一项为 true".into(),
            ));
        }
        let (bt, tid) = self.bt_op_engine(id, "强制宣告").await?;
        let mut errs: Vec<String> = Vec::new();
        if tracker {
            if let Err(e) = bt.force_reannounce(&tid).await {
                // batch6-P2：句柄已不存在（引擎侧被摘）→ NotFound 语义而非 500
                if matches!(e, smart_dl_core::types::EngineError::NotFound) {
                    return Err(DaemonError::NotFound(id.to_string()));
                }
                errs.push(format!("tracker 宣告失败: {e}"));
            }
        }
        if dht {
            if let Err(e) = bt.force_dht_announce(&tid).await {
                if matches!(e, smart_dl_core::types::EngineError::NotFound) {
                    return Err(DaemonError::NotFound(id.to_string()));
                }
                errs.push(format!("DHT 宣告失败: {e}"));
            }
        }
        if errs.len() == usize::from(tracker) + usize::from(dht) {
            return Err(DaemonError::Engine(errs.join("; ")));
        }
        // batch5-P2：部分失败不再静默吞——失败摘要入事件详情 + warn 日志
        //（原仅全部失败才上抛，单路失败无日志无事件，用户无从知晓）。
        if !errs.is_empty() {
            tracing::warn!("任务 {id} 强制宣告部分失败: {}", errs.join("; "));
        }
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.push_event(
                    "force_announce",
                    Some(if errs.is_empty() {
                        format!("tracker={tracker} dht={dht}")
                    } else {
                        format!("tracker={tracker} dht={dht}; 部分失败: {}", errs.join("; "))
                    }),
                );
            }
        }
        Ok(())
    }

    /// 强制重新校验（Task 46，qbit「强制重新校验」对标）：任务转入 checking，
    /// 校验期下载/做种挂起，完成后自动恢复。仅下发引擎，不改记录态（状态
    /// 推进由 bt_events 轮询自然回流）。
    pub async fn recheck_task(&self, id: &str) -> Result<(), DaemonError> {
        let (bt, tid) = self.bt_op_engine(id, "强制重新校验").await?;
        bt.force_recheck(&tid).await.map_err(|e| match e {
            // batch6-P2：句柄已不存在 → NotFound 语义而非 500
            smart_dl_core::types::EngineError::NotFound => DaemonError::NotFound(id.to_string()),
            other => DaemonError::Engine(other.to_string()),
        })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.push_event("force_recheck", None);
            }
        }
        Ok(())
    }

    /// 首尾块优先（batch5，qbit「首尾块优先」对标）：每文件首/末块优先级
    /// 置为 prio（0..=7；0 = 恢复默认）。需要 metadata（未就绪 → 409 可重试）。
    pub async fn set_task_piece_first_last(&self, id: &str, prio: i32) -> Result<(), DaemonError> {
        if !(0..=7).contains(&prio) {
            return Err(DaemonError::InvalidSource(format!(
                "piece 优先级须在 0..=7（收到的 {prio}）"
            )));
        }
        let (bt, tid) = self.bt_op_engine(id, "首尾块优先").await?;
        bt.set_piece_first_last(&tid, prio)
            .await
            .map_err(|e| match e {
                smart_dl_core::types::EngineError::Unsupported => {
                    DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持首尾块优先"))
                }
                smart_dl_core::types::EngineError::NotFound => {
                    DaemonError::NotFound(id.to_string())
                }
                other => DaemonError::Engine(other.to_string()),
            })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.push_event("piece_first_last", Some(format!("priority={prio}")));
            }
        }
        Ok(())
    }

    /// 导出 .torrent（Task 46，qbit「导出 .torrent」对标）：metainfo bencode
    /// 字节。magnet 任务 metadata 未就绪 → 409（batch5：NotReady/Unsupported
    /// 统一映射，原 Other → 500 与文档承诺 409 相悖）；任务不在引擎 → 404。
    pub async fn export_task_torrent(&self, id: &str) -> Result<Vec<u8>, DaemonError> {
        let (bt, tid) = self.bt_op_engine(id, "导出种子").await?;
        bt.export_torrent(&tid).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::Unsupported => {
                DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持导出种子"))
            }
            smart_dl_core::types::EngineError::NotReady(msg) => DaemonError::UnsupportedOp(msg),
            smart_dl_core::types::EngineError::NotFound => DaemonError::NotFound(id.to_string()),
            other => DaemonError::Engine(other.to_string()),
        })
    }

    /// 生成 magnet URI（Task 46）：btih + dn + 全量 tracker。
    pub async fn task_magnet_uri(&self, id: &str) -> Result<String, DaemonError> {
        let (bt, tid) = self.bt_op_engine(id, "生成磁链").await?;
        bt.magnet_uri(&tid).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::Unsupported => {
                DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持生成磁链"))
            }
            smart_dl_core::types::EngineError::NotFound => DaemonError::NotFound(id.to_string()),
            other => DaemonError::Engine(other.to_string()),
        })
    }

    /// 队列优先级设置（Task 46，qbit 队列位置对标）：仅对排队中任务（Queued
    /// 且未接入引擎）有实际递补语义，但对任意任务均可设置（提前设定，等它
    /// 未来落队时生效——qbit 同语义）。`action` 相对移动基于**当前排队集**
    /// 的 (queue_priority, added_at_ms) 排序：
    /// - top/bottom：取队列极值 ∓1（队空 = 不变，返回原值）
    /// - up/down：与相邻任务交换 priority；相邻并列时越级 ±1（保证严格移动）
    ///
    /// 返回设置后的 queue_priority（事件已记录）。
    pub async fn set_task_priority(
        &self,
        id: &str,
        value: Option<i32>,
        action: Option<String>,
    ) -> Result<i32, DaemonError> {
        // 相对移动先在 tasks 快照外计算目标值（短锁读队列快照）。
        // batch5-P2：语义对齐文档「同给以 value 优先」——原实现 action 一律
        // 覆盖 value；现在 value 已给出时跳过 action 计算。
        let mut new_val: Option<i32> = value;
        if new_val.is_none() {
            if let Some(act) = &action {
                let cur = {
                    let tasks = self.tasks.lock();
                    let rec = tasks
                        .get(id)
                        .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
                    rec.task.queue_priority
                };
                // 排队集快照：Queued 无句柄任务，排序键 (priority, added_at_ms,
                // created_at)（batch7：与递补排序键同口径，重启后 FIFO 保序）
                let queue: Vec<(String, i32, u64, std::time::Instant)> = {
                    let tasks = self.tasks.lock();
                    let mut v: Vec<(String, i32, u64, std::time::Instant)> = tasks
                        .iter()
                        .filter(|(_, r)| {
                            r.engine_tid.is_none() && r.task.state == TaskState::Queued
                        })
                        .map(|(tid, r)| {
                            (
                                tid.clone(),
                                r.task.queue_priority,
                                r.task.metadata.added_at_ms,
                                r.task.created_at,
                            )
                        })
                        .collect();
                    v.sort_by_key(|(_, p, ms, c)| (*p, *ms, *c));
                    v
                };
                match act.as_str() {
                    "top" => {
                        // 审查修复（P2）：saturating——用户先设 i32::MIN/MAX 后
                        // -1/+1 溢出（debug panic、release 回绕破坏 top/bottom 语义）
                        new_val = Some(
                            queue
                                .iter()
                                .map(|(_, p, _, _)| *p)
                                .min()
                                .unwrap_or(cur)
                                .saturating_sub(1),
                        );
                    }
                    "bottom" => {
                        new_val = Some(
                            queue
                                .iter()
                                .map(|(_, p, _, _)| *p)
                                .max()
                                .unwrap_or(cur)
                                .saturating_add(1),
                        );
                    }
                    "up" | "down" => {
                        let pos = queue.iter().position(|(tid, _, _, _)| tid == id);
                        match pos {
                            None => {
                                // 不在当前排队集（运行中/暂停等）：无相邻语义 → 按极值移动
                                new_val = if act == "up" {
                                    Some(
                                        queue
                                            .iter()
                                            .map(|(_, p, _, _)| *p)
                                            .min()
                                            .unwrap_or(cur)
                                            .saturating_sub(1),
                                    )
                                } else {
                                    Some(
                                        queue
                                            .iter()
                                            .map(|(_, p, _, _)| *p)
                                            .max()
                                            .unwrap_or(cur)
                                            .saturating_add(1),
                                    )
                                };
                            }
                            Some(i) if act == "up" && i > 0 => {
                                // 越过前一个：±1 而非取同值——并列时 (priority, added_at_ms)
                                // 排序仍按创建序，同值不保证上移（本任务创建晚则原地）
                                // batch5-P2：saturating（同 top/bottom 分支）——邻值
                                // 为 i32::MIN/MAX 时原裸 ±1 溢出（debug panic / release 回绕）
                                new_val = Some(queue[i - 1].1.saturating_sub(1));
                            }
                            Some(i) if act == "down" && i + 1 < queue.len() => {
                                new_val = Some(queue[i + 1].1.saturating_add(1));
                            }
                            _ => new_val = Some(cur), // 已在队首/队尾：不变
                        }
                    }
                    other => {
                        return Err(DaemonError::InvalidSource(format!(
                            "未知 priority action {other:?}（支持 top/bottom/up/down）"
                        )))
                    }
                }
            }
        }
        let Some(v) = new_val else {
            return Err(DaemonError::InvalidSource(
                "priority 请求无效：value 与 action 至少一项".into(),
            ));
        };
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            rec.task.queue_priority = v;
            rec.push_event(
                "priority_changed",
                Some(match &action {
                    Some(a) => format!("{a} → {v}"),
                    None => v.to_string(),
                }),
            );
        }
        self.autosave();
        Ok(v)
    }

    /// 手动添加 peer（qbit「添加 peer」对标）：addr 逐条注入 BT 任务；
    /// 逐条返回结果（部分成功语义——单条失败不影响其余）。非 BT 引擎 /
    /// 不支持注入 → 逐条 Err（HTTP 层 409）。事件一条汇总（成功率）。
    pub async fn add_task_peers(
        &self,
        id: &str,
        addrs: Vec<std::net::SocketAddr>,
    ) -> Vec<(String, Result<(), String>)> {
        let resolved: Result<
            (
                std::sync::Arc<dyn smart_dl_core::types::DownloadEngine>,
                String,
            ),
            DaemonError,
        > = {
            let rec = self.tasks.lock().get(id).cloned();
            match rec {
                None => Err(DaemonError::NotFound(id.to_string())),
                Some(rec) => match rec.engine_tid.clone() {
                    None => Err(DaemonError::NotFound(id.to_string())),
                    Some(tid) => self.engine_for(rec.engine_kind).map(|e| (e, tid)),
                },
            }
        };
        let (engine, tid) = match resolved {
            Ok(v) => v,
            Err(e) => {
                let msg = e.to_string();
                return addrs
                    .into_iter()
                    .map(|a| (a.to_string(), Err(msg.clone())))
                    .collect();
            }
        };
        // 能力询问：无 BT 面（非 BT 任务）→ 逐条 Err（与旧 kind 预拒同文案）
        let Some(bt) = engine.bt() else {
            return addrs
                .into_iter()
                .map(|a| (a.to_string(), Err("仅 BT 任务支持添加 peer".into())))
                .collect();
        };
        let mut results = Vec::with_capacity(addrs.len());
        for a in addrs {
            let key = a.to_string();
            let r = bt.add_peer(&tid, a).await;
            results.push((key, r.map_err(|e| e.to_string())));
        }
        let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.push_event("peers_added", Some(format!("{ok}/{}", results.len())));
            }
        }
        results
    }

    /// 任务级连接数上限（S1-c，qbit 每任务连接数）：仅 BT 任务（daemon 侧
    /// 预拒，其余 kind → `UnsupportedOp` 409，同 task_proxy 预拒惯例）。
    /// `n > 0` = 上限；`n == 0` = 复位会话级连接数默认。写入记录字段
    /// （持久化 + 恢复重放，metadata 未就绪也可设）+ 引擎即时下发。
    pub async fn set_task_max_connections(&self, id: &str, n: u32) -> Result<(), DaemonError> {
        let (engine, tid, kind) = {
            let rec = self
                .tasks
                .lock()
                .get(id)
                .cloned()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            // kind 预拒先于引擎解析（错误形态保真，同 set_task_file_priorities）。
            if rec.engine_kind != EngineKind::Bt {
                return Err(DaemonError::UnsupportedOp(format!(
                    "任务 {id} 非 BT 任务（{:?}），不支持连接数上限",
                    rec.engine_kind
                )));
            }
            let tid = rec
                .engine_tid
                .clone()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            (self.engine_for(rec.engine_kind)?, tid, rec.engine_kind)
        };
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!(
                "任务 {id} 非 BT 任务（{kind:?}），不支持连接数上限"
            ))
        })?;
        bt.set_max_connections(&tid, n).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::Unsupported => {
                DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持连接数上限"))
            }
            other => DaemonError::Engine(other.to_string()),
        })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                rec.task.max_connections = Some(n);
                rec.push_event("max_connections_changed", Some(n.to_string()));
            }
        }
        self.autosave();
        Ok(())
    }

    /// 任务级代理热改（E8）：`Some(url)` = 切任务专用 client（覆盖全局，
    /// add 时 E5 语义的运行时版）；`None` = 清除回引擎共享 client。
    ///
    /// - 仅 HTTP 任务：daemon 侧预拒（其余 kind → `UnsupportedOp` 409），
    ///   不依赖引擎 trait default 拒绝——错误信息带任务 kind，且避免
    ///   engine_for 未注册引擎时的笼统报错。
    /// - `Some` 空串拒绝（与 add 口径一致：空串是非法 URL 不是清除；清除
    ///   语义由 `None` 承担）。
    /// - URL 试水（`build_proxied_client`）先行 → `InvalidSource` 400；
    ///   远端不可达 ≠ 代理非法（不发起连接，纯本地构建校验）。
    /// - 引擎应用成功后才改记录（引擎侧对下载中任务 epoch+1 重入，
    ///   段账本恢复进度）；记录改写用 match 下钻 enum（`DownloadSource::Http`
    ///   的 `proxy` 字段）。
    /// - 事件 detail 不放 URL 原文（proxy 可含凭据；push_event 链路无
    ///   脱敏通道）——只记 set/cleared。
    pub async fn set_task_proxy(&self, id: &str, proxy: Option<String>) -> Result<(), DaemonError> {
        if let Some(p) = &proxy {
            if p.is_empty() {
                return Err(DaemonError::InvalidSource(
                    "proxy 不能为空字符串（清除语义请传 null）".into(),
                ));
            }
            smart_dl_httpdl::build_proxied_client(p)
                .map_err(|e| DaemonError::InvalidSource(format!("proxy 非法 {p:?}: {e}")))?;
        }
        let (engine, tid) = {
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
            // kind 预拒先于引擎解析（错误形态保真）：BT 任务即使 BT 引擎
            // 未加载也应拿 409（启用 bt feature 也不会让 BT 任务支持任务级代理）。
            if rec.engine_kind != EngineKind::Http {
                return Err(DaemonError::UnsupportedOp(format!(
                    "任务 {id}（{:?}）不支持任务级代理——仅 HTTP 任务（BT 代理属会话级配置）",
                    rec.engine_kind
                )));
            }
            (self.engine_for(rec.engine_kind)?, tid)
        };
        // 能力询问：仅 HTTP 任务暴露 HttpEngineExt（Bt 槽引擎理论不可达分支）
        let http = engine.http().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持任务级代理"))
        })?;
        http.set_task_proxy(&tid, proxy.clone())
            .await
            .map_err(|e| match e {
                smart_dl_core::types::EngineError::Unsupported => {
                    DaemonError::UnsupportedOp(format!("任务 {id} 的引擎不支持任务级代理"))
                }
                other => DaemonError::Engine(other.to_string()),
            })?;
        {
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(id) {
                // 引擎 kind 已预拒非 Http；此处必然命中 Http 变体（防御性 if let）
                if let DownloadSource::Http { proxy: p, .. } = &mut rec.task.source {
                    *p = proxy.clone();
                }
                rec.push_event(
                    "proxy_changed",
                    Some(if proxy.is_some() { "set" } else { "cleared" }.into()),
                );
            }
        }
        self.autosave();
        Ok(())
    }

    /// 子文件优先级重放收敛（单轮）：对恢复时 metadata 未就绪而挂起的任务，
    /// 探测就绪性（readback 非空）→ 成功后全量重放并移除 pending。
    /// 返回本轮成功重放的任务 id 列表（测试/日志用）。
    ///
    /// 容错口径：任务已移除 / engine_tid 缺失（恢复 add 失败，v1 不会自愈）/
    /// 引擎不支持（Unsupported）→ 移除 pending（永不收敛项不留尾）；
    /// 其余失败（引擎忙/暂不可用）保留 pending 下轮再试。
    pub async fn replay_pending_file_priorities(&self) -> Vec<TaskId> {
        let pending: Vec<TaskId> = self.pending_file_prio.lock().iter().cloned().collect();
        if pending.is_empty() {
            return Vec::new();
        }
        let mut done = Vec::new();
        for id in pending {
            let (bt, tid, prios) = {
                let tasks = self.tasks.lock();
                let Some(rec) = tasks.get(&id) else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                let Some(tid) = rec.engine_tid.clone() else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                let Some(prios) = rec.task.file_priorities.clone() else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                // 注册表借用（引擎 Arc 存活于 self.engines，借用可跨 await 持有；
                // 引擎缺失 → 下轮再试，无 BT 面 → 永不收敛项移除 pending）
                let Some(engine) = self.engines.get(&rec.engine_kind) else {
                    continue; // 引擎暂不可用：下轮再试（旧 engine_for Err 同口径）
                };
                let Some(bt) = engine.bt() else {
                    self.pending_file_prio.lock().remove(&id);
                    continue;
                };
                (bt, tid, prios)
            };
            // 就绪性探测：readback 成功且非空 = metadata 已就绪
            match bt.file_priorities(&tid).await {
                Ok(cur) if !cur.is_empty() => {
                    let pairs: Vec<(usize, u32)> =
                        prios.iter().enumerate().map(|(i, p)| (i, *p)).collect();
                    match bt.set_file_priorities(&tid, &pairs).await {
                        Ok(()) => {
                            self.pending_file_prio.lock().remove(&id);
                            if let Some(rec) = self.tasks.lock().get_mut(&id) {
                                rec.push_event("restored", Some("子文件优先级重放完成".into()));
                            }
                            tracing::info!("任务 {id} 子文件优先级重放完成（{} 项）", pairs.len());
                            done.push(id);
                        }
                        Err(smart_dl_core::types::EngineError::Unsupported) => {
                            self.pending_file_prio.lock().remove(&id);
                        }
                        Err(_) => {} // 引擎忙/瞬态错误：下轮再试
                    }
                }
                Err(smart_dl_core::types::EngineError::Unsupported) => {
                    self.pending_file_prio.lock().remove(&id);
                }
                _ => {} // 未就绪/暂不可读：下轮再试
            }
        }
        done
    }

    /// F5 P2SP：给运行中的 BT 任务逐条注入 web seed（云盘直链，BEP-19），
    /// 返回成功注入条数。仅 BT 任务可注入（其余 → UnsupportedOp，HTTP 层映射
    /// 409）；engine_tid 缺失（尚未入引擎/恢复失败）→ NotFound。
    /// **URL 必须原样使用**：禁止增删改任何 query 参数——云盘直链带 `at=`
    /// 防篡改签名，改动即失效；要多条链请重新调用取链 API 取新链（F5 PoC-1b）。
    pub async fn add_webseeds(&self, id: &str, urls: &[String]) -> Result<usize, DaemonError> {
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
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持注入 web seed（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let engine = self.engine_for(EngineKind::Bt)?;
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持注入 web seed（{id} 为 {:?}）",
                rec.engine_kind
            ))
        })?;
        let mut added = 0usize;
        for url in urls {
            bt.add_url_seed(&tid, url)
                .await
                .map_err(|e| DaemonError::Engine(e.to_string()))?;
            added += 1;
        }
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("webseed", Some(format!("+{added}")));
        }
        Ok(added)
    }

    /// 列举任务 tracker 表（E29，仅 BT 任务；metadata 未就绪也可查）。
    pub async fn list_trackers(&self, id: &str) -> Result<Vec<TrackerEntry>, DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let engine = self.engine_for(EngineKind::Bt)?;
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            ))
        })?;
        bt.list_trackers(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))
    }

    /// 批量追加 tracker（E29，仅 BT 任务）：URL 非空 + 无空白校验；
    /// 返回实际追加数。追加即时生效（libtorrent announce 表，metadata
    /// 未就绪也可设）。运行时追加不持久化（重启后以 magnet/.torrent 自带
    /// 表为准——与 webseed 注入同口径）。
    pub async fn add_trackers(&self, id: &str, urls: &[String]) -> Result<usize, DaemonError> {
        if urls.is_empty() {
            return Err(DaemonError::InvalidSource("urls 不能为空".into()));
        }
        for u in urls {
            if u.trim() != u || u.is_empty() || u.split_whitespace().count() != 1 {
                return Err(DaemonError::InvalidSource(format!(
                    "tracker URL 非法（空白/空串）: {u:?}"
                )));
            }
        }
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let engine = self.engine_for(EngineKind::Bt)?;
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            ))
        })?;
        bt.add_trackers(&tid, urls)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("tracker", Some(format!("+{}", urls.len())));
        }
        Ok(urls.len())
    }

    /// 删 tracker（E29，仅 BT 任务）：URL 精确匹配；无匹配 → NotFound（404）。
    pub async fn remove_tracker(&self, id: &str, url: &str) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if rec.engine_kind != EngineKind::Bt {
            return Err(DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            )));
        }
        let tid = rec
            .engine_tid
            .clone()
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        let engine = self.engine_for(EngineKind::Bt)?;
        let bt = engine.bt().ok_or_else(|| {
            DaemonError::UnsupportedOp(format!(
                "仅 BT 任务支持 tracker 管理（{id} 为 {:?}）",
                rec.engine_kind
            ))
        })?;
        bt.remove_tracker(&tid, url).await.map_err(|e| match e {
            smart_dl_core::types::EngineError::NotFound => {
                DaemonError::NotFound(format!("tracker 不存在: {url}"))
            }
            other => DaemonError::Engine(other.to_string()),
        })?;
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("tracker", Some(format!("-{url}")));
        }
        Ok(())
    }
}
