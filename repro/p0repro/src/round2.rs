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

    // ══════════════ P0-1: total_size 求和溢出 ══════════════

    /// 原样复刻 torrent_meta.rs:316
    fn total_size_original(sizes: &[u64]) -> u64 {
        sizes.iter().copied().sum()
    }

    /// 加固版
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
        println!("=== P0-1: total_size 求和溢出 ===");
        println!("每文件 size  = {} (i64::MAX)", per_file);
        println!("文件数        = {}", sizes.len());
        println!("真实总和     = {}", exact);
        println!("超过 u64::MAX = {}", exact > u64::MAX as u128);

        assert!(exact > u64::MAX as u128, "应溢出 u64");

        let wrapped = total_size_original(&sizes);
        println!("→ 原实现 (release 回绕) = {}", wrapped);
        println!("→ 应是 {} 却得到 {}", exact, wrapped);
        println!("→ 回绕后变小 → precheck_space 误判磁盘充足 → 放行入队");

        // 对照：checked 版本明确失败
        assert!(
            total_size_checked(&sizes).is_none(),
            "checked 版本应返回 None"
        );
        println!("✅ checked_add 版本正确返回 None（可转为明确错误）");
    }

    /// 单个文件也可能溢出相加（真实值略超 u64::MAX）
    #[test]
    fn p0_total_size_single_boundary() {
        let sizes = vec![u64::MAX, 1];
        let exact: u128 = sizes.iter().map(|&s| s as u128).sum();
        println!("=== 边界: u64::MAX + 1 ===");
        println!("真实 = {}", exact);
        println!("回绕 = {}", total_size_original(&sizes));
        assert_eq!(total_size_original(&sizes), 0, "回绕应为 0");
        assert!(total_size_checked(&sizes).is_none());
        println!("→ 回绕为 0 = 声称文件总大小 0 字节 → 预检必然放行");
    }

    // ══════════════ P1-1: sanitize_rel 放行 CurDir ══════════════

    /// 复刻 core/src/session/output.rs 的 sanitize_rel 逻辑
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
                // ↓ 原实现：CurDir 被显式放行（output.rs:22）
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(OutErr::UnsafePath(rel.into())),
            }
        }
        // 保留名校验：仅在 file_name() 为 Some 时执行
        if let Some(name) = pb.file_name().and_then(|n| n.to_str()) {
            let stem = name.split('.').next().unwrap_or(name);
            let upper = stem.to_ascii_uppercase();
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
        const ROOT: &str = "D:/dl";
        println!("=== P1-1: sanitize_rel 放行 CurDir ===\n");

        // 关键：`file_name()` 对 "." 返回 None → 保留名校验整块被跳过
        let dot = Path::new(".");
        let dotdot = Path::new("./");
        println!("Path::new(\".\").file_name()  = {:?}", dot.file_name());
        println!("Path::new(\"./\").file_name() = {:?}", dotdot.file_name());
        assert!(dot.file_name().is_none(), "'.' 的 file_name() 应为 None");

        // ── 正常防护仍然有效（对照组）──
        println!("\n--- 对照组：真穿越仍被拒 ---");
        for bad in ["../escape", "/abs", "C:/x", "a/../../b"] {
            match sanitize_rel(bad) {
                Err(_) => println!("  ✅ 拒绝 {bad:?}"),
                Ok(p) => println!("  ❌ 放行 {bad:?} -> {p:?}"),
            }
        }
        // ── 漏网项 ──
        println!("\n--- 漏网项：被放行但语义可疑 ---");
        for s in [".", "./", "a/b/", "sub/", "."] {
            match sanitize_rel(s) {
                Ok(p) => {
                    let dest = Path::new(ROOT).join(&p);
                    println!("  ⚠️  放行 {s:?} -> dest = {:?}", dest.display().to_string());
                }
                Err(_) => println!("  ✅ 拒绝 {s:?}"),
            }
        }

        // 断言："." 确实被放行，且 dest 指向 root 自身
        let r = sanitize_rel(".").expect("原实现放行 '.'");
        let dest = Path::new(ROOT).join(&r);
        println!("\n→ sanitize_rel(\".\") = Ok({:?})", r);
        println!("→ root.join(\".\") = {:?}", dest.display().to_string());
        println!("→ 与 root 等价: {}", dest == Path::new(ROOT) || dest.to_string_lossy().contains("/."));
        println!("→ 后果：finalize 时 dest 是目录，metadata().len() 为目录长度");
        println!("→ 若恰等于 expected_size → 幂等短路 remove_file(目录) 失败但被 `let _ =` 吞");
        println!("→ 静默「成功」但文件未落位");
    }

    #[test]
    fn p1_reserved_name_with_space() {
        println!("=== P1-2: 保留名 stem 未 trim 尾空格 ===\n");
        // Windows 上 "con .txt" 与 "con.txt" 行为等价（设备名比较忽略尾随空格）
        for s in ["con .txt", "COM1 .txt", "CON.txt", "com1.txt"] {
            match sanitize_rel(s) {
                Err(_) => println!("  ✅ 拒绝 {s:?}"),
                Ok(p) => println!("  ⚠️  放行 {s:?}  (stem 未 trim → 未命中 RESERVED)"),
            }
        }
        println!("\n→ 建议: stem.trim_end_matches([' ', '.']).to_ascii_uppercase() 再比较");
    }
}
