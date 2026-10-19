//! 第十六轮：审计 batch10 的 P0-4 空洞检测（verify_nonempty_blocks）
//!
//! 修复内容（engine.rs）：无摘要任务不再恒通过，改为 `verify_nonempty_blocks`。
//! 实现（verify.rs）：Unix 上检查 `md.blocks() == 0`（完全稀疏 = 零写入）。
//!
//! ⚠️ Windows 平台：`md.blocks()` 是 `std::os::unix::fs::MetadataExt` 专属，
//!    `#[cfg(unix)]` 块在 Windows 上不编译 → 函数**恒返回 Ok(true)**。
//!
//! 而项目的桌面主战场是 Windows（Tauri 2 / desktop/src-tauri）。
//! 本文件评估：Windows 上这个防线是否为空，主防线是否足够。

#![allow(dead_code)]

use std::io::Write;

#[cfg(unix)]
fn blocks_of(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.blocks())
}

#[cfg(not(unix))]
fn blocks_of(_path: &std::path::Path) -> Option<u64> {
    // Windows：std 无块计数 API
    None
}

#[cfg(test)]
mod round16 {
    use super::*;
    use std::path::PathBuf;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join("smartdl-hole-test");
        std::fs::create_dir_all(&d).ok();
        d
    }

    #[test]
    fn sparse_vs_real_write() {
        let dir = tmpdir();
        println!("=== 空洞文件检测能力评估 ===\n");

        // ① set_len 预分配的空洞文件（模拟 download.rs:111 的行为）
        let hole = dir.join("hole.bin");
        {
            let f = std::fs::File::create(&hole).unwrap();
            f.set_len(16 * 1024 * 1024).unwrap(); // 16MB，不写任何字节
        }
        let hole_len = std::fs::metadata(&hole).unwrap().len();
        let hole_blocks = blocks_of(&hole);
        println!("[空洞文件] set_len(16MB) 后不写");
        println!("  长度 = {} 字节（长度检查会通过！）", hole_len);
        println!("  块数 = {:?}", hole_blocks);

        // ② 真实写入（模拟正常下载）
        let real = dir.join("real.bin");
        {
            let mut f = std::fs::File::create(&real).unwrap();
            f.set_len(16 * 1024 * 1024).unwrap();
            // 模拟分段写入：3 段各 4MB
            for _seg in 0..3 {
                f.write_all(&vec![0xABu8; 4 * 1024 * 1024]).unwrap();
            }
            f.sync_all().unwrap();
        }
        let real_blocks = blocks_of(&real);
        println!("\n[正常文件] set_len + 写入 12MB 数据");
        println!("  块数 = {:?}", real_blocks);

        // ③ 全零内容但真实写入
        let zeros = dir.join("zeros.bin");
        {
            let mut f = std::fs::File::create(&zeros).unwrap();
            f.set_len(4 * 1024 * 1024).unwrap();
            f.write_all(&vec![0u8; 4 * 1024 * 1024]).unwrap();
            f.sync_all().unwrap();
        }
        let zero_blocks = blocks_of(&zeros);
        println!("\n[全零内容] set_len + 写入 4MB 全零字节");
        println!("  块数 = {:?}  ← 关键：真实写入全零也会分配块", zero_blocks);

        println!("\n=== 平台能力对比 ===\n");
        #[cfg(unix)]
        {
            println!("[Unix] md.blocks() 可用 → 能区分空洞与真实写入");
            println!("  空洞文件 blocks = {:?}", hole_blocks);
            println!("  全零写入 blocks = {:?}", zero_blocks);
            println!("  → 两者可区分（空洞=0，全零写入>0）");
            println!("  → ✅ 这道防线在 Unix 上**有效**");

            assert_eq!(hole_blocks, Some(0), "空洞文件应为 0 块");
            assert!(zero_blocks.unwrap_or(0) > 0, "全零写入也应有块分配");
        }
        #[cfg(not(unix))]
        {
            println!("[Windows] md.blocks() 不可用 → 拿到的是 None");
            println!("  空洞文件 blocks = {:?}", hole_blocks);
            println!("  全零写入 blocks = {:?}", zero_blocks);
            println!("  → 无法区分");
            println!("  → ❌ 这道防线在 Windows 上**完全不生效**（恒返回 Ok(true)）");
            println!();
            println!("  而项目桌面端主战场是 Windows（Tauri 2 / desktop/src-tauri）");
        }

        println!("\n=== 主防线是否足够？===\n");
        println!("  batch10 的崩溃一致性链：");
        println!("    1. download.rs:391  每段完成 → f.sync_data()（数据落盘）");
        println!("    2. ledger.rs       账本 save → f.sync_all() + rename");
        println!("    → 顺序：数据先于元数据持久（WAL 语义）");
        println!();
        println!("  因此「账本说段完成但数据丢失」的窗口已关闭。");
        println!("  剩余风险：");
        println!("    · 账本**丢失/损坏**（用户删除 .progress 文件、磁盘错误）");
        println!("      → resume 从零重下，不会留下空洞（会覆盖）");
        println!("    · 全部段失败但 finalize 仍执行");
        println!("      → 需要 total 长度检查（已有）+ blocks 检查（Unix 有）");
        println!("    · 磁盘满：set_len 稀疏成功、写段失败");
        println!("      → 段失败会重试/换源，最终 Failed 而非 Completed");
        println!();
        println!("  → 结论：主防线（段级 fsync + 账本顺序）在 Windows 上**基本够用**；");
        println!("     verify_nonempty_blocks 是**纵深防御**，Windows 缺失不构成 P0。");

        println!("\n=== 但如果要补齐 Windows ===\n");
        println!("  Windows 等价 API：");
        println!("    · GetCompressedFileSizeW / GetFileInformationByHandleEx(FileStandardInfo)");
        println!("      → AllocationSize 字段（稀疏文件 < EndOfFile）");
        println!("    · FSCTL_QUERY_ALLOCATED_RANGES（更精确，逐区间查）");
        println!();
        println!("  项目已有 windows-sys 依赖（crates/xunlei-ffi/Cargo.toml → 0.59）");
        println!("  但那是 xunlei-ffi 的依赖，httpdl 需要自己加。");
        println!();
        println!("  更简单的跨平台方案（零新依赖）：");
        println!("    采样读取首/中/尾各 64KB，若全零 → 高度怀疑空洞");
        println!("    缺点：合法全零内容会误报（需要 read 而非 blocks）");
        println!("    → 实际上「读出来全零」与「从未写入」不可区分，");
        println!("       除非用 FSCTL。所以采样法**不可靠**，不如不做。");

        println!("\n=== 定性 ===\n");
        println!("  P3（平台能力缺口，非缺陷）：");
        println!("  · Unix: 防线有效 ✅");
        println!("  · Windows: 防线空缺，但主防线（fsync 顺序）覆盖 ⚠️");
        println!("  · 注释已诚实说明（「非 Unix 保守放行」）");
        println!("  · 建议：在 verify_nonempty_blocks 的 doc 里明确写「Windows 为 no-op」，");
        println!("    或后续用 windows-sys 的 FileStandardInfo 补齐");
    }
}
