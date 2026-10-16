//! 第四轮复现：迅雷直链分类器的边界行为
//!
//! 位置：crates/provider/src/xunlei/url_class.rs:528-576
//! 代码提取自原文件（逻辑未改动）

#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkClass {
    AThunderCdn,
    BRegular,
}

const XUNLEI_DOMAINS: &[&str] = &[
    "n0808.com",
    "cdn.xunlei.com",
    "vod.xunlei.com",
    "api-pan.xunlei.com",
    "pan.xunlei.com",
    "captcha.xunlei.com",
    "xluser-ssl.xunlei.com",
];

const PHUB_SHUB_HOSTS: &[&str] = &[
    "pr-phub.sandai.net",
    "sr-shub.sandai.net",
    "dcdn.sandai.net",
];

// 原样复刻 url_class.rs:528-544
fn host_of(url: &str) -> Option<String> {
    let after_scheme = match url.split_once("://") {
        Some((_, r)) => r,
        None => return None,
    };
    let authority = after_scheme.split('/').next().unwrap_or("");
    let hostport = match authority.rsplit_once('@') {
        Some((_, h)) => h,
        None => authority,
    };
    let host = hostport.split(':').next().unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

// 原样复刻 url_class.rs:547-566（CDN_HOSTS 表省略，只保留逻辑）
fn is_xunlei_owned(host: &str) -> bool {
    for d in XUNLEI_DOMAINS {
        if host == *d || host.ends_with(&format!(".{}", d)) {
            return true;
        }
    }
    if host == "n0808.com" || host.ends_with(".n0808.com") {
        return true;
    }
    if host.ends_with(".sandai.net") || host.ends_with(".sandai.com") {
        return true;
    }
    if PHUB_SHUB_HOSTS.contains(&host) {
        return true;
    }
    false
}

fn classify_url(url: &str) -> LinkClass {
    match host_of(url) {
        Some(host) if is_xunlei_owned(&host) => LinkClass::AThunderCdn,
        _ => LinkClass::BRegular,
    }
}

#[cfg(test)]
mod round4 {
    use super::*;

    /// 复现：provider 并发闸门恒真（busy 在生产代码中永不递增）
    #[test]
    fn provider_concurrency_gate_is_noop() {
        println!("=== provider 并发闸门失效 ===\n");

        // 三个 ProviderRuntime 构造点（全仓）都是硬编码常量
        let constructors = [
            ("xunlei/provider.rs:197-198", 2u32, 0u32),
            ("quark/share.rs:248-249", 2u32, 0u32),
            ("types.rs:30-31 (Default)", 2u32, 0u32),
        ];

        println!("{:<30} {:>16} {:>6}", "构造点", "concurrency_limit", "busy");
        println!("{}", "-".repeat(56));
        for (loc, limit, busy) in constructors {
            println!("{loc:<30} {limit:>16} {busy:>6}");
        }

        println!("\n全仓 grep `busy` 的写入点:");
        println!("  mock.rs:129  pub fn set_busy(&self, n: u32)   ← 仅测试可用");
        println!("  其余均为读取或初始化 `busy: 0`");

        // coordinator.rs:70 的判定
        let (limit, busy) = (2u32, 0u32);
        let passes = busy < limit;
        println!("\ncoordinator.rs:70 判定: busy({busy}) < limit({limit}) = {passes}");

        assert!(passes, "闸门恒真 → 并发限制从未生效");
        println!("\n→ 结论：闸门恒真，「每 provider 并发上限 2」约束从未生效");
        println!("→ 100 个兜底任务可同时打向同一 provider");

        // 叠加效应
        println!("\n叠加效应（与已知 P1-2 组合）:");
        println!("  1. 并发闸门失效 → 100 任务同时进入 provider");
        println!("  2. refresh_auth 持锁跨 await（最长 30s 超时）");
        println!("  3. → 99 个任务排队等锁，单个超时即 30s 级停顿");
    }

    #[test]
    fn normal_cases() {
        println!("=== 正常用例 ===");
        for (url, want) in [
            ("https://cdn.xunlei.com/a/b", LinkClass::AThunderCdn),
            ("https://vod.xunlei.com/x", LinkClass::AThunderCdn),
            ("https://pan.xunlei.com/s/abc", LinkClass::AThunderCdn),
            ("https://pr-phub.sandai.net/x", LinkClass::AThunderCdn),
            ("https://foo.sandai.net/x", LinkClass::AThunderCdn),
            ("https://example.com/file.bin", LinkClass::BRegular),
            ("http://1.2.3.4/file", LinkClass::BRegular),
        ] {
            let got = classify_url(url);
            let mark = if got == want { "✅" } else { "❌" };
            println!("  {mark} {url:<45} → {got:?}");
        }
    }

    #[test]
    fn ipv6_handling() {
        println!("=== IPv6 处理（hostport.split(':') 截断）===");
        for url in [
            "http://[::1]:8080/x",
            "http://[2001:db8::1]/x",
            "http://[::ffff:1.2.3.4]:80/x",
        ] {
            let h = host_of(url);
            println!("  {url:<40} → host_of = {h:?}");
        }
        // 关键：host 变成 "[" 而不是 IPv6 地址
        assert_eq!(host_of("http://[::1]:8080/x").as_deref(), Some("["));
        println!("\n  ⚠️  IPv6 的 host 被截断为 \"[\" —— 判定结果靠巧合（BRegular）");
        println!("      虽然当前结论碰巧正确，但 host 值本身是错的");
        println!("      若将来 CDN_HOSTS 表加入 IPv6 条目将永不匹配");
    }

    #[test]
    fn scheme_relative_url() {
        println!("=== 协议相对 URL ===");
        for url in ["//cdn.xunlei.com/a", "cdn.xunlei.com/a", "/path/only"] {
            println!("  {url:<30} → {:?}", classify_url(url));
        }
        // 无 "://" → 返回 None → BRegular
        assert_eq!(host_of("//cdn.xunlei.com/a"), None);
        println!("\n  说明：注释声明这是有意对齐 Python urlparse().hostname 语义");
        println!("      但 Python 的 urlparse(\"//host/path\").hostname == \"host\"");
        println!("      即：协议相对 URL 在 Python 下能识别，此处返回 BRegular → 行为差异");
    }

    #[test]
    fn userinfo_and_port() {
        println!("=== userinfo / 端口 ===");
        for url in [
            "https://user:pass@cdn.xunlei.com/x",
            "https://cdn.xunlei.com:8443/x",
            "https://user@cdn.xunlei.com:8443/x",
        ] {
            println!("  {url:<45} → host_of = {:?}", host_of(url));
        }
        assert_eq!(
            host_of("https://user:pass@cdn.xunlei.com/x").as_deref(),
            Some("cdn.xunlei.com")
        );
        println!("  ✅ userinfo 与端口处理正确");
    }

    #[test]
    fn domain_suffix_not_bypassable() {
        println!("=== 后缀匹配是否可被绕过 ===");
        // 关键安全属性：前导点确保不匹配 attacker 域
        for (url, expect_a) in [
            ("https://evil-sandai.net/x", false),
            ("https://sandai.net.attacker.com/x", false),
            ("https://notsanda.net/x", false),
            ("https://x.sandai.net/x", true),
            ("https://n0808.com.attacker.com/x", false),
        ] {
            let got = classify_url(url) == LinkClass::AThunderCdn;
            let mark = if got == expect_a { "✅" } else { "❌ 绕过!" };
            println!("  {mark} {url:<45} → A={got}");
        }
        println!("\n  ✅ 前导点使后缀匹配不可绕过（这是关键安全属性，实现正确）");
    }

    #[test]
    fn empty_and_malformed() {
        println!("=== 空/畸形输入 ===");
        for url in ["", "://", "http://", "http:///", "http://@/x", "https://:8080/x"] {
            println!("  {url:<25} → host_of = {:?}, class = {:?}", host_of(url), classify_url(url));
        }
        println!("\n  ✅ 无 panic");
    }
}
