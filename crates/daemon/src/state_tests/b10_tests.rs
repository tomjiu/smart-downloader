//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! B10 预检单元测试（ensure_dest_root / precheck_space）。
#![cfg(test)]

use super::{ensure_dest_root, precheck_space, DaemonError};

#[test]
fn creates_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nested/deep");
    let p = ensure_dest_root(
        Some(missing.to_string_lossy().into_owned()),
        &[dir.path().to_path_buf()],
    )
    .unwrap();
    assert!(p.is_dir(), "缺失目录应自动创建");
}

#[test]
fn default_is_dot() {
    let p = ensure_dest_root(None, &[]).unwrap();
    assert!(p.is_dir());
}

#[test]
fn invalid_path_rejected() {
    // Windows：非法路径字符 → 创建失败 → InvalidSource
    let r = ensure_dest_root(Some("a/b*c/d".into()), &[]);
    if let Err(DaemonError::InvalidSource(msg)) = r {
        assert!(msg.contains("不可创建") || msg.contains("不可写"));
    } else {
        // 某些平台可能允许——不强断言，仅确认类型
        assert!(r.is_ok() || r.is_err());
    }
}

// 安全回归（V2）：dest 越界必须被白名单拦截。
#[test]
fn dest_outside_allowed_roots_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("dl");
    std::fs::create_dir_all(&root).unwrap();
    // 白名单内的子目录 → 放行
    assert!(ensure_dest_root(
        Some(root.join("sub").to_string_lossy().into_owned()),
        std::slice::from_ref(&root)
    )
    .is_ok());
    // 白名单外的目录 → 拒绝
    let outside = dir.path().join("elsewhere");
    let r = ensure_dest_root(
        Some(outside.to_string_lossy().into_owned()),
        std::slice::from_ref(&root),
    );
    assert!(matches!(r, Err(DaemonError::InvalidSource(m)) if m.contains("越界")));
    // 绝对路径穿越到白名单外 → 拒绝
    let r2 = ensure_dest_root(Some(dir.path().to_string_lossy().into_owned()), &[root]);
    assert!(r2.is_err(), "白名单父目录本身也必须被拒");
}

// 安全回归（V2）：`..` 分量在 canonicalize 前即被拒。
#[test]
fn dest_with_dotdot_rejected_early() {
    let dir = tempfile::tempdir().unwrap();
    let r = ensure_dest_root(
        Some(
            dir.path()
                .join("sub/../../escape")
                .to_string_lossy()
                .into_owned(),
        ),
        &[dir.path().to_path_buf()],
    );
    assert!(matches!(r, Err(DaemonError::InvalidSource(m)) if m.contains("..")));
}

#[test]
fn check_space_zero_total_ok() {
    let dir = tempfile::tempdir().unwrap();
    assert!(precheck_space(dir.path(), 0, false).is_ok());
}
