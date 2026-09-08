//! 迅雷 GCID / CID 文件内容哈希（xunlei-lixian 公开算法）。

use sha1::{Digest, Sha1};

/// 手写 hex 编码。
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// GCID 的 piece 大小：0x40000 起，file_size/piece_size > 0x200 时翻倍，上限 0x200000。
fn calc_block_size(file_size: u64) -> u64 {
    let mut psize: u64 = 0x40000;
    while file_size / psize > 0x200 && psize < 0x200000 {
        psize <<= 1;
    }
    psize
}

/// GCID = SHA1( SHA1(piece1) || SHA1(piece2) || ... ) 的 hex。
pub fn gcid(data: &[u8]) -> String {
    let mut hash1 = Sha1::new();
    let psize = calc_block_size(data.len() as u64) as usize;
    for chunk in data.chunks(psize) {
        hash1.update(Sha1::digest(chunk));
    }
    to_hex(&hash1.finalize())
}

/// CID (=DCID)：文件 <60KB → SHA1(全文)；否则 SHA1(头20KB || 1/3处20KB || 尾20KB)。
pub fn cid(data: &[u8]) -> String {
    if data.len() < 60 * 1024 {
        return to_hex(&Sha1::digest(data));
    }
    let head = &data[0..20 * 1024];
    let mid_start = data.len() / 3;
    let mid = &data[mid_start..mid_start + 20 * 1024];
    let tail = &data[data.len() - 20 * 1024..];
    let mut h = Sha1::new();
    h.update(head);
    h.update(mid);
    h.update(tail);
    to_hex(&h.finalize())
}

/// BCID：按 block_size 分块，逐块 SHA1 后的列表（用途：云端秒传 / 上传校验候选）。
///
/// 移植自 toolkit/xunlei_hash.py#calculate_bcid#L176-L194：
///   for each block of `block_size` bytes:
///       hashes.append(sha1(block).hexdigest().upper())
/// 返回 40 位小写 hex 字符串列表（与本项目其余哈希保持一致的小写风格；
/// 与 Python 版返回大写不同，来源：#L192 `.upper()`）。
///
/// 关于 block_size 动态规则（calc_block_size，来源 #L41-L58）：
///   Python 版在 block_size 为 None 时回退到 calc_block_size(file_size)，
///   但本 Rust 版签名直接将 block_size 作为参数（来源 #L176 `block_size: int = None` 的展开形态），
///   因此不在此调用 calc_block_size；调用方如需动态分块，可先以
///   `calc_block_size(file_size) as u32` 计算后传入（注意 Python 的 calc_block_size 返回 int，
///   本模块同名私有函数返回 u64，逻辑等价：0x40000 起、file_size/psize>0x200 时翻倍、上限 0x200000）。
///
/// 边界：block_size 为 0 视为非法、返回空列表（避免分块除零）；空输入返回空列表。
pub fn bcid(data: &[u8], block_size: u32) -> Vec<String> {
    if block_size == 0 {
        return Vec::new();
    }
    let bs = block_size as usize;
    data.chunks(bs)
        .map(|chunk| to_hex(&Sha1::digest(chunk)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_size_small_file_is_256k() {
        assert_eq!(calc_block_size(1024 * 1024), 0x40000);
    }

    #[test]
    fn block_size_caps_at_2m() {
        assert_eq!(calc_block_size(100 * 1024 * 1024 * 1024), 0x200000);
    }

    #[test]
    fn gcid_is_40_hex() {
        let data = vec![0xAAu8; 1024 * 1024];
        let gcid = gcid(&data);
        assert_eq!(gcid.len(), 40);
        assert!(gcid.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn gcid_is_deterministic() {
        let data = vec![0xBBu8; 4096];
        assert_eq!(gcid(&data), gcid(&data));
    }

    #[test]
    fn gcid_empty_data() {
        assert_eq!(gcid(&[]), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    #[test]
    fn cid_small_file_is_full_sha1() {
        let data = vec![0x11u8; 1000];
        assert_eq!(cid(&data), to_hex(&Sha1::digest(&data)));
    }

    #[test]
    fn cid_large_file_samples_three_regions() {
        let data = vec![0x22u8; 100 * 1024];
        let c = cid(&data);
        assert_eq!(c.len(), 40);

        let head = &data[0..20 * 1024];
        let mid_start = data.len() / 3;
        let mid = &data[mid_start..mid_start + 20 * 1024];
        let tail = &data[data.len() - 20 * 1024..];
        let mut h = Sha1::new();
        h.update(head);
        h.update(mid);
        h.update(tail);
        assert_eq!(c, to_hex(&h.finalize()));
    }

    #[test]
    fn bcid_empty_input_returns_empty() {
        // 边界：空输入返回空列表。来源：toolkit/xunlei_hash.py#calculate_bcid#L186-L194
        let out = bcid(&[], 1024);
        assert!(out.is_empty());
    }

    #[test]
    fn bcid_zero_block_size_returns_empty() {
        // 边界：block_size 为 0 视为非法，返回空列表（避免分块除零）。
        let data = vec![0xABu8; 4096];
        assert!(bcid(&data, 0).is_empty());
    }

    #[test]
    fn bcid_chunk_count_and_format() {
        // 已知字节模式：3000 字节，block_size=1024 → 3 块（ceil(3000/1024)=3）
        let mut data = vec![0u8; 3000];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let block_size: u32 = 1024;
        let out = bcid(&data, block_size);

        // 分块数：向上取整
        assert_eq!(out.len(), 3);

        // 每块应为 40 位小写 hex（20 字节 SHA1）
        for h in &out {
            assert_eq!(h.len(), 40);
            assert!(h
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)));
        }

        // 逐块与独立 SHA1 计算一致（来源：#L192 sha1(block)）
        let expected: Vec<String> = data
            .chunks(block_size as usize)
            .map(|c| to_hex(&Sha1::digest(c)))
            .collect();
        assert_eq!(out, expected);
    }

    #[test]
    fn bcid_exact_multiple_blocks() {
        // 2048 字节 / 1024 → 恰好 2 块，无尾随空块
        let data = vec![0x7Eu8; 2048];
        let out = bcid(&data, 1024);
        assert_eq!(out.len(), 2);
        // 两块哈希相同（同值输入）
        assert_eq!(out[0], out[1]);
    }
}
