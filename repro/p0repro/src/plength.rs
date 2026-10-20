//! P1 复现：恶意 piece_length 导致乘法溢出
//!
//! 位置：crates/core/src/xltd.rs:151-152（同类代码见 bt_alerts.rs:215-216）
//!
//! ```text
//! let piece_start = (idx as u64) * (piece_length as u64);
//! let piece_end = piece_start + piece_length as u64;
//! ```
//!
//! `piece_length` 来自 .torrent 文件（外部输入，`be_int(...) as u32`），
//! 全仓仅校验 `== 0`，无上限。release 下乘法回绕 → 区间判定错乱。

/// 原样复刻 xltd.rs:151-152 的计算
fn piece_bounds_original(idx: usize, piece_length: u32) -> (u64, u64) {
    let piece_start = (idx as u64) * (piece_length as u64);
    let piece_end = piece_start + piece_length as u64;
    (piece_start, piece_end)
}

/// 加保护的版本（对照）
fn piece_bounds_checked(idx: usize, piece_length: u32) -> Option<(u64, u64)> {
    let pl = piece_length as u64;
    let start = (idx as u64).checked_mul(pl)?;
    let end = start.checked_add(pl)?;
    Some((start, end))
}

#[cfg(test)]
mod p1 {
    use super::*;

    /// 正常情况：行为一致
    #[test]
    fn normal_case_matches() {
        for (idx, pl) in [(0usize, 16384u32), (100, 16384), (1000, 262144)] {
            let (s, e) = piece_bounds_original(idx, pl);
            let (s2, e2) = piece_bounds_checked(idx, pl).unwrap();
            assert_eq!((s, e), (s2, e2), "正常路径应一致 idx={idx} pl={pl}");
        }
        println!("✅ 正常 piece_length 下两者一致");
    }

    /// release 模式：乘法溢出
    ///
    /// `idx` 会随 `pieces_hash.len()` 增长 —— 而 pieces_hash 长度 = 文件总大小 /
    /// piece_length。攻击者可以用**小 piece_length + 声称超大文件**构造大量 piece。
    /// 关键是 `idx * piece_length` 超过 u64 需要 idx 很大，实践中更难达成；
    /// **真正容易达成的是 `piece_start + piece_length` 的加法溢出**（见下）。
    #[test]
    fn overflow_wraps_in_release() {
        // 构造：idx * piece_length 刚好超过 u64
        let pl: u32 = u32::MAX; // 恶意种子：piece length = 4294967295
        // idx 需 > u64::MAX / pl ≈ 4294967297
        let idx: usize = if usize::BITS >= 64 { 4_294_967_298 } else { return };

        let exact = (idx as u128) * (pl as u128);
        let overflow = exact > u64::MAX as u128;

        println!("piece_length = {}", pl);
        println!("idx          = {}", idx);
        println!("真实乘积 (u128) = {}", exact);
        println!("溢出 u64      = {}", overflow);

        assert!(overflow, "该组合应溢出 u64");

        let wrapped = (idx as u64).wrapping_mul(pl as u64);
        println!("release 下 piece_start 回绕为 {}", wrapped);
        println!("→ 应为 {} 但得到 {}", exact, wrapped);
        println!("→ 区间判定 [piece_start, piece_end) 与真实位置完全无关");
        println!("→ 后果：读错位置 / 漏标完成 / 误判 partial（静默数据损坏）");

        // 对照：checked 版本会明确失败
        assert!(
            piece_bounds_checked(idx, pl).is_none(),
            "checked 版本应返回 None"
        );
        println!("✅ checked_mul 版本正确返回 None（可转为明确错误）");
    }

    /// 更容易达成的溢出：加法 `piece_start + piece_length`
    ///
    /// 用 u128 计算以避免本测试自身在 debug 下 panic（被测代码在 debug 下
    /// 就是直接 panic —— 这本身就是证据）。
    #[test]
    fn addition_overflow_easier() {
        let pl: u32 = 16 * 1024 * 1024; // 16MiB，完全合法的 piece_length
        if usize::BITS < 64 {
            return;
        }
        // 选 idx 使 piece_start 接近 u64::MAX，再加 pl 即溢出
        let idx: usize = ((u64::MAX / pl as u64) as usize) + 1;

        let piece_start = (idx as u128) * (pl as u128); // u128 避免测试自身溢出
        let piece_end_exact = piece_start + (pl as u128);

        println!("piece_length = {} (完全合法值)", pl);
        println!("idx          = {}", idx);
        println!("piece_start  = {}", piece_start);
        println!("piece_end    = {}", piece_end_exact);
        println!("piece_end 溢出 u64 = {}", piece_end_exact > u64::MAX as u128);

        assert!(piece_end_exact > u64::MAX as u128, "应溢出");
        println!("→ 被测代码 `let piece_end = piece_start + piece_length as u64;`");
        println!("→ debug:  panic 'attempt to multiply/add with overflow'");
        println!("→ release: 回绕为 {}", (piece_start as u64).wrapping_add(pl as u64));
        println!("→ 后果：L155 的 `piece_end <= file_offset` 区间判定直接错乱");
    }

    /// div_ceil + 乘 的溢出（xltd.rs:131）
    #[test]
    fn expected_size_overflow() {
        let file_size: u64 = u64::MAX - 1000;
        // 原式：file_size.div_ceil(4096) * 4096
        let div = file_size.div_ceil(4096);
        let overflow = div.checked_mul(4096).is_none();
        println!("file_size       = {}", file_size);
        println!("div_ceil(4096)  = {}", div);
        println!("* 4096 溢出     = {}", overflow);
        if overflow {
            println!("→ debug: panic 'attempt to multiply with overflow'");
            println!("→ release: 回绕为 {}", div.wrapping_mul(4096));
        }
        assert!(overflow, "应溢出");
    }

    /// 恶意 torrent 的 piece_length 上限建议
    #[test]
    fn recommended_bound() {
        // libtorrent 默认 piece size 上限 16 MiB（1<<24）
        const LIBTORRENT_MAX_PIECE: u32 = 16 * 1024 * 1024;
        for bad in [u32::MAX, 1 << 30, (1 << 24) + 1] {
            println!("piece_length = {:>10}  超过 libtorrent 上限({}) → 应拒绝",
                bad, LIBTORRENT_MAX_PIECE);
        }
        println!("✅ 建议在 TorrentMeta::parse 中加：piece_length > 16MiB → InvalidSource");
    }
}
