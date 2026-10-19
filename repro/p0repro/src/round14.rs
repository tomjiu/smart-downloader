//! 第十四轮：审计 batch10 的 piece_length 上限（1<<24）
//!
//! 审查点：
//!   代码注释称「上限 16 MiB 与 libtorrent 一致（1<<24）」
//!
//! 核实结果（外部资料）：
//!   - libtorrent `load_torrent_limits` 只有 max_pieces(0x200000)、
//!     max_decode_depth(100) 等，**没有 max_piece_length**
//!   - issue #8139 "no support for piece length of 2147483648"（2 GiB）
//!     说明 2^31 才是 int 上限边界
//!   - PR #7736 "raise the max piece size to 512 MiB"
//!
//! → 「与 libtorrent 一致」的说法**不成立**。16 MiB 是自选的工程上限。
//!
//! 本文件评估：这个自选上限的实际影响。

#![allow(dead_code)]

/// libtorrent 的实际约束（来自资料）
mod reality {
    /// piece_length() 返回 int → 上限 i32::MAX
    pub const LT_INT_MAX: i64 = i32::MAX as i64; // 2_147_483_647 ≈ 2 GiB
    /// PR #7736 提到把上限抬到 512 MiB
    pub const LT_RAISED_MAX: i64 = 512 * 1024 * 1024;
}

/// 项目的自选上限
const PROJECT_MAX_PIECE_LEN: i64 = 1 << 24; // 16 MiB

/// 常见种子生成器的 piece 上限（实测/文档）
mod common {
    /// qBittorrent / 多数 GUI 客户端的自动选择上限
    pub const GUI_AUTO_MAX: i64 = 16 * 1024 * 1024;
    /// 一些老种子使用 32 MiB（大文件私有站）
    pub const OLD_PRIVATE_SITE: i64 = 32 * 1024 * 1024;
}

#[cfg(test)]
mod round14 {
    use super::*;

    #[test]
    fn piece_len_limit_analysis() {
        println!("=== batch10 piece_length 上限（1<<24）核查 ===\n");

        println!("[事实 1] 代码注释声称");
        println!("   「上限与 libtorrent 一致（1<<24）」\n");

        println!("[事实 2] libtorrent 实际限制");
        println!("   load_torrent_limits 字段（官方文档）:");
        println!("     max_buffer_size      = 10000000");
        println!("     max_pieces           = 0x200000  (2097152)");
        println!("     max_decode_depth     = 100");
        println!("     max_decode_tokens    = 3000000");
        println!("     max_duplicate_filenames = 500");
        println!("     max_directory_depth  = 100");
        println!("   → **没有任何 piece_length 上限字段**");

        println!("\n[事实 3] piece_length 的类型约束");
        println!("   torrent_info::piece_length() 返回 int");
        println!("   → 理论上限 i32::MAX = {}（≈ 2 GiB）", reality::LT_INT_MAX);
        println!("   issue #8139 报「不支持 piece length 2147483648」");
        println!("   → 2147483648 = 2^31 → 说明边界确实是 i32 上限附近");
        println!("   PR #7736 标题「raise the max piece size to 512 MiB」");
        println!("   → 512 MiB 是某版本后的显式上限");

        println!("\n=== 结论 1：注释理由不成立 ===");
        println!("   16 MiB ≠ libtorrent 上限（后者是 512 MiB 甚至 2 GiB）");
        println!("   所以「与 libtorrent 一致」是**错误归因**。");

        println!("\n=== 结论 2：但这个自选上限本身合理吗？===\n");
        println!("  {:<24} {:>14} {:>10} {:>12}", "场景", "piece_len", "是否通过", "说明");
        println!("  {}", "-".repeat(66));

        let cases: Vec<(i64, &str)> = vec![
            (16 * 1024, "16 KiB —— 传统小种子"),
            (256 * 1024, "256 KiB —— 常见"),
            (1024 * 1024, "1 MiB —— 常见"),
            (4 * 1024 * 1024, "4 MiB —— 大文件常见"),
            (16 * 1024 * 1024, "16 MiB —— 上限边界"),
            (32 * 1024 * 1024, "32 MiB —— 部分私有站"),
            (64 * 1024 * 1024, "64 MiB —— 罕见"),
            (512 * 1024 * 1024, "512 MiB —— libtorrent 上限"),
        ];
        for (plen, note) in cases {
            let pass = (1..=PROJECT_MAX_PIECE_LEN).contains(&plen);
            println!(
                "  {:<24} {:>14} {:>10} {:>12}",
                note, plen, if pass { "✅ 通过" } else { "❌ 拒绝" }, ""
            );
        }

        println!("\n  实测：32 MiB / 64 MiB / 512 MiB 的种子会被**直接拒绝**");

        println!("\n=== 影响评估 ===\n");
        println!("  真实世界里 piece_length > 16 MiB 的种子多吗？");
        println!("  · 主流 GUI（qBittorrent/Transmission）自动选择上限多为 16 MiB");
        println!("  · 但**手工创建**或某些私有站会用到 32/64 MiB（大文件减少 piece 数）");
        println!("  · 512 MiB 只在极端场景（超大 volume 镜像）");
        println!();
        println!("  → 工程上 16 MiB 是**合理默认**，能覆盖绝大多数种子");
        println!("  → 但拒绝策略是**硬失败**（Err），不是「警告但仍然尝试」");
        println!("  → 一个 32 MiB piece 的合法种子会被当成「非法输入」");

        println!("\n=== 定性 ===");
        println!("  1. 注释理由错误（P3 文档级）——「与 libtorrent 一致」不成立");
        println!("  2. 上限选择本身可接受（覆盖主流），但建议：");
        println!("     · 把上限从 16 MiB 提到 64 MiB 或 512 MiB（对齐 libtorrent）");
        println!("     · 或改注释，明确说明「这是本项目的工程选择，非 libtorrent 约束」");
        println!("     · 或在拒绝时给出更友好的错误：「piece length 16MiB 超出本项目");
        println!("       支持范围（libtorrent 可达 512MiB），如有需要请提 issue」");

        println!("\n  ⚠️ 严重性：**P3 文档/边界**，不是 P0。");
        println!("     因为 16 MiB 覆盖主流种子；只是注释误导了后续维护者，");
        println!("     且极端种子会被误拒。");
    }
}
