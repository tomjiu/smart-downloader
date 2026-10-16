//! 第三轮复现：config.rs 的 loopback 判定绕过 + 部分表默认值错位
//!
//! 对应审查发现：
//!   P1-a  config.rs:443  starts_with("127.") 前缀绕过 allowlist
//!   P1-b  config.rs 容器级 #[serde(default)] + derive Default 导致部分表默认值错位

#[cfg(test)]
mod round3 {
    // ══════════════ P1-a: is_loopback_addr 绕过 ══════════════

    /// 原样复刻 daemon/src/config.rs:424-444
    fn is_loopback_addr_original(addr: &str) -> bool {
        fn ip_loopback(ip: std::net::IpAddr) -> bool {
            match ip {
                std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                    Some(v4) => v4.is_loopback(),
                    None => v6.is_loopback(),
                },
                v4 => v4.is_loopback(),
            }
        }
        if let Ok(sa) = addr.parse::<std::net::SocketAddr>() {
            return ip_loopback(sa.ip());
        }
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return ip_loopback(ip);
        }
        // ↓ 原实现：前缀匹配（不安全）
        host.eq_ignore_ascii_case("localhost") || host.starts_with("127.") || host == "::1"
    }

    /// 加固版：只信任 parse 成功的结果 + localhost/::1 字面量
    fn is_loopback_addr_fixed(addr: &str) -> bool {
        fn ip_loopback(ip: std::net::IpAddr) -> bool {
            match ip {
                std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                    Some(v4) => v4.is_loopback(),
                    None => v6.is_loopback(),
                },
                v4 => v4.is_loopback(),
            }
        }
        if let Ok(sa) = addr.parse::<std::net::SocketAddr>() {
            return ip_loopback(sa.ip());
        }
        let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        if let Ok(ip) = host.parse::<std::net::IpAddr>() {
            return ip_loopback(ip);
        }
        // 只保留两个字面量；其它一律 fail closed
        host.eq_ignore_ascii_case("localhost") || host == "::1"
    }

    #[test]
    fn p1a_loopback_bypass() {
        println!("=== P1-a: is_loopback_addr 前缀绕过 ===\n");
        println!("{:<36} {:>10} {:>10}", "addr", "原实现", "加固版");
        println!("{}", "-".repeat(58));

        let cases = [
            // (输入, 期望是否回环, 说明)
            ("127.0.0.1:8787", true, "正确：真回环"),
            ("[::1]:8787", true, "正确：真回环"),
            ("localhost:8787", true, "正确：字面量"),
            // ── 绕过项 ──
            ("127.evil.com:8787", false, "★ 绕过：DNS 可解析到公网"),
            ("127.0.0.1.attacker.example:8787", false, "★ 绕过"),
            ("127.0.0.1.nip.io:8787", false, "★ 绕过：nip.io 万能解析"),
            ("127.0.0.1x:8787", false, "★ 绕过"),
            // ── 正确拒绝 ──
            ("0.0.0.0:8787", false, "正确拒绝：通配监听"),
            ("192.168.1.5:8787", false, "正确拒绝：内网"),
            ("example.com:8787", false, "正确拒绝：公网域名"),
        ];

        let mut bypass_count = 0;
        for (addr, want_loopback, note) in cases {
            let orig = is_loopback_addr_original(addr);
            let fixed = is_loopback_addr_fixed(addr);
            let mark = if orig != want_loopback {
                bypass_count += 1;
                " ← 原实现错误"
            } else {
                ""
            };
            println!("{:<36} {:>10} {:>10}   {}{}", addr, orig, fixed, note, mark);
        }

        println!("\n原实现错误 {} 项", bypass_count);
        assert!(bypass_count >= 3, "应有至少 3 项绕过");

        // 关键断言
        assert!(
            is_loopback_addr_original("127.evil.com:8787"),
            "原实现应错误放行 127.evil.com"
        );
        assert!(
            !is_loopback_addr_fixed("127.evil.com:8787"),
            "加固版应拒绝 127.evil.com"
        );

        println!("\n→ 下游影响（serve.rs:47 fail-closed 闸门）:");
        println!("   if !is_loopback_addr(&cfg.server.addr) && http_token.is_none() {{");
        println!("       return Err(...);   // 拒绝启动");
        println!("   }}");
        println!("→ addr = \"127.evil.com:8787\" + 未配 token → 闸门放行");
        println!("→ TcpListener::bind(\"127.evil.com:8787\") 经 DNS 解析到公网 IP");
        println!("→ API（含写文件 / hook 执行链）绑定公网且无鉴权");
    }

    // ══════════════ P1-b: 部分表默认值错位 ══════════════

    #[derive(Debug, serde::Deserialize)]
    #[serde(default)]
    struct DownloadCfg {
        dest_root: String,
        proxy: String,
    }
    // derive Default：字段类型默认值（String = ""）
    impl Default for DownloadCfg {
        fn default() -> Self {
            Self {
                dest_root: String::new(), // ← derive 生成的是空串
                proxy: String::new(),
            }
        }
    }

    #[derive(Debug, serde::Deserialize)]
    #[serde(default)]
    struct Config {
        download: DownloadCfg,
    }
    impl Default for Config {
        fn default() -> Self {
            Self {
                download: DownloadCfg {
                    dest_root: "./downloads".into(), // ← 手写默认：有值
                    proxy: String::new(),
                },
            }
        }
    }

    #[test]
    fn p1b_partial_table_default_mismatch() {
        println!("=== P1-b: 容器级 #[serde(default)] 默认值错位 ===\n");

        // 情形 1：整表缺失 → 走 Config::default() → dest_root 正确
        let absent: Config = toml::from_str("").expect("空配置");
        println!("[整表缺失]     dest_root = {:?}", absent.download.dest_root);

        // 情形 2：只写同表另一个键 → 走 DownloadCfg::default() → dest_root 变成空串
        let partial: Config =
            toml::from_str("[download]\nproxy = \"socks5://127.0.0.1:1080\"\n").expect("部分配置");
        println!("[只写 proxy]   dest_root = {:?}", partial.download.dest_root);

        println!();
        assert_eq!(absent.download.dest_root, "./downloads", "整表缺失应走正确默认");
        assert_eq!(partial.download.dest_root, "", "部分表会退化为 derive Default");

        println!("→ 用户只想改代理，写 [download] proxy = \"...\"");
        println!("→ dest_root 静默变成空串（而非 ./downloads）");
        println!("→ 空 dest_root 会一路传到 dest_roots() / 落盘 join");
        println!("→ 行为退化为「相对当前工作目录」→ 落盘位置静默改变");

        println!("\n同样模式影响其它表:");
        println!("  [server] http_token = \"x\"  → addr = \"\" → bind 失败 → daemon 起不来");
        println!("  [bt] max_upload_kb_s = 100  → bt.enabled = false → BT 静默关闭");
        println!("\n→ 修法: 为各子表手写 impl Default，值与 Config::default() 保持一致");
    }
}
