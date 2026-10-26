//! 查询域：任务快照/列表/过滤/统计/速率采样/provider 状态/任务日志。

use super::*;
impl DaemonState {
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
            max_connections: rec.task.max_connections,
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

    /// 过滤 + 排序 + 分页列表（E7 + R13 排序下推）：
    /// - 基础排序：task_id 数值后缀升序（创建序；HashMap 迭代序不稳定，分页必须
    ///   确定性排序。task_id = `t{u64}` 自增序号，parse 失败兜底排序键 u64::MAX）。
    /// - R13 用户排序（`q.sort`）：在过滤后、分页前应用（先排序后切片，
    ///   分页语义才成立）；平局以创建序决出（确定性：同输入同输出，
    ///   不依赖 HashMap 迭代序/Rust sort 稳定性）。`q.order` 反转主键比较，
    ///   平局决出不受反转影响（升序平局 = 创建序，降序平局 = 创建序）。
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
        // R13：用户排序（过滤后、分页前）。created 键与 order 组合在
        // sort_records 内统一处理（含「仅 order=desc 无 sort」的最新优先语义）。
        let mut filtered = filtered;
        sort_records(&mut filtered, q);
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
        let (sd, su) = *self.session_traffic.lock();
        st.session_down_bytes = sd;
        st.session_up_bytes = su;
        st
    }

    /// 会话累计流量累加（batch5 对标 qB 会话统计）：按引擎缓存速率 × 轮询
    /// 间隔估算（down/up 各自 saturating_add）。由状态轮询循环每轮调用——
    /// 估算口径：速率快照为轮询期代表值，间隔恒定故累计曲线与真实流量
    /// 同形状；重启清零（会话语义）。
    pub(crate) fn accumulate_session_traffic(&self, interval: std::time::Duration) {
        let tasks = self.tasks.lock();
        let mut d = 0u64;
        let mut u = 0u64;
        for rec in tasks.values() {
            if let Some(s) = &rec.engine_status {
                d += s.down_rate;
                u += s.up_rate;
            }
        }
        drop(tasks);
        let secs = interval.as_secs_f64();
        let mut t = self.session_traffic.lock();
        t.0 = t.0.saturating_add((d as f64 * secs) as u64);
        t.1 = t.1.saturating_add((u as f64 * secs) as u64);
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

    /// Provider 运行态快照（健康/配额/冷却）。
    pub fn provider_status(&self) -> Vec<(String, ProviderRuntime)> {
        self.providers
            .iter()
            .map(|p| (p.name().to_string(), p.runtime()))
            .collect()
    }

    /// 任务操作日志（`GET /tasks/:id/logs`）：快照 + 事件序列。
    pub fn task_logs(&self, id: &str) -> Result<crate::state::TaskLogs, DaemonError> {
        let tasks = self.tasks.lock();
        let rec = tasks
            .get(id)
            .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
        Ok(crate::state::TaskLogs {
            task_id: rec.task.id.clone(),
            state: state_label(&rec.task.state),
            source: rec.task.source.redacted_debug(),
            error: rec.engine_status.as_ref().and_then(|s| s.error.clone()),
            // 记录级 events 克隆（响应即终值快照，无需持有锁的引用返回）。
            events: rec.events.clone(),
        })
    }
}
/// 引擎状态 → 对外任务状态（快照实时化；元数据获取中归入 Downloading）。
pub(super) fn engine_state_to_task(st: &EngineState, kind: EngineKind) -> TaskState {
    match st {
        EngineState::MetadataPending | EngineState::Downloading => TaskState::Downloading(kind),
        EngineState::Paused => TaskState::Paused,
        EngineState::Completed => TaskState::Completed,
        EngineState::Seeding => TaskState::Seeding,
        EngineState::Error => TaskState::Failed,
    }
}

/// R13：对过滤后的记录集应用用户排序（原地）。入口前 `filtered` 已是创建序。
///
/// - `sort=None, order=Asc`：无操作（既有行为，零开销路径）。
/// - `sort=None, order=Desc`：创建序倒序（最新优先——「仅给 order」的便利语义）。
/// - `sort=Some(key)`：主键比较 + 创建序平局决出；`order=Desc` 只反转主键
///   （平局决出恒为创建序，保证升/降序下同键同值的相对序稳定可复现）。
fn sort_records(filtered: &mut [&TaskRecord], q: &ListQuery) {
    let key = match q.sort {
        None => {
            if q.order == ListOrder::Desc {
                // 最新优先：直接反转创建序（含 parse 失败兜底项的确定性）。
                filtered.reverse();
            }
            return;
        }
        Some(k) => k,
    };
    filtered.sort_by(|a, b| {
        let primary = match key {
            ListSort::Created => creation_key(a).cmp(&creation_key(b)),
            ListSort::Name => display_name_key(a).cmp(&display_name_key(b)),
            ListSort::State => state_label(&a.task.state).cmp(&state_label(&b.task.state)),
            ListSort::Progress => progress_key(a)
                .partial_cmp(&progress_key(b))
                .unwrap_or(std::cmp::Ordering::Equal),
            ListSort::Size => size_key(a).cmp(&size_key(b)),
            ListSort::Speed => speed_key(a).cmp(&speed_key(b)),
            ListSort::Eta => eta_key(a).cmp(&eta_key(b)),
        };
        let primary = if q.order == ListOrder::Desc {
            primary.reverse()
        } else {
            primary
        };
        // 平局决出：创建序（不受 Desc 反转影响——见函数文档）。
        primary.then_with(|| creation_key(a).cmp(&creation_key(b)))
    });
}

/// 创建序键：task_id 数值后缀（`t{u64}`；parse 失败兜底 u64::MAX，
/// 与 list_filtered 基础排序同口径）。
fn creation_key(rec: &TaskRecord) -> u64 {
    rec.task
        .id
        .strip_prefix('t')
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(u64::MAX)
}

/// 名称排序键：展示名小写（显式名 → 来源 URL 末段派生，与 UI displayName
/// 同链；服务端 Unicode 简单小写，不依赖 locale）。
fn display_name_key(rec: &TaskRecord) -> String {
    let name = rec
        .task
        .metadata
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match name {
        Some(n) => n.to_lowercase(),
        None => {
            // 与 UI deriveNameFromSource 对齐：URL 末段（去 query/fragment）。
            // 语料用 search_urls（脱敏 URL——凭据/userinfo 不参与排序键，
            // 末段文件名不受脱敏影响）。
            let url = rec
                .task
                .source
                .search_urls()
                .into_iter()
                .next()
                .unwrap_or_default();
            let last = url.rsplit('/').next().unwrap_or("");
            let last = last.split(['?', '#']).next().unwrap_or("");
            if last.is_empty() {
                url.to_lowercase()
            } else {
                last.to_lowercase()
            }
        }
    }
}

/// 进度排序键：done/total 比值（f64；total=0 或无快照 = 0.0）。
fn progress_key(rec: &TaskRecord) -> f64 {
    match &rec.engine_status {
        Some(s) if s.total > 0 => s.total_done as f64 / s.total as f64,
        _ => 0.0,
    }
}

/// 大小排序键：total 字节（无快照 = 0）。
fn size_key(rec: &TaskRecord) -> u64 {
    rec.engine_status.as_ref().map(|s| s.total).unwrap_or(0)
}

/// 速率排序键：down_rate（无快照 = 0；非活跃任务缓存已清零）。
fn speed_key(rec: &TaskRecord) -> u64 {
    rec.engine_status.as_ref().map(|s| s.down_rate).unwrap_or(0)
}

/// ETA 排序键：剩余秒数。无速率/已完成/无快照 = u64::MAX（升序排末尾，
/// 与 UI Infinity 语义一致；降序时自然到头部）。
fn eta_key(rec: &TaskRecord) -> u64 {
    match &rec.engine_status {
        Some(s) if s.total > 0 && s.total_done < s.total && s.down_rate > 0 => {
            (s.total - s.total_done) / s.down_rate
        }
        _ => u64::MAX,
    }
}
