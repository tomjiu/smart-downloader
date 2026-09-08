//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! torrent_infohash（bencode 最小解析）单元测试。
#![cfg(all(test, feature = "bt"))]

use super::*;

/// 最小合法 .torrent：d4:info<infodict>e
fn sample_torrent() -> Vec<u8> {
    let mut t = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
    t.extend_from_slice(&[0xAB; 20]);
    t.extend_from_slice(b"ee");
    t
}

#[test]
fn extracts_infohash_from_info_dict() {
    let ih = torrent_infohash(&sample_torrent()).unwrap();
    // 预计算：SHA1(info dict) = 7ac2e18f...（info dict = t[7..=86]，80B）
    assert_eq!(ih, "7ac2e18f65f50b19e6bb1069e15ff2398aac220d");
}

#[test]
fn rejects_non_dict_root() {
    assert!(torrent_infohash(b"nonsense").is_none());
    assert!(torrent_infohash(b"").is_none());
}

#[test]
fn rejects_missing_info_key() {
    // 合法 bencode dict 但无 info 键
    let t = b"d3:foo3:bare";
    assert!(torrent_infohash(t).is_none());
}

#[test]
fn skips_values_before_info_key() {
    // info 前有其他键值（含嵌套 list/int）仍能定位
    let mut t = b"d5:hello5:world7:payloadli3ee4:info".to_vec();
    t.extend_from_slice(&sample_torrent()[7..]); // info dict 起点起 + 顶层 e
    let ih = torrent_infohash(&t).unwrap();
    assert_eq!(ih, "7ac2e18f65f50b19e6bb1069e15ff2398aac220d");
}

#[test]
fn total_size_single_file() {
    let t = sample_torrent();
    // length=123（单文件）
    assert_eq!(torrent_total_size(&t), Some(123));
}

#[test]
fn total_size_multi_file_returns_none() {
    // 多文件 torrent：files 列表 → v1 None
    let mut t = b"d4:infod5:filesld6:lengthi10e4:pathl1:aeed6:lengthi20e4:pathl1:beeee".to_vec();
    assert_eq!(torrent_total_size(&t), None);
    let _ = &mut t;
}

#[test]
fn total_size_missing_length_none() {
    let mut t = b"d4:info4:name4:teste".to_vec();
    assert_eq!(torrent_total_size(&t), None);
    let _ = &mut t;
}

#[test]
fn parse_minimal_single_file_torrent() {
    // 构造最小单文件 torrent（bencode）
    let mut torrent = Vec::new();
    torrent.extend_from_slice(b"d"); // dict
    torrent.extend_from_slice(b"8:announce14:http://tracker"); // announce
    torrent.extend_from_slice(b"4:infod"); // info dict
    torrent.extend_from_slice(b"6:lengthi12345e"); // length
    torrent.extend_from_slice(b"4:name8:filename"); // name
    torrent.extend_from_slice(b"12:piece lengthi16384e"); // piece length
    torrent.extend_from_slice(b"6:pieces"); // pieces key
    torrent.extend_from_slice(b"20:"); // pieces value 长度前缀
    torrent.extend_from_slice(&[0u8; 20]); // dummy piece hash
    torrent.extend_from_slice(b"ee"); // end info dict, end dict

    let meta = TorrentMeta::parse(&torrent).unwrap();
    assert_eq!(meta.info_hash.len(), 40);
    assert_eq!(meta.piece_length, 16384);
    assert_eq!(meta.pieces_hash.len(), 1);
    assert_eq!(meta.name, "filename");
    assert_eq!(meta.file_size, 12345);
}

#[test]
fn parse_multi_file() {
    // 多文件 torrent 应被解析（files 列表含 2 个文件）
    let mut torrent = Vec::new();
    torrent.extend_from_slice(b"d4:infod"); // 顶层 + info dict
    torrent.extend_from_slice(b"12:piece lengthi16384e"); // piece length
    torrent.extend_from_slice(b"6:pieces20:"); // pieces (1 piece)
    torrent.extend_from_slice(&[0u8; 20]);
    torrent.extend_from_slice(b"4:name8:multidir"); // name
    torrent.extend_from_slice(b"5:filesl"); // files list
                                            // 文件1: length=10, path=["a"]
    torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
    // 文件2: length=20, path=["b"]
    torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
    torrent.extend_from_slice(b"e"); // end files list
    torrent.extend_from_slice(b"ee"); // end info dict + top dict

    let meta = TorrentMeta::parse(&torrent).unwrap();
    assert_eq!(meta.files.len(), 2, "应解析出 2 个文件");
    assert_eq!(meta.files[0].path, "a");
    assert_eq!(meta.files[0].size, 10);
    assert_eq!(meta.files[1].path, "b");
    assert_eq!(meta.files[1].size, 20);
}

#[test]
fn precheck_total_multi_file_sums_files() {
    // 多文件 torrent：预检总量 = 各文件 size 之和（torrent_total_size 遇 files
    // 返回 None，此处验证升级后 parse 路径生效）
    let mut torrent = Vec::new();
    torrent.extend_from_slice(b"d4:infod");
    torrent.extend_from_slice(b"12:piece lengthi16384e");
    torrent.extend_from_slice(b"6:pieces20:");
    torrent.extend_from_slice(&[0u8; 20]);
    torrent.extend_from_slice(b"4:name8:multidir");
    torrent.extend_from_slice(b"5:filesl");
    torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
    torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
    torrent.extend_from_slice(b"e");
    torrent.extend_from_slice(b"ee");
    assert_eq!(torrent_precheck_total(&torrent), Some(30));
}

#[test]
fn precheck_total_single_file_uses_length() {
    let t = sample_torrent();
    // length=123（单文件走 file_size）
    assert_eq!(torrent_precheck_total(&t), Some(123));
}

#[test]
fn precheck_total_parse_fail_falls_back() {
    // TorrentMeta::parse 失败（缺 piece length/pieces）但 info dict 内 length
    // 可定位 → 回退 torrent_total_size 路径
    let t = b"d4:infod6:lengthi999e4:name4:testee";
    assert_eq!(torrent_precheck_total(t), Some(999));
}

/// xunlei 导入测试：add_xunlei_import_task / xunlei_convert 仅在
/// xunlei-import feature 下存在，单独门控以免 --features bt 编译失败。
#[cfg(all(test, feature = "xunlei-import"))]
mod xunlei_import_tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn add_xunlei_import_task_e2e() {
        // 使用 tools/xunlei-migrate/e2e_out 合成样本测试完整导入流程
        let e2e = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../tools/xunlei-migrate/e2e_out");
        let torrent_path = e2e.join("test.torrent");
        let cfg_path = e2e.join("test.xlbt.cfg");
        let xltd_path = e2e.join("test.bt.xltd");
        if !torrent_path.exists() || !cfg_path.exists() || !xltd_path.exists() {
            eprintln!("SKIP: e2e files not found at {:?}", e2e);
            return;
        }

        let torrent = std::fs::read(&torrent_path).expect("read torrent");
        let cfg = std::fs::read(&cfg_path).expect("read cfg");
        let xltd = std::fs::read(&xltd_path).expect("read xltd");

        // 构造 DaemonState（FakeEngine 作为 BT 引擎）
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

        let tid = state
            .add_xunlei_import_task(torrent, cfg, vec![xltd], None)
            .await
            .expect("add_xunlei_import_task should succeed");

        // 验证任务已创建
        let tasks = state.tasks.lock();
        let rec = tasks.get(&tid).expect("task exists");
        assert_eq!(rec.task.state, TaskState::Queued);
        assert_eq!(rec.engine_kind, EngineKind::Bt);
        assert!(rec.engine_tid.is_some());
        assert_eq!(fake.xunlei_resumes().len(), 1);
    }

    #[tokio::test]
    async fn add_xunlei_import_task_requires_matching_xltds() {
        // 多文件 torrent（2 文件）传入 0 个 xltd → 数量不匹配
        let mut torrent = Vec::new();
        torrent.extend_from_slice(b"d4:infod");
        torrent.extend_from_slice(b"12:piece lengthi16384e");
        torrent.extend_from_slice(b"6:pieces20:");
        torrent.extend_from_slice(&[0u8; 20]);
        torrent.extend_from_slice(b"4:name8:multidir");
        torrent.extend_from_slice(b"5:filesl");
        torrent.extend_from_slice(b"d6:lengthi10e4:pathl1:aee");
        torrent.extend_from_slice(b"d6:lengthi20e4:pathl1:bee");
        torrent.extend_from_slice(b"e");
        torrent.extend_from_slice(b"ee");

        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

        let err = state
            .add_xunlei_import_task(torrent, vec![], vec![], None)
            .await
            .expect_err("应拒绝 xltd 数量不匹配");
        assert!(
            err.to_string().contains("不匹配"),
            "错误信息应提示数量不匹配: {err}"
        );
        assert_eq!(fake.xunlei_resumes().len(), 0);
    }

    #[tokio::test]
    async fn add_xunlei_import_task_rejects_duplicate() {
        // 使用 e2e 合成样本
        let e2e = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../tools/xunlei-migrate/e2e_out");
        let torrent_path = e2e.join("test.torrent");
        let cfg_path = e2e.join("test.xlbt.cfg");
        let xltd_path = e2e.join("test.bt.xltd");
        if !torrent_path.exists() || !cfg_path.exists() || !xltd_path.exists() {
            eprintln!("SKIP: e2e files not found at {:?}", e2e);
            return;
        }

        let torrent = std::fs::read(&torrent_path).expect("read torrent");
        let cfg = std::fs::read(&cfg_path).expect("read cfg");
        let xltd = std::fs::read(&xltd_path).expect("read xltd");

        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

        // 第一次导入成功
        let tid1 = state
            .add_xunlei_import_task(torrent.clone(), cfg.clone(), vec![xltd.clone()], None)
            .await
            .expect("first import should succeed");

        // 第二次导入相同 infohash → Duplicate
        let err = state
            .add_xunlei_import_task(torrent, cfg, vec![xltd], None)
            .await
            .expect_err("应拒绝重复 infohash");
        assert!(
            err.to_string().contains("duplicate") || err.to_string().contains("重复"),
            "错误信息应提示重复: {err}"
        );
        // 验证只有第一次任务被创建
        let tasks = state.tasks.lock();
        assert!(tasks.contains_key(&tid1));
        assert_eq!(tasks.len(), 1);
    }

    #[tokio::test]
    async fn add_xunlei_import_task_rejects_bad_torrent() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

        let err = state
            .add_xunlei_import_task(vec![], vec![], vec![], None)
            .await
            .expect_err("应拒绝无效 torrent");
        assert!(
            err.to_string().contains("解析失败") || err.to_string().contains("无法定位"),
            "错误信息应提示解析失败: {err}"
        );
        assert_eq!(fake.xunlei_resumes().len(), 0);
    }

    #[tokio::test]
    async fn add_xunlei_import_task_marks_lenient_partial() {
        // 读取预生成的 partial_test.torrent（16KB piece, 16 pieces, 256KB 文件）
        let e2e = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("../../tools/xunlei-migrate/e2e_out");
        let torrent_path = e2e.join("partial_test.torrent");
        if !torrent_path.exists() {
            eprintln!("SKIP: partial_test.torrent not found");
            return;
        }
        let torrent = std::fs::read(&torrent_path).expect("read torrent");
        let info_hash = torrent_infohash(&torrent).expect("infohash");

        // 构造 minimal cfg（magic + infohash + 头部观测值）
        let mut cfg = Vec::new();
        cfg.extend_from_slice(b"XDLCTX\x00\x00");
        cfg.extend_from_slice(&[0u8; 16]); // 0x08-0x17 随机区
                                           // 0x18-0x37 8 个 u32 观测值（与 e2e 样本一致）
        cfg.extend_from_slice(&30025u32.to_le_bytes());
        cfg.extend_from_slice(&7u32.to_le_bytes());
        cfg.extend_from_slice(&0u32.to_le_bytes());
        cfg.extend_from_slice(&4u32.to_le_bytes());
        cfg.extend_from_slice(&0u32.to_le_bytes());
        cfg.extend_from_slice(&28584u32.to_le_bytes());
        cfg.extend_from_slice(&4u32.to_le_bytes());
        cfg.extend_from_slice(&262145u32.to_le_bytes());
        cfg.extend_from_slice(&40u32.to_le_bytes()); // 0x38 infohash 长度 = 40 (u32)
        cfg.extend_from_slice(info_hash.as_bytes()); // 0x3C-0x63 ASCII infohash

        // 构造 xltd：piece 5 为 partial（前 8192 字节非零 = 50%）
        let piece_length = 16384usize;
        let file_size = 256 * 1024u64;
        let xltd_size = ((file_size + 4095) / 4096 * 4096) as usize;
        let mut xltd = vec![0u8; xltd_size];
        let p5_offset = 5 * piece_length;
        for i in 0..8192 {
            xltd[p5_offset + i] = 0xAB;
        }

        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().expect("tempdir");
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

        let tid = state
            .add_xunlei_import_task(torrent, cfg, vec![xltd], None)
            .await
            .expect("add_xunlei_import_task should succeed");

        let tasks = state.tasks.lock();
        let rec = tasks.get(&tid).expect("task exists");
        assert_eq!(rec.task.state, TaskState::Queued);
        assert_eq!(rec.engine_kind, EngineKind::Bt);
        assert!(rec.engine_tid.is_some());
        assert_eq!(fake.xunlei_resumes().len(), 1);

        // 验证 bitfield：piece 0 和 piece 5 应被标记为完成（lenient 策略）
        let resumes = fake.xunlei_resumes();
        let fastresume = resumes.last().unwrap();
        let bened = xunlei_convert::fastresume::bdecode(fastresume).expect("bdecode fastresume");
        let pieces = bened
            .dict_get(b"pieces")
            .expect("pieces")
            .as_bytes()
            .expect("pieces bytes");
        assert_eq!(pieces.len(), 2); // 16 pieces = 2 bytes
        assert_eq!(
            pieces[0], 0x84,
            "piece 0 and 5 should be set: got 0x{:02X}",
            pieces[0]
        );
    }
}
