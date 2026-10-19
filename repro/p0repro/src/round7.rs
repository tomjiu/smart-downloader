//! 第七轮：评估 PR #114 的 fsync 性能影响
//!
//! ⚠️ **测试环境限制声明**
//!
//! 本机 E: 盘（项目所在盘 + TEMP）位于 **5400rpm 机械硬盘**（ST1000LM035），
//! 单次 16MB fsync 实测 **4097ms**；C: 盘为 NVMe SSD（ZHITAI TiPlus7100），
//! 单次 16MB fsync 实测 **9-12ms**。两者差 ~400 倍。
//!
//! 因此本测试**必须**在 SSD 上进行，且结果仅代表 SSD 场景。
//! 机械盘场景下，PR 新增的每段 fsync 会使下载吞吐崩溃（已在 E: 盘实测到
//! 2022 MB/s → 6.4 MB/s）。
//!
//! 结论定位：
//! - SSD 上：fsync 开销 6-30%（100-500 MB/s 网速下），可接受但非免费
//! - 机械盘上：灾难性（下载速度被 fsync 完全支配）

#[cfg(test)]
mod round7 {
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::Instant;

    /// 选择 SSD 路径（优先 C:\Users\Public，回退 TEMP）
    fn bench_dir() -> PathBuf {
        let candidates = [
            PathBuf::from(r"C:\Users\Public"),
            std::env::temp_dir(),
        ];
        for c in &candidates {
            if c.exists() {
                return c.join("smartdl-fsync-bench");
            }
        }
        std::env::temp_dir()
    }

    /// 测量 N 次「写 seg_mb + 可选 sync_data」的耗时（秒）
    fn measure(path: &std::path::Path, rounds: usize, seg_mb: usize, sync: bool) -> f64 {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .expect("open");
        let buf = vec![0xABu8; seg_mb * 1024 * 1024];
        let t0 = Instant::now();
        for _ in 0..rounds {
            f.write_all(&buf).expect("write");
            if sync {
                f.sync_data().expect("sync");
            }
        }
        t0.elapsed().as_secs_f64()
    }

    #[test]
    fn fsync_cost_measurement() {
        let dir = bench_dir();
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("bench.bin");

        println!("=== fsync 成本实测 ===");
        println!("测试路径: {}\n", path.display());

        const ROUNDS: usize = 8;
        const SEG_MB: usize = 16;
        let total_mb = (ROUNDS * SEG_MB) as f64;

        // 预热（排除首写开销）
        let _ = measure(&path, 1, 1, true);

        let t_no = measure(&path, ROUNDS, SEG_MB, false);
        let t_yes = measure(&path, ROUNDS, SEG_MB, true);

        let per_sync_ms = (t_yes - t_no) * 1000.0 / ROUNDS as f64;

        println!("  {} × {}MB = {:.0}MB", ROUNDS, SEG_MB, total_mb);
        println!("    无 sync_data : {:.3}s  → {:.1} MB/s", t_no, total_mb / t_no);
        println!("    有 sync_data : {:.3}s  → {:.1} MB/s", t_yes, total_mb / t_yes);
        println!("\n    单次 fsync 成本: {:.2} ms/段\n", per_sync_ms);

        // 环境判别
        if per_sync_ms > 500.0 {
            println!("  ⚠️⚠️ 单次 fsync > 500ms —— 本机测试盘是机械盘，");
            println!("      以下外推不适用于 SSD 场景，仅说明机械盘上的灾难性影响\n");
        }

        println!("=== 对下载吞吐的影响外推（段粒度 16MB）===");
        println!("\n  {:>10}  {:>8}  {:>14}  {:>12}  {:>10}",
                 "网速", "段/秒", "fsync开销/秒", "有效吞吐", "损失");
        println!("  {}", "-".repeat(62));

        for speed_mbs in [10.0f64, 50.0, 100.0, 200.0, 500.0] {
            let segs_per_sec = speed_mbs / SEG_MB as f64;
            let overhead_ms = segs_per_sec * per_sync_ms;
            let overhead_frac = (overhead_ms / 1000.0).min(0.99);
            let effective = speed_mbs * (1.0 - overhead_frac);
            println!(
                "  {:>7.0} MB/s  {:>8.2}  {:>11.1} ms  {:>8.1} MB/s  {:>7.1}%",
                speed_mbs,
                segs_per_sec,
                overhead_ms,
                effective,
                overhead_frac * 100.0
            );
        }

        // 账本 sync_all 成本
        println!("\n=== 账本原子写（含 sync_all）成本 ===");
        let ledger = dir.join("ledger.json");
        let t0 = Instant::now();
        for i in 0..ROUNDS {
            let tmp = ledger.with_extension(format!("tmp{i}"));
            let mut f = std::fs::File::create(&tmp).expect("create");
            f.write_all(b"{\"version\":1,\"done\":[]}").expect("write");
            f.sync_all().expect("sync");
            drop(f);
            std::fs::rename(&tmp, &ledger).expect("rename");
        }
        let t_ledger = t0.elapsed().as_secs_f64();
        println!(
            "  {} 次: {:.3}s → {:.2} ms/次",
            ROUNDS,
            t_ledger,
            t_ledger * 1000.0 / ROUNDS as f64
        );

        std::fs::remove_dir_all(&dir).ok();

        println!("\n=== 审计结论 ===");
        println!("  正确性：P0-4 修复方向正确（WAL 排序：数据先于元数据）。");
        println!("  成本：PR 未量化。SSD 上 6-30% 吞吐损失（视网速）；");
        println!("        机械盘上吞吐被 fsync 完全支配（实测 2022 → 6.4 MB/s）。");
        println!("  建议：");
        println!("    1. 机械盘用户应可配置 `fsync_per_segment=false`（默认开）");
        println!("    2. 或改为「每 N 段 / T 秒批量 fsync」，N=8 或 T=2s");
        println!("    3. min_split 若降到 1-4MB，fsync 频率上升 4-16 倍，必须批量");
    }
}
