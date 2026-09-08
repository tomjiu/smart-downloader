//! P4: 段账本续传决策（ledger::decide）。
//! 决策矩阵：账本合法+ETag 匹配 → 恢复；ETag 失配 / 无账本 / total 失配 /
//! part 超长 / 不支持 Range / 段非法 → 作废重下。
//! （旧版 decide_resume 的"失配仍试探续传"语义已被否决——G2 混合内容缺陷。）

use smart_dl_httpdl::ledger::{decide, Ledger, ResumeDecision, LEDGER_VERSION};
use smart_dl_httpdl::range::Probe;

fn probe(range: bool, etag: Option<&str>, total: u64) -> Probe {
    Probe {
        range_supported: range,
        etag: etag.map(str::to_string),
        total: Some(total),
        last_modified: None,
        filename: None,
        content_type: None,
    }
}

fn ledger(total: u64, min_split: u64, etag: &str, done: Vec<(u64, u64)>) -> Ledger {
    Ledger {
        version: LEDGER_VERSION,
        total,
        min_split,
        etag: Some(etag.to_string()),
        last_modified: None,
        done,
    }
}

#[test]
fn ledger_match_resumes_with_done_and_granularity() {
    let l = ledger(4096, 1024, "etag-1", vec![(0, 1023)]);
    let d = decide(4096, Some(&l), &probe(true, Some("etag-1"), 4096));
    assert_eq!(
        d,
        ResumeDecision::Resume {
            done: vec![(0, 1023)],
            min_split: 1024
        },
        "账本 + ETag 一致 → 恢复已完成段并沿用账本粒度"
    );
}

#[test]
fn etag_mismatch_restarts() {
    // ETag 变了 = 远端内容变化证据 → 作废重下（G2 修复；不再"试探续传"）
    let l = ledger(4096, 1024, "etag-1", vec![(0, 1023)]);
    let d = decide(4096, Some(&l), &probe(true, Some("etag-v2"), 4096));
    assert_eq!(d, ResumeDecision::Restart, "ETag 失配必须重下");
}

#[test]
fn missing_ledger_restarts_even_when_part_len_matches() {
    // G1 核心回归：预分配 .part 长度恒等于 total，无账本 = 无法区分
    // "真完成"与"稀疏空洞"→ 一律重下
    let d = decide(100, None, &probe(true, Some("etag-1"), 100));
    assert_eq!(d, ResumeDecision::Restart);
}

#[test]
fn part_longer_than_file_restarts() {
    let l = ledger(4096, 1024, "etag-1", vec![]);
    let d = decide(6144, Some(&l), &probe(true, Some("etag-1"), 4096));
    assert_eq!(
        d,
        ResumeDecision::Restart,
        "part 比文件还长（源变小）→ 重下"
    );
}

#[test]
fn total_mismatch_restarts() {
    let l = ledger(8192, 1024, "etag-1", vec![(0, 1023)]);
    let d = decide(4096, Some(&l), &probe(true, Some("etag-1"), 4096));
    assert_eq!(d, ResumeDecision::Restart, "账本 total 与探测不一致 → 重下");
}

#[test]
fn range_unsupported_restarts() {
    // 200 探测：服务器忽略 Range，段下载（强制 206）无法进行
    let l = ledger(4096, 1024, "etag-1", vec![(0, 1023)]);
    let d = decide(4096, Some(&l), &probe(false, Some("etag-1"), 4096));
    assert_eq!(d, ResumeDecision::Restart);
}

#[test]
fn tampered_segments_restart() {
    // 篡改账本：未对齐段 → 校验失败 → 重下（绝不信任来路不明的"已完成"声明）
    let l = ledger(4096, 1024, "etag-1", vec![(7, 1023)]);
    let d = decide(4096, Some(&l), &probe(true, Some("etag-1"), 4096));
    assert_eq!(d, ResumeDecision::Restart);
}

#[test]
fn ledger_without_etag_and_fresh_has_one_still_resumes() {
    // 账本本就无 ETag（add 时服务器未发）：无从核对 → 放行，
    // 其余防线（total/段对齐/Range）仍把关
    let mut l = ledger(4096, 1024, "", vec![(0, 1023)]);
    l.etag = None;
    let d = decide(4096, Some(&l), &probe(true, Some("fresh-etag"), 4096));
    assert!(matches!(d, ResumeDecision::Resume { .. }));
}

#[test]
fn etag_disappeared_after_saved_restarts() {
    // E26 加固语义：账本存有指纹、本次探测指纹消失 → 无法确认服务器文件
    // 未变 → 宁枉勿纵，作废重下（错误续传产出混合文件，代价远高于重下）
    let l = ledger(4096, 1024, "saved-etag", vec![(0, 1023)]);
    let d = decide(4096, Some(&l), &probe(true, None, 4096));
    assert_eq!(d, ResumeDecision::Restart);
}

// ==================== E26：Last-Modified 备援指纹核对 ====================

fn probe_lm(range: bool, etag: Option<&str>, lm: Option<&str>, total: u64) -> Probe {
    Probe {
        range_supported: range,
        etag: etag.map(str::to_string),
        last_modified: lm.map(str::to_string),
        total: Some(total),
        filename: None,
        content_type: None,
    }
}

fn ledger_lm(
    total: u64,
    min_split: u64,
    etag: Option<&str>,
    lm: Option<&str>,
    done: Vec<(u64, u64)>,
) -> Ledger {
    Ledger {
        version: LEDGER_VERSION,
        total,
        min_split,
        etag: etag.map(str::to_string),
        last_modified: lm.map(str::to_string),
        done,
    }
}

#[test]
fn last_modified_mismatch_restarts() {
    // E26 主例：服务器无 ETag 场景，Last-Modified 失配 = 内容已变 → 重下
    let l = ledger_lm(
        4096,
        1024,
        None,
        Some("Mon, 01 Jan 2026 00:00:00 GMT"),
        vec![(0, 1023)],
    );
    let d = decide(
        4096,
        Some(&l),
        &probe_lm(true, None, Some("Tue, 02 Jan 2026 00:00:00 GMT"), 4096),
    );
    assert_eq!(d, ResumeDecision::Restart, "Last-Modified 失配必须重下");
}

#[test]
fn last_modified_match_resumes() {
    // 双指纹均等 → 续传（确认服务器文件未变）
    let l = ledger_lm(
        4096,
        1024,
        Some("etag-1"),
        Some("Mon, 01 Jan 2026 00:00:00 GMT"),
        vec![(0, 1023)],
    );
    let d = decide(
        4096,
        Some(&l),
        &probe_lm(
            true,
            Some("etag-1"),
            Some("Mon, 01 Jan 2026 00:00:00 GMT"),
            4096,
        ),
    );
    assert!(matches!(d, ResumeDecision::Resume { .. }));
}

#[test]
fn last_modified_disappeared_restarts() {
    // E26 加固：账本有 Last-Modified、探测消失 → 宁枉勿纵重下
    let l = ledger_lm(
        4096,
        1024,
        None,
        Some("Mon, 01 Jan 2026 00:00:00 GMT"),
        vec![(0, 1023)],
    );
    let d = decide(4096, Some(&l), &probe_lm(true, None, None, 4096));
    assert_eq!(d, ResumeDecision::Restart);
}

#[test]
fn ledger_without_last_modified_but_fresh_has_one_resumes() {
    // 账本本就无 Last-Modified（旧账本/服务器当时未发）：无从核对 → 放行
    let l = ledger_lm(4096, 1024, Some("etag-1"), None, vec![(0, 1023)]);
    let d = decide(
        4096,
        Some(&l),
        &probe_lm(
            true,
            Some("etag-1"),
            Some("Mon, 01 Jan 2026 00:00:00 GMT"),
            4096,
        ),
    );
    assert!(matches!(d, ResumeDecision::Resume { .. }));
}

#[test]
fn etag_ok_but_last_modified_mismatch_restarts() {
    // 双指纹独立核对：etag 相等不豁免 last_modified 失配
    let l = ledger_lm(
        4096,
        1024,
        Some("etag-1"),
        Some("Mon, 01 Jan 2026 00:00:00 GMT"),
        vec![(0, 1023)],
    );
    let d = decide(
        4096,
        Some(&l),
        &probe_lm(
            true,
            Some("etag-1"),
            Some("Tue, 02 Jan 2026 00:00:00 GMT"),
            4096,
        ),
    );
    assert_eq!(d, ResumeDecision::Restart);
}
