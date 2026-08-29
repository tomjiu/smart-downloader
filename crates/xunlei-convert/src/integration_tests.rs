//! 真实样本集成测试（`tools/xunlei-migrate/samples/`）。
//!
//! 需要以下文件存在：
//! - `samples/C5AA149AE0776344A270EAFEE49FDADB43FF6097.xlbt.cfg`
//! - `samples/cover.jpg.bt.xltd`
//! - `tools/xunlei-migrate/e2e_out/test.torrent`（fastresume bencode 测试需要）
//!
//! 真实样本来自 `audio-books-cjk` 任务（2026-08-17 采集）。

#[cfg(test)]
mod real_sample_tests {
    use std::path::PathBuf;

    fn samples_dir() -> PathBuf {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let mut p = PathBuf::from(manifest);
        p.push("../../tools/xunlei-migrate/samples");
        p
    }

    fn e2e_dir() -> PathBuf {
        let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let mut p = PathBuf::from(manifest);
        p.push("../../tools/xunlei-migrate/e2e_out");
        p
    }

    #[test]
    fn parse_real_cfg() {
        let dir = samples_dir();
        let cfg_path = dir.join("C5AA149AE0776344A270EAFEE49FDADB43FF6097.xlbt.cfg");
        if !cfg_path.exists() {
            eprintln!("SKIP: real cfg not found at {:?}", cfg_path);
            return;
        }

        let cfg = crate::XlbtCfg::from_path(&cfg_path).expect("parse real cfg");

        // V1: magic
        assert_eq!(cfg.file_size, 32768);

        // V2: downloaded piece count (key=1)
        assert_eq!(cfg.downloaded_piece_count(), Some(1868));

        // V7: infohash
        assert_eq!(cfg.info_hash, "C5AA149AE0776344A270EAFEE49FDADB43FF6097");

        // V1: peers (validation report listed 4, actual file has 8)
        assert!(
            cfg.peers.iter().any(|p| p.starts_with("bt://102.132.137.234:51229")),
            "expected peer bt://102.132.137.234:51229 in {:?}",
            cfg.peers
        );
        assert!(
            cfg.peers.iter().any(|p| p.starts_with("bt://185.149.91.151:51028")),
            "expected peer bt://185.149.91.151:51028 in {:?}",
            cfg.peers
        );
        assert!(
            cfg.peers.iter().any(|p| p.starts_with("bt://185.21.217.29:32777")),
            "expected peer bt://185.21.217.29:32777 in {:?}",
            cfg.peers
        );
        assert!(
            cfg.peers.iter().any(|p| p.starts_with("bt://205.147.16.145:52961")),
            "expected peer bt://205.147.16.145:52961 in {:?}",
            cfg.peers
        );
        assert_eq!(cfg.peers.len(), 8, "expected 8 peers, got {:?}", cfg.peers);
    }

    #[test]
    fn analyze_real_xltd() {
        let dir = samples_dir();
        let xltd_path = dir.join("cover.jpg.bt.xltd");
        if !xltd_path.exists() {
            eprintln!("SKIP: real xltd not found at {:?}", xltd_path);
            return;
        }

        let analysis = crate::XltdAnalysis::analyze(&xltd_path).expect("analyze xltd");

        // V3: size = ceil(file_size/4096)*4096, no header
        assert_eq!(analysis.file_size, 741376);
        assert!(analysis.page_aligned);
    }

    /// 端到端验证：用 e2e 合成样本生成 bencode fastresume，校验 bitfield。
    #[test]
    fn e2e_bencode_fastresume_roundtrip() {
        let e2e = e2e_dir();
        let torrent_path = e2e.join("test.torrent");
        let cfg_path = e2e.join("test.xlbt.cfg");
        let xltd_path = e2e.join("test.bt.xltd");
        if !torrent_path.exists() || !cfg_path.exists() || !xltd_path.exists() {
            eprintln!("SKIP: e2e files not found");
            return;
        }

        // 1. 解析 e2e 样本（与 Python e2e_test_converter.py 一致）
        let cfg = crate::XlbtCfg::from_path(&cfg_path).expect("parse e2e cfg");
        // e2e: 2MB file, libtorrent 自动 piece_length=16KB → 128 pieces, half=64
        assert_eq!(cfg.downloaded_piece_count(), Some(64));

        let xltd_analysis = crate::XltdAnalysis::analyze(&xltd_path).expect("analyze xltd");
        assert_eq!(xltd_analysis.file_size, 2097152);
        assert!(xltd_analysis.page_aligned);

        // 2. 从 torrent 提取实际参数
        let piece_length = 16384u32;   // e2e 实际 piece_length
        let file_size = 2097152u64;    // 2MB
        let file_offset = 0u64;
        let num_pieces = 128usize;     // e2e 实际 piece 数

        // 3. 生成与 e2e_test_converter.py 一致的 piece 哈希
        let pieces_hash = generate_e2e_piece_hashes(num_pieces, piece_length as usize);

        // 4. 验证 piece 哈希（前 64 个 piece 应匹配）
        let mut analysis = xltd_analysis;
        analysis
            .verify_piece_hashes(&xltd_path, piece_length, &pieces_hash, file_offset, file_size)
            .expect("verify piece hashes");
        assert_eq!(analysis.completed_pieces, 64);
        assert_eq!(analysis.partial_pieces, 0);
        assert_eq!(analysis.missing_pieces, 64);

        // 5. 构建 bitfield（前 64 个 piece 完成）
        let bitfield = build_bitfield(num_pieces, 64);
        assert_eq!(bitfield.len(), 16); // 128 pieces = 16 bytes
        assert_eq!(set_bits(&bitfield), 64);

        // 6. 生成 fastresume bencode
        let mut converter = crate::FastresumeConverter::new();
        let report = converter
            .analyze(&torrent_path, &cfg_path, &xltd_path, piece_length, &pieces_hash, file_offset, file_size)
            .expect("analyze");
        assert_eq!(report.completed_pieces, 64);

        let fr = converter
            .build_fastresume("2a7e369a7aaa242458bf20d7426a68e75556b053", &bitfield, "source_file.bin", "./output", &[[file_size, 0]])
            .expect("build fastresume");
        let tmp = tempfile::NamedTempFile::new().expect("tmp file");
        converter.write_fastresume(&fr, tmp.path()).expect("write fastresume");
        let encoded = std::fs::read(tmp.path()).expect("read fastresume");

        // 7. 校验 bencode 内容
        let s = String::from_utf8_lossy(&encoded);
        assert!(s.contains("libtorrent resume file"), "missing file-format");
        assert!(s.contains("file-version"), "missing file-version");
        assert!(s.contains("info-hash"), "missing info-hash");
        assert!(s.contains("pieces"), "missing pieces");
        assert!(s.contains("file sizes"), "missing file sizes");

        // 8. 用 bencode 解码器校验 bitfield + file sizes 嵌套结构
        let decoded = crate::fastresume::bdecode(&encoded).expect("bdecode fastresume");
        let pieces = decoded.dict_get(b"pieces").expect("pieces key").as_bytes().expect("pieces bytes");
        assert_eq!(pieces.len(), 16);
        assert_eq!(&pieces[0..8], &[0xFF; 8], "first 64 bits should be 0xFF");
        assert_eq!(&pieces[8..16], &[0u8; 8], "last 64 bits should be 0x00");

        let file_sizes = decoded.dict_get(b"file sizes").expect("file sizes key").as_list().expect("file sizes list");
        assert_eq!(file_sizes.len(), 1, "single-file torrent should have one [size,0] pair");
        let pair = file_sizes[0].as_list().expect("file size pair");
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].as_int(), Some(file_size as i64));
        assert_eq!(pair[1].as_int(), Some(0));
    }

    // 辅助：生成与 e2e_test_converter.py 一致的 piece 哈希。
    // e2e source_file.bin 由 8 个 256KB source piece 组成，libtorrent 创建 128 个 16KB torrent pieces，
    // 因此 torrent piece p 对应 source piece p/16。
    fn generate_e2e_piece_hashes(num_pieces: usize, piece_length: usize) -> Vec<[u8; 20]> {
        use sha1::Digest;
        let source_piece_size = 256 * 1024;
        let num_source_pieces = 8;
        let mut hashes = Vec::new();
        for p in 0..num_pieces {
            let src_idx = p / (source_piece_size / piece_length); // = p / 16
            let mut buf = vec![0u8; piece_length];
            let pattern = (src_idx % num_source_pieces) as u32;
            for j in (0..piece_length).step_by(4) {
                buf[j..j + 4].copy_from_slice(&pattern.to_le_bytes());
            }
            let hash = sha1::Sha1::digest(&buf);
            let mut arr = [0u8; 20];
            arr.copy_from_slice(&hash);
            hashes.push(arr);
        }
        hashes
    }

    fn build_bitfield(num_pieces: usize, completed_count: usize) -> Vec<u8> {
        let byte_len = num_pieces.div_ceil(8);
        let mut bitfield = vec![0u8; byte_len];
        for i in 0..completed_count {
            bitfield[i / 8] |= 1 << (7 - (i % 8));
        }
        bitfield
    }

    fn build_bitfield_lenient(
        num_pieces: usize,
        completed_count: usize,
        partial_details: &[(usize, usize, usize)],
        min_nonzero_ratio: f32,
    ) -> Vec<u8> {
        let partials = partial_details
            .iter()
            .map(|&(idx, nonzero, total)| crate::PartialPieceInfo {
                index: idx,
                nonzero_bytes: nonzero,
                total_bytes: total,
            })
            .collect::<Vec<_>>();
        crate::build_bitfield_lenient(num_pieces, completed_count, &partials, min_nonzero_ratio)
    }

    fn set_bits(bitfield: &[u8]) -> usize {
        bitfield.iter().map(|&b| b.count_ones() as usize).sum()
    }

    #[test]
    fn build_bitfield_lenient_marks_high_ratio_partial() {
        // piece 5: 12/16 nonzero = 0.75 >= 0.5 → 应标记为完成
        let idx = 5usize;
        let partials = vec![(idx, 12, 16)];
        let bf = build_bitfield_lenient(16, 4, &partials, 0.5);
        assert_eq!(set_bits(&bf), 5);
        assert!(bf[idx / 8] & (1 << (7 - (idx % 8))) != 0);
    }

    #[test]
    fn build_bitfield_lenient_skips_low_ratio_partial() {
        // piece 5: 4/16 nonzero = 0.25 < 0.5 → 不标记
        let idx = 5usize;
        let partials = vec![(idx, 4, 16)];
        let bf = build_bitfield_lenient(16, 4, &partials, 0.5);
        assert_eq!(set_bits(&bf), 4);
        assert!(bf[idx / 8] & (1 << (7 - (idx % 8))) == 0);
    }

    #[test]
    fn build_bitfield_lenient_ignores_out_of_range() {
        let partials = vec![(99, 10, 16)]; // 超出 num_pieces=16
        let bf = build_bitfield_lenient(16, 4, &partials, 0.5);
        assert_eq!(set_bits(&bf), 4);
    }
}

