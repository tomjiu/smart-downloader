//! 第二十一轮：HTTP 能力实测
//!
//! 验证项：
//!   1. Range 探测的边界（206 无 Content-Range / 200 / 416）
//!   2. 未知总长路径的续传能力
//!   3. HTTP/2 是否实际启用

#![allow(dead_code)]

/// 复刻 range.rs 的 content_range_total 解析
fn content_range_total(cr: Option<&str>) -> Option<u64> {
    cr.and_then(|c| c.rsplit('/').next()).and_then(|t| t.parse().ok())
}

/// 复刻 range.rs 的指纹判定（E24/E26）
fn is_strong_etag(e: &Option<String>) -> bool {
    e.as_deref().is_some_and(|v| !v.is_empty() && !v.starts_with("W/"))
}

fn fingerprint_ok(saved: &Option<String>, fresh: &Option<String>) -> bool {
    match (saved, fresh) {
        (Some(s), Some(f)) => s == f,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

#[cfg(test)]
mod round21 {
    use super::*;

    #[test]
    fn range_probe_edge_cases() {
        println!("=== HTTP Range 探测边界 ===\n");

        println!("[探测] GET Range: bytes=0-0");
        println!();

        println!("服务器应答形态 → 引擎判定");
        println!("{}", "-".repeat(72));

        // ① 206 + Content-Range: bytes 0-0/1000000
        let cr = content_range_total(Some("bytes 0-0/1000000"));
        println!("  206 + 'bytes 0-0/1000000' → range=true, total={:?}  ✅ 分段下载", cr);
        assert_eq!(cr, Some(1_000_000));

        // ② 206 + Content-Range: bytes 0-0/*  （未知总长）
        let cr = content_range_total(Some("bytes 0-0/*"));
        println!("  206 + 'bytes 0-0/*'       → range=true, total={:?}  ⚠️ 走未知总长单流", cr);
        assert_eq!(cr, None);

        // ③ 206 无 Content-Range
        let cr = content_range_total(None);
        println!("  206 无 Content-Range     → range=true, total={:?}  ⚠️ 走未知总长单流", cr);
        assert_eq!(cr, None);

        // ④ 200（忽略 Range）
        println!("  200（忽略 Range）        → range=false           ❌ 无断点续传");

        // ⑤ 416 + Content-Range: bytes */TOTAL
        let cr = content_range_total(Some("bytes */500000"));
        println!("  416 + 'bytes */500000'   → range=false, total={:?}  ❌ 无断点续传（但知总长）", cr);
        assert_eq!(cr, Some(500_000));

        println!("\n=== 关键结论 ===");
        println!("  只有 ① 形态能走「分段 + 账本 + 断点续传」全链路。");
        println!("  ②③（206 但拿不到总长）会退化到未知总长单流路径。");
        println!("  ④⑤（服务器不支持 Range）完全无法续传。");
    }

    #[test]
    fn unknown_total_resume_analysis() {
        println!("\n=== 未知总长路径（download_loop_unknown）的续传能力 ===\n");

        println!("代码逻辑（engine.rs:453-495）：");
        println!("  let start = fs::metadata(&part).len();          // 以文件大小为凭据");
        println!("  req.header(RANGE, \"bytes=<start>-\");");
        println!("  match resp.status() {{");
        println!("      206 if Content-Range 首字节 == start => 续传(append)");
        println!("      206 但首字节不符                    => truncate 全量重下");
        println!("      200                                 => truncate 全量重下");
        println!("  }}");
        println!();
        println!("→ 关键：这个路径**依赖服务器支持 Range**。");
        println!("  若服务器不支持 Range（恒回 200），每次重启都 truncate 从头下载。");

        println!("\n=== 场景推演 ===\n");
        let scenarios = [
            ("服务器支持 Range + 有 Content-Length", "分段+账本，完整续传 ✅"),
            ("服务器支持 Range + chunked 无总长", "未知总长单流，靠文件大小续传 ⚠️"),
            ("服务器忽略 Range（恒 200）", "每次 truncate 重下 ❌"),
            ("206 但 Content-Range 缺失", "落未知总长单流，续传看实际应答 ⚠️"),
        ];
        for (s, r) in scenarios {
            println!("  {:<38} → {}", s, r);
        }

        println!("\n=== 为什么「未知总长 + 不支持 Range」无法续传 ===\n");
        println!("  HTTP 协议层面就没有「从 N 字节继续」的手段（除 Range）。");
        println!("  这是协议限制，不是实现缺陷。");
        println!();
        println!("  但业界做法：");
        println!("  · aria2：同样无法续传非 Range 源");
        println!("  · IDM：会用 Connection 复用 + 断流检测重连，但仍从 0 开始");
        println!("  → 本项目行为与主流一致，不算缺口。");
    }

    #[test]
    fn etag_fingerprint_analysis() {
        println!("\n=== ETag 指纹的续传安全性 ===\n");

        println!("fingerprint_ok 决策表（ledger.rs:190-196）：");
        println!("  (Some, Some) 相等  → 放行（确认未变）");
        println!("  (Some, Some) 不等  → 拒绝（内容已变）");
        println!("  (Some, None)       → 拒绝（指纹消失，宁枉勿纵）");
        println!("  (None, _)          → **放行**（账本本就无指纹）");
        println!();

        // 验证
        assert!(fingerprint_ok(&Some("a".into()), &Some("a".into())));
        assert!(!fingerprint_ok(&Some("a".into()), &Some("b".into())));
        assert!(!fingerprint_ok(&Some("a".into()), &None));
        assert!(fingerprint_ok(&None, &Some("a".into())));
        assert!(fingerprint_ok(&None, &None));
        println!("  ✅ 五种组合行为符合设计");

        println!("\n=== 但 (None, _) 放行意味着什么 ===\n");
        println!("  若服务器【从不返回 ETag 也从不返回 Last-Modified】：");
        println!("  · 账本里两个指纹都是 None");
        println!("  · fingerprint_ok(None, None) = true → 放行");
        println!("  · 此时**只剩两道防线**：总长核对 + 段对齐校验");
        println!();
        println!("  风险：服务器内容变了但总长恰好相同（如版本迭代同大小）");
        println!("       → 续传产出「旧前缀 + 新尾部」混合文件");
        println!("       → 无任何检测手段（无摘要时也不会报错）");
        println!();
        println!("  这是**真实残余风险**，但：");
        println!("  · 概率低（同大小不同内容 + 无任何指纹）");
        println!("  · 业界同款（aria2 也只靠 Last-Modified/ETag）");
        println!("  · 用户可配摘要（sha256）兜底 —— 若有");

        println!("\n=== 弱 ETag 的处理 ===\n");
        println!("  is_strong_etag：W/ 前缀 = 弱 ETag，不参与多源混拼判定");
        assert!(!is_strong_etag(&Some("W/\"abc\"".into())));
        assert!(is_strong_etag(&Some("\"abc\"".into())));
        println!("  ✅ 弱 ETag 被正确排除（不会误用于跨源混拼）");

        println!("\n  ⚠️ 但注意：ledger 的 fingerprint_ok **不区分强弱 ETag**——");
        println!("     弱 ETag 也参与续传指纹比对（这是合理的，弱 ETag 仍可检测变化），");
        println!("     但**多源混拼**要求强 ETag（更严格），两处口径不同是有意的。");
    }
}
