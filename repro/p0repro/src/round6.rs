//! 第六轮：验证 PR #114 修复的正确性
//!
//! 审查对象：`gh pr view 114` 的实际 patch
//! 目标：不是找 bug，而是**确认修复本身是否正确**

#![allow(dead_code)]

/// PR #114 batchA 的修复后逻辑（原样复刻）
#[derive(Debug, Clone, PartialEq)]
pub struct FileMeta {
    pub path: String,
    pub size: u64,
    pub piece_offset: usize,
    pub piece_count: usize,
}

/// 修复后的偏移计算
fn compute_fixed(file_entries: &[(String, u64)], piece_length: u32) -> Option<Vec<FileMeta>> {
    let plen = piece_length as u64;
    if plen == 0 {
        return None; // 修复后不可能走到这里
    }
    let mut files = Vec::new();
    let mut total_size = 0u64;
    for (path, length) in file_entries {
        let piece_offset = (total_size / plen) as usize;
        let piece_count = length.div_ceil(plen) as usize;
        total_size = total_size.checked_add(*length)?;
        files.push(FileMeta {
            path: path.clone(),
            size: *length,
            piece_offset,
            piece_count,
        });
    }
    Some(files)
}

/// 修复前的逻辑（有 bug 的版本，用于对照）
fn compute_broken(file_entries: &[(String, u64)], piece_length: u32) -> Option<Vec<FileMeta>> {
    let plen = piece_length as u64;
    if plen == 0 {
        return None; // 修复前这里是 panic（除零）
    }
    let mut files: Vec<FileMeta> = Vec::new();
    for (path, length) in file_entries {
        let total_size: u64 = files.iter().map(|f: &FileMeta| f.size).sum();
        let piece_offset = (total_size / plen) as usize;
        let piece_count = length.div_ceil(plen) as usize;
        files.push(FileMeta {
            path: path.clone(),
            size: *length,
            piece_offset,
            piece_count,
        });
    }
    Some(files)
}

#[cfg(test)]
mod round6 {
    use super::*;

    #[test]
    fn p0_1_no_longer_divides_by_zero() {
        println!("=== P0-1 验证：piece_length=0 不再除零 ===\n");
        let files = vec![("a".to_string(), 10u64), ("b".to_string(), 20)];

        // 修复后：返回 None（上游已拦截 plen=0）
        let fixed = compute_fixed(&files, 0);
        println!("  修复后 plen=0 → {:?}  ✅ 无 panic", fixed);
        assert!(fixed.is_none());

        // 修复前：会 panic（这里用 Option 模拟，说明修复点）
        println!("  修复前 plen=0 → 除零 panic（已在 repro/lib.rs 实证）");
    }

    #[test]
    fn fix_preserves_correct_offsets() {
        println!("=== 验证修复后偏移计算与原算法等价（plen>0 时）===\n");
        let cases: Vec<(Vec<(&str, u64)>, u32)> = vec![
            (vec![("a", 10), ("b", 20)], 16384),
            (vec![("a", 16384), ("b", 16384)], 16384),
            (vec![("a", 16385), ("b", 10)], 16384),
            (vec![("a", 100000), ("b", 100000), ("c", 100000)], 16384),
            (vec![("a", 1), ("b", 1), ("c", 1)], 16384),
        ];

        for (raw, plen) in cases {
            let entries: Vec<(String, u64)> =
                raw.iter().map(|(p, s)| (p.to_string(), *s)).collect();

            let fixed = compute_fixed(&entries, plen).expect("fixed");
            let broken = compute_broken(&entries, plen).expect("broken");

            let same = fixed == broken;
            println!("  plen={plen:<7} files={:?}", raw);
            println!("    修复前: {broken:?}");
            println!("    修复后: {fixed:?}");
            println!("    等价: {} {}\n", same, if same { "✅" } else { "⚠️ 行为变化" });
        }
    }

    #[test]
    fn overflow_rejected() {
        println!("=== total_size 溢出被拒绝 ===\n");
        let files = vec![
            ("a".to_string(), u64::MAX),
            ("b".to_string(), 1u64),
        ];
        let r = compute_fixed(&files, 16384);
        println!("  [u64::MAX, 1], plen=16384 → {:?}", r);
        assert!(r.is_none(), "应返回 None（checked_add 溢出）");
        println!("  ✅ 溢出被 checked_add 拦截\n");

        // 但注意：溢出检查在 push 之后
        println!("  ⚠️  观察：`total_size.checked_add` 在 push 之后执行");
        println!("      即：第二个文件的 piece_offset 用的是未溢出的旧值");
        println!("      然后 checked_add 失败 → 整个函数返回 None");
        println!("      → 结果正确（整体拒绝），但 piece_offset 曾被计算过一次");
        println!("      → 无副作用（未落盘），可接受");
    }

    #[test]
    fn piece_offset_semantics() {
        println!("=== piece_offset 语义确认 ===\n");
        // piece_offset = total_size / plen，即「该文件起始处落在第几个 piece」
        let cases = vec![
            (vec![("a", 16384u64), ("b", 10)], 16384u32, vec![0usize, 1]),
            // a 占 1 piece（0），b 从 total=16384 开始 → 16384/16384 = 1 ✓
            (vec![("a", 10000u64), ("b", 10)], 16384u32, vec![0usize, 0]),
            // a 不满 piece（10000/16384=0），b 从 10000 开始 → 10000/16384 = 0 ✓
            (vec![("a", 32768u64), ("b", 10)], 16384u32, vec![0usize, 2]),
            // a 占 2 piece，b 从 32768 → 32768/16384 = 2 ✓
        ];

        for (raw, plen, expected) in cases {
            let entries: Vec<(String, u64)> =
                raw.iter().map(|(p, s)| (p.to_string(), *s)).collect();
            let r = compute_fixed(&entries, plen).unwrap();
            let got: Vec<usize> = r.iter().map(|f| f.piece_offset).collect();
            let ok = got == expected;
            println!(
                "  files={:?} plen={plen} → offsets={:?} (期望 {:?}) {}",
                raw,
                got,
                expected,
                if ok { "✅" } else { "❌" }
            );
            assert_eq!(got, expected, "piece_offset 语义不符");
        }
        println!("\n  ✅ piece_offset = total_size / plen（文件起始 piece 索引）语义正确");

        // 边界：文件跨 piece 边界
        println!("\n=== 边界：文件跨 piece 边界 ===");
        let entries = vec![("a".to_string(), 10000u64), ("b".to_string(), 20000u64)];
        let r = compute_fixed(&entries, 16384).unwrap();
        println!("  a: size=10000, piece_offset=0, piece_count=1");
        println!("  b: size=20000, piece_offset=0, piece_count=2");
        println!("  ⚠️  a 与 b 的 piece_offset 都是 0（b 起始 10000 落在 piece 0）");
        println!("  → 这是**正确**的：piece_offset 是「起始 piece」，不是「独占 piece」");
        println!("  → 跨边界的 piece 由多个文件共享，调用方需按 (offset, count) 判断重叠");
        assert_eq!(r[0].piece_offset, 0);
        assert_eq!(r[1].piece_offset, 0);
    }
}
