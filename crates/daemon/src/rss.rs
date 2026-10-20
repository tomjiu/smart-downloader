//! RSS 订阅管理面（daemon 侧）。
//!
//! TD-4 建议 9 拆分后本文件只保留 `impl DaemonState` 管理面：订阅增删改/
//! 规则管理/周期刷新调度/命中自动建任务（复用 add_link_task_opts 全链）。
//! 纯逻辑（解析/匹配/状态类型/持久化）在 `smart-dl-rss` crate。

use smart_dl_rss::{item_matches, parse_feed, RssFeed, RssItem, RssRule, RssState};

use crate::state::{DaemonError, DaemonState};

impl DaemonState {
    /// RSS 状态锁入口（字段在 DaemonState.rss，parking_lot Mutex）。
    pub(crate) fn rss_state(&self) -> &parking_lot::Mutex<RssState> {
        &self.rss
    }

    /// RSS 持久化路径（persist_path 同目录 rss.json；None = 不落盘）。
    pub(crate) fn rss_persist_path(&self) -> Option<std::path::PathBuf> {
        self.rss_persist_path.clone()
    }

    fn rss_save(&self, st: &RssState) {
        if let Some(p) = self.rss_persist_path() {
            st.save(&p);
        }
    }

    /// P1-9 修复：锁外落盘辅助——克隆快照后释放调用方 guard 再序列化写盘。
    /// 原实现全部调用点持 rss_state 锁调 rss_save（整表 JSON 序列化 +
    /// 磁盘写毫秒级），并发 add/update/refresh/规则匹配全部串行排队。
    /// 调用方在临界区内收集变更后，用本方法在锁外落盘。
    fn rss_save_snapshot(&self, snapshot: &RssState) {
        self.rss_save(snapshot);
    }

    /// 添加订阅（立即拉取一次，失败即 Err——坏 URL 不应静默入列）。
    pub async fn rss_add_feed(&self, url: String) -> Result<(u64, String, usize), DaemonError> {
        let client = self.rss_client();
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| DaemonError::InvalidSource(format!("rss feed 拉取失败: {e}")))?;
        if !resp.status().is_success() {
            return Err(DaemonError::InvalidSource(format!(
                "rss feed 拉取 HTTP {}",
                resp.status()
            )));
        }
        // 审查修复（P2）：限长读取（与 rss_refresh_all 的 16MB 上限同口径），
        // 防恶意/超大 feed 撑爆内存（resp.text() 无上限）。
        const FEED_MAX: usize = 16 * 1024 * 1024;
        let mut xml_buf: Vec<u8> = Vec::new();
        let mut resp = resp;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| DaemonError::InvalidSource(format!("rss feed 读取失败: {e}")))?
        {
            if xml_buf.len() + chunk.len() > FEED_MAX {
                return Err(DaemonError::InvalidSource(
                    "rss feed 响应超过 16MB 上限".into(),
                ));
            }
            xml_buf.extend_from_slice(&chunk);
        }
        let xml = String::from_utf8_lossy(&xml_buf).into_owned();
        let parsed = parse_feed(&xml).map_err(DaemonError::InvalidSource)?;

        let (id, title_out, count, snapshot) = {
            let mut st = self.rss_state().lock();
            // 同 URL 订阅去重（409 语义由 handler 转）
            if st.feeds.iter().any(|f| f.url == url) {
                return Err(DaemonError::Duplicate(url));
            }
            st.next_feed_id += 1;
            let id = st.next_feed_id;
            let title = if parsed.title.is_empty() {
                url.clone()
            } else {
                parsed.title
            };
            let count = parsed.items.len();
            st.feeds.push(RssFeed {
                id,
                url,
                title,
                added_at_unix: now_unix(),
                last_refresh_unix: Some(now_unix()),
                interval_override_secs: 0,
                items: parsed.items,
            });
            let title_out = st.feeds.last().unwrap().title.clone();
            (id, title_out, count, st.clone())
        };
        // P1-9：锁外落盘（原锁内整表 JSON 写盘阻塞并发操作）
        self.rss_save_snapshot(&snapshot);
        Ok((id, title_out, count))
    }

    /// 更新订阅设置（batch5.1）：interval_override_secs（0 = 跟随全局）。
    /// 未知 id → Err NotFound。
    pub fn rss_update_feed(&self, id: u64, interval_override_secs: u64) -> Result<(), DaemonError> {
        let snapshot = {
            let mut st = self.rss_state().lock();
            let feed = st
                .feeds
                .iter_mut()
                .find(|f| f.id == id)
                .ok_or_else(|| DaemonError::NotFound(format!("feed {id} 不存在")))?;
            feed.interval_override_secs = interval_override_secs;
            st.clone()
        };
        self.rss_save_snapshot(&snapshot); // P1-9：锁外落盘
        Ok(())
    }

    /// 移除订阅（未知 id → false）。
    pub fn rss_remove_feed(&self, id: u64) -> bool {
        let (removed, snapshot) = {
            let mut st = self.rss_state().lock();
            let before = st.feeds.len();
            st.feeds.retain(|f| f.id != id);
            let removed = st.feeds.len() != before;
            (removed, st.clone())
        };
        if removed {
            self.rss_save_snapshot(&snapshot); // P1-9：锁外落盘
        }
        removed
    }

    /// 添加规则。
    #[allow(clippy::too_many_arguments)]
    pub fn rss_add_rule(
        &self,
        name: String,
        enabled: bool,
        must_contain: Vec<String>,
        must_not_contain: Vec<String>,
        feed_id: Option<u64>,
        tags: Vec<String>,
        dest: Option<String>,
        use_regex: bool,
        episode_filter: Option<String>,
    ) -> Result<u64, DaemonError> {
        // batch5：use_regex 时逐条编译校验（非法正则 add 即拒，避免静默不命中）
        if use_regex {
            for k in &must_contain {
                if regex::Regex::new(&format!("(?i){}", k.trim())).is_err() {
                    return Err(DaemonError::InvalidSource(format!(
                        "正则非法（must_contain）: {k:?}"
                    )));
                }
            }
            for k in &must_not_contain {
                if regex::Regex::new(&format!("(?i){}", k.trim())).is_err() {
                    return Err(DaemonError::InvalidSource(format!(
                        "正则非法（must_not_contain）: {k:?}"
                    )));
                }
            }
        }
        if name.trim().is_empty() {
            return Err(DaemonError::InvalidSource("规则名不可为空".into()));
        }
        if must_contain.is_empty() && must_not_contain.is_empty() {
            return Err(DaemonError::InvalidSource(
                "规则至少需要一个关键词（must_contain / must_not_contain 之一）".into(),
            ));
        }
        // batch6-P2：空白关键词拒绝——trim 后空串的 contains("") 恒 true，
        // must_contain 空白 = 匹配一切，must_not_contain 空白 = 排除一切（语义翻转）。
        for k in must_contain.iter().chain(must_not_contain.iter()) {
            if k.trim().is_empty() {
                return Err(DaemonError::InvalidSource(
                    "关键词不可为空白（空串会恒命中/恒排除）".into(),
                ));
            }
        }
        let (id, snapshot) = {
            let mut st = self.rss_state().lock();
            if let Some(fid) = feed_id {
                if !st.feeds.iter().any(|f| f.id == fid) {
                    return Err(DaemonError::InvalidSource(format!("feed_id {fid} 不存在")));
                }
            }
            st.next_rule_id += 1;
            let id = st.next_rule_id;
            st.rules.push(RssRule {
                id,
                name: name.trim().to_string(),
                enabled,
                must_contain,
                must_not_contain,
                feed_id,
                tags,
                dest,
                use_regex,
                episode_filter,
            });
            (id, st.clone())
        };
        self.rss_save_snapshot(&snapshot); // P1-9：锁外落盘
        Ok(id)
    }

    /// 移除规则（未知 id → false）。
    pub fn rss_remove_rule(&self, id: u64) -> bool {
        let (removed, snapshot) = {
            let mut st = self.rss_state().lock();
            let before = st.rules.len();
            st.rules.retain(|r| r.id != id);
            let removed = st.rules.len() != before;
            (removed, st.clone())
        };
        if removed {
            self.rss_save_snapshot(&snapshot); // P1-9：锁外落盘
        }
        removed
    }

    /// 刷新全部订阅（手动入口，force=true 不做到期过滤）：拉取 → 条目合并
    ///（guid 去重）→ 规则匹配 → 自动建任务。
    ///
    /// 单 feed 拉取失败不整体失败（错误收集进返回 Vec）；返回
    /// `(新增条目总数, 命中并建任务数, task_ids, 错误列表)`。
    pub async fn rss_refresh_all(&self) -> (usize, usize, Vec<String>, Vec<String>) {
        self.rss_refresh_impl(true).await
    }

    /// 刷新到期订阅（batch5 每 feed 独立间隔；ticker 入口）：
    /// feed 自带 `interval_override_secs > 0` 且距上次刷新未到期 → 跳过。
    pub async fn rss_refresh_due(&self) -> (usize, usize, Vec<String>, Vec<String>) {
        self.rss_refresh_impl(false).await
    }

    /// 刷新实现（force=false 时按 feed 级间隔过滤）。
    /// 持有 `rss_refresh_gate` 串行化整轮刷新（batch6-P1）：手动刷新与
    /// ticker 并发（或 UI 双击）时，后到者排队而非基于同一未处理快照
    /// 各自建任务（同一条目重复下载）。
    async fn rss_refresh_impl(&self, force: bool) -> (usize, usize, Vec<String>, Vec<String>) {
        let _gate = self.rss_refresh_gate.lock().await;
        let client = self.rss_client();
        let mut new_items_total = 0usize;
        let mut matched = 0usize;
        let mut task_ids: Vec<String> = Vec::new();
        let mut errors: Vec<String> = Vec::new();

        let now = now_unix();
        let feed_urls: Vec<(u64, String)> = {
            let st = self.rss_state().lock();
            st.feeds
                .iter()
                .filter(|f| {
                    force
                        || f.interval_override_secs == 0
                        || f.last_refresh_unix
                            .map(|t| now.saturating_sub(t) >= f.interval_override_secs)
                            .unwrap_or(true)
                })
                .map(|f| (f.id, f.url.clone()))
                .collect()
        };

        for (feed_id, url) in feed_urls {
            let fetch = async {
                let resp = client
                    .get(&url)
                    .timeout(std::time::Duration::from_secs(30))
                    .send()
                    .await
                    .map_err(|e| format!("拉取失败: {e}"))?;
                if !resp.status().is_success() {
                    return Err(format!("HTTP {}", resp.status()));
                }
                // batch3-P2：限长读取（16MB 封顶，防恶意 feed 打爆内存）
                const FEED_MAX: usize = 16 * 1024 * 1024;
                let mut xml: Vec<u8> = Vec::new();
                let mut resp = resp;
                while let Some(chunk) = resp.chunk().await.map_err(|e| format!("读取失败: {e}"))?
                {
                    if xml.len() + chunk.len() > FEED_MAX {
                        return Err("feed 响应超过 16MB 上限".to_string());
                    }
                    xml.extend_from_slice(&chunk);
                }
                let xml = String::from_utf8_lossy(&xml).into_owned();
                parse_feed(&xml)
            };
            let parsed = match fetch.await {
                Ok(p) => p,
                Err(e) => {
                    errors.push(format!("feed #{feed_id} ({url}): {e}"));
                    continue;
                }
            };

            // 合并条目 + 标题锁定（首次空标题回填）+ 已处理条目上限截断
            let existing: Vec<RssItem> = {
                let mut st = self.rss_state().lock();
                let Some(feed) = st.feeds.iter_mut().find(|f| f.id == feed_id) else {
                    continue;
                };
                let known: std::collections::HashSet<String> =
                    feed.items.iter().map(|i| i.guid.clone()).collect();
                let fresh: Vec<RssItem> = parsed
                    .items
                    .into_iter()
                    .filter(|i| !known.contains(&i.guid))
                    .collect();
                new_items_total += fresh.len();
                feed.items.extend(fresh.clone());
                feed.last_refresh_unix = Some(now_unix());
                if feed.title.is_empty() && !parsed.title.is_empty() {
                    feed.title = parsed.title;
                }
                // 防膨胀：已处理条目保留最近 N 条（N=[rss]
                // max_processed_items_per_feed）。
                // P1-10 修复：未处理条目同样设上限（5000/feed）——原实现
                // 「未处理全保留」在无规则命中/规则停用的 feed 上单调增长
                // 撑爆内存（恶意/异常源可无限推条目）。超限丢弃最旧未处理
                // 条目：被丢弃的旧条目若再次出现于源会重复建任务，但 5000
                // 条未处理的积压本身已是异常态，内存防护优先。
                const MAX_UNPROCESSED_PER_FEED: usize = 5000;
                let cap = self.rss_max_processed_items();
                let mut processed: Vec<RssItem> = feed
                    .items
                    .iter()
                    .filter(|i| i.task_id.is_some())
                    .cloned()
                    .collect();
                let mut unprocessed: Vec<RssItem> = feed
                    .items
                    .iter()
                    .filter(|i| i.task_id.is_none())
                    .cloned()
                    .collect();
                if processed.len() > cap {
                    let keep = processed.split_off(processed.len() - cap);
                    processed = keep;
                }
                if unprocessed.len() > MAX_UNPROCESSED_PER_FEED {
                    let keep = unprocessed.split_off(unprocessed.len() - MAX_UNPROCESSED_PER_FEED);
                    unprocessed = keep;
                }
                feed.items = processed;
                feed.items.extend(unprocessed);
                feed.items.clone()
            };

            // 规则匹配（对全部未处理条目；含旧条目——新建规则能回溯未处理历史）。
            // 命中后同步标记本地快照（batch6-P1）：否则同一 feed 内多条规则
            // 命中同一条目时会重复建任务（违背首见规则生效语义）。
            let rules: Vec<RssRule> = {
                let st = self.rss_state().lock();
                st.rules.iter().filter(|r| r.enabled).cloned().collect()
            };
            let mut existing = existing;
            for rule in rules {
                if rule.feed_id.map(|fid| fid != feed_id).unwrap_or(false) {
                    continue;
                }
                for item in existing.iter_mut() {
                    if item.task_id.is_some() {
                        continue;
                    }
                    if !item_matches(&rule, &item.title) {
                        continue;
                    }
                    let opts = crate::state::AddHttpOpts {
                        name: Some(item.title.clone()),
                        ..Default::default()
                    };
                    match self
                        .add_link_task_opts(item.url.clone(), rule.dest.clone(), opts)
                        .await
                    {
                        Ok(task_id) => {
                            if !rule.tags.is_empty() {
                                let _ = self.set_task_tags(&task_id, Some(rule.tags.clone()));
                            }
                            // P1-9：锁内只改状态 + 克隆快照，锁外落盘
                            let snapshot = {
                                let mut st = self.rss_state().lock();
                                if let Some(f) = st.feeds.iter_mut().find(|f| f.id == feed_id) {
                                    if let Some(it) =
                                        f.items.iter_mut().find(|i| i.guid == item.guid)
                                    {
                                        it.task_id = Some(task_id.clone());
                                    }
                                }
                                st.clone()
                            };
                            self.rss_save_snapshot(&snapshot);
                            // 本地快照同步标记：后续规则跳过该条目
                            item.task_id = Some(task_id.clone());
                            matched += 1;
                            task_ids.push(task_id);
                        }
                        Err(e) => {
                            errors.push(format!(
                                "feed #{feed_id} 条目 {:?} 建任务失败: {e}",
                                item.title
                            ));
                        }
                    }
                }
            }
        }
        (new_items_total, matched, task_ids, errors)
    }

    /// RSS 拉取 client（bootstrap_client 同口径克隆；None 时裸建）。
    fn rss_client(&self) -> reqwest::Client {
        self.bootstrap_client_opt().unwrap_or_default()
    }

    /// 已处理条目保留上限（live_config 注入时取 [rss] 配置；否则默认 200）。
    /// 0 视为无效回落默认（batch6-P2）：cap=0 会在每次刷新清空全部已处理
    /// 去重标记 → 源 feed 仍列出这些条目 → 规则命中无限重下。
    fn rss_max_processed_items(&self) -> usize {
        self.rss_max_processed_items_opt()
            .filter(|&v| v > 0)
            .unwrap_or(200)
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod rss_daemon_tests {
    use super::*;

    #[test]
    fn rss_update_feed_interval_and_notfound() {
        use crate::state::state_tests::FakeEngine;
        use crate::state::DaemonState;
        use std::sync::Arc;

        let state = DaemonState::new(
            Arc::new(FakeEngine::new(smart_dl_core::types::EngineKind::Http)),
            vec![],
        );
        {
            let mut st = state.rss_state().lock();
            st.next_feed_id += 1;
            let fid = st.next_feed_id;
            st.feeds.push(RssFeed {
                id: fid,
                url: "https://example/rss.xml".into(),
                title: "t".into(),
                added_at_unix: 0,
                last_refresh_unix: None,
                interval_override_secs: 0,
                items: vec![],
            });
        }
        let id = 1;
        state.rss_update_feed(id, 300).unwrap();
        assert_eq!(
            state.rss_state().lock().feeds[0].interval_override_secs,
            300
        );
        assert!(state.rss_update_feed(id, 0).is_ok());
        assert_eq!(state.rss_state().lock().feeds[0].interval_override_secs, 0);
        assert!(
            state.rss_update_feed(99, 100).is_err(),
            "未知 id → NotFound"
        );
    }
}
