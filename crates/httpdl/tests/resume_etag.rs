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
    }
}

fn ledger(total: u64, min_split: u64, etag: &str, done: Vec<(u64, u64)>) -> Ledger {
    Ledger {
        version: LEDGER_VERSION,
        total,
        min_split,
        etag: Some(etag.to_string()),
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
fn either_side_missing_etag_still_resumes() {
    // 双方任一缺 ETag：无法证明内容变化 → 按 Range 语义继续（不因服务器
    // 不发/开始发 ETag 而误杀续传）
    let mut l = ledger(4096, 1024, "", vec![(0, 1023)]);
    l.etag = None;
    let d = decide(4096, Some(&l), &probe(true, Some("fresh-etag"), 4096));
    assert!(matches!(d, ResumeDecision::Resume { .. }));

    let l2 = ledger(4096, 1024, "saved-etag", vec![(0, 1023)]);
    let d2 = decide(4096, Some(&l2), &probe(true, None, 4096));
    assert!(matches!(d2, ResumeDecision::Resume { .. }));
}
