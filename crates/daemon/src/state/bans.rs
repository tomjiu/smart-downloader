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

// ==================== B7（#214 §三-7）：IP 过滤 URL 订阅 ====================

/// 订阅文本解析（B7 纯函数）：DAT/P2P 区间行（与手动导入同解析器）+
/// 单 IP 行；注释/空行跳过；无法解析的行记入错误（部分行损坏不阻断
/// 整体——qB 对标）。条目上限 100 万（防病态内存）。
pub(crate) fn parse_ip_filter_text(
    text: &str,
) -> (std::collections::BTreeSet<String>, Vec<String>) {
    let mut entries = std::collections::BTreeSet::new();
    let mut errors = Vec::new();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if entries.len() >= 1_000_000 {
            errors.push(format!("L{}: 条目超限（>100 万），余下行跳过", lineno + 1));
            break;
        }
        if let Some((a, b)) = parse_ban_range_line(line) {
            entries.insert(format!("{a}-{b}"));
        } else if let Ok(c) = normalize_ban_entry(line) {
            entries.insert(c);
        } else if errors.len() < 50 {
            // 错误列表封顶 50 条（防错误源把响应/日志撑爆；计数语义存疑时
            // 用户应先看 last_error 的整体结论）
            errors.push(format!("L{}: {}", lineno + 1, truncate_len(line, 48)));
        }
    }
    (entries, errors)
}

impl DaemonState {
    /// 订阅刷新（B7，#214 §三-7）：拉取 → 解析 → 与上次条目集 diff →
    /// 引擎增量下发（新增 ban / 消失 unban）→ bt_bans 同步 → 双文件落盘。
    ///
    /// 语义要点：
    /// - **分账**：手动封禁不在订阅集内，永不受刷新影响（增删只碰 diff 命中者）
    /// - **失败保持**（#160 防假阳性）：拉取/解析失败 → last_error 记账，
    ///   条目与引擎不动（保持上次生效集，绝不因瞬时故障清空防线）
    /// - **部分生效收敛**：单条引擎失败 → 该条不入 entries（下次刷新
    ///   diff 会重试），last_error 汇总部分失败
    /// - **BT-only**：引擎不可用 → Err（无 BT 无意义，不静默入表制造
    ///   「看似生效」的假阳性；条目不动）
    /// - 成功 = 至少拉取+解析完成（部分引擎失败也算「有进度」：
    ///   last_ok 更新 + last_error 记部分失败摘要）
    pub async fn refresh_ip_filter(&self, url: &str) -> Result<usize, String> {
        let _serial = self.ban_ops.lock().await; // 与手动 ban/unban/import 互斥
        let now = super::now_unix();
        // 失败记账（所有失败路径统一：last_error + last_attempt——退避基准
        // 与状态面诚实回显都依赖它；条目/引擎不动 = 保持上次生效集）
        let fail = |msg: String| -> Result<usize, String> {
            self.ip_filter.lock().last_error = Some(msg.clone());
            self.ip_filter.lock().last_attempt_unix = now;
            Err(msg)
        };

        // ---- 1. 拉取（30s 总超时 + 64MB 上限；webhook_client 复用连接池）----
        let resp = match self
            .webhook_client
            .get(url)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return fail(format!("订阅源请求失败: {e}")),
        };
        if !resp.status().is_success() {
            return fail(format!("订阅源返回 {}", resp.status()));
        }
        if let Some(len) = resp.content_length() {
            if len > 64 * 1024 * 1024 {
                return fail(format!("订阅源过大（{len} 字节 > 64MB 上限）"));
            }
        }
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return fail(format!("订阅源读取失败: {e}")),
        };
        if body.len() > 64 * 1024 * 1024 {
            return fail(format!("订阅源过大（{} 字节 > 64MB 上限）", body.len()));
        }

        // ---- 2. 解析（部分行损坏不阻断；错误摘要入 last_error）----
        let (new_entries, parse_errors) = parse_ip_filter_text(&body);
        // 安全护栏：200 但零条目且全为无法解析的行 → 大概率指错了源
        //（HTML 错误页等）——拒绝应用（防「空集清空全部订阅条目」的
        // 误伤路径；真正的空列表文件 = 零条目零错误，仍合法清空）
        if new_entries.is_empty() && !parse_errors.is_empty() {
            return fail(format!(
                "订阅源内容无法解析（{} 行全失败，疑似格式错误或 HTML 页），保持上次生效集",
                parse_errors.len()
            ));
        }

        // ---- 3. 引擎在线门槛（BT-only 功能）----
        let engine = match self.engine_for(EngineKind::Bt) {
            Ok(e) => e,
            Err(e) => {
                return fail(format!("BT 引擎不可用（{e}）：订阅刷新需要 BT 引擎在线"));
            }
        };
        let Some(bt) = engine.bt() else {
            return fail("BT 引擎无扩展面：订阅刷新需要 BT 引擎在线".to_string());
        };

        // ---- 4. diff + 引擎增量下发 ----
        let old_entries = self.ip_filter.lock().entries.clone();
        let to_add: Vec<_> = new_entries.difference(&old_entries).cloned().collect();
        let to_remove: Vec<_> = old_entries.difference(&new_entries).cloned().collect();
        let mut engine_errors: Vec<String> = Vec::new();
        // 实际生效集 = 保留交集 ∪ 成功新增 ∪ 解禁失败者（仍生效）
        let mut applied: std::collections::BTreeSet<String> =
            new_entries.intersection(&old_entries).cloned().collect();
        for entry in &to_add {
            let res = match entry.split_once('-') {
                Some((a, b)) => bt.ban_ip_range(a, b).await,
                None => bt.ban_ip(entry).await,
            };
            match res {
                Ok(()) => {
                    applied.insert(entry.clone());
                    let mut bans = self.bt_bans.lock();
                    if !bans.contains(entry) {
                        bans.push(entry.clone());
                    }
                }
                Err(e) => engine_errors.push(format!("{entry}: {e}")),
            }
        }
        for entry in &to_remove {
            let res = bt.unban_ip(entry).await; // ffi 层原生支持区间串
            match res {
                Ok(()) => {
                    self.bt_bans.lock().retain(|b| b != entry);
                }
                Err(e) => {
                    // 解禁失败：条目仍生效 → 留在 applied（下次刷新再试摘除）
                    applied.insert(entry.clone());
                    engine_errors.push(format!("解禁 {entry}: {e}"));
                }
            }
        }

        // ---- 5. 记账 + 落盘（bans.json = 全量生效集；ip_filter.json =
        //      订阅归属基准——重启后保有 diff 基线）----
        let count = applied.len();
        let mut partial = String::new();
        if !parse_errors.is_empty() {
            partial = format!("（{} 行无法解析）", parse_errors.len());
        }
        if !engine_errors.is_empty() {
            if !partial.is_empty() {
                partial.push_str(" + ");
            }
            partial.push_str(&format!("（{} 条引擎失败）", engine_errors.len()));
        }
        {
            let mut st = self.ip_filter.lock();
            st.entries = applied;
            st.source_url = url.to_string();
            st.last_ok_unix = now;
            st.last_attempt_unix = now;
            st.last_error = (!partial.is_empty()).then_some(partial.clone());
        }
        self.persist_bans();
        self.persist_ip_filter_state();
        tracing::info!(
            "IP 过滤订阅刷新: +{} / -{} / 生效 {} 条{partial}",
            to_add.len(),
            to_remove.len(),
            count
        );
        Ok(count)
    }

    /// 禁用订阅（B7）：url 改空时清除全部订阅条目（手动封禁保留）。
    /// 引擎在线 → 逐条解禁；不在线 → 仅列表/落盘摘除（引擎侧规则随
    /// 重启消失——本会话从未生效过，无残留）。幂等：entries 空 = 直过。
    pub async fn clear_ip_filter_subscription(&self) {
        let _serial = self.ban_ops.lock().await;
        let old: Vec<String> = {
            let mut st = self.ip_filter.lock();
            std::mem::take(&mut st.entries).into_iter().collect()
        };
        if old.is_empty() {
            return;
        }
        let engine = self.engine_for(EngineKind::Bt).ok();
        let bt = engine.as_ref().and_then(|e| e.bt());
        let mut unbanned = 0usize;
        for entry in &old {
            if let Some(bt) = bt {
                if bt.unban_ip(entry).await.is_ok() {
                    unbanned += 1;
                }
            }
            self.bt_bans.lock().retain(|b| b != entry);
        }
        {
            let mut st = self.ip_filter.lock();
            st.entries.clear();
            st.source_url.clear();
            st.last_ok_unix = 0;
            st.last_error = None;
        }
        self.persist_bans();
        self.persist_ip_filter_state();
        tracing::info!(
            "IP 过滤订阅已禁用：解除 {} 条（引擎侧 {} 条）",
            old.len(),
            unbanned
        );
    }

    /// ip_filter.json 落盘（best-effort：失败仅 warn）。
    pub(crate) fn persist_ip_filter_state(&self) {
        let Some(path) = self.ip_filter_persist_path.clone() else {
            return;
        };
        let st = self.ip_filter.lock().clone();
        if let Err(e) = super::persistence::write_ip_filter_state_atomic(&path, &st) {
            tracing::warn!("ip_filter.json 落盘失败 {path:?}: {e}");
        }
    }

    /// 订阅配置现值（B7 ticker 用）：live_config 现读（热改跟随）。
    pub(crate) fn ip_filter_cfg_now(&self) -> (String, u64) {
        let cfg = self.live_config.lock();
        match cfg.as_ref() {
            Some(c) => (c.ip_filter.url.clone(), c.ip_filter.refresh_interval_secs),
            None => (String::new(), 0),
        }
    }
}

#[cfg(test)]
mod ip_filter_parse_tests {
    use super::*;

    /// B7：混合格式解析（DAT/P2P/单 IP/v4-mapped 去重/注释跳过/坏行记账）。
    #[test]
    fn parse_ip_filter_text_mixed_formats() {
        let text = "# level1 header\n\
                    1.2.3.4\n\
                    range:5.6.7.0-5.6.7.255,bad isp\n\
                    9.9.9.0 - 9.9.9.255 , 000 , some range\n\
                    ::ffff:1.2.3.4\n\
                    garbage\n\
                    \n";
        let (entries, errors) = parse_ip_filter_text(text);
        assert_eq!(entries.len(), 3, "v4-mapped 与字面 v4 去重: {entries:?}");
        assert!(entries.contains("1.2.3.4"));
        assert!(entries.contains("5.6.7.0-5.6.7.255"));
        assert!(entries.contains("9.9.9.0-9.9.9.255"));
        assert_eq!(errors.len(), 1, "仅 garbage 行报错: {errors:?}");
    }

    /// 空内容 = 空集（合法——订阅源显式清空 = 全部解除，由调用方应用）。
    #[test]
    fn parse_ip_filter_text_empty() {
        let (entries, errors) = parse_ip_filter_text("");
        assert!(entries.is_empty() && errors.is_empty());
        let (entries, errors) = parse_ip_filter_text("# only comment\n\n");
        assert!(entries.is_empty() && errors.is_empty());
    }
}
