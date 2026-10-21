//! M3: 磁盘预检（§12 D36 分段公式）。
//! required = max(total×1.1, total + min(500MB, total))；不足拒绝入队。

use smart_dl_storage::{evaluate_disk, required_disk, DiskCheck};

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * MB;

#[test]
fn ten_mb_file_requires_twenty_mb() {
    // 10MB: max(11MB, 10MB+10MB) = 20MB
    assert_eq!(required_disk(10 * MB), 20 * MB);
}

#[test]
fn one_gb_file_requires_one_point_five_gb() {
    // 1GB: max(1.1GB, 1GB+500MB) = 1.5GB
    // 注：计划文档 §M3 写"1GB→1.1GB"，与 §12 公式不符；公式为唯一事实源 → 1.5GB
    assert_eq!(required_disk(GB), GB + 500 * MB);
}

#[test]
fn below_500mb_adds_whole_total() {
    // 400MB < 500MB → 额外留 total：400+400=800MB > 440MB
    assert_eq!(required_disk(400 * MB), 800 * MB);
}

#[test]
fn zero_size_requires_zero() {
    assert_eq!(required_disk(0), 0);
}

#[test]
fn sufficient_space_is_ok() {
    assert!(matches!(evaluate_disk(20 * MB, 10 * MB), DiskCheck::Ok));
}

#[test]
fn insufficient_space_reports_required_and_available() {
    match evaluate_disk(19 * MB, 10 * MB) {
        DiskCheck::Insufficient {
            required,
            available,
        } => {
            assert_eq!(required, 20 * MB);
            assert_eq!(available, 19 * MB);
        }
        other => panic!("期望 Insufficient，得到 {other:?}"),
    }
}
