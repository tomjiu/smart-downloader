//! 第十一轮：确认 cid_store 二分优化的缺陷根因
//!
//! 假设：新实现只取 `paths[..after_idx].last()` 然后 filter，
//! 若该元素**距离过远**（`i > off + 64`），filter 返回 None 就放弃，
//! 而原实现会**继续往前找**满足 `i <= off + 64` 的那个。
//!
//! 即：原语义是「最后一个满足 (off < i && i <= off+64) 的」，
//!     新实现是「最后一个 off < i 的，若它满足距离则用，否则放弃」。
//! 两者在「最近的路径也够远」时才等价。

#![allow(dead_code)]

fn find_before_original(paths: &[(usize, String)], i: usize) -> Option<(usize, String)> {
    paths
        .iter()
        .rev()
        .find(|&&(off, _)| off < i && i <= off + 64)
        .map(|(o, s)| (*o, s.clone()))
}

fn find_before_new(paths: &[(usize, String)], i: usize) -> Option<(usize, String)> {
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

/// 正确的二分实现：在 [0, after_idx) 范围内找**最后一个**满足 i <= off+64 的
/// 由于 off 升序，条件 i <= off+64 ⟺ off >= i-64，即找最后一个 off >= i-64 且 off < i
fn find_before_correct(paths: &[(usize, String)], i: usize) -> Option<(usize, String)> {
    // 所有 off < i 的元素范围是 [0, partition_point(off < i))
    let hi = paths.partition_point(|&(off, _)| off < i);
    if hi == 0 {
        return None;
    }
    // 在 [0, hi) 中找最后一个 off >= i.saturating_sub(64)
    // 由于升序，用 binary search 找下界
    let lo_bound = i.saturating_sub(64);
    let lo = paths[..hi].partition_point(|&(off, _)| off < lo_bound);
    if lo < hi {
        let (o, s) = &paths[hi - 1];
        // hi-1 是最后一个 off < i；若它 >= lo_bound 就是答案
        let _ = lo;
        Some((*o, s.clone()))
    } else {
        None
    }
}

#[cfg(test)]
mod round11 {
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
    fn diagnose_root_cause() {
        println!("=== 根因诊断 ===\n");

        // 复现一个失败用例
        let paths = make_paths(500, 0xDEADBEEF);
        println!("路径表前 12 条:");
        for (k, (off, name)) in paths.iter().take(12).enumerate() {
            println!("  [{k:>2}] off={off:<5} {name}");
        }

        let i = 329usize;
        println!("\n查询 i = {i}");
        println!("  原实现在 [0, i) 中找最后一个满足 i <= off+64（即 off >= 265）的：");
        for (off, name) in paths.iter().filter(|(o, _)| *o < i).rev().take(4) {
            let ok = i <= off + 64;
            println!("    off={off:<5} {name}  距离={} → {}", i - off, if ok { "✅ 满足" } else { "❌ 太远" });
        }
        let orig = find_before_original(&paths, i);
        println!("  → 原结果: {orig:?}");

        println!("\n  新实现取 paths[..after_idx].last() 然后 filter：");
        let after_idx = paths.partition_point(|&(off, _)| off <= i);
        println!("    after_idx = {after_idx}");
        if after_idx > 0 {
            let (last_off, last_name) = &paths[after_idx - 1];
            let ok = i <= last_off + 64;
            println!("    last = off={last_off:<5} {last_name}  距离={} → {}",
                     i - last_off, if ok { "✅ 保留" } else { "❌ filter 丢弃 → 返回 None" });
        }
        let new = find_before_new(&paths, i);
        println!("  → 新结果: {new:?}");

        println!("\n  ⚠️ 根因确认：");
        println!("    原：遍历**全部** off<i 的元素，找最后一个满足「距离<=64」的");
        println!("    新：只看**最后一个** off<i 的元素，距离不满足就放弃");
        println!("    → 当最近的路径也距离 >64 时，新实现返回 None（原实现同样 None，一致）");
        println!("    → 但当最后一个距离>64 而**更早的某个**距离<=64 时，");
        println!("      原实现能找到，新实现找不到 → **不一致**");

        // 统计不一致模式
        println!("\n=== 不一致模式统计 ===");
        let mut diff = 0;
        let mut orig_some_new_none = 0;
        let mut other = 0;
        for i in (0..700).step_by(3) {
            let o = find_before_original(&paths, i);
            let n = find_before_new(&paths, i);
            if o != n {
                diff += 1;
                if o.is_some() && n.is_none() {
                    orig_some_new_none += 1;
                } else {
                    other += 1;
                }
            }
        }
        println!("  总不一致: {diff}");
        println!("  其中「原有值/新为 None」: {orig_some_new_none}  ← 主因");
        println!("  其中「值不同」: {other}");

        println!("\n=== 影响评估 ===");
        println!("  原语义: 找「最近的、且距离<=64 的前驱路径」");
        println!("  新语义: 找「最近的前驱路径，若它距离<=64 则用，否则没有」");
        println!("  → 新实现会**漏掉**一批本应匹配的 hash↔path 配对");
        println!("  → 表现为：candidate_count 偏少、部分条目缺失");
        println!("  → 不会崩溃、不会产出错误条目，只是**召回率下降**");

        println!("\n=== 建议的正确二分 ===");
        println!("  let hi = paths.partition_point(|(off,_)| *off < i);        // off<i 上界");
        println!("  if hi > 0 {{");
        println!("      let (o, s) = &paths[hi-1];                             // 最后一个 off<i");
        println!("      if i <= o + 64 {{ Some((*o, s.clone())) }} else {{ None }}");
        println!("  }}");
        println!("  ↑ 等等，这与新实现**等价**！");

        println!("\n  → 所以真正的问题是：**原实现的条件本身**是");
        println!("    find(|off| off < i && i <= off + 64)");
        println!("    对 rev() 后的序列找第一个满足条件的 —— 那就是「最后一个满足条件的」");
        println!("    而新实现找的是「最后一个 off<i」，两者**不等价**");
        println!("    ✅ 正确二分：在 off<i 的子集中，二分找第一个 off >= i-64，");
        println!("       若该位置 < hi 则有解（但要取的是**最后一个**满足者 = hi-1，");
        println!("       仅当 hi-1 的 off >= i-64 时成立）");

        println!("\n  → 结论：新实现**仅在 hi-1 满足距离时正确**。");
        println!("    当 hi-1 不满足距离时，原实现返回 None（因为 rev 扫下来第一个就是 hi-1，");
        println!("    它不满足，继续扫更早的 —— 但更早的 off 更小，距离更大，更不可能满足）");
        println!("    ⚠️ 等等！off 升序 → i - off 随 off 减小而**增大** →");
        println!("       所以 hi-1 不满足时，更早的也一定不满足！");
        println!("       那么原实现应该在 hi-1 不满足时也返回 None 才对……");

        println!("\n  ❗ 那么不一致来自哪里？重新检查……");
        // 逐点定位第一个不一致
        for i in 0..700 {
            let o = find_before_original(&paths, i);
            let n = find_before_new(&paths, i);
            if o != n {
                println!("  第一个不一致 i={i}: 原={o:?} 新={n:?}");
                let hi = paths.partition_point(|&(off, _)| off < i);
                println!("    hi={hi}");
                for k in hi.saturating_sub(3)..hi.min(paths.len()) {
                    let (off, name) = &paths[k];
                    println!("      [{k}] off={off} {name}  距离={} 满足距离条件={}",
                             i - off, i <= off + 64);
                }
                break;
            }
        }
    }
}
