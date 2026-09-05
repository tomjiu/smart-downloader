//! 任务操作面：添加（HTTP/FTP/BT/迅雷/链接）、查询与列表、批量、控制（pause/resume/限速/优先级/顺序/代理/改名/标签）、tracker/webseed、移除、provider 兜底、完成回调与清理、HTTP/FTP 状态轮询。

use super::*;

impl DaemonState {
    /// 任务完成事件统一出口（E17）：广播 `SchedulerEvent::Completed` +
    /// 触发完成 Webhook。三个完成转移点（HTTP/FTP 轮询循环、BT alert 流
    /// Seeding 转移、Provider 兜底成功）一律经此，保证事件与通知不脱钩。
    pub fn publish_task_completed(&self, task_id: &str) {
        // E20：完成时刻入档（自动清理判龄依据；记录不存在则跳过写点）
        {
            if let Some(rec) = self.tasks.lock().get_mut(task_id) {
                rec.task.metadata.finished_at_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
            }
        }
        self.hub.publish(SchedulerEvent::Completed {
            task_id: task_id.to_string(),
        });
        self.fire_completion_webhook(task_id);
        self.run_post_download_actions(task_id);
    }

    /// 完成自动处理（E27，清单 #15）：`[post_download] move_to` 移动 +
    /// `hook` 外部程序。fire-and-forget——失败仅记日志/事件，不反压链路；
    /// 未配置两者时零开销直返。锁内快照、锁外行动（同 webhook 纪律）。
    pub(super) fn run_post_download_actions(&self, task_id: &str) {
        let move_to = self.post_move_to.lock().clone();
        let hook = self.post_hook.lock().clone();
        if move_to.is_none() && hook.is_none() {
            return;
        }
        // 锁内快照：任务名/引擎/落盘路径/conflict-skip 标记
        let snap = {
            let tasks = self.tasks.lock();
            let Some(rec) = tasks.get(task_id) else {
                return; // 任务已移除 → 无处理主体
            };
            let conflict_skip = rec
                .events
                .iter()
                .any(|e| e.op == "add" && e.detail.as_deref() == Some("conflict_skip"));
            Some((
                rec.task.metadata.name.clone(),
                kind_label(&rec.engine_kind),
                rec.task.dest_root.clone(),
                conflict_skip,
            ))
        };
        let Some((Some(name), engine, dest_root, conflict_skip)) = snap else {
            return; // 无名任务（BT metadata 未回填等）→ 无落盘文件可定位
        };
        let src = dest_root.join(&name);
        // 单文件门控：路径不存在 / 是目录（BT 多文件）→ 移动无意义；
        // hook 仍照发（webhook 同口径：通知尽力而为）
        let is_file = src.is_file();
        let mut final_path = src.clone();

        // 1) 移动（conflict-skip 任务不动既有文件——尊重 skip 语义）
        if let Some(dst_dir) = &move_to {
            if conflict_skip {
                tracing::info!("post_download: 任务 {task_id} 为 conflict-skip，既有文件不移动");
            } else if !is_file {
                tracing::info!(
                    "post_download: 任务 {task_id} 落盘路径非单文件（{:?}），移动跳过",
                    src
                );
            } else {
                match Self::move_completed_file(&src, dst_dir, &name) {
                    Ok(target) => {
                        tracing::info!("post_download: 任务 {task_id} 文件已移动 → {target:?}");
                        final_path = target.clone();
                        let mut tasks = self.tasks.lock();
                        if let Some(rec) = tasks.get_mut(task_id) {
                            rec.push_event(
                                "post_move",
                                Some(target.to_string_lossy().into_owned()),
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!("post_download: 任务 {task_id} 移动失败: {e}");
                        let mut tasks = self.tasks.lock();
                        if let Some(rec) = tasks.get_mut(task_id) {
                            rec.push_event("post_move", Some(format!("failed: {e}")));
                        }
                    }
                }
            }
        }

        // 2) 外部钩子（移动后终路径经 SD_FILE_PATH 传递；后台线程收尾）
        if let Some(prog) = &hook {
            let prog = prog.clone();
            let envs = vec![
                ("SD_TASK_ID".to_string(), task_id.to_string()),
                ("SD_TASK_NAME".to_string(), name),
                (
                    "SD_FILE_PATH".to_string(),
                    final_path.to_string_lossy().into_owned(),
                ),
                ("SD_ENGINE".to_string(), engine.to_string()),
            ];
            let hook_task_id = task_id.to_string();
            let hook_prog = prog.clone();
            std::thread::spawn(move || {
                match std::process::Command::new(&hook_prog)
                    .envs(envs)
                    .stdin(std::process::Stdio::null())
                    .output()
                {
                    Ok(out) if out.status.success() => {
                        tracing::info!("post_download: 任务 {hook_task_id} 钩子执行成功");
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        tracing::warn!(
                            "post_download: 任务 {hook_task_id} 钩子非零退出 status={:?} stdout={stdout} stderr={stderr}",
                            out.status.code()
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            "post_download: 任务 {hook_task_id} 钩子启动失败 {hook_prog:?}: {e}"
                        );
                    }
                }
            });
            let mut tasks = self.tasks.lock();
            if let Some(rec) = tasks.get_mut(task_id) {
                rec.push_event("post_hook", Some(prog));
            }
        }
    }

    /// 完成通知投递（E17）：fire-and-forget——单次 POST、5s 超时、失败仅记
    /// 警告日志（不重试不排队；通知属尽力而为，不得反压下载主链路）。
    /// 未配置 URL 时零开销直返。payload 从任务记录快照构建（锁内取值锁外投递）。
    pub(super) fn fire_completion_webhook(&self, task_id: &str) {
        let Some(url) = self.webhook_url.lock().clone() else {
            return;
        };
        let payload = {
            let tasks = self.tasks.lock();
            match tasks.get(task_id) {
                None => return, // 任务已移除（完成通知失去主体）→ 静默
                Some(rec) => {
                    // 总字节：优先 add 探测 identity，缺省回退 E11 引擎快照缓存
                    //（HTTP 探测失败/信息聚合型源 identity.size=0 时仍有值）
                    let total_bytes = match &rec.task.identity {
                        ContentIdentity::SingleFile { size, .. } if *size > 0 => Some(*size),
                        _ => rec
                            .engine_status
                            .as_ref()
                            .map(|s| s.total)
                            .filter(|t| *t > 0),
                    };
                    serde_json::json!({
                        "event": "task_completed",
                        "task_id": rec.task.id,
                        "name": rec.task.metadata.name,
                        "engine": kind_label(&rec.engine_kind),
                        "total_bytes": total_bytes,
                        "finished_at_unix": std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                    })
                }
            }
        };
        let client = self.webhook_client.clone();
        tokio::spawn(async move {
            let result = client
                .post(&url)
                .timeout(std::time::Duration::from_secs(5))
                .json(&payload)
                .send()
                .await;
            match result {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => {
                    tracing::warn!("完成 Webhook 非成功响应: {url} status={}", resp.status())
                }
                Err(e) => tracing::warn!("完成 Webhook 投递失败: {url} {e}"),
            }
        });
    }

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
                        // 双检：激活间隙任务可能已被 resume 路径抢先激活/被移除。
                        // 激活成功即消费重试安排（E30：next_retry_at 清零）。
                        Some(rec) if rec.engine_tid.is_none() => {
                            rec.engine_tid = Some(tid);
                            rec.task.metadata.next_retry_at_unix = 0;
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

    /// 调度激活循环驱动点（E23+E30）：把到期任务（未接入引擎、Queued）逐个
    /// 接入引擎。到期判定：任务带重试安排（next_retry_at > 0）→ 按 next_retry_at
    /// 判定（重试安排优先，避免定时任务首次激活间隙被误读）；否则按 E23
    /// start_at 判定。serve 以 1s 周期驱动；测试可直接调用。返回激活成功的
    /// task_id 列表（保持迭代序）。
    pub async fn activate_due_tasks(&self) -> Vec<String> {
        let now = now_unix();
        let due: Vec<(String, DownloadTask, EngineKind)> = {
            let tasks = self.tasks.lock();
            tasks
                .iter()
                .filter(|(_, rec)| {
                    if rec.engine_tid.is_some() || rec.task.state != TaskState::Queued {
                        return false;
                    }
                    let m = &rec.task.metadata;
                    if m.next_retry_at_unix > 0 {
                        // E30：重试等待中——到期才激活
                        m.next_retry_at_unix <= now
                    } else {
                        // E23：定时启动等待中——到期才激活
                        m.start_at_unix > 0 && m.start_at_unix <= now
                    }
                })
                .map(|(id, rec)| (id.clone(), rec.task.clone(), rec.engine_kind))
                .collect()
        };
        let mut activated = Vec::new();
        for (id, task, kind) in due {
            if self.activate_one(&id, task, kind).await {
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
                    return self
                        .add_bt_task_opts(m, dest_root, opts.sequential, opts.start_at_unix)
                        .await;
                }
                #[cfg(not(feature = "bt"))]
                {
                    let _ = opts.sequential;
                    let _ = opts.start_at_unix;
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
                        .add_ftp_task_opts(u, dest_root, opts.start_at_unix)
                        .await;
                }
                #[cfg(not(feature = "ftp"))]
                {
                    let _ = opts.start_at_unix;
                    Err(DaemonError::InvalidSource(format!(
                        "ftp 需 FTP 引擎（编译时启用 --features ftp）: {u}"
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
    /// 到点由调度循环激活。
    #[cfg(feature = "bt")]
    pub(super) async fn add_bt_task_opts(
        &self,
        magnet: String,
        dest_root: Option<String>,
        sequential: bool,
        start_at_unix: Option<u64>,
    ) -> Result<TaskId, DaemonError> {
        // B10：目标目录预检（创建/可写）；magnet 总大小元数据前未知 → 空间预检跳过
        // dest 未指定 → 默认落盘目录（与 HTTP 一致：default_dest_root 配置）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;
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
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎（记录 Queued + 无句柄），
        // 到点由调度循环接入（engine.add 与查重/预检后置同链路）。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Bt));
        }

        let engine_tid = self
            .engine_for(EngineKind::Bt)?
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // 顺序下载立即下发（handle 级 flag，metadata 未就绪也可设；
        // 失败不回滚任务，恢复重放 + set_sequential 端点可补）。
        if sequential {
            let engine = self.engine_for(EngineKind::Bt)?;
            if let Err(e) = engine.set_sequential(&engine_tid, true).await {
                tracing::warn!("BT 任务 {task_id} 顺序下载 flag 下发失败: {e}");
            }
        }
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);
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

    /// 添加 .torrent 文件任务（feature `bt`）：infohash canonical 查重 → 引擎
    /// add_torrent_file → TaskCreated 事件。torrent 字节来自 API base64 解码。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
    ) -> Result<TaskId, DaemonError> {
        self.add_torrent_task_opts(torrent_bytes, dest_root, false, None)
            .await
    }

    /// 顺序下载变体：`sequential` 写入任务 + 引擎 add 后立即下发 flag。
    /// `start_at_unix`（E23）：Some(未来) = 延迟入引擎，到点由调度循环激活。
    #[cfg(feature = "bt")]
    pub async fn add_torrent_task_opts(
        &self,
        torrent_bytes: Vec<u8>,
        dest_root: Option<String>,
        sequential: bool,
        start_at_unix: Option<u64>,
    ) -> Result<TaskId, DaemonError> {
        // B10：目标目录预检（创建/可写）；dest 未指定 → 默认落盘目录（与 HTTP/BT-magnet 一致）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;
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
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Bt));
        }

        let engine_tid = self
            .engine_for(EngineKind::Bt)?
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // 顺序下载立即下发（同 magnet 路径：handle 级 flag，失败不回滚）。
        if sequential {
            let engine = self.engine_for(EngineKind::Bt)?;
            if let Err(e) = engine.set_sequential(&engine_tid, true).await {
                tracing::warn!("BT 任务 {task_id} 顺序下载 flag 下发失败: {e}");
            }
        }
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);
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
        use xunlei_convert::{build_bitfield_lenient, FastresumeConverter, XlbtCfg};

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
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;

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
        let mut completed_total = 0usize;
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
            // 累加局部完成数（completed_pieces 是局部索引的前缀计数，这里用位图直接设置更稳妥）
            // completed_pieces 语义：前 N 个 piece 完成（局部），映射到全局连续区间。
            completed_total += report.completed_pieces;
        }

        // 用 lenient 策略构建全局 bitfield（合并所有文件的 partial）
        bitfield =
            build_bitfield_lenient(meta.pieces_hash.len(), completed_total, &partial_infos, 0.5);

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

        // 6. 通过 btcore 导入
        let engine_tid = self
            .engine_for(EngineKind::Bt)?
            .add_xunlei_resume(fastresume_bytes)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;

        // 7. 创建任务记录
        let task = DownloadTask {
            id: task_id.clone(),
            canonical_id: canonical,
            source: DownloadSource::TorrentFile(torrent),
            identity: ContentIdentity::SingleFile {
                size: total_size,
                etag: None,
                sha256: None,
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
        };
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid.clone()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("xunlei-import", None);

        // 8. peer 注入（best-effort）：把 cfg 里的 bt:// 地址注入引擎
        if let Ok(cfg_obj) = XlbtCfg::parse(&cfg) {
            let engine = self.engine_for(EngineKind::Bt)?;
            for peer_str in cfg_obj.peers {
                if let Some((ip, port)) = parse_bt_peer(&peer_str) {
                    let addr = format!("{}:{}", ip, port);
                    if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
                        let _ = engine.add_peer(&engine_tid, addr).await;
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
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest = dest_root.or(Some(def));
        let dest_root = ensure_dest_root(dest, &self.dest_roots())?;
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E21 skip：目标文件已在 → 不入引擎，任务直接落 Completed
        //（既有文件保持原样；完成事件/Webhook 照常——publish_task_completed
        // 一并写 finished_at）
        if skip_download {
            let mut rec = TaskRecord {
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
        let engine_tid = self
            .engine_for(EngineKind::Http)?
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid),
            engine_kind: EngineKind::Http,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);
        self.tasks.lock().insert(task_id.clone(), rec);
        self.autosave();
        self.hub.publish(SchedulerEvent::TaskCreated {
            task_id: task_id.clone(),
        });
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: task_id.clone(),
            from: TaskState::Queued,
            to: TaskState::Downloading(EngineKind::Http),
        });
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
        self.add_ftp_task_opts(url, dest_root, None).await
    }

    /// 定时变体（E23）：`start_at_unix` Some(未来) = 延迟入引擎，到点由
    /// 调度循环激活。
    #[cfg(feature = "ftp")]
    pub async fn add_ftp_task_opts(
        &self,
        url: String,
        dest_root: Option<String>,
        start_at_unix: Option<u64>,
    ) -> Result<TaskId, DaemonError> {
        if !url.starts_with("ftp://") {
            return Err(DaemonError::InvalidSource(url));
        }
        // B10：目标目录预检；目录总大小需 LIST 才可知 → 空间预检跳过（同 HTTP 逻辑）
        let def = self.default_dest_root.lock().to_string_lossy().into_owned();
        let dest_root = ensure_dest_root(dest_root.or(Some(def)), &self.dest_roots())?;
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
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            metadata: TaskMetadata {
                name: if is_dir { None } else { name },
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: self.resolve_start_at(start_at_unix),
                next_retry_at_unix: 0,
            },
            limits: None,
        };

        // E23 定时启动：start_at 未来 → 延迟入引擎，到点由调度循环接入。
        if task.metadata.start_at_unix > now_unix() {
            return Ok(self.insert_scheduled_task(task, EngineKind::Ftp));
        }

        let engine = self.engine_for(EngineKind::Ftp)?;
        let engine_tid = engine
            .add(&task)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let mut rec = TaskRecord {
            task,
            engine_tid: Some(engine_tid.clone()),
            engine_kind: EngineKind::Ftp,
            engine_status: None,
            events: vec![],
        };
        rec.push_event("add", None);

        // 目录任务：有限次 files 同步（FtpEngine::add 已同步 LIST，首轮通常即可命中）
        if is_dir {
            for _ in 0..8 {
                if let Ok(st) = engine.status(&engine_tid).await {
                    if !st.files.is_empty() {
                        rec.task.files = st
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
                        break;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(30)).await;
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
            to: TaskState::Downloading(EngineKind::Ftp),
        });
        Ok(task_id)
    }

    /// 任务快照（实时读引擎状态；未完成时引擎可能已移动）。
    pub async fn task_snapshot(&self, id: &str) -> Option<TaskSnapshot> {
        let rec = self.tasks.lock().get(id).cloned()?;
        let engine = self.engine_for(rec.engine_kind).ok();
        let (engine_name, status) = match (&rec.engine_tid, &engine) {
            (Some(tid), Some(eng)) => {
                let st = eng.status(tid).await.ok();
                (Some(eng.id().to_string()), st)
            }
            _ => (None, None),
        };
        // 显示层权威（qB 式）：用户暂停是记录级事实，不被引擎实时态覆盖
        // （lt 暂停后 status 枚举仍报 downloading，ABI 未透出 paused 位）。
        let effective_state = match &rec.task.state {
            TaskState::Paused => rec.task.state.clone(),
            other => match &status {
                Some(s) => engine_state_to_task(&s.state, rec.engine_kind),
                None => other.clone(),
            },
        };
        let state = state_label(&effective_state);
        // E13：速率与 done/total 同源同新鲜度；Paused 记录态是显示层权威
        // （qB 式），速率同样以记录为准清零——引擎侧 <200ms 窗口会沿用
        // 平滑值（陈旧非零），不锁则暂停后最长 200ms 内快照仍报旧速率。
        let rates = status.as_ref().map(|s| {
            if matches!(effective_state, TaskState::Paused) {
                TaskRates {
                    down_bytes_s: 0,
                    up_bytes_s: 0,
                }
            } else {
                TaskRates {
                    down_bytes_s: s.down_rate,
                    up_bytes_s: s.up_rate,
                }
            }
        });
        Some(TaskSnapshot {
            task_id: id.to_string(),
            state,
            // 安全修复（V6）：source 可能含凭据（headers/auth/userinfo），不得 {:?} 直通
            source: rec.task.source.redacted_debug(),
            dest_root: rec.task.dest_root.clone(),
            engine: engine_name,
            done: status.as_ref().map(|s| s.total_done).unwrap_or(0),
            total: status.as_ref().map(|s| s.total).unwrap_or(0),
            error: status.as_ref().and_then(|s| s.error.clone()),
            files: status.as_ref().map(|s| s.files.clone()).unwrap_or_default(),
            rates,
            // E33：累计统计与分享率（同一次引擎快照取样，非 2s 轮询缓存）
            total_downloaded: status.as_ref().map(|s| s.total_downloaded).unwrap_or(0),
            total_uploaded: status.as_ref().map(|s| s.total_uploaded).unwrap_or(0),
            share_ratio: status
                .as_ref()
                .and_then(|s| share_ratio(s.total_uploaded, s.total_downloaded)),
            limits: rec.task.limits.clone(),
            file_priorities: rec.task.file_priorities.clone(),
            sequential: rec.task.sequential,
            name: rec.task.metadata.name.clone(),
            tags: rec.task.metadata.tags.clone(),
            start_at_unix: rec.task.metadata.start_at_unix,
            retries: rec.task.retry.retries as u64,
            max_retries: rec.task.retry.max_retries as u64,
            next_retry_at_unix: rec.task.metadata.next_retry_at_unix,
        })
    }

    /// 全量列表（兼容入口：无过滤无分页，形状与 E7 之前一致）。
    pub fn list(&self) -> Vec<TaskSummary> {
        self.list_filtered(&ListQuery::default()).0
    }

    /// 过滤 + 排序 + 分页列表（E7）：
    /// - 排序：task_id 数值后缀升序（创建序；HashMap 迭代序不稳定，分页必须
    ///   确定性排序。task_id = `t{u64}` 自增序号，parse 失败兜底排序键 u64::MAX）。
    /// - 过滤：states/engines 任一命中即保留（OR 语义）；空集合 = 该维度跳过；
    ///   两维度间 AND；匹配大小写不敏感（查询方可写小写）。
    /// - 返回 `(当前页, 过滤后总数)`——总数供 `X-Total-Count`，客户端算页数。
    pub fn list_filtered(&self, q: &ListQuery) -> (Vec<TaskSummary>, usize) {
        let tasks = self.tasks.lock();
        let mut rows: Vec<(&String, &TaskRecord)> = tasks.iter().collect();
        rows.sort_by_cached_key(|(id, _)| {
            id.strip_prefix('t')
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        });
        let filtered: Vec<&TaskRecord> = rows
            .into_iter()
            .filter(|(_, rec)| {
                let st = state_label(&rec.task.state);
                let en = kind_label(&rec.engine_kind);
                (q.states.is_empty() || q.states.iter().any(|s| s.eq_ignore_ascii_case(&st)))
                    && (q.engines.is_empty()
                        || q.engines.iter().any(|e| e.eq_ignore_ascii_case(en)))
                    // E14：名字或脱敏 URL 子串命中即保留（大小写不敏感；
                    // 空针 contains 恒真 → 自然退化为不过滤）
                    && match &q.search {
                        None => true,
                        Some(needle) => {
                            let n = needle.trim().to_lowercase();
                            let name_hit = rec
                                .task
                                .metadata
                                .name
                                .as_deref()
                                .is_some_and(|s| s.to_lowercase().contains(&n));
                            let url_hit = rec
                                .task
                                .source
                                .search_urls()
                                .iter()
                                .any(|u| u.to_lowercase().contains(&n));
                            // E18：标签入搜索语料（名字/URL 同款子串命中）
                            let tag_hit = rec
                                .task
                                .metadata
                                .tags
                                .iter()
                                .any(|t| t.to_lowercase().contains(&n));
                            name_hit || url_hit || tag_hit
                        }
                    }
                    // E18：标签 any-of 过滤（空集合跳过；大小写不敏感）
                    && (q.tags.is_empty()
                        || rec.task.metadata.tags.iter().any(|t| {
                            q.tags
                                .iter()
                                .any(|want| want.eq_ignore_ascii_case(t))
                        }))
            })
            .map(|(_, rec)| rec)
            .collect();
        let total = filtered.len();
        let page = filtered
            .into_iter()
            .skip(q.offset)
            .take(q.limit.unwrap_or(usize::MAX))
            .map(|rec| TaskSummary {
                task_id: rec.task.id.clone(),
                state: state_label(&rec.task.state),
                // 安全修复（V6）：同快照，source 脱敏
                source: rec.task.source.redacted_debug(),
                engine: kind_label(&rec.engine_kind),
                name: rec.task.metadata.name.clone(),
                tags: rec.task.metadata.tags.clone(),
                start_at_unix: rec.task.metadata.start_at_unix,
                retries: rec.task.retry.retries as u64,
                max_retries: rec.task.retry.max_retries as u64,
                next_retry_at_unix: rec.task.metadata.next_retry_at_unix,
            })
            .collect();
        (page, total)
    }

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

    /// 全局统计快照（`GET /stats`）：总数 + 按状态/引擎聚合 + 速率求和。
    pub fn stats(&self) -> DaemonStats {
        let mut st = DaemonStats::default();
        let tasks = self.tasks.lock();
        st.total = tasks.len();
        for rec in tasks.values() {
            *st.by_state.entry(state_label(&rec.task.state)).or_insert(0) += 1;
            *st.by_engine
                .entry(kind_label(&rec.engine_kind))
                .or_insert(0) += 1;
            if let Some(s) = &rec.engine_status {
                st.down_bytes_s += s.down_rate;
                st.up_bytes_s += s.up_rate;
            }
        }
        st
    }

    /// 任务级速率样本（A4 `/metrics` histogram 数据源）：`(engine 标签,
    /// down, up)` 三元组。仅含引擎缓存中**任一方向速率 > 0** 的任务（活跃
    /// 传输分布口径；全零任务不进样本——histogram count = 传输中任务数，
    /// 空闲任务堆积不会灌爆低桶）。`/stats` JSON 面保持既有字段不变。
    pub fn task_speed_samples(&self) -> Vec<(&'static str, u64, u64)> {
        let tasks = self.tasks.lock();
        tasks
            .values()
            .filter_map(|rec| {
                let s = rec.engine_status.as_ref()?;
                (s.down_rate > 0 || s.up_rate > 0)
                    .then(|| (kind_label(&rec.engine_kind), s.down_rate, s.up_rate))
            })
            .collect()
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
        }
        // 暂停意图必须立刻持久化（P4 G5）：否则重启后暂停任务被当作运行任务恢复
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: TaskState::Downloading(rec.engine_kind),
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
        if let Some(rec) = self.tasks.lock().get_mut(id) {
            rec.push_event("resume", None);
            rec.task.state = TaskState::Downloading(rec.engine_kind);
        }
        // 恢复态同步持久化（P4 G5：与 pause 对称）
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged {
            task_id: id.to_string(),
            from: TaskState::Paused,
            to: TaskState::Downloading(rec.engine_kind),
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
        let (engine, tid) = {
            let rec = self
                .tasks
                .lock()
                .get(id)
                .cloned()
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
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
            (self.engine_for(rec.engine_kind)?, tid)
        };
        // metadata 就绪探测 + 文件数锚定（当前优先级表长度 = 引擎侧文件数）。
        // 引擎 NotFound（torrent/metadata 缺失）→ 409「metadata 未就绪」，
        // 与任务记录级 404（tasks 表无此 id）语义分离。
        let current = engine.file_priorities(&tid).await.map_err(|e| match e {
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
        engine
            .set_file_priorities(&tid, priorities)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        let snapshot = engine
            .file_priorities(&tid)
            .await
            .map_err(|e| DaemonError::Engine(e.to_string()))?;
        // 持久化全量快照：readback 的 None（内核未定值）按 libtorrent 默认
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
            if rec.engine_kind != EngineKind::Http {
                return Err(DaemonError::UnsupportedOp(format!(
                    "任务 {id}（{:?}）不支持任务级代理——仅 HTTP 任务（BT 代理属会话级配置）",
                    rec.engine_kind
                )));
            }
            (self.engine_for(rec.engine_kind)?, tid)
        };
        engine
            .set_task_proxy(&tid, proxy.clone())
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

    /// 任务重命名（E15）：`POST /tasks/:id/name`。显示层改名——落盘路径在
    /// 引擎 add 时即已决定（httpdl `resolved_name` 决策链），改名不迁移已
    /// 落盘/在传文件；名字是列表/快照透出与 E14 搜索语料（name 分量）。
    /// `None` = 清除显式名（E9 回填仅在"名字为空且引擎报名"的轮询点发生，
    /// 活跃任务下一轮可能自动补回派生名——清除语义即"交还派生链"）。
    /// 事件 detail 只记 set/cleared（与 proxy_changed 同口径，名字本体走
    /// 快照/列表查询，不进事件链路）。
    pub fn set_task_name(&self, id: &str, name: Option<String>) -> Result<(), DaemonError> {
        if let Some(n) = &name {
            if n.trim().is_empty() {
                return Err(DaemonError::InvalidSource(
                    "name 不能为空白（清除语义请传 null）".into(),
                ));
            }
            // V3 终审同函数：与 add 入参同一裁决点（非法路径分量即拒）
            smart_dl_core::session::output::sanitize_rel(n)
                .map_err(|e| DaemonError::InvalidSource(format!("name 非法: {e}")))?;
        }
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let detail = if name.is_some() { "set" } else { "cleared" };
            rec.task.metadata.name = name;
            rec.push_event("name_changed", Some(detail.into()));
        }
        self.autosave();
        Ok(())
    }

    /// 任务标签设置（E18）：**替换式**全量覆盖（请求携带的标签列表即为最终权威，
    /// 语义可预测）；`None`/空表 = 清除全部。显示/分组元数据——引擎无关零副作用
    /// （对齐 set_task_name 边界），持久化随 tasks.json（TaskMetadata serde default
    /// 兼容旧档案），入 `?tag=` 过滤与 `?search=` 语料。
    ///
    /// 归一化：逐个 trim → 丢空串 → 去重（保留首次出现序，大小写敏感——
    /// 标签匹配大小写不敏感但显示保留原样）→ 上限 16 个/单个 1..=64 字符
    ///（超限 400 InvalidSource，调用方可先归一化再展示）。
    pub fn set_task_tags(
        &self,
        id: &str,
        tags: Option<Vec<String>>,
    ) -> Result<Vec<String>, DaemonError> {
        let normalized = match tags {
            None => Vec::new(),
            Some(list) => {
                if list.len() > 16 {
                    return Err(DaemonError::InvalidSource(format!(
                        "标签数量超上限 16（实际 {}）",
                        list.len()
                    )));
                }
                let mut out: Vec<String> = Vec::new();
                for t in list {
                    let t = t.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if t.chars().count() > 64 {
                        return Err(DaemonError::InvalidSource(format!("标签超 64 字符: {t:?}")));
                    }
                    if !out.iter().any(|e| e == t) {
                        out.push(t.to_string());
                    }
                }
                out
            }
        };
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let detail = if normalized.is_empty() {
                "cleared"
            } else {
                "set"
            };
            rec.task.metadata.tags = normalized.clone();
            rec.push_event("tags_changed", Some(detail.into()));
        }
        self.autosave();
        Ok(normalized)
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
            let (engine, tid, prios) = {
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
                match self.engine_for(rec.engine_kind) {
                    Ok(e) => (e, tid, prios),
                    Err(_) => continue, // 引擎暂不可用：下轮再试
                }
            };
            // 就绪性探测：readback 成功且非空 = metadata 已就绪
            match engine.file_priorities(&tid).await {
                Ok(cur) if !cur.is_empty() => {
                    let pairs: Vec<(usize, u32)> =
                        prios.iter().enumerate().map(|(i, p)| (i, *p)).collect();
                    match engine.set_file_priorities(&tid, &pairs).await {
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
        let mut added = 0usize;
        for url in urls {
            engine
                .add_url_seed(&tid, url)
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
        engine
            .list_trackers(&tid)
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
        engine
            .add_trackers(&tid, urls)
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
        engine
            .remove_tracker(&tid, url)
            .await
            .map_err(|e| match e {
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

    /// 删除任务（E7 前 semantics：保留已下载数据）。
    pub async fn remove(&self, id: &str) -> Result<(), DaemonError> {
        self.remove_with(id, false).await
    }

    /// 删除任务 + 数据处置开关（E7）：`delete_data = true` 时引擎侧同步删除
    /// 已下载数据（BT 删种子数据 / HTTP 删落盘文件）。引擎删除失败不阻塞
    /// 记录移除（引擎 remove 本就是尽力而为——任务可能已不在引擎侧）。
    pub async fn remove_with(&self, id: &str, delete_data: bool) -> Result<(), DaemonError> {
        let rec = self
            .tasks
            .lock()
            .remove(id)
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        if let Some(tid) = rec.engine_tid {
            if let Ok(engine) = self.engine_for(rec.engine_kind) {
                let _ = engine.remove(&tid, delete_data).await;
            }
        }
        self.autosave();
        Ok(())
    }

    /// Provider 运行态快照（健康/配额/冷却）。
    pub fn provider_status(&self) -> Vec<(String, ProviderRuntime)> {
        self.providers
            .iter()
            .map(|p| (p.name().to_string(), p.runtime()))
            .collect()
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
        if let (Some(tid), Ok(bt)) = (&rec.engine_tid, self.engine_for(EngineKind::Bt)) {
            let _ = bt.remove(tid, false).await;
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

    /// 任务操作日志（`GET /tasks/:id/logs`）：快照 + 事件序列。
    pub fn task_logs(&self, id: &str) -> Result<serde_json::Value, DaemonError> {
        let tasks = self.tasks.lock();
        let rec = tasks
            .get(id)
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        Ok(serde_json::json!({
            "task_id": rec.task.id,
            "state": state_label(&rec.task.state),
            "source": rec.task.source.redacted_debug(),
            "error": rec.engine_status.as_ref().and_then(|s| s.error.clone()),
            "events": rec.events,
        }))
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
                    EngineKind::Http | EngineKind::Ftp => matches!(
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
                let mut tasks = self.tasks.lock();
                if let Some(rec) = tasks.get_mut(&id) {
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
                    }
                }
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
        // 目标父目录（rel_path 可能含子目录）
        if let Some(rel) = &name {
            if let Some(parent) = dest_root.join(rel).parent() {
                let _ = std::fs::create_dir_all(parent);
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
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
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
    fs::create_dir_all(&p)
        .map_err(|e| DaemonError::InvalidSource(format!("目标目录不可创建: {e}")))?;
    // 2) 白名单校验：canonicalize 后比对前缀（同时拦截 symlink 指向白名单外）
    if !allowed_roots.is_empty() {
        let cp = p
            .canonicalize()
            .map_err(|e| DaemonError::InvalidSource(format!("目标目录规范化失败: {e}")))?;
        let inside = allowed_roots.iter().any(|r| {
            // root 不存在则先建（首启场景 root == dest 本身，上一步已建好）
            let _ = fs::create_dir_all(r);
            match r.canonicalize() {
                Ok(cr) => cp.starts_with(&cr),
                Err(_) => false,
            }
        });
        if !inside {
            return Err(DaemonError::InvalidSource(format!(
                "dest 越界（不在允许的下载根目录内）: {raw}"
            )));
        }
    }
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
    use smart_dl_core::session::output::{evaluate_disk, DiskCheck};
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

/// 引擎状态 → 对外任务状态（快照实时化；元数据获取中归入 Downloading）。
fn engine_state_to_task(st: &EngineState, kind: EngineKind) -> TaskState {
    match st {
        EngineState::MetadataPending | EngineState::Downloading => TaskState::Downloading(kind),
        EngineState::Paused => TaskState::Paused,
        EngineState::Completed => TaskState::Completed,
        EngineState::Seeding => TaskState::Seeding,
        EngineState::Error => TaskState::Failed,
    }
}
