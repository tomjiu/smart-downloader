//! IP 封禁域：批量封禁/解禁、DAT/P2P 段导入、持久化与规范化。

use super::*;
impl DaemonState {
    /// Session 级 IP 封禁（Task 46，qbit「永久封禁」对标）：逐条校验 IP →
    /// 下发 BT 引擎 → 成功者入列表并落盘。返回逐条结果（部分成功语义）。
    /// 锁纪律：短锁判存，await 在锁外（guard !Send 不跨 await）。
    /// batch5-P2：全程持 `ban_ops` 串行锁（tokio Mutex，guard Send 可跨
    /// await）——原 contains→await→push 三段非原子，并发 ban(A)/unban(A)
    /// 交錯使列表与内核脱节；batch5 另支持 v4-mapped IPv6 归一与区间条目。
    pub async fn ban_ips(&self, ips: Vec<String>) -> Vec<(String, Result<(), String>)> {
        let _serial = self.ban_ops.lock().await; // 串行化：与 unban/import 互斥
        let mut out = Vec::with_capacity(ips.len());
        let engine = self.engine_for(EngineKind::Bt).ok();
        let mut newly: Vec<String> = Vec::new();
        for ip in ips {
            let ip_trim = ip.trim().to_string();
            let canon = normalize_ban_entry(&ip_trim);
            let res = match canon {
                Err(e) => Err(format!("非法 IP: {e}")),
                Ok(c) => {
                    // 锁纪律：短锁，不跨 await（guard !Send）
                    // 审查修复（P2）：contains + push 合并为单次加锁，消除
                    // 并发 ban 同一 IP 时两段短锁窗口产生的重复条目（重复项
                    // 会持久化进 bans.json）。
                    let already = self.bt_bans.lock().contains(&c);
                    if already {
                        Ok(()) // 幂等：已封禁直接成功
                    } else if let Some(bt) = engine.as_ref().and_then(|e| e.bt()) {
                        match bt.ban_ip(&c).await {
                            Ok(()) => {
                                // 引擎成功后确认式入列（双重 contains 防御并发窗口）
                                let mut bans = self.bt_bans.lock();
                                if !bans.contains(&c) {
                                    bans.push(c.clone());
                                    newly.push(c);
                                }
                                Ok(())
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    } else {
                        Err("BT 引擎不可用（feature 未启用）".into())
                    }
                }
            };
            out.push((ip_trim, res));
        }
        if !newly.is_empty() {
            self.persist_bans();
        }
        out
    }

    /// 解除封禁（Task 46）：逐条下发 + 列表移除 + 落盘（幂等）。
    /// 锁纪律：短锁判存/移除，await 一律在锁外（guard !Send 不跨 await）；
    /// batch5-P2：全程持 `ban_ops` 串行锁（同 ban_ips）。
    pub async fn unban_ips(&self, ips: Vec<String>) -> Vec<(String, Result<(), String>)> {
        let _serial = self.ban_ops.lock().await; // 串行化：与 ban/import 互斥
        let mut out = Vec::with_capacity(ips.len());
        let engine = self.engine_for(EngineKind::Bt).ok();
        let mut removed = false;
        for ip in ips {
            let ip_trim = ip.trim().to_string();
            let canon = normalize_ban_entry(&ip_trim);
            let res = match canon {
                Err(e) => Err(format!("非法 IP: {e}")),
                Ok(c) => {
                    let present = self.bt_bans.lock().contains(&c);
                    if !present {
                        Ok(()) // 幂等：未封禁也算成功
                    } else if let Some(bt) = engine.as_ref().and_then(|e| e.bt()) {
                        match bt.unban_ip(&c).await {
                            Ok(()) => {
                                self.bt_bans.lock().retain(|b| b != &c);
                                removed = true;
                                Ok(())
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    } else {
                        Err("BT 引擎不可用（feature 未启用）".into())
                    }
                }
            };
            out.push((ip_trim, res));
        }
        if removed {
            self.persist_bans();
        }
        out
    }

    /// IP 段批量导入（batch5 对标：qB/BitComet IP filter 文件语义）：解析
    /// DAT（"000.000.000.000 - 000.000.000.255 , 000 , name"）与 P2P
    /// （"range:1.2.3.0-1.2.3.255,desc"）两格式文本，逐区间下发引擎
    /// ban_ip_range，成功者以 "start-end" 复合串入列表并落盘。
    /// 锁纪律：全程持 `ban_ops` 串行锁（同 ban/unban）。
    /// 返回 (成功区间数, 逐行错误列表)。
    pub async fn import_ban_ranges(&self, text: &str) -> (usize, Vec<String>) {
        let _serial = self.ban_ops.lock().await; // 串行化
        let engine = self.engine_for(EngineKind::Bt).ok();
        let mut imported = 0usize;
        let mut errors: Vec<String> = Vec::new();
        let mut newly: Vec<String> = Vec::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((a, b)) = parse_ban_range_line(line) else {
                errors.push(format!(
                    "L{}: 无法解析区间（{}）",
                    lineno + 1,
                    truncate_len(line, 48)
                ));
                continue;
            };
            let Some(bt) = engine.as_ref().and_then(|e| e.bt()) else {
                errors.push("BT 引擎不可用（feature 未启用）".into());
                break;
            };
            match bt.ban_ip_range(&a, &b).await {
                Ok(()) => {
                    let entry = format!("{a}-{b}");
                    let mut bans = self.bt_bans.lock();
                    if !bans.contains(&entry) {
                        bans.push(entry.clone());
                        newly.push(entry);
                    }
                    imported += 1;
                }
                Err(e) => errors.push(format!("L{}: 引擎拒绝 {a}-{b}: {}", lineno + 1, e)),
            }
        }
        if !newly.is_empty() {
            self.persist_bans();
        }
        (imported, errors)
    }

    /// 当前显式封禁列表快照（GET /security/bans）。
    pub fn list_bans(&self) -> Vec<String> {
        self.bt_bans.lock().clone()
    }

    /// 封禁列表落盘（best-effort：失败仅 warn，运行时列表为准）。
    fn persist_bans(&self) {
        let Some(path) = self.bans_persist_path.clone() else {
            return;
        };
        let bans = self.bt_bans.lock().clone();
        if let Err(e) = super::persistence::write_bans_atomic(&path, &bans) {
            tracing::warn!("bans.json 落盘失败 {path:?}: {e}");
        }
    }
}
/// 封禁条目归一化（batch5-P2）：单 IP 解析为规范字符串；v4-mapped IPv6
/// （`::ffff:1.2.3.4`）优先转回 v4 形式——否则 libtorrent add_rule 落在
/// v6 族规则，拦不住实际以 v4 连入的 peer。区间条目（"a-b"）两端同样
/// 归一后再校验同族/有序（batch6-P2）。非法 → Err。
pub(crate) fn normalize_ban_entry(s: &str) -> Result<String, String> {
    let t = s.trim();
    if let Some((a, b)) = t.split_once('-') {
        // 区间条目（batch6-P2）：两端各自归一（v4-mapped IPv6 回 v4——否则
        // add_rule 落 v6 族拦不住实际以 v4 连入的 peer，与单 IP 路径同口径）
        // + 归一后同族校验
        let norm = |x: &str| -> Result<std::net::IpAddr, String> {
            let ip: std::net::IpAddr = x
                .trim()
                .parse()
                .map_err(|e| format!("区间端点非法 {x:?}: {e}"))?;
            Ok(match ip {
                std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                    Some(v4) => std::net::IpAddr::V4(v4),
                    None => std::net::IpAddr::V6(v6),
                },
                v4 => v4,
            })
        };
        let pa = norm(a)?;
        let pb = norm(b)?;
        if pa.is_ipv4() != pb.is_ipv4() {
            return Err(format!("区间 {t:?} 地址族不一致"));
        }
        return Ok(format!("{}-{}", pa, pb));
    }
    let ip: std::net::IpAddr = t.parse().map_err(|e| format!("{t:?}: {e}"))?;
    Ok(match ip {
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => v6.to_string(),
        },
        v4 => v4.to_string(),
    })
}

/// IP filter 文本行 → 区间 (start, end)（batch5 对标：qB/BitComet 导入）：
/// - DAT（PeerGuardian .dat）：`000.000.000.000 - 000.000.000.255 , 000 , name`
///   （按第一个 ` - ` 切两端，再截 `,` 后的元信息）；
/// - P2P（.p2p）：`range:1.2.3.0-1.2.3.255,description` 或
///   `1.2.3.0-1.2.3.255,description`。
///
/// 无法解析 → None。
pub(crate) fn parse_ban_range_line(line: &str) -> Option<(String, String)> {
    let l = line.trim();
    let l = l.strip_prefix("range:").unwrap_or(l);
    // DAT 逗号元信息截断（保留前两段中的 IP 区间部分）
    let body = l.split(',').next().unwrap_or(l);
    let body = body.trim();
    let (a, b) = body.split_once('-')?;
    let a = a.trim();
    let b = b.trim();
    if a.is_empty() || b.is_empty() {
        return None;
    }
    // batch6-P2：两端 v4-mapped IPv6 归一（与 normalize_ban_entry 同口径）
    let norm = |x: &str| -> Option<std::net::IpAddr> {
        match x.parse::<std::net::IpAddr>().ok()? {
            std::net::IpAddr::V6(v6) => Some(match v6.to_ipv4_mapped() {
                Some(v4) => std::net::IpAddr::V4(v4),
                None => std::net::IpAddr::V6(v6),
            }),
            v4 => Some(v4),
        }
    };
    let pa = norm(a)?;
    let pb = norm(b)?;
    if pa.is_ipv4() != pb.is_ipv4() || pb < pa {
        return None;
    }
    Some((pa.to_string(), pb.to_string()))
}

/// 事件/日志用行截断（batch5）：超长行截尾加省略号，防错误列表膨胀。
fn truncate_len(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        s.to_string()
    } else {
        let cut: String = s.chars().take(cap).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod ban_entry_tests {
    use super::*;

    #[test]
    fn normalize_single_ips() {
        assert_eq!(normalize_ban_entry(" 1.2.3.4 ").unwrap(), "1.2.3.4");
        assert_eq!(
            normalize_ban_entry("::ffff:1.2.3.4").unwrap(),
            "1.2.3.4",
            "v4-mapped IPv6 必须转回 v4（否则 ip_filter 落 v6 族拦不住 v4 peer）"
        );
        assert_eq!(normalize_ban_entry("2001:db8::1").unwrap(), "2001:db8::1");
        assert!(normalize_ban_entry("not-an-ip").is_err());
    }

    #[test]
    fn normalize_range_entries() {
        assert_eq!(
            normalize_ban_entry("1.2.3.0-1.2.3.255").unwrap(),
            "1.2.3.0-1.2.3.255"
        );
        assert!(normalize_ban_entry("1.2.3.0-::1").is_err(), "跨族拒绝");
        assert!(normalize_ban_entry("1.2.3.9-x").is_err());
    }

    #[test]
    fn parse_dat_and_p2p_lines() {
        // DAT（PeerGuardian）
        assert_eq!(
            parse_ban_range_line("000.000.000.000 - 000.000.000.255 , 000 , bogus"),
            None,
            "DAT 前导零不是合法 IP 字面量（std 解析拒绝）→ 跳过该行"
        );
        assert_eq!(
            parse_ban_range_line("1.2.3.0 - 1.2.3.255 , 000 , some range"),
            Some(("1.2.3.0".into(), "1.2.3.255".into()))
        );
        // P2P
        assert_eq!(
            parse_ban_range_line("range:5.6.7.0-5.6.7.255,bad isp"),
            Some(("5.6.7.0".into(), "5.6.7.255".into()))
        );
        assert_eq!(parse_ban_range_line("garbage line"), None);
        assert_eq!(parse_ban_range_line("2.2.2.2-1.1.1.1"), None, "逆序拒绝");
        // 注释与空行由调用方跳过；此处保证解析器对前缀 range: 的容忍
        assert_eq!(
            parse_ban_range_line("range:9.9.9.0-9.9.9.9"),
            Some(("9.9.9.0".into(), "9.9.9.9".into()))
        );
    }

    #[test]
    fn truncate_len_caps() {
        assert_eq!(truncate_len("abc", 5), "abc");
        assert_eq!(truncate_len("abcdef", 5), "abcde…");
    }
}
