//! 第十轮：验证 cid_store 二分优化的**语义等价性**
//!
//! 审查点：batch10 把 `paths.iter().find()` 线性扫改成 `partition_point` 二分。
//! 性能修好了（10ms vs 6s），但**必须确认输出与原实现一致** ——
//! 否则「修性能改坏了结果」比不修更糟。
//!
//! 原语义：
//!   after  = paths 中**首个** `off > i && off <= i + 64 + hlen`
//!   before = paths 中**最后一个** `off < i && i <= off + 64`
//!
//! 新实现：
//!   after_idx = partition_point(|off| off <= i)   → 首个 off > i 的索引
//!   after     = paths.get(after_idx).filter(off <= i + 64 + hlen)
//!   before    = paths[..after_idx].last().filter(i <= off + 64)

#![allow(dead_code)]

/// 原实现（线性扫，O(#paths) 每候选）
fn find_neighbors_original(
    paths: &[(usize, String)],
    i: usize,
    hlen: usize,
) -> (Option<(usize, String)>, Option<(usize, String)>) {
    let after = paths
        .iter()
        .find(|&&(off, _)| off > i && off <= i + 64 + hlen)
        .map(|(o, s)| (*o, s.clone()));
    let before = paths
        .iter()
        .rev()
        .find(|&&(off, _)| off < i && i <= off + 64)
        .map(|(o, s)| (*o, s.clone()));
    (after, before)
}

/// batch10 新实现（二分，O(log #paths)）
fn find_neighbors_bisect(
    paths: &[(usize, String)],
    i: usize,
    hlen: usize,
) -> (Option<(usize, String)>, Option<(usize, String)>) {
    let after_idx = paths.partition_point(|&(off, _)| off <= i);
    let after = paths
        .get(after_idx)
        .filter(|&&(off, _)| off <= i + 64 + hlen)
        .map(|(o, s)| (*o, s.clone()));
    let before = if after_idx > 0 {
        paths[..after_idx]
            .last()
            .filter(|&&(off, _)| i <= off + 64)
            .map(|(o, s)| (*o, s.clone()))
    } else {
        None
    };
    (after, before)
}

#[cfg(test)]
mod round10 {
    use super::*;

    /// 构造一个确定性的伪随机路径表（已按 off 升序）
    fn make_paths(n: usize, seed: u64) -> Vec<(usize, String)> {
        let mut v = Vec::new();
        let mut x = seed;
        let mut off = 0usize;
        for k in 0..n {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            off += 10 + (x >> 58) as usize; // 10..74 递增
            v.push((off, format!("/p/{k}.bin")));
        }
        v
    }

    #[test]
    fn bisect_matches_original() {
        println!("=== cid_store 二分优化语义等价性验证 ===\n");

        let paths = make_paths(500, 0xDEADBEEF);
        println!("路径表规模: {} 条（off 范围 {}..{}）",
                 paths.len(), paths[0].0, paths[paths.len()-1].0);

        let mut checked = 0usize;
        let mut mismatch = 0usize;

        // 遍历所有可能的窗口起点
        let max_off = paths[paths.len()-1].0 + 200;
        for i in (0..max_off).step_by(7) {
            for hlen in [16usize, 20, 32] {
                let (ao, bo) = find_neighbors_original(&paths, i, hlen);
                let (an, bn) = find_neighbors_bisect(&paths, i, hlen);
                checked += 1;
                if ao != an || bo != bn {
                    mismatch += 1;
                    if mismatch <= 5 {
                        println!("  ❌ i={i} hlen={hlen}");
                        println!("     原 after={ao:?}  新 after={an:?}");
                        println!("     原 before={bo:?}  新 before={bn:?}");
                    }
                }
            }
        }

        println!("\n检查点: {checked}");
        println!("不一致:  {mismatch}");

        if mismatch == 0 {
            println!("\n✅ 二分实现与原线性实现在所有检查点上**完全等价**");
        } else {
            println!("\n❌ 存在语义差异 —— 需修正");
        }
        assert_eq!(mismatch, 0, "二分优化改变了输出语义");
    }

    #[test]
    fn edge_cases() {
        println!("=== 边界用例 ===\n");

        // 空表
        let empty: Vec<(usize, String)> = vec![];
        for i in [0usize, 100] {
            let (a, b) = find_neighbors_bisect(&empty, i, 16);
            assert_eq!((a, b), (None, None));
        }
        println!("  ✅ 空表 → (None, None)");

        // 单元素，i 在它之前
        let one = vec![(100usize, "x".to_string())];
        let (a, b) = find_neighbors_bisect(&one, 0, 16);
        println!("  单元素 off=100, i=0 → after={:?} before={:?}", a, b);
        assert_eq!(find_neighbors_original(&one, 0, 16), (a, b));

        // i 恰等于某个 off（边界：原实现用 `off > i` 严格大于）
        let v = vec![(50usize, "a".to_string()), (100usize, "b".to_string())];
        let (ao, bo) = find_neighbors_original(&v, 50, 16);
        let (an, bn) = find_neighbors_bisect(&v, 50, 16);
        println!("  i=50 恰等 off=50 → 原({ao:?},{bo:?}) 新({an:?},{bn:?})");
        assert_eq!((ao, bo), (an, bn), "i 恰等于 off 时行为必须一致");
        println!("  ✅ i 恰等于 off 时一致（严格大于语义保留）");

        // i 超出所有 off
        let (a2, b2) = find_neighbors_bisect(&v, 10000, 16);
        let (o2, ob2) = find_neighbors_original(&v, 10000, 16);
        println!("  i 超出所有 off → 原({o2:?},{ob2:?}) 新({a2:?},{b2:?})");
        assert_eq!((a2, b2), (o2, ob2));

        println!("\n✅ 全部边界用例一致");
    }
}
