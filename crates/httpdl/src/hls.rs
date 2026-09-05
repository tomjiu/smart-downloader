//! HLS（HTTP Live Streaming，RFC 8216）下载支持（C-HLS）。
//!
//! v1 范围（VOD 最小可用面）：
//! - **识别**：`HttpEngine::add` 对 URL 路径以 `.m3u8` 结尾（剥 query/fragment，
//!   大小写无关）的任务分流至本模块；其余任务照常走探测/分段链。
//! - **Master playlist**：`#EXT-X-STREAM-INF` 变体 → 选 BANDWIDTH 最高者递归
//!   解析一层（VOD）。无变体直接视为 media playlist。
//! - **Media playlist**：`#EXTINF` + URI 行；`#EXT-X-KEY`（METHOD=AES-128 →
//!   key URI 拉取一次缓存 + IV 显式/缺省 = media sequence 大端 16B）；
//!   `#EXT-X-MEDIA-SEQUENCE` / `#EXT-X-ENDLIST` 识别；**仅接受 VOD**
//!   （无 ENDLIST = live → 拒绝：清单内容随时间变化，续传语义不成立）。
//!   `#EXT-X-BYTERANGE` / `#EXT-X-MAP` / SAMPLE-AES → v1 明确不支持。
//! - **下载**：段**顺序**下载（TS 拼接对顺序敏感；段间无并行收益优先正确性）
//!   逐段 append 到 `.part`，段账本（`.part.hls-ledger` JSON）记录已完成段
//!   索引与字节数；恢复 = 重拉 m3u8 → 对账（清单指纹/段数）→ 从断点段续传。
//! - **进度**：段长事先未知（v1 不预 HEAD）→ total = 0（未知，BT metadata
///  前同语义），done 按字节累计。
//
// 边界（v1 明确不做）：并发段下载、EXT-X-DISCONTINUITY 语义校验、
// EXT-X-MAP（fMP4 init 段）、fMP4 段合流校验（裸拼接交付，由播放器解码）。
use crate::rate::RateLimiter;
use smart_dl_core::types::EngineError;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// abort 约定错误串：pause()/remove() 置位 abort flag → 段间检查点返回此
/// 错误，spawn_hls_loop 识别后静默返回（任务状态由 pause()/remove() 管理）。
pub const HLS_ABORTED_MSG: &str = "hls-aborted";
use std::io::Write as _;

/// 单段解密参数（RFC 8216 §4.3.2.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegCrypto {
    None,
    /// METHOD=AES-128：key URL + 16B IV。
    Aes128 {
        key_url: String,
        iv: [u8; 16],
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HlsSegment {
    pub url: String,
    pub crypto: SegCrypto,
    /// EXTINF 声明时长（秒，展示用；不参与下载）。
    pub duration_secs: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaPlaylist {
    pub segments: Vec<HlsSegment>,
    /// EXT-X-MEDIA-SEQUENCE（缺省 0）：IV 缺省推导基值。
    pub media_sequence: u64,
}

/// `.m3u8` 后缀识别（剥 query/fragment，大小写无关；B1 metalink 同口径）。
pub fn is_hls_url(url: &str) -> bool {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_lowercase();
    lower.ends_with(".m3u8")
}

/// 解析相对 URL（段/key URI 对清单 URL 的解析：绝对 http(s) 原样；`//host/..`
/// 补协议；相对路径拼目录。清单 URL 自身必为 http(s)——调用方保证）。
pub fn resolve_url(base_url: &str, target: &str) -> String {
    if target.starts_with("http://") || target.starts_with("https://") {
        return target.to_string();
    }
    if let Some(rest) = target.strip_prefix("//") {
        let scheme = base_url.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    // 相对路径：剥 query/frag 后取 base 目录（最后 `/` 之前）
    let base_path = base_url.split(['?', '#']).next().unwrap_or(base_url);
    let dir = match base_path.rfind('/') {
        Some(i) => &base_path[..=i],
        None => "/",
    };
    if target.starts_with('/') {
        // scheme://host（不含 path）——`: `split_once('/')` 会截到 "https:"，
        // 需按 "://" 定位再取 host 段
        let (scheme_host, host) = match base_path.find("://") {
            Some(i) => {
                let rest = &base_path[i + 3..];
                let scheme = &base_path[..i + 3];
                (scheme, rest.split('/').next().unwrap_or(""))
            }
            None => return base_path.to_string(),
        };
        return format!("{scheme_host}{host}{target}");
    }
    format!("{dir}{target}")
}

/// 解析 media playlist 文本（RFC 8216 §4.3.3 子集）。
pub fn parse_media_playlist(base_url: &str, text: &str) -> Result<MediaPlaylist, String> {
    let mut segments = Vec::new();
    let mut media_sequence: u64 = 0;
    let mut current_key: Option<SegCrypto> = None;
    let mut pending_duration: Option<f64> = None;
    let mut seen_endlist = false;
    let mut seen_stream_inf = false;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("#EXT-X-STREAM-INF") {
            // master playlist 顶层：本函数不展开变体（上游先选流再进来）；
            // 出现即说明传入了 master → 由调用方处理，这里仅标记供报错。
            seen_stream_inf = true;
            continue;
        }
        if let Some(v) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = v
                .trim()
                .parse()
                .map_err(|_| format!("EXT-X-MEDIA-SEQUENCE 非法: {v:?}"))?;
            continue;
        }
        if line.starts_with("#EXT-X-ENDLIST") {
            seen_endlist = true;
            continue;
        }
        if let Some(v) = line.strip_prefix("#EXT-X-KEY:") {
            current_key = parse_key_attr(v, base_url)?;
            continue;
        }
        if line.starts_with("#EXT-X-BYTERANGE") {
            return Err("HLS v1 不支持 EXT-X-BYTERANGE 子范围段".into());
        }
        if line.starts_with("#EXT-X-MAP") {
            return Err("HLS v1 不支持 EXT-X-MAP（fMP4 init 段）".into());
        }
        if let Some(v) = line.strip_prefix("#EXTINF:") {
            let dur = v
                .split(',')
                .next()
                .unwrap_or("")
                .trim()
                .parse::<f64>()
                .map_err(|_| format!("EXTINF 时长非法: {v:?}"))?;
            pending_duration = Some(dur);
            continue;
        }
        if line.starts_with('#') {
            continue; // 其余标签忽略（DISCONTINUITY/PROGRAM-DATE-TIME/注释等）
        }
        // URI 行：前必有 EXTINF（结构健全性）
        let duration = pending_duration
            .take()
            .ok_or_else(|| format!("清单结构非法：URI 行前无 EXTINF: {line:?}"))?;
        let crypto = match &current_key {
            Some(SegCrypto::Aes128 { key_url, iv }) => SegCrypto::Aes128 {
                key_url: key_url.clone(),
                iv: *iv,
            },
            _ => SegCrypto::None,
        };
        segments.push(HlsSegment {
            url: resolve_url(base_url, line),
            crypto,
            duration_secs: duration,
        });
    }

    if seen_stream_inf && segments.is_empty() {
        return Err("master playlist 需先选流（上游应展开变体后传入 media playlist）".into());
    }
    if segments.is_empty() {
        return Err("HLS 清单无段（空清单或不认识的格式）".into());
    }
    if !seen_endlist {
        return Err("HLS 清单为 live 流（无 EXT-X-ENDLIST），v1 仅支持 VOD 点播".into());
    }
    // IV 缺省：未在 EXT-X-KEY 声明 IV 时，AES-128 用该 key 下所有段的
    // media sequence number（大端 16B）。key 是行内状态 → 解析时即知每段
    // 的序号（段 i 的序列号 = media_sequence + i），无需延迟推导。
    for (seq_off, seg) in (0u64..).zip(segments.iter_mut()) {
        let seq = media_sequence + seq_off;
        if let SegCrypto::Aes128 { iv, .. } = &mut seg.crypto {
            // 解析时未显式 IV 的 key 已在 parse_key_attr 填了占位 0——
            // 这里按序号回填仅当「key 声明无 IV」。为此 parse_key_attr 用
            // has_iv 区分：无 IV → 此处回填。实现见下方重放逻辑。
            if seg_has_placeholder_iv(iv, seq) {
                *iv = iv_from_sequence(seq);
            }
        }
    }
    Ok(MediaPlaylist {
        segments,
        media_sequence,
    })
}

/// 占位判断：parse_key_attr 无 IV 时填 0 哨兵 + 下方重放按序号回填。
/// （简化：显式 IV=全零与哨兵不可区分——全零 IV 现实中不存在，注释明示。）
fn seg_has_placeholder_iv(iv: &[u8; 16], _seq: u64) -> bool {
    iv.iter().all(|b| *b == 0)
}

/// IV 缺省推导：media sequence number 64-bit 大端，左补零 16 字节。
pub fn iv_from_sequence(seq: u64) -> [u8; 16] {
    let mut iv = [0u8; 16];
    iv[8..].copy_from_slice(&seq.to_be_bytes());
    iv
}

/// 解析 `#EXT-X-KEY:` 属性串 → SegCrypto。METHOD=NONE → None；
/// AES-128 必带 URI（IV 可缺省 → 16B 全零哨兵，由 parse_media_playlist
/// 按段序号回填——全零 IV 现实中不存在，可安全作哨兵）。
fn parse_key_attr(attrs: &str, base_url: &str) -> Result<Option<SegCrypto>, String> {
    let mut method = String::new();
    let mut uri: Option<String> = None;
    let mut iv: Option<[u8; 16]> = None;
    for part in split_attrs(attrs) {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches('"');
        match k {
            "METHOD" => method = v.to_string(),
            "URI" => uri = Some(resolve_url(base_url, v)),
            "IV" => {
                let hex = v.trim_start_matches("0x").trim_start_matches("0X");
                if hex.len() != 32 {
                    return Err(format!("EXT-X-KEY IV 非法（需 32 hex）: {v:?}"));
                }
                let mut b = [0u8; 16];
                for i in 0..16 {
                    b[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
                        .map_err(|_| format!("EXT-X-KEY IV hex 非法: {v:?}"))?;
                }
                iv = Some(b);
            }
            _ => {}
        }
    }
    match method.as_str() {
        "NONE" => Ok(None),
        "AES-128" => {
            let key_url = uri.ok_or("EXT-X-KEY METHOD=AES-128 缺 URI")?;
            Ok(Some(SegCrypto::Aes128 {
                key_url,
                iv: iv.unwrap_or([0u8; 16]),
            }))
        }
        other => Err(format!("HLS v1 不支持加密方式: {other}")),
    }
}

/// `K=V,K="V,V",K=0x...` 属性切分（引号内逗号不分隔）。
fn split_attrs(attrs: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in attrs.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ',' if !in_quotes => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Master playlist：返回 BANDWIDTH 最高的变体 URI（v1 策略：质量优先）。
pub fn pick_best_variant(base_url: &str, text: &str) -> Result<String, String> {
    let mut best: Option<(u64, String)> = None;
    let mut pending_bw: Option<u64> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(v) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending_bw = None;
            for part in split_attrs(v) {
                if let Some((k, val)) = part.split_once('=') {
                    if k.trim() == "BANDWIDTH" {
                        pending_bw = val.trim().parse().ok();
                    }
                }
            }
            continue;
        }
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if let Some(bw) = pending_bw.take() {
            let uri = resolve_url(base_url, line);
            if best.as_ref().map(|(b, _)| bw > *b).unwrap_or(true) {
                best = Some((bw, uri));
            }
        }
    }
    best.map(|(_, u)| u)
        .ok_or("master playlist 无可解析变体".to_string())
}

/// AES-128-CBC + PKCS7 解密（每段独立密文单元）。
pub fn aes128_cbc_decrypt(
    ciphertext: &[u8],
    key: &[u8; 16],
    iv: &[u8; 16],
) -> Result<Vec<u8>, String> {
    use cbc::cipher::{BlockDecryptMut, KeyIvInit};
    type Dec = cbc::Decryptor<aes::Aes128>;
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(16) {
        return Err(format!("AES 段长度非 16 倍数: {}", ciphertext.len()));
    }
    let mut buf = ciphertext.to_vec();
    let pt = Dec::new(key.into(), iv.into())
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|e| format!("AES-PKCS7 解密失败: {e}"))?;
    Ok(pt.to_vec())
}

/// 段账本（恢复凭据）：清单指纹 + 已完成段数 + 累计字节。
#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq, Debug)]
pub struct HlsLedger {
    pub version: u32,
    /// 清单指纹（sha256 hex， playlist 文本 + base_url）——恢复时对账，
    /// 失配（清单变化）→ 作废重下。
    pub playlist_fingerprint: String,
    /// 已完成段数（顺序语义 → 前缀完整）。
    pub segments_done: usize,
    /// 已落盘字节（= .part 当前长度，冗余记录供校验）。
    pub bytes_done: u64,
}

pub const HLS_LEDGER_VERSION: u32 = 1;

pub fn hls_ledger_path(part: &Path) -> PathBuf {
    let mut s = part.as_os_str().to_os_string();
    s.push(".hls-ledger");
    PathBuf::from(s)
}

pub fn hls_fingerprint(base_url: &str, playlist_text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(base_url.as_bytes());
    h.update(b"\n");
    h.update(playlist_text.as_bytes());
    hex_encode(&h.finalize())
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push_str(&format!("{x:02x}"));
    }
    s
}

/// HLS 下载核心：拉清单（master 展开）→ 解析 → 续传对账 → 顺序段下载
/// （解密 + append）→ finalize 交付。
/// `on_progress` 累计回调（字节）；`limiter` 段级限速（与既有引擎同口径）。
/// 拉取文本资源（清单/key 共用：任务级 headers 透传，30s 超时对齐探测口径）。
async fn fetch_text(
    client: &reqwest::Client,
    headers: &[(String, String)],
    url: &str,
) -> Result<String, EngineError> {
    let mut req = client.get(url).timeout(std::time::Duration::from_secs(30));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let resp = req
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| EngineError::Other(format!("HLS 清单拉取失败: {e}")))?;
    resp.text()
        .await
        .map_err(|e| EngineError::Other(format!("HLS 清单读取失败: {e}")))
}

#[allow(clippy::too_many_arguments)] // 协议会话要素，同 ftp.rs 惯例
pub async fn download_hls(
    client: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    dest: PathBuf,
    limiter: Arc<RateLimiter>,
    on_progress: Arc<dyn Fn(u64) + Send + Sync>,
    abort: Arc<AtomicBool>,
) -> Result<(), EngineError> {
    // master → media（最多一层变体）
    let mut playlist_url = url.clone();
    let mut playlist_text = fetch_text(&client, &headers, &playlist_url).await?;
    if playlist_text.contains("#EXT-X-STREAM-INF") {
        playlist_url =
            pick_best_variant(&playlist_url, &playlist_text).map_err(EngineError::Other)?;
        playlist_text = fetch_text(&client, &headers, &playlist_url).await?;
    }
    let playlist =
        parse_media_playlist(&playlist_url, &playlist_text).map_err(EngineError::Other)?;

    // .part 续传对账
    let part = part_path_of(&dest);
    let ledger_path = hls_ledger_path(&part);
    let fingerprint = hls_fingerprint(&playlist_url, &playlist_text);
    let mut segments_done: usize = 0;
    let mut bytes_done: u64 = 0;
    if let Some(ld) = load_ledger(&ledger_path) {
        if ld.version == HLS_LEDGER_VERSION
            && ld.playlist_fingerprint == fingerprint
            && ld.segments_done <= playlist.segments.len()
            && part.is_file()
            && std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) == ld.bytes_done
        {
            segments_done = ld.segments_done;
            bytes_done = ld.bytes_done;
        } else {
            let _ = std::fs::remove_file(&part);
        }
    } else {
        let _ = std::fs::remove_file(&part);
    }

    // 追加打开（续传沿用，全新建 0）
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&part)
        .map_err(|e| EngineError::Other(format!("part open: {e}")))?;
    if segments_done == 0 {
        // 全新/作废：append 模式下 truncate 无意义，显式置空
        f.set_len(0)
            .map_err(|e| EngineError::Other(format!("part truncate: {e}")))?;
        on_progress(0); // 起点锚（total=0 未知口径）
    } else {
        on_progress(bytes_done); // 恢复口径回填
    }

    // key 缓存（同 key URL 只拉一次）
    let mut key_cache: std::collections::HashMap<String, [u8; 16]> = Default::default();

    let total_segs = playlist.segments.len();
    for (idx, seg) in playlist.segments.iter().enumerate().skip(segments_done) {
        // abort 检查点（段间）：pause/remove 置位 → 立即中断（账本已落，
        // resume 凭 segments_done 续传）
        if abort.load(Ordering::SeqCst) {
            return Err(EngineError::Other(HLS_ABORTED_MSG.into()));
        }
        let mut req = client
            .get(&seg.url)
            .timeout(std::time::Duration::from_secs(120));
        for (k, v) in &headers {
            req = req.header(k, v);
        }
        let resp = req
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| EngineError::Other(format!("段 {idx}/{total_segs} 拉取失败: {e}")))?;
        let raw = resp
            .bytes()
            .await
            .map_err(|e| EngineError::Other(format!("段 {idx}/{total_segs} 读取失败: {e}")))?;
        let payload = match &seg.crypto {
            SegCrypto::None => raw.to_vec(),
            SegCrypto::Aes128 { key_url, iv } => {
                let key = match key_cache.get(key_url) {
                    Some(k) => *k,
                    None => {
                        let mut kreq = client
                            .get(key_url)
                            .timeout(std::time::Duration::from_secs(30));
                        for (k, v) in &headers {
                            kreq = kreq.header(k, v);
                        }
                        let kresp = kreq
                            .send()
                            .await
                            .and_then(|r| r.error_for_status())
                            .map_err(|e| EngineError::Other(format!("key 拉取失败: {e}")))?;
                        let kb = kresp
                            .bytes()
                            .await
                            .map_err(|e| EngineError::Other(format!("key 读取失败: {e}")))?;
                        if kb.len() != 16 {
                            return Err(EngineError::Other(format!(
                                "AES-128 key 长度非 16B: {}",
                                kb.len()
                            )));
                        }
                        let mut k16 = [0u8; 16];
                        k16.copy_from_slice(&kb);
                        key_cache.insert(key_url.clone(), k16);
                        k16
                    }
                };
                aes128_cbc_decrypt(&raw, &key, iv).map_err(EngineError::Other)?
            }
        };
        limiter.wait(payload.len() as u64).await;
        f.write_all(&payload)
            .map_err(|e| EngineError::Other(format!("part 写入: {e}")))?;
        bytes_done += payload.len() as u64;
        on_progress(payload.len() as u64);
        // 段完成即落账本（顺序前缀语义 → 崩溃后从下一段续）
        save_ledger(
            &ledger_path,
            &HlsLedger {
                version: HLS_LEDGER_VERSION,
                playlist_fingerprint: fingerprint.clone(),
                segments_done: idx + 1,
                bytes_done,
            },
        );
    }
    drop(f);

    // finalize：.part → dest
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| EngineError::Other(format!("dest mkdir: {e}")))?;
    }
    std::fs::rename(&part, &dest).map_err(|e| {
        EngineError::Other(format!(
            "finalize {} → {}: {e}",
            part.display(),
            dest.display()
        ))
    })?;
    let _ = std::fs::remove_file(&ledger_path);
    Ok(())
}

fn load_ledger(path: &Path) -> Option<HlsLedger> {
    let raw = std::fs::read(path).ok()?;
    serde_json::from_slice(&raw).ok()
}

fn save_ledger(path: &Path, ld: &HlsLedger) {
    if let Ok(json) = serde_json::to_vec(ld) {
        let tmp = {
            let mut s = path.as_os_str().to_os_string();
            s.push(".tmp");
            PathBuf::from(s)
        };
        if std::fs::write(&tmp, &json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }
}

/// .part 路径（HLS 无 gen 语义：`<dest>.part`，与 engine.rs gen0 同形）。
fn part_path_of(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

/// 派生落盘名：`<m3u8 文件名>.ts`（x/path/ep.m3u8 → ep.ts）。
pub fn derive_ts_name(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let last = path.rsplit('/').next().unwrap_or("");
    let stem = last
        .strip_suffix(".m3u8")
        .or_else(|| last.strip_suffix(".M3U8"))?;
    if stem.is_empty() {
        return None;
    }
    smart_dl_core::session::output::sanitize_rel(&format!("{stem}.ts"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VOD: &str = "#EXTM3U\n\
#EXT-X-VERSION:3\n\
#EXT-X-TARGETDURATION:10\n\
#EXT-X-MEDIA-SEQUENCE:5\n\
#EXTINF:9.0,\nseg0.ts\n\
#EXTINF:9.5,\nseg1.ts\n\
#EXT-X-ENDLIST\n";

    #[test]
    fn parses_vod_with_sequence_iv_default() {
        let p = parse_media_playlist("https://h/v/list.m3u8", VOD).unwrap();
        assert_eq!(p.media_sequence, 5);
        assert_eq!(p.segments.len(), 2);
        assert_eq!(p.segments[0].url, "https://h/v/seg0.ts");
        assert_eq!(p.segments[0].crypto, SegCrypto::None);
        assert_eq!(p.segments[0].duration_secs, 9.0);
    }

    #[test]
    fn aes_key_with_explicit_iv_and_relative_uri() {
        let text = "#EXTM3U\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x9c7db8778570d05c3177c349fd9236aa\n\
#EXTINF:10,\n01.ts\n\
#EXTINF:10,\n02.ts\n\
#EXT-X-ENDLIST\n";
        let p = parse_media_playlist("https://h/sub/list.m3u8", text).unwrap();
        assert_eq!(p.segments[0].url, "https://h/sub/01.ts");
        assert_eq!(
            p.segments[0].crypto,
            SegCrypto::Aes128 {
                key_url: "https://h/sub/key.bin".into(),
                iv: [
                    0x9c, 0x7d, 0xb8, 0x77, 0x85, 0x70, 0xd0, 0x5c, 0x31, 0x77, 0xc3, 0x49, 0xfd,
                    0x92, 0x36, 0xaa
                ]
            }
        );
    }

    #[test]
    fn aes_key_without_iv_derives_from_sequence() {
        let text = "#EXTM3U\n\
#EXT-X-MEDIA-SEQUENCE:7\n\
#EXT-X-KEY:METHOD=AES-128,URI=\"https://k/key\"\n\
#EXTINF:10,\ns0.ts\n\
#EXTINF:10,\ns1.ts\n\
#EXT-X-ENDLIST\n";
        let p = parse_media_playlist("https://h/list.m3u8", text).unwrap();
        assert_eq!(
            p.segments[0].crypto,
            SegCrypto::Aes128 {
                key_url: "https://k/key".into(),
                iv: iv_from_sequence(7)
            }
        );
        assert_eq!(
            p.segments[1].crypto,
            SegCrypto::Aes128 {
                key_url: "https://k/key".into(),
                iv: iv_from_sequence(8)
            }
        );
    }

    #[test]
    fn live_playlist_rejected() {
        let text = "#EXTM3U\n#EXTINF:10,\ns0.ts\n"; // 无 ENDLIST
        assert!(parse_media_playlist("https://h/list.m3u8", text)
            .unwrap_err()
            .contains("live"));
    }

    #[test]
    fn byterange_and_map_rejected() {
        assert!(parse_media_playlist(
            "https://h/l.m3u8",
            "#EXTM3U\n#EXT-X-MAP:URI=\"i.mp4\"\n#EXTINF:1,\ns.m4s\n#EXT-X-ENDLIST\n"
        )
        .unwrap_err()
        .contains("EXT-X-MAP"));
        assert!(parse_media_playlist(
            "https://h/l.m3u8",
            "#EXTM3U\n#EXT-X-BYTERANGE:1000@0\n#EXTINF:1,\ns.ts\n#EXT-X-ENDLIST\n"
        )
        .unwrap_err()
        .contains("BYTERANGE"));
    }

    #[test]
    fn sample_aes_rejected() {
        let text =
            "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n#EXTINF:1,\ns.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_media_playlist("https://h/l.m3u8", text).is_err());
    }

    #[test]
    fn uri_before_extinf_rejected() {
        let text = "#EXTM3U\nseg0.ts\n#EXT-X-ENDLIST\n";
        assert!(parse_media_playlist("https://h/l.m3u8", text).is_err());
    }

    #[test]
    fn master_picks_highest_bandwidth() {
        let master = "#EXTM3U\n\
#EXT-X-STREAM-INF:BANDWIDTH=1280000,RESOLUTION=640x360\nlow.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=4128000,RESOLUTION=1920x1080\nhigh/index.m3u8\n\
#EXT-X-STREAM-INF:BANDWIDTH=2560000\nmid.m3u8\n";
        assert_eq!(
            pick_best_variant("https://h/master.m3u8", master).unwrap(),
            "https://h/high/index.m3u8"
        );
    }

    #[test]
    fn resolves_urls() {
        assert_eq!(
            resolve_url("https://h/a/b.m3u8", "seg.ts"),
            "https://h/a/seg.ts"
        );
        assert_eq!(
            resolve_url("https://h/a/b.m3u8", "/x/seg.ts"),
            "https://h/x/seg.ts"
        );
        assert_eq!(
            resolve_url("https://h/a/b.m3u8", "//cdn.io/s.ts"),
            "https://cdn.io/s.ts"
        );
        assert_eq!(
            resolve_url("https://h/a/b.m3u8?tok=1", "http://o/s.ts"),
            "http://o/s.ts"
        );
    }

    #[test]
    fn hls_url_detection() {
        assert!(is_hls_url("https://h/live.M3U8?a=1"));
        assert!(is_hls_url("https://h/v/list.m3u8"));
        assert!(!is_hls_url("https://h/v/list.m3u9"));
        assert!(!is_hls_url("https://h/file.ts"));
    }

    #[test]
    fn ts_name_derivation() {
        assert_eq!(
            derive_ts_name("https://h/v/ep01.m3u8?tok=x").unwrap(),
            "ep01.ts"
        );
        assert_eq!(derive_ts_name("https://h/片头.M3U8").unwrap(), "片头.ts");
        assert!(derive_ts_name("https://h/v/").is_none());
    }

    #[test]
    fn aes_roundtrip_pkcs7() {
        use cbc::cipher::{BlockEncryptMut, KeyIvInit};
        type Enc = cbc::Encryptor<aes::Aes128>;
        let key = [7u8; 16];
        let iv = [9u8; 16];
        let pt = b"hello hls segment payload!!"; // 27B → PKCS7 补 5B
        let mut buf = vec![0u8; pt.len() + 16];
        buf[..pt.len()].copy_from_slice(pt);
        let ct = Enc::new((&key).into(), (&iv).into())
            .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf, pt.len())
            .unwrap();
        let dec = aes128_cbc_decrypt(ct, &key, &iv).unwrap();
        assert_eq!(dec, pt);
    }
}
