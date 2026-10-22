//! 完成回调域：任务完成事件统一出口（E17）、完成动作判定（Task 46 关机/休眠）、完成 Webhook。

use super::lifecycle::execute_completion_action;
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
        // Task 46：全部任务终态后的全局动作（qbit「下载完成后关机」对标）
        self.maybe_run_completion_action();
    }

    /// 完成动作判定（Task 46）：action != none 且未触发过 → 全任务终态检查。
    /// 空任务表不触发（无意义）；非终态任务存在（含 Paused/Queued/Seeding）
    /// 阻塞触发——qbit 同语义（暂停/做种中均算未完成）。触发后置位 fired。
    /// pub(crate)：由「任务完成事件」触发（batch5-P2 的热更补判已随 P0-10
    /// 修复移除——设置即执行是语义缺陷：全终态时配置 shutdown 会立刻关机）。
    ///
    /// batch7-P1（TOCTOU 根治）：终态判定与 fired 置位在同一 tasks 锁临界区；
    /// execute 的 exit 分支已改为独立线程延时退出（P1-8：原实现锁内
    /// sleep 250ms 阻塞全部 API，与「锁内亚毫秒」的注释自相矛盾——
    /// 审查模式 C），判定→执行间隙的新 add 由 fired 语义保证不重复触发。
    pub(crate) fn maybe_run_completion_action(&self) {
        let action = self.completion_action.lock().clone();
        if action == "none" || action.is_empty() {
            return;
        }
        if self
            .completion_fired
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_err()
        {
            return; // 本会话已触发过
        }
        let fired = {
            let tasks = self.tasks.lock();
            let all_terminal = !tasks.is_empty()
                && tasks.values().all(|r| {
                    matches!(
                        r.task.state,
                        TaskState::Completed | TaskState::Failed | TaskState::Stopped
                    )
                });
            if all_terminal {
                execute_completion_action(&action);
                true
            } else {
                false
            }
        };
        if !fired {
            // 尚有活跃/暂停/排队任务 → 复位旗标，下次完成事件再判
            self.completion_fired
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
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
        // batch5：单文件（move_completed_file）与目录（BT 多文件任务，
        // move_completed_dir）都支持；不存在 → 跳过；hook 仍照发
        //（webhook 同口径：通知尽力而为）
        let is_file = src.is_file();
        let is_dir = src.is_dir();
        let mut final_path = src.clone();

        // 1) 移动（conflict-skip 任务不动既有文件——尊重 skip 语义）
        if let Some(dst_dir) = &move_to {
            if conflict_skip {
                tracing::info!("post_download: 任务 {task_id} 为 conflict-skip，既有文件不移动");
            } else if !is_file && !is_dir {
                tracing::info!(
                    "post_download: 任务 {task_id} 落盘路径不存在（{:?}），移动跳过",
                    src
                );
            } else if is_dir {
                match Self::move_completed_dir(&src, dst_dir, &name) {
                    Ok(target) => {
                        tracing::info!("post_download: 任务 {task_id} 目录已移动 → {target:?}");
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
                        tracing::warn!("post_download: 任务 {task_id} 目录移动失败: {e}");
                        let mut tasks = self.tasks.lock();
                        if let Some(rec) = tasks.get_mut(task_id) {
                            rec.push_event("post_move", Some(format!("failed: {e}")));
                        }
                    }
                }
            } else if is_file {
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
                // batch8（P3）：剥离敏感环境变量再继承——hook 子进程此前
                // 可读到 SMART_DL_HTTP_TOKEN / SD_L1_TOKEN / SD_NAS_*，
                // 第三方 hook 脚本（或其依赖链）泄凭据面收窄。
                let mut clean: Vec<(String, String)> = std::env::vars()
                    .filter(|(k, _)| {
                        k != "SMART_DL_HTTP_TOKEN"
                            && !k.starts_with("SD_L1_")
                            && !k.starts_with("SD_NAS_")
                    })
                    .collect();
                clean.extend(envs);
                match std::process::Command::new(&hook_prog)
                    .env_clear()
                    .envs(clean)
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
}
