//! 第二十二轮：验证 PR #114 的 P0 修复（9921a19）
//!
//! 修复内容：
//!   cid_store.rs —— before 改用独立的 `partition_point(off < i)`
//!   ops.rs       —— 统一锁序为 tasks → slot_reservations
//!
//! 本文件复刻修复后的实现，与 main 的原实现逐点对比，确认等价。

#![allow(dead_code)]

// ═══════════ 原实现（main，作为黄金标准）═══════════

fn find_after_original(
    paths: &[(usize, String)],
    i: usize,
    hlen: usize,
) -> Option<(usize, String)> {
    paths
        .iter()
        .find(|&&(off, _)| off > i && off <= i + 64 + hlen)
        .map(|(o, s)| (*o, s.clone()))
}

fn find_before_original(paths: &[(usize, String)], i: usize) -> Option<(usize, String)> {
    paths
        .iter()
        .rev()
        .find(|&&(off, _)| off < i && i <= off + 64)
        .map(|(o, s)| (*o, s.clone()))
}

// ═══════════ 修复后实现（9921a19）═══════════

fn find_after_fixed(
    paths: &[(usize, String)],
    i: usize,
    hlen: usize,
) -> Option<(usize, String)> {
    let gt_i = paths.partition_point(|&(off, _)| off <= i);
    paths
        .get(gt_i)
        .filter(|&&(off, _)| off <= i + 64 + hlen)
        .map(|(o, s)| (*o, s.clone()))
}

fn find_before_fixed(paths: &[(usize, String)], i: usize) -> Option<(usize, String)> {
    let lt_i = paths.partition_point(|&(off, _)| off < i);
    if lt_i > 0 {
        Some(&paths[lt_i - 1])
            .filter(|&&(off, _)| i <= off + 64)
            .map(|(o, s)| (*o, s.clone()))
    } else {
        None
    }
}

// ═══════════ 有缺陷的版本（batch10，用于演示）═══════════

fn find_before_buggy(paths: &[(usize, String)], i: usize) -> Option<(usize, String)> {
    let after_idx = paths.partition_point(|&(off, _)| off <= i);
    if after_idx > 0 {
        paths[..after_idx]
            .last()
            .filter(|&&(off, _)| i <= off + 64)
            .map(|(o, s)| (*o, s.clone()))
    } else {
        None
    }
}

#[cfg(test)]
mod round22 {
    use super::*;

    fn make_paths(n: usize, seed: u64) -> Vec<(usize, String)> {
        let mut v = Vec::new();
        let mut x = seed;
        let mut off = 0usize;
        for k in 0..n {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            off += 10 + (x >> 58) as usize;
            v.push((off, format!("/p/{k}.bin")));
        }
        v
    }

    #[test]
    fn p0_1_fix_is_equivalent_to_original() {
        println!("=== P0-1 cid_store 二分边界修复验证 ===\n");

        // ── 最小复现：单元素 i == off ──
        let one = vec![(51usize, "p0".to_string())];
        println!("[最小复现] paths=[(51,\"p0\")], i=51");
        println!("  原实现      : before = {:?}", find_before_original(&one, 51));
        println!("  batch10(坏) : before = {:?}", find_before_buggy(&one, 51));
        println!("  修复后      : before = {:?}", find_before_fixed(&one, 51));

        assert_eq!(find_before_original(&one, 51), None);
        assert_eq!(find_before_buggy(&one, 51), Some((51, "p0".to_string())));
        assert_eq!(
            find_before_fixed(&one, 51),
            None,
            "修复后必须与原实现一致"
        );
        println!("  ✅ 修复正确：不再把自身当前驱\n");

        // ── 全量等价性 ──
        println!("[全量验证] 3 组种子 × 500 路径 × 3 种 hlen");
        let mut total_checked = 0usize;
        let mut buggy_diffs = 0usize;
        let mut fixed_diffs = 0usize;

        for seed in [0xDEADBEEFu64, 0x1234_5678, 0xCAFE_BABE] {
            let paths = make_paths(500, seed);
            let max_off = paths[paths.len() - 1].0 + 200;

            for i in 0..max_off {
                // after 部分
                for hlen in [16usize, 20, 32] {
                    total_checked += 1;
                    if find_after_original(&paths, i, hlen) != find_after_fixed(&paths, i, hlen) {
                        fixed_diffs += 1;
                    }
                }
                // before 部分
                total_checked += 1;
                if find_before_original(&paths, i) != find_before_buggy(&paths, i) {
                    buggy_diffs += 1;
                }
                if find_before_original(&paths, i) != find_before_fixed(&paths, i) {
                    fixed_diffs += 1;
                }
            }
            println!(
                "  seed={:#010x}: 完成",
                seed
            );
        }

        println!("\n  检查点总数 : {total_checked}");
        println!("  batch10 差异: {buggy_diffs}");
        println!("  修复后差异  : {fixed_diffs}");

        assert_eq!(fixed_diffs, 0, "修复后必须与原实现零差异");
        assert!(buggy_diffs > 0, "batch10 版本应该有差异（证明缺陷真实存在）");

        println!("\n  ✅ 修复后与原实现**零差异**");
        println!("  ✅ 同时确认 batch10 版本有 {buggy_diffs} 处差异（缺陷真实存在）");
    }

    #[test]
    fn p0_1_edge_cases() {
        println!("=== P0-1 边界用例 ===\n");

        // 空表
        let empty: Vec<(usize, String)> = vec![];
        assert_eq!(find_before_fixed(&empty, 100), None);
        assert_eq!(find_after_fixed(&empty, 100, 16), None);
        println!("  ✅ 空表 → (None, None)");

        // i 在第一个元素之前
        let v = vec![(100usize, "a".to_string())];
        assert_eq!(find_before_fixed(&v, 50), None);
        println!("  ✅ i 在所有 off 之前 → before = None");

        // i 恰等于某 off
        let v2 = vec![(50usize, "a".to_string()), (100usize, "b".to_string())];
        let b = find_before_fixed(&v2, 50);
        assert_eq!(b, None, "i==50 时不应把 off=50 的自己当前驱");
        println!("  ✅ i 恰等于 off → 不把自身当前驱");

        // i 比所有 off 大
        let b = find_before_fixed(&v2, 10000);
        assert_eq!(find_before_original(&v2, 10000), b);
        println!("  ✅ i 超出所有 off → 与原实现一致");

        // 距离边界：off + 64 == i（应该匹配）
        let v3 = vec![(36usize, "x".to_string())];
        let b = find_before_fixed(&v3, 100);
        assert_eq!(b, Some((36, "x".to_string())), "i == off+64 应匹配");
        println!("  ✅ i == off + 64 边界 → 匹配");

        // 距离边界：off + 64 == i - 1（应该不匹配）
        let v4 = vec![(35usize, "x".to_string())];
        assert_eq!(find_before_fixed(&v4, 100), None, "i > off+64 不应匹配");
        println!("  ✅ i > off + 64 → 不匹配");

        println!("\n  ✅ 全部边界用例通过");
    }
}
