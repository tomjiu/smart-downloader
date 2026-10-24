//! rss crate 独立集成测试（#161 P3）。
//!
//! 内联单测（src/lib.rs `mod tests`）已覆盖解析矩阵（RSS2/Atom/CDATA/
//! guid 兜底去重/坏 XML）与关键词路径；daemon/tests/rss_api.rs 覆盖
//! HTTP e2e——但同样只走关键词路径。本文件补齐三个**零覆盖面**：
//!
//! 1. `episode_filter_matches`（batch5 集数过滤）语义矩阵：单集/区间/
//!    S-E 形态/分号多模式/多集标题/词边界，以及保守拒绝方向（非法
//!    token、跨季区间、逆序区间、空串——防误下整季）。
//! 2. `use_regex`（batch5 正则模式）：关键词即正则本体、(?i) 注入、
//!    非法正则保守不命中、排除列表、与集数过滤的 AND 组合。
//! 3. `RssState` 持久化：save→load 全字段往返、文件不存在静默空状态、
//!    坏 JSON 留存改名 `.json.corrupt` 后按空启动（batch6-P1 fail-open）。

use smart_dl_rss::{episode_filter_matches, item_matches, RssFeed, RssItem, RssRule, RssState};

fn rule(use_regex: bool, must: &[&str], not: &[&str], ep: Option<&str>) -> RssRule {
    RssRule {
        id: 1,
        name: "r".into(),
        enabled: true,
        must_contain: must.iter().map(|s| s.to_string()).collect(),
        must_not_contain: not.iter().map(|s| s.to_string()).collect(),
        feed_id: None,
        tags: vec![],
        dest: None,
        use_regex,
        episode_filter: ep.map(|s| s.to_string()),
    }
}

// ===== 1. 集数过滤语义矩阵 =====

#[test]
fn episode_single_and_range() {
    // 单集
    assert!(episode_filter_matches("1x02", "Show 1x02 1080p"));
    assert!(!episode_filter_matches("1x02", "Show 1x03 1080p"));
    // 闭区间：端点命中 + 中间命中 + 界外不命中
    assert!(episode_filter_matches("1x02-1x04", "Show 1x02"));
    assert!(episode_filter_matches("1x02-1x04", "Show 1x03"));
    assert!(episode_filter_matches("1x02-1x04", "Show 1x04"));
    assert!(!episode_filter_matches("1x02-1x04", "Show 1x05"));
    assert!(!episode_filter_matches("1x02-1x04", "Show 1x01"));
}

#[test]
fn episode_s_e_forms_case_insensitive() {
    // S/E 形态（标题大小写不敏感）
    assert!(episode_filter_matches("S02E03", "Show s02e03"));
    assert!(episode_filter_matches("S02E03", "SHOW S02E03"));
    assert!(!episode_filter_matches("S02E03", "Show S02E04"));
    // S/E 区间（同季）
    assert!(episode_filter_matches("S02E03-S02E06", "Show S02E05"));
    assert!(!episode_filter_matches("S02E03-S02E06", "Show S02E07"));
    // 混合形态 token（x 与 S/E 同串混用）
    assert!(episode_filter_matches("1x02;S01E01", "Show S01E01"));
    assert!(episode_filter_matches("1x02;S01E01", "Show 1x02"));
    assert!(!episode_filter_matches("1x02;S01E01", "Show 1x03"));
}

#[test]
fn episode_multi_token_semicolon() {
    assert!(episode_filter_matches("1x02;1x05", "Show 1x05"));
    assert!(episode_filter_matches("1x02;1x05", "Show 1x02"));
    assert!(!episode_filter_matches("1x02;1x05", "Show 1x03"));
    // token 周边空白容忍
    assert!(episode_filter_matches(" 1x02 ; 1x05 ", "Show 1x05"));
}

#[test]
fn episode_conservative_rejects() {
    // 非法 token → 一律不命中（保守，防误下整季）
    assert!(!episode_filter_matches("abc", "Show 1x02"));
    assert!(!episode_filter_matches("1x02;abc", "Show 1x02"));
    // 跨季区间不支持（保守拒绝）
    assert!(!episode_filter_matches("1x02-2x04", "Show 1x03"));
    // 逆序区间
    assert!(!episode_filter_matches("1x04-1x02", "Show 1x03"));
    // 空 / 纯分隔符 / 纯空白
    assert!(!episode_filter_matches("", "Show 1x02"));
    assert!(!episode_filter_matches(";;", "Show 1x02"));
    assert!(!episode_filter_matches("  ", "Show 1x02"));
    // 标题无集数可提取
    assert!(!episode_filter_matches("1x02", "Show season finale"));
}

#[test]
fn episode_word_boundaries_and_multi_episode_titles() {
    // 多集标题（batch6-P2 captures_iter）：任一集命中即通过
    assert!(episode_filter_matches("1x02", "S01E01 1x02 dual-episode"));
    assert!(episode_filter_matches("S01E01", "S01E01 1x02 dual-episode"));
    // 词边界：CASSEROLE 不含合法 S..E.. 数字结构（注释锚例）
    assert!(!episode_filter_matches("S01E01", "CASSEROLE recipe"));
    // x 形态要求组前导非字母数字（防误吸）："v21x011" 的 21 前是字母 v
    // → 整体提取不出 (21,011)，两种过滤串都不命中
    assert!(!episode_filter_matches("1x01", "v21x011"));
    assert!(!episode_filter_matches("21x011", "v21x011"));
    // 前导为空格/词首时 (21,011) 正常提取
    assert!(episode_filter_matches("21x011", "ver 21x011"));
}

// ===== 2. use_regex 路径 =====

#[test]
fn regex_mode_basic_and_case_insensitive() {
    let r = rule(true, &["ubuntu.*24"], &[], None);
    assert!(item_matches(&r, "Ubuntu 24.04 Desktop amd64"));
    assert!(item_matches(&r, "UBUNTU 24.04")); // (?i) 由实现注入
    assert!(!item_matches(&r, "Ubuntu 23.10"));
    assert!(!item_matches(&r, "Debian 24"));
}

#[test]
fn regex_mode_invalid_pattern_never_matches() {
    // 非法正则按不命中处理（unwrap_or(false) 保守路径）
    let bad = rule(true, &["["], &[], None);
    assert!(!item_matches(&bad, "anything"));
    // must_contain 含一条非法正则 → 全体不命中（all 语义）
    let mixed = rule(true, &["ubuntu", "["], &[], None);
    assert!(!item_matches(&mixed, "Ubuntu 24.04"));
}

#[test]
fn regex_mode_exclude_list() {
    let r = rule(true, &["show"], &["beta|alpha"], None);
    assert!(item_matches(&r, "Show 1x02"));
    assert!(!item_matches(&r, "Show 1x02 BETA"));
    assert!(!item_matches(&r, "Show 1x02 Alpha"));
}

#[test]
fn regex_mode_combines_with_episode_filter_as_and() {
    // 关键词（正则）与集数过滤为 AND 组合
    let r = rule(true, &["show.*\\d"], &[], Some("1x02-1x04"));
    assert!(item_matches(&r, "Show 1x03 1080p"));
    assert!(!item_matches(&r, "Show 1x09 1080p")); // 集数界外
    assert!(!item_matches(&r, "Other 1x03 1080p")); // 关键词未命中
}

#[test]
fn keyword_mode_unaffected_by_episode_empty() {
    // 回归锚：episode_filter 为 None/空串 = 不过滤（关键词路径不受影响）
    let r = rule(false, &["ubuntu"], &[], None);
    assert!(item_matches(&r, "Ubuntu 24.04"));
    let r_empty_ep = rule(false, &["ubuntu"], &[], Some("  "));
    assert!(item_matches(&r_empty_ep, "Ubuntu 24.04"));
}

// ===== 3. RssState 持久化 =====

fn state_with_all_fields() -> RssState {
    RssState {
        next_feed_id: 7,
        next_rule_id: 11,
        feeds: vec![RssFeed {
            id: 3,
            url: "https://example.com/feed.xml?token=abc".into(),
            title: "Example Feed".into(),
            added_at_unix: 1_760_000_000,
            last_refresh_unix: Some(1_760_000_360),
            interval_override_secs: 900,
            items: vec![RssItem {
                guid: "g-1".into(),
                title: "Ubuntu 24.04 iso".into(),
                url: "https://example.com/u.iso".into(),
                pub_date: Some("Tue, 01 Sep 2026 10:00:00 GMT".into()),
                task_id: Some("task-42".into()),
            }],
        }],
        rules: vec![rule(false, &["ubuntu"], &["beta"], Some("1x02-1x04"))],
    }
}

#[test]
fn rss_state_save_load_roundtrip() {
    let dir = std::env::temp_dir().join(format!(
        "rss-it-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rss.json");

    let st = state_with_all_fields();
    st.save(&path);
    let back = RssState::load(&path).expect("save 后 load 必须成功");
    assert_eq!(back.next_feed_id, 7);
    assert_eq!(back.next_rule_id, 11);
    assert_eq!(back.feeds.len(), 1);
    assert_eq!(back.feeds[0].url, "https://example.com/feed.xml?token=abc");
    assert_eq!(back.feeds[0].interval_override_secs, 900);
    assert_eq!(back.feeds[0].items.len(), 1);
    assert_eq!(back.feeds[0].items[0].task_id.as_deref(), Some("task-42"));
    assert_eq!(back.rules.len(), 1);
    assert_eq!(back.rules[0].episode_filter.as_deref(), Some("1x02-1x04"));

    // 重复 save 幂等（旧文件被原子替换，不残留 tmp）
    st.save(&path);
    assert!(RssState::load(&path).is_some());
    let leftovers: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "save 后不应残留 .tmp 文件");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rss_state_load_missing_file_is_silent_empty() {
    // 首次启动：文件不存在 → None（上层按空状态初始化，不报错）
    let dir = std::env::temp_dir().join(format!(
        "rss-it-missing-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    assert!(RssState::load(&dir.join("nope.json")).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rss_state_load_corrupt_file_preserved_and_fail_open() {
    // batch6-P1：坏 JSON 不得静默覆盖销毁——留存改名 .json.corrupt 后
    // 按空状态启动（fail-open）。
    let dir = std::env::temp_dir().join(format!(
        "rss-it-corrupt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rss.json");
    std::fs::write(&path, "{ this is not valid json").unwrap();

    assert!(RssState::load(&path).is_none(), "坏 JSON 按空状态启动");
    let preserved = path.with_extension("json.corrupt");
    assert!(preserved.exists(), "坏文件必须留存改名（用户数据唯一证据）");
    assert_eq!(
        std::fs::read_to_string(&preserved).unwrap(),
        "{ this is not valid json",
        "留存内容与原文件一致"
    );

    // 再次 load：原路径已不存在 → 静默空状态（不重复报 corrupt）
    assert!(RssState::load(&path).is_none());

    let _ = std::fs::remove_dir_all(&dir);
}
