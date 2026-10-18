//! 第二轮复现：core 侧整数溢出 + sanitize_rel 放行 CurDir
//!
//! 对应审查发现：
//!   P0-1  core/src/torrent_meta.rs:316  total_size 求和溢出 → 绕过磁盘预检
//!   P1-1  core/src/session/output.rs:22  sanitize_rel 放行 Component::CurDir
//!
//! 运行：cargo test round2 -- --nocapture

#[cfg(test)]
mod round2 {
    use std::path::{Component, Path, PathBuf};

    // ══════════════ P0-11: total_size 求和溢出（已修复，回归守护） ══════════════
    //
    // 主仓修复（core/src/torrent_meta.rs）：`sum()` → `try_fold(checked_add)`，
    // 溢出报 TotalSizeOverflow。本测试验证修复后行为：
    // ① checked 版对溢出输入返回 None（可转为明确错误）
    // ② 正常输入精确求和

    /// 修复后实现（与主仓 torrent_meta.rs 同步）
    fn total_size_checked(sizes: &[u64]) -> Option<u64> {
        sizes.iter().try_fold(0u64, |a, &s| a.checked_add(s))
    }

    #[test]
    fn p0_total_size_overflow() {
        // .torrent 里 length 字段是 i64（bencode int），core 侧 `size.max(0) as u64`
        // 无上界校验。攻击者可声明多个 i64::MAX 大小的文件。
        let per_file = i64::MAX as u64;
        let sizes = vec![per_file, per_file, per_file]; // 3 个文件

        let exact: u128 = sizes.iter().map(|&s| s as u128).sum();
        println!("=== P0-11: total_size 求和溢出（修复后回归） ===");
        println!("每文件 size  = {} (i64::MAX)", per_file);
        println!("文件数        = {}", sizes.len());
        println!("真实总和     = {}", exact);
        println!("超过 u64::MAX = {}", exact > u64::MAX as u128);

        assert!(exact > u64::MAX as u128, "应溢出 u64");

        // 修复后：checked 版本明确失败 → 上游报 TotalSizeOverflow 而非回绕
        assert!(
            total_size_checked(&sizes).is_none(),
            "checked 版本应返回 None（修复后主仓行为）"
        );
        // 正常输入不受影响
        assert_eq!(total_size_checked(&[100, 200, 50]), Some(350));
        println!("✅ checked_add 版本正确返回 None（主仓已转为明确错误）");
    }

    /// 单个文件也可能溢出相加（真实值略超 u64::MAX）
    #[test]
    fn p0_total_size_single_boundary() {
        let sizes = vec![u64::MAX, 1];
        let exact: u128 = sizes.iter().map(|&s| s as u128).sum();
        println!("=== 边界: u64::MAX + 1（修复后回归） ===");
        println!("真实 = {}", exact);
        // 修复前：原实现 sum() 回绕为 0 → 声称总大小 0 字节 → 预检必然放行
        // 修复后：checked 返回 None → 报错拒绝
        assert!(total_size_checked(&sizes).is_none());
        println!("✅ 修复后返回 None（不再回绕为 0 绕过预检）");
    }

    // ══════════════ P1-1: sanitize_rel 放行 CurDir ══════════════

    /// 复刻 core/src/session/output.rs 的 sanitize_rel 逻辑（已同步主仓修复：
    /// P1-1 裸 CurDir 拒绝 + P1-2 保留名 stem trim）
    #[derive(Debug)]
    enum OutErr {
        UnsafePath(String),
    }

    fn sanitize_rel(rel: &str) -> Result<PathBuf, OutErr> {
        let pb = PathBuf::from(rel);
        if pb.as_os_str().is_empty() {
            return Err(OutErr::UnsafePath(rel.into()));
        }
        if pb.is_absolute() {
            return Err(OutErr::UnsafePath(rel.into()));
        }
        for comp in pb.components() {
            match comp {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(OutErr::UnsafePath(rel.into())),
            }
        }
        // P1-1 修复：裸 CurDir（"." / "./"）——file_name() 为 None → 拒绝
        if pb.file_name().is_none() {
            return Err(OutErr::UnsafePath(rel.into()));
        }
        // 保留名校验：仅在 file_name() 为 Some 时执行
        if let Some(name) = pb.file_name().and_then(|n| n.to_str()) {
            // P1-2 修复：stem trim 尾空格/点后再比对（Win32 设备名语义）
            let stem = name.split('.').next().unwrap_or(name);
            let upper = stem.trim_end_matches([' ', '.']).to_ascii_uppercase();
            const RESERVED: [&str; 22] = [
                "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
                "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8",
                "LPT9",
            ];
            if RESERVED.contains(&upper.as_str()) {
                return Err(OutErr::UnsafePath(rel.into()));
            }
            if name.ends_with('.') || name.ends_with(' ') {
                return Err(OutErr::UnsafePath(rel.into()));
            }
        }
        Ok(pb)
    }

    #[test]
    fn p1_curdir_bypasses_reserved_check() {
        println!("=== P1-1: sanitize_rel 裸 CurDir（修复后回归） ===\n");

        // 关键：`file_name()` 对 "." 返回 None → 修复后裸 CurDir 被拒绝
        let dot = Path::new(".");
        let dotdot = Path::new("./");
        println!("Path::new(\".\").file_name()  = {:?}", dot.file_name());
        println!("Path::new(\"./\").file_name() = {:?}", dotdot.file_name());
        assert!(dot.file_name().is_none(), "'.' 的 file_name() 应为 None");

        // ── 正常防护仍然有效（对照组）──
        println!("\n--- 对照组：真穿越仍被拒 ---");
        for bad in ["../escape", "/abs", "a/../../b"] {
            assert!(sanitize_rel(bad).is_err(), "穿越路径 {bad:?} 必须被拒");
            println!("  ✅ 拒绝 {bad:?}");
        }
        // Windows 盘符形态仅在 Windows 目标上是穿越（Linux 上 "C:/x" 的
        // "C:" 是普通 Normal 组件，is_absolute() 只认 '/' 开头——主仓测试
        // 同样以 cfg(windows) 门控，两端口径一致）
        #[cfg(windows)]
        {
            assert!(sanitize_rel("C:/x").is_err(), "Windows 盘符前缀必须被拒");
            println!("  ✅ 拒绝 \"C:/x\"");
        }
        // ── 修复验证：裸 CurDir 现在被拒 ──
        println!("\n--- 修复验证：裸 CurDir 被拒 ---");
        for s in [".", "./"] {
            assert!(
                sanitize_rel(s).is_err(),
                "修复后 {s:?} 必须被拒绝（dest 不得指向 root 自身）"
            );
            println!("  ✅ 拒绝 {s:?}");
        }
        // ── 合法路径不受影响 ──
        for s in ["a/b.bin", "./x.bin", "sub/file.zip"] {
            assert!(sanitize_rel(s).is_ok(), "合法路径 {s:?} 不应被误伤");
            println!("  ✅ 放行 {s:?}");
        }
    }

    #[test]
    fn p1_reserved_name_with_space() {
        println!("=== P1-2: 保留名 stem trim（修复后回归） ===\n");
        // Windows 上 "con .txt" 与 "con.txt" 行为等价（设备名比较忽略尾随空格）
        // 修复后：stem trim 尾空格/点 → 命中 RESERVED → 拒绝
        for s in ["con .txt", "COM1 .txt", "CON.txt", "com1.txt"] {
            assert!(
                sanitize_rel(s).is_err(),
                "修复后 {s:?} 必须被拒绝（Win32 等价设备名）"
            );
            println!("  ✅ 拒绝 {s:?}");
        }
        // 合法名不受影响
        for s in ["console .bin", "my com1.txt"] {
            assert!(sanitize_rel(s).is_ok(), "合法名 {s:?} 不应误伤");
            println!("  ✅ 放行 {s:?}");
        }
    }
}
