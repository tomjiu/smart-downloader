//! M4a: 静态分块规划（§14 D11/D25）——段数公式与全文件覆盖。
//! 说明：P0 起 HTTP 主路径改用动态分段（SegmentManager），
//! 静态规划仅保留 segment_count（worker 数公式）与 plan_segments（FTP 串行下载用）。

use smart_dl_httpdl::static_split::{plan_segments, segment_count};

const MB: u64 = 1024 * 1024;

#[test]
fn plan_100mb_is_2_segments() {
    // 100MB/64MB = 1.56 → floor 1 → clamp 2
    assert_eq!(segment_count(100 * MB), 2);
    assert_eq!(plan_segments(100 * MB).len(), 2);
}

#[test]
fn plan_1gb_is_8_segments() {
    // 1024MB/64MB = 16 → clamp 8。
    // 注：计划文档 §M4 写"1GB→4"，与设计文档 §14 公式 N=clamp(size/64MB,2,8) 冲突；公式为准。
    assert_eq!(segment_count(1024 * MB), 8);
    assert_eq!(plan_segments(1024 * MB).len(), 8);
}

#[test]
fn plan_10gb_capped_at_8() {
    assert_eq!(segment_count(10 * 1024 * MB), 8);
}

#[test]
fn plan_small_file_floor_is_2() {
    // 10MB < 64MB → floor 0 → clamp 下限 2
    assert_eq!(segment_count(10 * MB), 2);
    assert_eq!(plan_segments(10 * MB).len(), 2);
}

#[test]
fn segments_cover_file_exactly() {
    // 不相交 + 覆盖全文件 + 连续
    for size in [
        1u64,
        10 * MB,
        100 * MB,
        1024 * MB,
        10 * 1024 * MB,
        7 * 1024 * 1024 * MB,
    ] {
        let segs = plan_segments(size);
        assert_eq!(segs.first().unwrap().start, 0);
        assert_eq!(segs.last().unwrap().end, size - 1);
        for w in segs.windows(2) {
            assert_eq!(w[0].end + 1, w[1].start, "段必须连续（无重叠无空洞）");
        }
    }
}

#[test]
fn zero_size_plan_is_empty() {
    assert!(plan_segments(0).is_empty());
}
