//! `cid_store.dat` 假设解析器（附录 A #7 解封 · 2026-08-30）。
//!
//! ⚠️ **状态：HYPOTHESIS（格式知识 V11 = D 级，无真实样本校准）**
//!
//! 已知事实（xunlei_research_complete.md / sample_collection_guide.md）：
//! - 位置：`%APPDATA%\Thunder Network\`（或版本相关子目录）
//! - 内容：用户全部迅雷下载历史的 hash 缓存（强隐私，样本采集时被标记
//!   「强烈建议不提供」——这也是至今零样本的原因）
//! - 同族先例：`.xlbt.cfg` magic = `XDLCTX\x00\x00` + TLV（xlbt_cfg.rs，A 级规格）
//!   —— cid_store 大概率同为「自定义二进制 / TLV / JSON」三选一
//!
//! 本模块策略：**三形态自适应探测**，不锁死单一假设：
//! 1. JSON 形态（新版迅雷常见）：顶层 `{...}` / `[...]` → 提取
//!    `cid/gcid/hash/md5` 类字段 + `path/filepath/name` 类字段配对
//! 2. TLV 形态（XDLCTX 同族假设）：`XDLCTX` 开头 → 按标签-长度-值走查，
//!    抽取「二进制 hash 块（16/20B）+ 相邻路径字符串」对
//! 3. 裸二进制形态：启发式扫描——不可打印块（疑似 hash）与
//!    可打印路径串（ASCII/UTF-16LE）在 64B 窗口内的配对
//!
//! 输出 = `CidStoreReport`（结构统计）+ `Vec<CidStoreEntry>`（候选条目）。
//! **候选 ≠ 事实**：字段语义必须等真实样本到达后以 cidstore_scan.py 报告
//! 交叉校准，再升级为 A 级规格。解析全程零 panic（垃圾输入 → 空报告）。
//!
//! 隐私（sample_collection_guide 口径延续）：本模块只做**本地只读**解析，
//! 不上传、不落盘日志；条目内容仅留在调用方内存中。

/// 候选条目（语义待真实样本校准）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CidStoreEntry {
    /// 疑似 hash（hex；16B=CID 候选 / 20B=SHA1 候选 / 32B=GCID 候选）。
    pub hash_hex: String,
    /// hash 字节长度。
    pub hash_len: usize,
    /// 疑似本地路径。
    pub path: String,
    /// hash 块在文件中的偏移（供扫描器回溯定位）。
    pub offset: usize,
}

/// 结构探测报告。
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CidStoreReport {
    pub file_size: usize,
    /// 前 8 字节（hex，用于人工比对 magic）。
    pub magic8_hex: String,
    /// 是否 XDLCTX 同族（与 .xlbt.cfg 同 magic 前缀）。
    pub xdlctx_family: bool,
    /// 是否 JSON 形态。
    pub json_like: bool,
    /// ASCII 可打印比例（0.0-1.0）。
    pub printable_ratio: f64,
    /// 候选条目数。
    pub candidate_count: usize,
    /// 解析备注（形态判定依据、异常等）。
    pub notes: Vec<String>,
}

/// 分析入口：自动探测形态并抽取候选条目。
/// 返回（报告, 候选条目）。**候选 ≠ 事实**：字段语义必须等真实样本到达后
/// 以 cidstore_scan.py 报告交叉校准，再升级为 A 级规格。
pub fn analyze_cid_store(bytes: &[u8]) -> (CidStoreReport, Vec<CidStoreEntry>) {
    let mut report = CidStoreReport {
        file_size: bytes.len(),
        magic8_hex: hex::encode(&bytes[..bytes.len().min(8)]),
        printable_ratio: printable_ratio(bytes),
        ..Default::default()
    };
    report.xdlctx_family = bytes.starts_with(b"XDLCTX");
    report.json_like = bytes.iter().take(64).any(|&b| b == b'{') && bytes.len() > 2;

    let entries = if report.json_like {
        report
            .notes
            .push("形态判定: JSON-like（前 64B 内出现 '{'）".into());
        extract_json_entries(bytes, &mut report)
    } else if report.xdlctx_family {
        report
            .notes
            .push("形态判定: XDLCTX 同族 TLV（与 .xlbt.cfg 同 magic）".into());
        extract_binary_entries(bytes, true, &mut report)
    } else {
        report
            .notes
            .push("形态判定: 裸二进制启发式（无可识别 magic）".into());
        extract_binary_entries(bytes, false, &mut report)
    };
    report.candidate_count = entries.len();
    report.notes.push(format!(
        "候选条目: {}（候选≠事实，待样本校准）",
        entries.len()
    ));
    (report, entries)
}

fn printable_ratio(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let printable = bytes
        .iter()
        .filter(|&&b| (0x20..0x7f).contains(&b) || b == b'\n' || b == b'\r' || b == b'\t')
        .count();
    printable as f64 / bytes.len() as f64
}

/// JSON 形态：serde_json 解析 + 递归字段名配对（hash 字段 × path 字段）。
fn extract_json_entries(bytes: &[u8], report: &mut CidStoreReport) -> Vec<CidStoreEntry> {
    let mut out = Vec::new();
    let text = String::from_utf8_lossy(bytes);
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        report
            .notes
            .push("JSON 解析失败（截断/非 UTF-8）→ 回退裸二进制启发式".into());
        return extract_binary_entries(bytes, false, report);
    };
    const HASH_KEYS: [&str; 8] = [
        "cid", "gcid", "bcid", "hash", "md5", "sha1", "fileHash", "infohash",
    ];
    const PATH_KEYS: [&str; 6] = ["path", "filepath", "file_path", "name", "filename", "url"];
    walk_json(&value, &mut out, &HASH_KEYS, &PATH_KEYS, 0);
    out
}

/// JSON 遍历深度上限（P2-30，审查第三轮）：显式防护——此前隐式依赖
/// serde_json 解析器自身的 128 层上限；防御纵深：解析器上限变化时本层
/// 不跟随回归（递归 walk 栈溢出不可 catch_unwind）。
const JSON_WALK_MAX_DEPTH: usize = 64;

fn walk_json(
    value: &serde_json::Value,
    out: &mut Vec<CidStoreEntry>,
    hash_keys: &[&str],
    path_keys: &[&str],
    depth: usize,
) {
    if depth > JSON_WALK_MAX_DEPTH {
        return; // 超深子树静默剪枝（与解析器 128 层上限内的行为一致：
                // 提取器对不可理解的结构本就跳过）
    }
    match value {
        serde_json::Value::Object(map) => {
            let mut hash: Option<(String, usize)> = None;
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    let kl = k.to_ascii_lowercase();
                    if hash_keys.contains(&kl.as_str()) && looks_like_hash(s) {
                        hash = Some((s.trim().to_ascii_lowercase(), s.trim().len()));
                    }
                }
            }
            for (k, v) in map {
                if let Some(s) = v.as_str() {
                    let kl = k.to_ascii_lowercase();
                    if path_keys.contains(&kl.as_str()) && s.len() >= 4 && hash.is_some() {
                        let (h, hl) = hash.clone().unwrap();
                        out.push(CidStoreEntry {
                            hash_hex: h,
                            hash_len: hl,
                            path: s.to_string(),
                            offset: 0, // JSON 无文件偏移语义
                        });
                    }
                }
            }
            for v in map.values() {
                walk_json(v, out, hash_keys, path_keys, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                walk_json(v, out, hash_keys, path_keys, depth + 1);
            }
        }
        _ => {}
    }
}

fn looks_like_hash(s: &str) -> bool {
    let s = s.trim();
    (s.len() == 32 || s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// 路径近邻查找（P0-14 二分化，PR #114 催修校准）：
/// paths 需按 off 升序。两函数与 main 原线性实现【逐点等价】
///（等价性回归测试见 tests::neighbor_lookup_matches_linear_reference）：
/// - `find_after` ≡ `find(|&(off,_)| off > i && off <= i + 64 + hlen)`
/// - `find_before` ≡ `rev().find(|&(off,_)| off < i && i <= off + 64)`
///
/// 等价性依据（单调性）：
/// - after：`off <= i + 64 + hlen` 随 off 单调递减 → 首个 `off > i` 的条目
///   不满足窗口时，其后条目同样不满足；
/// - before：`i <= off + 64`（即 `off >= i - 64`）随 off 单调递增 → 最后一个
///   `off < i` 的条目不满足窗口时，更早的条目同样不满足。
///
/// 催修复现的缺陷（已修）：before 若复用 `partition_point(off <= i)` 的前驱，
/// 在存在 `off == i` 条目时会把【窗口自身】当相邻路径（hash↔path 配对错乱）。
/// 必须用独立的 `partition_point(off < i)`。
fn find_after(paths: &[(usize, String)], i: usize, hlen: usize) -> Option<(usize, &str)> {
    let gt_i = paths.partition_point(|&(off, _)| off <= i);
    paths
        .get(gt_i)
        .filter(|&&(off, _)| off <= i + 64 + hlen)
        .map(|&(off, ref s)| (off, s.as_str()))
}

fn find_before(paths: &[(usize, String)], i: usize) -> Option<(usize, &str)> {
    let lt_i = paths.partition_point(|&(off, _)| off < i);
    if lt_i > 0 {
        Some(&paths[lt_i - 1])
            .filter(|&&(off, _)| i <= off + 64)
            .map(|&(off, ref s)| (off, s.as_str()))
    } else {
        None
    }
}

/// 二进制形态（TLV 或裸启发式）：hash 块 × 相邻路径串 配对。
///
/// `tlv_mode=true`（XDLCTX 同族）时要求 hash 块前有可读 tag 字节；
/// 裸模式放宽为纯启发式。两种模式的共同不变量：
/// **hash 候选必须不可打印且长度 ∈ {16,20,32}；路径候选必须可打印 ≥6 字节**。
fn extract_binary_entries(
    bytes: &[u8],
    tlv_mode: bool,
    report: &mut CidStoreReport,
) -> Vec<CidStoreEntry> {
    let mut out = Vec::new();
    if bytes.len() < 24 {
        report.notes.push("文件过小（<24B），无候选".into());
        return out;
    }
    let ascii_paths = printable_strings(bytes);
    let utf16_paths = utf16le_strings(bytes);
    let mut paths: Vec<(usize, String)> = ascii_paths
        .iter()
        .chain(utf16_paths.iter())
        .cloned()
        .collect();
    paths.sort_by_key(|&(off, _)| off);

    let hash_lens = [16usize, 20, 32];
    // 候选收集：hash 窗口（不可打印+随机）× 最近路径，按 gap（路径起点到窗口终点的距离）升序贪心接受，
    // 已接受窗口重叠的候选跳过 —— 避免把 tag 字节并入 hash（对齐歧义）。
    //
    // P0-14 修复（二次方爆炸，已实证 384KB 输入 6 秒 + 95 万候选）：
    // ① 路径查找改二分（paths 已按 offset 排序，partition_point）——
    //    原实现每个候选 O(#paths) 线性扫 ×2（after/before），输入每翻倍
    //    时间×3-4（真实 cid_store.dat 是 hash+路径交错形态，用户导入即命中）；
    // ② 候选总量上限 CAP（10_000）——原实现无上限 push，384KB 产生 95 万
    //    候选每个带 path.clone()，内存放大数百倍；
    // ③ 重叠判定改 BTreeMap 区间查询——原 taken_windows 线性扫每候选
    //    O(#taken)。
    const MAX_CANDS: usize = 10_000;
    #[derive(Clone)]
    struct Cand {
        off: usize,
        hlen: usize,
        path: String,
        gap: i64,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for &hlen in &hash_lens {
        let mut i = 0usize;
        while i + hlen <= bytes.len() {
            let win = &bytes[i..i + hlen];
            if !is_printable(win) && looks_random(win) {
                // ① 二分：首个 off > i 的路径（paths 已按 off 升序）
                let after = find_after(&paths, i, hlen);
                // ① 二分：最后一个严格 off < i 的路径（独立 partition_point，
                //    见 find_before 文档注释——催修复现的 off == i 自匹配缺陷）
                let before = find_before(&paths, i);
                let pick = after
                    .map(|(po, s)| (po, s, (po as i64 - (i + hlen) as i64).abs()))
                    .or_else(|| {
                        before.map(|(po, s)| (po, s, (i as i64 - (po + s.len()) as i64).abs()))
                    });
                if let Some((_po, s, gap)) = pick {
                    let tag_ok = !tlv_mode
                        || i >= 2 && bytes[i - 2..i].iter().any(|&b| (0x01..0x40).contains(&b));
                    if tag_ok {
                        cands.push(Cand {
                            off: i,
                            hlen,
                            path: s.to_string(),
                            gap,
                        });
                        // ② 候选上限：防极端输入（随机块+路径交替的海量窗口）
                        //    撑爆内存——截断后仅损失 best-effort 精度
                        if cands.len() >= MAX_CANDS {
                            break;
                        }
                    }
                }
            }
            i += 1;
        }
        if cands.len() >= MAX_CANDS {
            break;
        }
    }
    cands.sort_by_key(|c| (c.gap, c.off));
    // ③ 重叠判定：BTreeMap 区间（start → end），每候选 O(log n)
    let mut taken_windows: std::collections::BTreeMap<usize, usize> =
        std::collections::BTreeMap::new();
    for c in cands {
        let (s, e) = (c.off, c.off + c.hlen);
        // 前驱区间 end > s 或后继区间 start < e → 重叠
        let overlapped = taken_windows
            .range(..=s)
            .next_back()
            .is_some_and(|(_, &pe)| pe > s)
            || taken_windows
                .range(s..)
                .next()
                .is_some_and(|(&ns, _)| ns < e);
        if overlapped {
            continue; // 与已接受窗口重叠 → 跳过（保最小 gap 的对齐候选）
        }
        taken_windows.insert(s, e);
        out.push(CidStoreEntry {
            hash_hex: hex::encode(&bytes[c.off..c.off + c.hlen]),
            hash_len: c.hlen,
            path: c.path,
            offset: c.off,
        });
    }
    // 去重（同 hash+path 保留首见）
    out.dedup_by(|a, b| a.hash_hex == b.hash_hex && a.path == b.path);
    out
}

fn is_printable(bytes: &[u8]) -> bool {
    bytes.iter().all(|&b| (0x20..0x7f).contains(&b))
}

fn looks_random(bytes: &[u8]) -> bool {
    // 粗略熵代理：出现 ≥6 个不同字节值即视为随机块（hash 特征）
    let mut seen = [false; 256];
    let mut distinct = 0usize;
    for &b in bytes {
        if !seen[b as usize] {
            seen[b as usize] = true;
            distinct += 1;
        }
    }
    distinct >= 6
}

/// ASCII 可打印串提取（≥6 字节，含路径特征字符）。
fn printable_strings(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, &b) in bytes.iter().enumerate() {
        if (0x20..0x7f).contains(&b) {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            push_path(&bytes[s..i], s, &mut out);
        }
    }
    if let Some(s) = start.take() {
        push_path(&bytes[s..], s, &mut out);
    }
    out
}

/// UTF-16LE 串提取（ASCII 段 + 中文 CJK 段，≥6 字符）。
fn utf16le_strings(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    if bytes.len() < 12 {
        return out;
    }
    let mut start = None;
    let mut buf: Vec<u16> = Vec::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let u = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let is_char = (0x20..0x7f).contains(&u) || (0x4e00..0xa000).contains(&u);
        if is_char {
            if start.is_none() {
                start = Some(i);
            }
            buf.push(u);
            i += 2;
        } else {
            if let Some(off) = start {
                if buf.len() >= 6 {
                    let s: String = String::from_utf16_lossy(&buf);
                    if s.contains('/') || s.contains('\\') || s.contains('.') {
                        out.push((off, s));
                    }
                }
            }
            start = None;
            buf.clear();
            i += 2;
        }
    }
    if let Some(off) = start {
        if buf.len() >= 6 {
            let s = String::from_utf16_lossy(&buf);
            if s.contains('/') || s.contains('\\') || s.contains('.') {
                out.push((off, s));
            }
        }
    }
    out
}

fn push_path(raw: &[u8], off: usize, out: &mut Vec<(usize, String)>) {
    if raw.len() < 6 {
        return;
    }
    let s = String::from_utf8_lossy(raw).into_owned();
    if s.contains('/') || s.contains('\\') || s.contains('.') {
        out.push((off, s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P2-30（审查第三轮）：超深 JSON 子树剪枝——嵌套 >64 层的 hash
    /// 不再被提取（显式深度上限，不再隐式依赖 serde_json 解析器的
    /// 128 层递归上限）。
    #[test]
    fn deep_json_subtree_pruned() {
        // 构造 70 层嵌套，最深处放一个合法 hash
        let mut body = String::from("{\"k\":");
        for _ in 0..70 {
            body.push_str("{\"deep\":");
        }
        body.push_str("{\"cid\":\"D41D8CD98F00B204E9800998ECF8427E\"}");
        for _ in 0..70 {
            body.push('}');
        }
        body.push('}');
        let (_, entries) = analyze_cid_store(body.as_bytes());
        assert!(
            entries.is_empty(),
            "超深子树应被剪枝，实际提取到 {} 条",
            entries.len()
        );
        // 对照：同样的 hash 在浅层正常提取
        let shallow = br#"{"cid":"D41D8CD98F00B204E9800998ECF8427E","path":"a.bin"}"#;
        let (_, e2) = analyze_cid_store(shallow);
        assert_eq!(e2.len(), 1);
    }

    #[test]
    fn json_form_extracts_pairs() {
        let body = br#"{"version":1,"items":[
            {"cid":"D41D8CD98F00B204E9800998ECF8427E","path":"D:/downloads/a.rar","size":123},
            {"cid":"0011223344556677889900aabbccddee","path":"C:/b.bin","size":1}
        ]}"#;
        let (report, entries) = analyze_cid_store(body);
        assert!(report.json_like);
        assert_eq!(entries.len(), 2, "notes={:?}", report.notes);
        assert_eq!(entries[0].path, "D:/downloads/a.rar");
        assert_eq!(entries[0].hash_hex, "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(entries[0].hash_len, 32);
    }

    #[test]
    fn binary_form_pairs_hash_and_path() {
        let mut v = b"XDLCTX\x00\x00".to_vec();
        v.extend_from_slice(&[0x02, 0x00]); // tag
        let hash: [u8; 16] = core::array::from_fn(|i| (i * 17 + 3) as u8);
        v.extend_from_slice(&hash);
        v.extend_from_slice(b"D:/dl/video.mkv");
        v.extend_from_slice(&[0x00, 0x00]);
        let (report, entries) = analyze_cid_store(&v);
        assert!(report.xdlctx_family);
        assert!(!entries.is_empty(), "notes={:?}", report.notes);
        assert_eq!(entries[0].path, "D:/dl/video.mkv");
        assert_eq!(entries[0].hash_len, 16);
        assert_eq!(entries[0].hash_hex, hex::encode(hash));
        assert!(entries[0].offset > 0);
    }

    #[test]
    fn garbage_yields_empty_without_panic() {
        let zeros = vec![0u8; 4096];
        let (report, entries) = analyze_cid_store(&zeros);
        assert_eq!(entries.len(), 0);
        assert_eq!(report.candidate_count, 0);
        assert!(!report.json_like);
    }

    #[test]
    fn tiny_file_no_panic() {
        assert_eq!(analyze_cid_store(&[1, 2, 3]).1.len(), 0);
        assert_eq!(analyze_cid_store(&[]).0.file_size, 0);
    }

    /// P0-14 回归（整合批）：真实 cid_store.dat 形态（随机 hash 块 + 路径串
    /// 交替）必须线性时间完成——原实现三处线性扫描叠加呈二次方爆炸，
    /// 384KB 输入实测 6s（repro/p0repro round5 实证：输入每翻倍耗时 ×3-4）。
    /// 512KB 输入修复后应为亚秒级；2s 上限给 debug 构建留 20 倍余量。
    #[test]
    fn hash_path_alternating_linear_time() {
        let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
        let mut rng: u64 = 0x1234_5678_9abc_def0;
        let mut next = || -> u64 {
            // xorshift64（测试确定性，无需密码学质量）
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        while buf.len() < 512 * 1024 {
            // 32 字节随机块（hash 形态：不可打印 + 高熵）
            let h = next().to_le_bytes();
            let h2 = next().to_le_bytes();
            buf.extend_from_slice(&h);
            buf.extend_from_slice(&h2);
            buf.extend_from_slice(&h);
            buf.extend_from_slice(&h2);
            // 路径串（可打印 + 路径特征字符）
            buf.extend_from_slice(
                format!("/home/user/dl/file_{}.rar\n", next() % 99999).as_bytes(),
            );
        }
        let t0 = std::time::Instant::now();
        let (_report, entries) = analyze_cid_store(&buf);
        let el = t0.elapsed();
        assert!(
            el < std::time::Duration::from_secs(2),
            "512KB hash+路径交替输入必须在 2s 内完成（P0-14 二次方回归），实际 {el:?}"
        );
        assert!(!entries.is_empty(), "hash+路径交替形态应产出候选条目");
    }

    /// P0-14 回归（整合批）：纯可打印大输入（无随机块）快速完成——对照组，
    /// 锁定「复杂度由随机块×路径邻近配对驱动，而非输入大小本身」。
    #[test]
    fn printable_only_large_input_fast() {
        let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
        let mut i = 0u64;
        while buf.len() < 512 * 1024 {
            buf.extend_from_slice(format!("path segment {} /dir/file.txt ", i).as_bytes());
            i += 1;
        }
        let t0 = std::time::Instant::now();
        let (_r, entries) = analyze_cid_store(&buf);
        let el = t0.elapsed();
        assert!(
            el < std::time::Duration::from_secs(2),
            "纯可打印输入应快速: {el:?}"
        );
        assert_eq!(entries.len(), 0, "无随机块 → 无候选");
    }

    /// PR #114 催修 P0-1 回归：单条路径 off == 窗口起点 i 时，
    /// before 必须是 None——不能把【窗口自身】当前驱。
    /// 原缺陷写法（复用 partition_point(off <= i) 的前驱）会返回 Some((51, "p0"))。
    #[test]
    fn before_neighbor_requires_strictly_less_offset() {
        let paths = vec![(51usize, "p0".to_string())];
        // off == i：不是严格小于 → None
        assert_eq!(find_before(&paths, 51), None);
        // i 在窗口内前方（i <= off + 64 且 i > off）→ 命中
        assert_eq!(find_before(&paths, 52), Some((51, "p0")));
        assert_eq!(find_before(&paths, 115), Some((51, "p0")));
        // i 超出窗口（i > off + 64）→ None
        assert_eq!(find_before(&paths, 116), None);
        // i 在 off 之前 → after 侧负责，before 为 None
        assert_eq!(find_before(&paths, 50), None);
    }

    /// PR #114 催修 P0-1 回归：二分近邻查找与 main 原线性实现【逐点等价】
    /// （随机属性测试——审查者 repro round10.rs 同口径：多 seed × 多密度，
    /// 覆盖 off == i / off 重叠窗口 / 窗口边界 i-64 / i+64+hlen 等分支）。
    /// 原缺陷写法在 off == i 存在时把窗口自身当前驱（实测 243/8889 不一致）。
    #[test]
    fn neighbor_lookup_matches_linear_reference() {
        // main 原线性实现（等价性基准，逐字复刻）
        let lin_after = |paths: &[(usize, String)], i: usize, hlen: usize| {
            paths
                .iter()
                .find(|&&(off, _)| off > i && off <= i + 64 + hlen)
                .map(|&(off, ref s)| (off, s.clone()))
        };
        let lin_before = |paths: &[(usize, String)], i: usize| {
            paths
                .iter()
                .rev()
                .find(|&&(off, _)| off < i && i <= off + 64)
                .map(|&(off, ref s)| (off, s.clone()))
        };
        let bin_after =
            |paths: &[(usize, String)], i: usize, hlen: usize| -> Option<(usize, String)> {
                find_after(paths, i, hlen).map(|(off, s)| (off, s.to_string()))
            };
        let bin_before = |paths: &[(usize, String)], i: usize| -> Option<(usize, String)> {
            find_before(paths, i).map(|(off, s)| (off, s.to_string()))
        };

        for seed in [0xdead_beefu64, 0x1234_5678u64, 0xcafe_babeu64, 42, 1] {
            let mut rng = seed;
            let mut next = || -> u64 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            for _case in 0..40 {
                // 路径集：off 落在窄域（高密度触发 off == i 与重叠窗口）
                let n = 1 + (next() % 24) as usize;
                let mut paths: Vec<(usize, String)> = (0..n)
                    .map(|k| ((next() % 200) as usize, format!("p{k}_{}", next() % 1000)))
                    .collect();
                paths.sort_by_key(|&(off, _)| off);
                // 检查点：密集扫 [0, 300) 全量 + hash 三种长度
                for i in 0..300usize {
                    for &hlen in &[16usize, 20, 32] {
                        assert_eq!(
                            bin_after(&paths, i, hlen),
                            lin_after(&paths, i, hlen),
                            "after 不等价: seed={seed:#x} i={i} hlen={hlen} paths={paths:?}"
                        );
                        assert_eq!(
                            bin_before(&paths, i),
                            lin_before(&paths, i),
                            "before 不等价: seed={seed:#x} i={i} paths={paths:?}"
                        );
                    }
                }
            }
        }
    }
}
