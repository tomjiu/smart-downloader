//! M3: .part 管理（§12 输出层）。
//! 命名 / 长度校验 / 完成落位（rename，跨盘 copy fallback + 删源）。

use smart_dl_core::session::output::{OutputError, OutputManager};
use std::fs;
use std::path::PathBuf;

#[test]
fn part_path_appends_dot_part() {
    let om = OutputManager::new(PathBuf::from("D:/dl"));
    assert_eq!(
        om.part_path("a/b.bin").unwrap(),
        PathBuf::from("D:/dl/a/b.bin.part")
    );
}

// 安全回归（V3）：穿越路径在 part_path 入口即被拒绝。
#[test]
fn part_path_rejects_traversal() {
    let om = OutputManager::new(PathBuf::from("D:/dl"));
    assert!(om.part_path("../evil.bin").is_err());
    assert!(om.part_path("/etc/passwd").is_err());
    assert!(om.part_path("a/../../x").is_err());
}

#[test]
fn finalize_renames_part_to_destination() {
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    let rel = "movie.bin";
    let part = om.part_path(rel).unwrap();
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, b"1234567890").unwrap();

    om.finalize(rel, 10).unwrap();
    let dest = dir.path().join(rel);
    assert_eq!(fs::read(&dest).unwrap(), b"1234567890");
    assert!(!part.exists(), ".part 落位后应删除");
}

#[test]
fn finalize_size_mismatch_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    let part = om.part_path("m.bin").unwrap();
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, b"short").unwrap();

    match om.finalize("m.bin", 999) {
        Err(OutputError::SizeMismatch { expected, actual }) => {
            assert_eq!(expected, 999);
            assert_eq!(actual, 5);
        }
        other => panic!("期望 SizeMismatch，得到 {other:?}"),
    }
    assert!(part.exists(), "校验失败不得删除 .part");
}

#[test]
fn finalize_missing_part_is_error() {
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    assert!(matches!(
        om.finalize("ghost.bin", 10),
        Err(OutputError::PartMissing)
    ));
}

#[test]
fn finalize_idempotent_when_dest_matches() {
    // 完成信号重复 → 目标已存在且大小一致 → Ok，不重做
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    let dest = dir.path().join("done.bin");
    fs::write(&dest, b"done-data").unwrap();

    om.finalize("done.bin", 9).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"done-data");
}

#[test]
fn copy_fallback_copies_verifies_and_removes_part() {
    // 跨卷 rename 的环境无法在单盘 CI 注入 → fallback 逻辑独立测
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    let part = om.part_path("f2.bin").unwrap();
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, b"cross-volume-payload").unwrap();
    let dest = dir.path().join("f2.bin");

    om.copy_fallback(&part, &dest, 20).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"cross-volume-payload");
    assert!(!part.exists(), "fallback 成功应删源 .part");
}

#[test]
fn finalize_to_unwritable_dest_reports_io() {
    // rename/copy 双失败路径 → Io 错误（覆盖失败分支行）
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    let part = om.part_path("x.bin").unwrap();
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, b"data").unwrap();
    // dest 指向不存在的父目录 → rename 与 copy 都失败
    let dest = dir.path().join("no/such/dir/x.bin");

    assert!(matches!(
        om.finalize_to(&part, &dest, 4),
        Err(OutputError::Io(_))
    ));
    assert!(part.exists(), "失败时不得删除源 .part");
}

#[test]
fn finalize_idempotent_cleans_residual_part() {
    // Bug C：BT 已完成并 rename 落位同名文件 → .part 可能残留。
    // finalize_to 幂等短路时应清理 .part，避免后续落位冲突。
    let dir = tempfile::tempdir().unwrap();
    let om = OutputManager::new(dir.path().to_path_buf());
    let part = om.part_path("seeded.bin").unwrap();
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, b"residual-part-data").unwrap();
    let dest = dir.path().join("seeded.bin");
    fs::write(&dest, b"seeded-data").unwrap();

    om.finalize("seeded.bin", b"seeded-data".len() as u64).unwrap();
    assert_eq!(fs::read(&dest).unwrap(), b"seeded-data");
    assert!(!part.exists(), "幂等短路应清理残留 .part");
}
