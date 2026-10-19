//! 第十二轮：cid_store 二分优化的**精确缺陷定位**与正确实现
//!
//! 发现（round10/round11）：
//!   batch10 的二分实现与原实现在 8889 个检查点上有 243 处不一致。
//!
//! 根因（本文件精确定位）：
//!   1. `after_idx = partition_point(|off| off <= i)` 用 **<=**
//!      → `paths[..after_idx].last()` 可能是 `off == i` 的元素
//!      而原实现要求 `off < i`（严格小于）
//!   2. `before` 的语义因此从「最后一个 off<i 且距离<=64」变成
//!      「最后一个 off<=i 且距离<=64」—— 放行了 off==i
//!
//! 影响：candidate 的 path 归属错误（指向自身或错误邻居）
//!       → 输出的 hash↔path 配对错乱（不是漏检，是错配）

#![allow(dead_code)]

// ═══════════ 原实现（main/修复前基线）═══════════
fn find_original(paths: &[(usize, String)], i: usize, hlen: usize) -> (Option<usize>, Option<usize>) {
    let after = paths
        .iter()
        .find(|&&(off, _)| off > i && off <= i + 64 + hlen)
        .map(|(o, _)| *o);
    let before = paths
        .iter()
        .rev()
        .find(|&&(off, _)| off < i && i <= off + 64)
        .map(|(o, _)| *o);
    (after, before)
}

// ═══════════ batch10 实现（有缺陷）═══════════
fn find_batch10(paths: &[(usize, String)], i: usize, hlen: usize) -> (Option<usize>, Option<usize>) {
    let after_idx = paths.partition_point(|&(off, _)| off <= i);
    let after = paths
        .get(after_idx)
        .filter(|&&(off, _)| off <= i + 64 + hlen)
        .map(|(o, _)| *o);
    // 缺陷所在：after_idx 用的是 `off <= i`，导致 paths[..after_idx] 包含 off==i
    let before = if after_idx > 0 {
        paths[..after_idx]
            .last()
            .filter(|&&(off, _)| i <= off + 64)
            .map(|(o, _)| *o)
    } else {
        None
    };
    (after, before)
}

// ═══════════ 修正实现（等价二分）═══════════
fn find_correct(paths: &[(usize, String)], i: usize, hlen: usize) -> (Option<usize>, Option<usize>) {
    // after: 首个 off > i 且 off <= i+64+hlen
    // 用 partition_point(|off| off <= i) 得首个 off>i 的索引（这个对 after 是正确的）
    let gt_i = paths.partition_point(|&(off, _)| off <= i);
    let after = paths
        .get(gt_i)
        .filter(|&&(off, _)| off <= i + 64 + hlen)
        .map(|(o, _)| *o);

    // before: 最后一个 off < i 且 i <= off+64
    // 先取 off < i 的严格上界
    let lt_i = paths.partition_point(|&(off, _)| off < i);
    let before = if lt_i > 0 {
        paths[lt_i - 1]
            .clone()
            .into_tuple_check()
    } else {
        None
    };
    let before = before.filter(|off| i <= off + 64);
    (after, before)
}

/// 辅助 trait：把 (usize,String) 转成 usize 方便链式
trait TupleCheck {
    fn into_tuple_check(self) -> Option<usize>;
}
impl TupleCheck for (usize, String) {
    fn into_tuple_check(self) -> Option<usize> {
        Some(self.0)
    }
}

#[cfg(test)]
mod round12 {
    use super::*;

    fn make_paths(n: usize, seed: u64) -> Vec<(usize, String)> {
        let mut v = Vec::new();
        let mut x = seed;
        let mut off = 0usize;
        for k in 0..n {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            off += 10 + (x >> 58) as usize;
            v.push((off, format!("/p/{k}.bin")));
        }
        v
    }

    #[test]
    fn exact_defect_and_fix() {
        println!("=== 精确缺陷定位 ===\n");

        // ── 最小复现：单元素，i 恰等于 off ──
        let one = vec![(51usize, "p0".to_string())];
        let i = 51usize;
        println!("最小复现：paths=[(51, \"p0\")], i=51");
        println!("  原实现   : {:?}", find_original(&one, i, 16));
        println!("  batch10  : {:?}", find_batch10(&one, i, 16));
        println!("  修正实现 : {:?}", find_correct(&one, i, 16));

        let orig = find_original(&one, i, 16);
        let b10 = find_batch10(&one, i, 16);
        let fix = find_correct(&one, i, 16);

        println!("\n  → 原 before = {:?}（None：51 < 51 为假，不算前驱）", orig.1);
        println!("  → batch10 before = {:?}（Some：用了 off<=i，把自身当成了前驱）", b10.1);
        println!("  → 修正 before = {:?}", fix.1);

        assert_eq!(orig.1, None, "原实现要求严格 off < i");
        assert_eq!(b10.1, Some(51), "batch10 把 off==i 当成了前驱（缺陷）");
        assert_eq!(fix.1, None, "修正实现应恢复严格小于语义");
        println!("\n  ✅ 缺陷确认为 `<=' vs '<' 混用");

        // ── 全量对比 ──
        println!("\n=== 全量等价性验证（3 组种子 × 500 路径）===");
        for seed in [0xDEADBEEFu64, 0x1234_5678u64, 0xCAFE_BABEu64] {
            let paths = make_paths(500, seed);
            let mut orig_b10_diff = 0;
            let mut orig_fix_diff = 0;
            let mut checked = 0usize;

            let max_off = paths[paths.len() - 1].0 + 200;
            for i in 0..max_off {
                for hlen in [16usize, 20, 32] {
                    let o = find_original(&paths, i, hlen);
                    let b = find_batch10(&paths, i, hlen);
                    let f = find_correct(&paths, i, hlen);
                    checked += 1;
                    if o != b { orig_b10_diff += 1; }
                    if o != f { orig_fix_diff += 1; }
                }
            }
            println!(
                "  seed={:#x}: 检查 {checked} 点  batch10 差异={orig_b10_diff}  修正版差异={orig_fix_diff}",
                seed
            );
            assert_eq!(orig_fix_diff, 0, "修正实现必须与原实现完全等价");
        }

        println!("\n✅ 修正实现与原实现在 3×三组 hlen 全量遍历下**零差异**");

        println!("\n=== 缺陷性质说明 ===");
        println!("  不是「漏检」而是「错配」：");
        println!("  - original: 只有当存在 off<i 且 i<=off+64 的前驱时才配对");
        println!("  - batch10 : 当 off==i 时把**自身位置**当成前驱 → path 指向错位条目");
        println!("  - 后果：输出 CidStoreEntry 的 path 与 hash 不匹配（数据正确性受损）");
        println!("  - 影响范围：hash 块窗口起点 i 恰好等于某路径 offset 时触发");
        println!("  - 实测触发率：round10 中 8889 点有 243 处（2.7%）");

        println!("\n=== 建议修复（等价的二分写法）===");
        println!("  let gt_i = paths.partition_point(|&(off,_)| off <= i);   // after 用");
        println!("  let lt_i = paths.partition_point(|&(off,_)| off <  i);   // before 用 ← 关键");
        println!("  before = (lt_i > 0).then(|| paths[lt_i-1].0)");
        println!("           .filter(|off| i <= off + 64);");
        println!("  // 或合并：lt_i 与 gt_i 只在 paths[lt_i].off == i 时不同，");
        println!("  //         即 paths 中存在 off 恰等于 i 的条目");
    }
}
