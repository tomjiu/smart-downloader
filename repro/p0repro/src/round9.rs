//! 第九轮：审计 PR #114 batch10 的 FTP is_terminal 修复
//!
//! 审查点：修复只化简了「恒真死代码」（`starts_with("550") || starts_with("5")`），
//! 但**保留了「所有 5xx 都是永久失败」的激进语义**。
//!
//! 问题：is_terminal 用于 4 个场景，其中 3 个是**探测**（probe_list / probe_size），
//! 而 `500 Command not understood` / `502 Not implemented` 在 FTP 服务器上是
//! **能力缺失的常见表达**，应当降级重试或走替代路径，而非直接放弃整个任务。

#![allow(dead_code)]

/// PR #114 修复后的实现（batch10，L1349）
fn is_terminal_fixed(e: &str) -> bool {
    e.starts_with('5')
}

/// 修复前的实现
fn is_terminal_before(e: &str) -> bool {
    e.starts_with("550") || e.starts_with("5")
}

/// 建议的细化实现：区分「永久失败」与「能力缺失」
fn is_terminal_refined(e: &str) -> bool {
    // RFC 959 5xx = Permanent Negative Completion，但下列码在实务中
    // 常表示「服务器能力缺失/语法不支持」，应允许重试或降级：
    //   500 Command not understood         → 能力缺失（如不支持 SIZE/REST）
    //   502 Command not implemented        → 能力缺失
    //   504 Command not implemented for that parameter → 能力缺失
    // 而真正的永久失败：
    //   530 Not logged in
    //   550 Requested action not taken (file unavailable / no access)
    //   551 Page type unknown
    //   552 Exceeded storage allocation
    //   553 Bad file name
    const CAPABILITY_MISSING: [&str; 3] = ["500", "502", "504"];
    if CAPABILITY_MISSING.iter().any(|c| e.starts_with(c)) {
        return false; // 可重试/可降级
    }
    e.starts_with('5')
}

#[cfg(test)]
mod round9 {
    use super::*;

    #[test]
    fn ftp_is_terminal_analysis() {
        println!("=== FTP is_terminal 三版对比 ===\n");
        println!("{:<42} {:>10} {:>10} {:>10}", "服务器应答", "修复前", "修复后", "建议");
        println!("{}", "-".repeat(78));

        let cases = [
            // (应答片段, 说明)
            ("550 Requested action not taken", "文件不存在/无权限 —— 真永久"),
            ("530 Not logged in", "未登录 —— 真永久"),
            ("552 Exceeded storage allocation", "存储超限 —— 真永久"),
            ("553 Bad file name", "文件名非法 —— 真永久"),
            ("500 Command not understood", "★ 能力缺失：服务器不支持该命令"),
            ("502 Command not implemented", "★ 能力缺失"),
            ("504 Command not implemented for that parameter", "★ 能力缺失"),
            ("421 Service not available", "临时 —— 可重试（4xx）"),
            ("425 Can't open data connection", "临时 —— 可重试（4xx）"),
        ];

        for (resp, note) in cases {
            println!(
                "{:<42} {:>10} {:>10} {:>10}   {}",
                resp,
                is_terminal_before(resp),
                is_terminal_fixed(resp),
                is_terminal_refined(resp),
                note
            );
        }

        println!("\n=== 关键分歧点 ===");
        for code in ["500", "502", "504"] {
            let r = format!("{code} something");
            assert_eq!(
                is_terminal_before(&r),
                is_terminal_fixed(&r),
                "修复前后对 {code} 判定相同"
            );
            assert!(
                !is_terminal_refined(&r),
                "建议版应把 {code} 视为可重试"
            );
            println!(
                "  {code}: 修复前={} 修复后={} → **两版都是 true（放弃），建议版是 false（重试）**",
                is_terminal_before(&r),
                is_terminal_fixed(&r)
            );
        }

        println!("\n=== 影响面（is_terminal 的 4 个调用点）===");
        println!("  L256  probe_list  —— 探测目录列表（能力缺失时整个目录任务失败）");
        println!("  L862  download_segment_attempts —— 段下载（终态则不二分降粒）");
        println!("  L905  download_segment —— 段重试（终态则不再退避重试）");
        println!("  L1498 probe_size   —— 探测文件大小（★ 最关键）");

        println!("\n=== 为什么 probe_size 最关键 ===");
        println!("  SIZE 命令是可选扩展（RFC 3659），不少服务器返回：");
        println!("    500 Command not understood");
        println!("    502 Command not implemented");
        println!("  这是**能力缺失**而非文件问题。修复前同修复后都将它判为终态");
        println!("  → probe_size 不重试（正常，重试也没用）");
        println!("  → 但若调用方因此**放弃整个任务**，就是错误行为：");
        println!("     应该降级为「未知总长单流下载」而非失败");

        println!("\n=== 结论 ===");
        println!("  ✅ 修复正确消除了恒真死代码（starts_with(\"550\") 被 starts_with(\"5\") 包含）");
        println!("  ⚠️  但保留了「所有 5xx 终态」的语义 —— 这与我原报告 P1-5 的诉求");
        println!("     （「500/504 是可重试/可降级信号」）不一致");
        println!("  → 定性：死代码清理正确，但异常分类未细化。非阻断，建议后续跟进。");
    }
}
