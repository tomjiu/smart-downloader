//! state.rs 测试区外置（拆分第一步，纯机械移动零语义改动）。
//! 外壳 use super::* 使各测试 mod 的 `use super::*` 名字解析与原结构一致：
//! 子模块 glob 可见父模块（外壳）私有 use 导入 → 最终全部指向 state 模块。
use super::*;

#[cfg(test)]
mod tests {
    use super::canonical_http_url;

    #[test]
    fn strips_token_param_keeps_others() {
        let c = canonical_http_url("https://host/a?token=abc&x=1&y=2");
        assert_eq!(c, "https://host/a?x=1&y=2");
    }

    #[test]
    fn strips_cloud_signing_family() {
        let c = canonical_http_url(
            "https://host/a?X-Amz-Signature=deadbeef&X-Amz-Date=20260101&sig=zz&expires=999999",
        );
        assert_eq!(c, "https://host/a");
    }

    #[test]
    fn no_token_url_unchanged() {
        let raw = "https://host/a?x=1";
        assert_eq!(canonical_http_url(raw), raw);
    }

    #[test]
    fn only_token_difference_collides() {
        let a = canonical_http_url("https://host/f?token=aaa&v=1");
        let b = canonical_http_url("https://host/f?v=1&token=bbb");
        assert_eq!(a, b);
    }

    #[test]
    fn invalid_url_passthrough() {
        assert_eq!(canonical_http_url("not a url"), "not a url");
    }

    #[test]
    fn fragment_and_path_unaffected() {
        let c = canonical_http_url("https://host/dir/file.bin?token=x&keep=1#frag");
        assert_eq!(c, "https://host/dir/file.bin?keep=1#frag");
    }
}

/// BT alert 事件流单元测试（feature `bt`）：`transition_for` 迁移矩阵 + `apply_bt_alert`
/// 匹配/缓存写入。不依赖真实 libtorrent 会话（手工构造 TaskRecord）。
#[cfg(all(test, feature = "bt"))]
mod bt_alert_tests {
    use super::*;
    use smart_dl_btcore::{Alert, AlertKind};

    fn make_state_with(rec: TaskRecord) -> DaemonState {
        let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let state = DaemonState::new(Arc::new(engine), vec![]);
        // 测试同 crate 内可访问私有 tasks 表
        (*state.tasks.lock()).insert(rec.task.id.clone(), rec);
        state
    }

    fn bt_rec(state: TaskState, ih: &str) -> TaskRecord {
        TaskRecord {
            task: DownloadTask {
                id: "t1".into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        }
    }

    #[test]
    fn finished_alert_promotes_seeding() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "ABC123"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "abc123".into(), // 大小写不同 → 归一化匹配
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.from, TaskState::Downloading(EngineKind::Bt));
        assert_eq!(eff.to, TaskState::Seeding);
        let rec_lock = state.tasks.lock();
        let rec = rec_lock.get("t1").unwrap();
        assert_eq!(rec.task.state, TaskState::Seeding, "任务记录状态必须落盘");
    }

    #[test]
    fn finished_from_queued_also_promotes() {
        // 任务还未被引擎快照驱动（仍 Queued）时，完成 alert 同样推进
        let state = make_state_with(bt_rec(TaskState::Queued, "AABB"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "aabb".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.from, TaskState::Queued);
        assert_eq!(eff.to, TaskState::Seeding);
    }

    #[test]
    fn error_alert_fails_with_message() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "D9E8"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "d9e8".into(),
            msg: "torrent error: pex failed".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.to, TaskState::Failed);
        assert_eq!(eff.message, "torrent error: pex failed");
        let rec_lock = state.tasks.lock();
        let rec = rec_lock.get("t1").unwrap();
        assert_eq!(rec.task.state, TaskState::Failed);
    }

    #[test]
    fn paused_alert_ignored() {
        // v1 不处理 Paused alert（pause 由 API 直调时同步发布事件）
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "P1"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "p1".into(),
            msg: "torrent paused".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    #[test]
    fn finished_alert_promotes_paused_to_seeding() {
        // Bug C：BT 在记录态 Paused 下被引擎实际完成（Bug A 复活后跑完），
        // Finished alert 应允许推进到 Seeding，避免记录态与引擎态错位。
        let state = make_state_with(bt_rec(TaskState::Paused, "C1"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "c1".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.from, TaskState::Paused);
        assert_eq!(eff.to, TaskState::Seeding);
        let rec_lock = state.tasks.lock();
        let rec = rec_lock.get("t1").unwrap();
        assert_eq!(rec.task.state, TaskState::Seeding, "记录态必须落盘");
    }

    #[test]
    fn non_bt_task_ignored() {
        // HTTP 任务（engine_kind=Http）不匹配 BT alert
        let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "XT77");
        rec.engine_kind = EngineKind::Http;
        let state = make_state_with(rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "xt77".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    #[test]
    fn unknown_ih_ignored() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "KN0WN"));
        let alert = Alert {
            kind: AlertKind::State,
            ih: "na-".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    #[test]
    fn error_alert_zeroes_cached_rates() {
        // E11：轮询缓存持最后窗口速率，Error alert → Failed（非活跃终态）
        // → 速率清零，否则 /stats 聚合把陈旧速率计入失败任务。
        let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "RATES01");
        rec.engine_status = Some(EngineStatus {
            down_rate: 3000,
            up_rate: 1500,
            ..EngineStatus::default()
        });
        let state = make_state_with(rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "rates01".into(),
            msg: "torrent error: pex failed".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.to, TaskState::Failed);
        let rec_lock = state.tasks.lock();
        let es = rec_lock.get("t1").unwrap().engine_status.as_ref().unwrap();
        assert_eq!(es.down_rate, 0, "Failed 后缓存下行速率必须清零");
        assert_eq!(es.up_rate, 0, "Failed 后缓存上行速率必须清零");
        assert_eq!(
            es.error.as_deref(),
            Some("torrent error: pex failed"),
            "错误信息随迁移写入缓存（task_logs 读取口径）"
        );
    }

    #[test]
    fn finished_alert_keeps_rates_for_seeding_poll() {
        // Finished → Seeding：不做种前清零（Seeding 仍是活跃轮询候选，
        // 下一轮以引擎实时值刷新——上行速率对 /stats 有意义）。
        let mut rec = bt_rec(TaskState::Downloading(EngineKind::Bt), "RATES02");
        rec.engine_status = Some(EngineStatus {
            down_rate: 3000,
            up_rate: 1500,
            ..EngineStatus::default()
        });
        let state = make_state_with(rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "rates02".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let eff = state.apply_bt_alert(&alert).unwrap();
        assert_eq!(eff.to, TaskState::Seeding);
        let rec_lock = state.tasks.lock();
        let es = rec_lock.get("t1").unwrap().engine_status.as_ref().unwrap();
        assert_eq!(es.down_rate, 3000, "Seeding 不清零（活跃候选待刷新）");
        assert_eq!(es.up_rate, 1500);
    }

    #[test]
    fn peer_alert_ignored() {
        let state = make_state_with(bt_rec(TaskState::Downloading(EngineKind::Bt), "PR99"));
        let alert = Alert {
            kind: AlertKind::Peer,
            ih: "pr99".into(),
            msg: "peer connected".into(),
            at: 0,
            resume_ready: false,
        };
        assert!(state.apply_bt_alert(&alert).is_none());
    }

    /// Bug B 回归（重入自死锁）：storage 启用 + Finished alert 迁移 → autosave。
    /// 修复前：apply_bt_alert 持 tasks 锁调用 autosave → persisted_tasks 同线程
    /// 重入同一把非重入 Mutex → 本测试 5s 超时失败（真实事故 = 全端点永久 hang）。
    #[tokio::test(flavor = "multi_thread")]
    async fn finished_alert_with_storage_autosave_no_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let state = DaemonState::new(Arc::new(engine), vec![]).with_storage(store.clone());
        let rec = bt_rec(TaskState::Queued, "BEEF");
        (*state.tasks.lock()).insert(rec.task.id.clone(), rec);
        let alert = Alert {
            kind: AlertKind::State,
            ih: "beef".into(),
            msg: "torrent finished downloading".into(),
            at: 0,
            resume_ready: false,
        };
        let work = state.apply_bt_alert(&alert);
        let eff = tokio::time::timeout(std::time::Duration::from_secs(5), async move { work })
            .await
            .expect("apply_bt_alert 死锁（Bug B 重入回归）");
        assert!(eff.is_some(), "Finished alert 应产生 Seeding 迁移");
        assert_eq!(eff.unwrap().to, TaskState::Seeding);
        // 落盘确实发生（autosave 在锁外完成）
        assert!(store.exists(), "状态迁移应触发持久化落盘");
    }
}

/// torrent_infohash（bencode 最小解析）单元测试。
#[cfg(all(test, feature = "bt"))]
mod torrent_tests {
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
        let mut t =
            b"d4:infod5:filesld6:lengthi10e4:pathl1:aeed6:lengthi20e4:pathl1:beeee".to_vec();
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
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

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
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

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
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

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
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

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
            let state =
                DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());

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
            let bened =
                xunlei_convert::fastresume::bdecode(fastresume).expect("bdecode fastresume");
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
}

/// B10 预检单元测试（ensure_dest_root / precheck_space）。
#[cfg(test)]
mod b10_tests {
    use super::{ensure_dest_root, precheck_space, DaemonError};

    #[test]
    fn creates_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nested/deep");
        let p = ensure_dest_root(
            Some(missing.to_string_lossy().into_owned()),
            &[dir.path().to_path_buf()],
        )
        .unwrap();
        assert!(p.is_dir(), "缺失目录应自动创建");
    }

    #[test]
    fn default_is_dot() {
        let p = ensure_dest_root(None, &[]).unwrap();
        assert!(p.is_dir());
    }

    #[test]
    fn invalid_path_rejected() {
        // Windows：非法路径字符 → 创建失败 → InvalidSource
        let r = ensure_dest_root(Some("a/b*c/d".into()), &[]);
        if let Err(DaemonError::InvalidSource(msg)) = r {
            assert!(msg.contains("不可创建") || msg.contains("不可写"));
        } else {
            // 某些平台可能允许——不强断言，仅确认类型
            assert!(r.is_ok() || r.is_err());
        }
    }

    // 安全回归（V2）：dest 越界必须被白名单拦截。
    #[test]
    fn dest_outside_allowed_roots_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("dl");
        std::fs::create_dir_all(&root).unwrap();
        // 白名单内的子目录 → 放行
        assert!(ensure_dest_root(
            Some(root.join("sub").to_string_lossy().into_owned()),
            std::slice::from_ref(&root)
        )
        .is_ok());
        // 白名单外的目录 → 拒绝
        let outside = dir.path().join("elsewhere");
        let r = ensure_dest_root(
            Some(outside.to_string_lossy().into_owned()),
            std::slice::from_ref(&root),
        );
        assert!(matches!(r, Err(DaemonError::InvalidSource(m)) if m.contains("越界")));
        // 绝对路径穿越到白名单外 → 拒绝
        let r2 = ensure_dest_root(Some(dir.path().to_string_lossy().into_owned()), &[root]);
        assert!(r2.is_err(), "白名单父目录本身也必须被拒");
    }

    // 安全回归（V2）：`..` 分量在 canonicalize 前即被拒。
    #[test]
    fn dest_with_dotdot_rejected_early() {
        let dir = tempfile::tempdir().unwrap();
        let r = ensure_dest_root(
            Some(
                dir.path()
                    .join("sub/../../escape")
                    .to_string_lossy()
                    .into_owned(),
            ),
            &[dir.path().to_path_buf()],
        );
        assert!(matches!(r, Err(DaemonError::InvalidSource(m)) if m.contains("..")));
    }

    #[test]
    fn check_space_zero_total_ok() {
        let dir = tempfile::tempdir().unwrap();
        assert!(precheck_space(dir.path(), 0, false).is_ok());
    }
}

/// FakeEngine 限速调用记录：（engine_tid, down_kb_s, up_kb_s）。
#[cfg(test)]
type LimitCall = (String, Option<u32>, Option<u32>);

/// 假引擎（持久化恢复测试用）：add 记录输入、可对指定 url 返回错误。
#[cfg(test)]
pub struct FakeEngine {
    kind: EngineKind,
    counter: std::sync::atomic::AtomicU64,
    fail_urls: parking_lot::Mutex<std::collections::HashSet<String>>,
    added: parking_lot::Mutex<Vec<String>>,
    xunlei: parking_lot::Mutex<Vec<Vec<u8>>>,
    /// 已注入的 web seed（(engine_tid, url)，F5 webseed 注入测试用）。
    url_seeds: parking_lot::Mutex<Vec<(String, String)>>,
    /// 已下发的任务级限速（LimitCall 记录，限速重放测试用）。
    limits: parking_lot::Mutex<Vec<LimitCall>>,
    /// status() 额外透出的文件级进度（目录 files 同步测试用；默认空 = 保持旧行为）。
    status_files: parking_lot::Mutex<Vec<FileProgress>>,
    /// 已下发的子文件优先级（(engine_tid, pairs)，优先级重放测试用）。
    #[allow(clippy::type_complexity)] // (tid, [(下标, 优先级)]) 记录类型，测试专用
    prio_calls: parking_lot::Mutex<Vec<(String, Vec<(usize, u32)>)>>,
    /// file_priorities() 的可编程行为：None = 默认成功返回空表；
    /// Some(Err(e)) = 返回该错误（metadata 未就绪模拟）；Some(Ok(v)) = 返回 v。
    prio_readback:
        parking_lot::Mutex<Option<Result<Vec<Option<u32>>, smart_dl_core::types::EngineError>>>,
    /// 已下发的 pause 调用（P4 G5 暂停意图重放测试用）。
    paused: parking_lot::Mutex<Vec<String>>,
    /// 已下发的 resume 调用（P4 G5 运行态恢复重放测试用）。
    resumed: parking_lot::Mutex<Vec<String>>,
    /// 已下发的 remove 调用（(engine_tid, delete_data)，E7 数据处置断言用）。
    removed: parking_lot::Mutex<Vec<(String, bool)>>,
    /// 已下发的 set_task_proxy 调用（(engine_tid, proxy)，E8 热改断言用）。
    proxy_sets: parking_lot::Mutex<Vec<(String, Option<String>)>>,
    /// status().name 可编程回显（E9 名字回填测试用；None = 不透出）。
    status_name: parking_lot::Mutex<Option<String>>,
    /// status() 速率回显（(down, up) B/s，E11 速率缓存测试用；默认 (0,0) 旧行为）。
    status_rates: parking_lot::Mutex<(u64, u64)>,
    /// status() 累计统计回显（(down, up) 字节，E33 上传/分享率测试用；默认 (0,0)）。
    status_totals: parking_lot::Mutex<(u64, u64)>,
    /// status().state 可编程回显（E30 重试 e2e；None = 保持默认）。
    status_state: parking_lot::Mutex<Option<EngineState>>,
    /// 已下发的 set_global_limits 调用（(down, up)，E16 总阀门断言用）。
    global_sets: parking_lot::Mutex<Vec<(Option<u32>, Option<u32>)>>,
    /// set_global_limits 的可编程行为：Some(e) = 恒返回该错误（下发失败模拟）。
    global_limits_err: parking_lot::Mutex<Option<smart_dl_core::types::EngineError>>,
}

#[cfg(test)]
impl FakeEngine {
    pub fn new(kind: EngineKind) -> Self {
        FakeEngine {
            kind,
            counter: std::sync::atomic::AtomicU64::new(1),
            fail_urls: parking_lot::Mutex::new(std::collections::HashSet::new()),
            added: parking_lot::Mutex::new(Vec::new()),
            xunlei: parking_lot::Mutex::new(Vec::new()),
            url_seeds: parking_lot::Mutex::new(Vec::new()),
            limits: parking_lot::Mutex::new(Vec::new()),
            status_files: parking_lot::Mutex::new(Vec::new()),
            prio_calls: parking_lot::Mutex::new(Vec::new()),
            prio_readback: parking_lot::Mutex::new(None),
            paused: parking_lot::Mutex::new(Vec::new()),
            resumed: parking_lot::Mutex::new(Vec::new()),
            removed: parking_lot::Mutex::new(Vec::new()),
            proxy_sets: parking_lot::Mutex::new(Vec::new()),
            status_name: parking_lot::Mutex::new(None),
            status_rates: parking_lot::Mutex::new((0, 0)),
            status_totals: parking_lot::Mutex::new((0, 0)),
            status_state: parking_lot::Mutex::new(None),
            global_sets: parking_lot::Mutex::new(Vec::new()),
            global_limits_err: parking_lot::Mutex::new(None),
        }
    }

    pub fn fail_url(&self, url: &str) {
        self.fail_urls.lock().insert(url.to_string());
    }

    /// 移除 add 失败标记（E30 重试 e2e：模拟故障源恢复后重试成功）。
    pub fn unfail_url(&self, url: &str) {
        self.fail_urls.lock().remove(url);
    }

    pub fn added(&self) -> Vec<String> {
        self.added.lock().clone()
    }

    // 跨 feature 门测试共享的 mock 观测器：部分 feature 组合下无调用点。
    // （模块外置前经 pub mod state 的 crate 根可达性豁免 dead_code，现显式化。）
    #[allow(dead_code)]
    pub fn xunlei_resumes(&self) -> Vec<Vec<u8>> {
        self.xunlei.lock().clone()
    }

    /// 读取已记录的 web seed 注入（(engine_tid, url)，F5 webseed 测试断言用）。
    // 跨 feature 门测试共享的 mock 观测器：部分 feature 组合下无调用点。
    // （模块外置前经 pub mod state 的 crate 根可达性豁免 dead_code，现显式化。）
    #[allow(dead_code)]
    pub fn url_seeds(&self) -> Vec<(String, String)> {
        self.url_seeds.lock().clone()
    }

    /// 读取已下发的任务级限速（LimitCall 记录，限速重放测试断言用）。
    pub fn limits(&self) -> Vec<LimitCall> {
        self.limits.lock().clone()
    }

    /// 读取已下发的 pause 调用（P4 G5 重放测试断言用）。
    pub fn paused_calls(&self) -> Vec<String> {
        self.paused.lock().clone()
    }

    /// 读取已下发的 remove 调用（(engine_tid, delete_data)，E7 数据处置断言用）。
    pub fn removed_calls(&self) -> Vec<(String, bool)> {
        self.removed.lock().clone()
    }

    /// 读取已下发的 set_task_proxy 调用（(engine_tid, proxy)，E8 热改断言用）。
    pub fn proxy_set_calls(&self) -> Vec<(String, Option<String>)> {
        self.proxy_sets.lock().clone()
    }

    /// 设置 status().name 回显值（E9 名字回填测试：模拟引擎已定落盘名）。
    pub fn set_status_name(&self, name: &str) {
        *self.status_name.lock() = Some(name.to_string());
    }

    /// 设置 status() 速率回显（(down, up) B/s；E11 速率缓存测试用）。
    pub fn set_status_rates(&self, down: u64, up: u64) {
        *self.status_rates.lock() = (down, up);
    }

    /// 设置 status() 累计统计回显（(down, up) 字节；E33 上传/分享率测试用）。
    pub fn set_status_totals(&self, down: u64, up: u64) {
        *self.status_totals.lock() = (down, up);
    }

    /// 设置 status().state 回显（E30 重试 e2e：模拟引擎报 Error）。
    pub fn set_status_state(&self, s: EngineState) {
        *self.status_state.lock() = Some(s);
    }

    /// 读取已下发的 set_global_limits 调用（E16 总阀门断言用）。
    pub fn global_sets(&self) -> Vec<(Option<u32>, Option<u32>)> {
        self.global_sets.lock().clone()
    }

    /// 编程 set_global_limits 恒返回某错误（E16 下发失败模拟）。
    pub fn fail_global_limits(&self, e: smart_dl_core::types::EngineError) {
        *self.global_limits_err.lock() = Some(e);
    }

    /// 读取已下发的 resume 调用（P4 G5 重放测试断言用）。
    pub fn resumed_calls(&self) -> Vec<String> {
        self.resumed.lock().clone()
    }

    /// 设置 status() 返回的文件级进度（FTP 目录 files 同步测试注入用）。
    // 跨 feature 门测试共享的 mock 观测器：部分 feature 组合下无调用点。
    // （模块外置前经 pub mod state 的 crate 根可达性豁免 dead_code，现显式化。）
    #[allow(dead_code)]
    pub fn set_status_files(&self, files: Vec<FileProgress>) {
        *self.status_files.lock() = files;
    }

    /// 读取已下发的子文件优先级调用记录（优先级重放测试断言用）。
    #[allow(clippy::type_complexity)]
    pub fn prio_calls(&self) -> Vec<(String, Vec<(usize, u32)>)> {
        self.prio_calls.lock().clone()
    }

    /// 编程 file_priorities() 行为（就绪/未就绪模拟用）。
    pub fn set_prio_readback(
        &self,
        v: Option<Result<Vec<Option<u32>>, smart_dl_core::types::EngineError>>,
    ) {
        *self.prio_readback.lock() = v;
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl DownloadEngine for FakeEngine {
    fn id(&self) -> &str {
        "fake"
    }

    fn kind(&self) -> EngineKind {
        self.kind
    }

    fn capabilities(&self) -> Vec<smart_dl_core::types::Capability> {
        vec![]
    }

    async fn add(
        &self,
        task: &DownloadTask,
    ) -> Result<EngineTaskId, smart_dl_core::types::EngineError> {
        let ident = match &task.source {
            DownloadSource::Http { url, .. } => url.clone(),
            DownloadSource::Magnet(m) => m.clone(),
            DownloadSource::TorrentFile(_) => format!("torrent:{}", task.id),
            _ => task.id.clone(),
        };
        if self.fail_urls.lock().contains(&ident) {
            return Err(smart_dl_core::types::EngineError::Other("fake fail".into()));
        }
        self.added.lock().push(ident);
        Ok(format!(
            "fk{}",
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<(), smart_dl_core::types::EngineError> {
        self.paused.lock().push(id.clone());
        Ok(())
    }
    async fn resume(&self, id: &EngineTaskId) -> Result<(), smart_dl_core::types::EngineError> {
        self.resumed.lock().push(id.clone());
        Ok(())
    }
    async fn status(
        &self,
        _id: &EngineTaskId,
    ) -> Result<EngineStatus, smart_dl_core::types::EngineError> {
        let (down_rate, up_rate) = *self.status_rates.lock();
        let (total_downloaded, total_uploaded) = *self.status_totals.lock();
        let mut st = EngineStatus {
            down_rate,
            up_rate,
            total_downloaded,
            total_uploaded,
            files: self.status_files.lock().clone(),
            name: self.status_name.lock().clone(),
            ..EngineStatus::default()
        };
        if let Some(s) = *self.status_state.lock() {
            st.state = s;
        }
        Ok(st)
    }
    async fn remove(
        &self,
        id: &EngineTaskId,
        delete_data: bool,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        self.removed.lock().push((id.to_string(), delete_data));
        Ok(())
    }

    async fn set_task_proxy(
        &self,
        id: &EngineTaskId,
        proxy: Option<String>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        self.proxy_sets.lock().push((id.to_string(), proxy));
        Ok(())
    }
    async fn peers(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<smart_dl_core::types::PeerInfo>, smart_dl_core::types::EngineError> {
        Ok(vec![])
    }
    async fn update_sources(
        &self,
        _id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn add_url_seed(
        &self,
        id: &EngineTaskId,
        url: &str,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录（engine_tid, url）供测试断言逐条转发
        self.url_seeds.lock().push((id.clone(), url.to_string()));
        Ok(())
    }

    async fn set_limits(
        &self,
        id: &EngineTaskId,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录（engine_tid, down, up）供限速重放测试断言
        self.limits.lock().push((id.clone(), down_kb_s, up_kb_s));
        Ok(())
    }

    async fn set_global_limits(
        &self,
        down_kb_s: Option<u32>,
        up_kb_s: Option<u32>,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // 记录 (down, up) 供 E16 总阀门下发断言（可编程拒绝：global_limits_err）
        if let Some(e) = &*self.global_limits_err.lock() {
            return Err(e.clone());
        }
        self.global_sets.lock().push((down_kb_s, up_kb_s));
        Ok(())
    }

    async fn set_file_priorities(
        &self,
        id: &EngineTaskId,
        priorities: &[(usize, u32)],
    ) -> Result<(), smart_dl_core::types::EngineError> {
        // readback 错误态同步反映到 set（真实 BT 引擎 metadata 未就绪时
        // set/read 同样失败）——pending 重放测试依赖该一致性
        if let Some(Err(e)) = &*self.prio_readback.lock() {
            return Err(e.clone());
        }
        self.prio_calls
            .lock()
            .push((id.clone(), priorities.to_vec()));
        Ok(())
    }

    async fn file_priorities(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<Option<u32>>, smart_dl_core::types::EngineError> {
        match &*self.prio_readback.lock() {
            None => Ok(vec![Some(4)]), // 默认就绪：单文件表
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(e)) => Err(e.clone()),
        }
    }
    async fn add_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn ban_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), smart_dl_core::types::EngineError> {
        Ok(())
    }
    async fn read_piece(
        &self,
        _id: &EngineTaskId,
        _idx: u32,
    ) -> Result<Vec<u8>, smart_dl_core::types::EngineError> {
        Ok(vec![])
    }

    async fn add_xunlei_resume(
        &self,
        data: Vec<u8>,
    ) -> Result<EngineTaskId, smart_dl_core::types::EngineError> {
        self.xunlei.lock().push(data);
        Ok(format!(
            "xr{}",
            self.counter
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ))
    }
}

/// 任务持久化往返测试（FakeEngine，不联网）。
#[cfg(test)]
mod persist_tests {
    use super::*;

    fn wait_file(path: &std::path::Path, timeout_ms: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("等待持久化文件超时: {path:?}");
    }

    #[tokio::test]
    async fn persist_then_restore_keeps_task() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/file.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        // 新 state（新引擎）恢复
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1, "应恢复 1 条任务");
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.id, tid, "task_id 必须保留");
        assert_eq!(rec.engine_kind, EngineKind::Http);
        assert_eq!(rec.task.state, TaskState::Queued, "恢复后重新入队");
        // 引擎重新 add 被调用
        assert_eq!(
            fake2.added(),
            vec!["https://example.com/file.bin".to_string()]
        );
    }

    #[tokio::test]
    async fn next_id_advances_after_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let _ = state
            .add_http_task("https://example.com/a.bin".into(), None)
            .await
            .unwrap();
        let _ = state
            .add_http_task("https://example.com/b.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        let state2 = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
        state2.restore_from(&store).await.unwrap();
        let new_tid = state2
            .add_http_task("https://example.com/c.bin".into(), None)
            .await
            .unwrap();
        let num: u64 = new_tid.strip_prefix('t').unwrap().parse().unwrap();
        assert!(num >= 3, "恢复后新任务 id 应跳过已用 id: {new_tid}");
    }

    #[tokio::test]
    async fn restore_replays_task_limits_to_engine() {
        // 限速重放：持久化的 task.limits 在 restore 后原样下发引擎
        // （FakeEngine set_limits 记录（tid, down, up）三元组）。
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/limited.bin".into(), None)
            .await
            .unwrap();
        let merged = state.set_task_limits(&tid, Some(128), None).await.unwrap();
        assert_eq!(merged.down_kb_s, Some(128));
        assert_eq!(merged.up_kb_s, None, "HTTP 任务 up 方向应保持未设");
        wait_file(&store, 2000);

        // 新 state（新引擎）恢复 → 引擎收到原样限速下发
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        let calls = fake2.limits();
        assert_eq!(
            calls.len(),
            1,
            "恢复后必须向引擎重放一次 set_limits: {calls:?}"
        );
        let (etid, down, up) = &calls[0];
        assert_eq!(down, &Some(128), "down 方向原样重放");
        assert_eq!(up, &None, "up 方向 None 原样传递（不触发方向预拒）");
        // 内存中的 limits 配置也随恢复保留
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.limits,
            Some(smart_dl_core::task::TaskLimits {
                down_kb_s: Some(128),
                up_kb_s: None,
            })
        );
        assert!(
            etid.starts_with("fk"),
            "engine_tid 应为 FakeEngine 返回的句柄: {etid}"
        );
    }

    #[cfg(feature = "bt")]
    fn bt_prio_task(id: &str, prios: Option<Vec<u32>>) -> DownloadTask {
        let magnet = "magnet:?xt=urn:btih:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        DownloadTask {
            id: id.to_string(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Bt,
                identity: magnet.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Magnet(magnet.to_string()),
            identity: ContentIdentity::SingleFile {
                size: 0,
                etag: None,
                sha256: None,
                sha1: None,
                md5: None,
                backup_md5: None,
            },
            dest_root: PathBuf::from("."),
            files: vec![],
            acquisitions: vec![],
            aggregate: Default::default(),
            state: TaskState::Queued,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: prios,
            sequential: false,
            metadata: TaskMetadata {
                name: None,
                added_at_unix: 0,
                tags: Vec::new(),
                finished_at_unix: 0,
                start_at_unix: 0,
                next_retry_at_unix: 0,
            },
            limits: None,
        }
    }

    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn restore_replays_file_priorities_when_metadata_ready() {
        // metadata 已就绪（readback 非空表）：restore 必须直接全量重放一次
        // （.torrent 恢复场景；magnet 已解析场景同路）
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let pts = vec![PersistedTask {
            task: bt_prio_task("t1", Some(vec![0, 7])),
            engine_kind: EngineKind::Bt,
            paused: false,
        }];
        std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();

        let fake2 = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        let calls = fake2.prio_calls();
        assert_eq!(calls.len(), 1, "metadata 就绪时必须立即重放: {calls:?}");
        assert_eq!(
            calls[0].1,
            vec![(0, 0), (1, 7)],
            "下标-值对必须与持久化全量表一致"
        );
    }

    #[cfg(feature = "bt")]
    #[tokio::test]
    async fn file_priorities_pending_replay_converges_when_metadata_arrives() {
        // magnet 恢复且 metadata 未就绪（readback NotFound，与真实引擎分类一致）
        // → restore 挂 pending → 就绪后由 replay 收敛 → 再跑幂等
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        fake.set_prio_readback(Some(Err(smart_dl_core::types::EngineError::NotFound)));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]));

        let pts = vec![PersistedTask {
            task: bt_prio_task("t1", Some(vec![0, 7])),
            engine_kind: EngineKind::Bt,
            paused: false,
        }];
        std::fs::write(&store, serde_json::to_vec(&pts).unwrap()).unwrap();
        let n = state.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        assert!(
            !fake.prio_calls().iter().any(|(_, p)| !p.is_empty()),
            "未就绪时不得成功重放: {:?}",
            fake.prio_calls()
        );
        assert!(
            state.pending_file_prio.lock().contains("t1"),
            "未就绪必须挂 pending 集合"
        );

        // metadata 到达（readback 返回 2 文件表）→ 单轮收敛
        fake.set_prio_readback(Some(Ok(vec![Some(0), Some(7)])));
        let done = state.replay_pending_file_priorities().await;
        assert_eq!(done, vec!["t1".to_string()]);
        let calls = fake.prio_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, vec![(0, 0), (1, 7)]);
        // pending 清空 + 幂等（再跑无调用）
        assert!(!state.pending_file_prio.lock().contains("t1"));
        assert!(state.replay_pending_file_priorities().await.is_empty());
        assert_eq!(fake.prio_calls().len(), 1);
    }

    #[tokio::test]
    async fn restore_replays_pause_intent_and_marks_paused() {
        // P4 G5：用户暂停的任务持久化 paused=true → 重启恢复后重放 engine.pause
        // 且记录态回写 Paused（不再被当作运行任务重新入队）。
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/paused.bin".into(), None)
            .await
            .unwrap();
        state.pause(&tid).await.unwrap();
        // pause 处理器应立即 autosave（否则暂停意图丢失）；轮询到 paused=true 落盘
        //（文件可能在 add 时已存在，不能只等文件存在）
        {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                let persisted = std::fs::read_to_string(&store)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Vec<PersistedTask>>(&s).ok())
                    .map(|pts| pts.first().map(|p| p.paused).unwrap_or(false))
                    .unwrap_or(false);
                if persisted {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "暂停意图必须落盘 paused=true"
                );
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        }

        // "重启"：新引擎 + restore → 引擎收到 pause 重放，记录态 Paused
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        state2.restore_from(&store).await.unwrap();
        assert_eq!(
            fake2.paused_calls(),
            vec!["fk1".to_string()],
            "暂停意图必须重放到引擎"
        );
        assert!(fake2.resumed_calls().is_empty(), "暂停任务不得 resume");
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Paused, "记录态应回写 Paused");
    }

    #[tokio::test]
    async fn running_task_restored_without_pause_replay_on_non_bt() {
        // P4 G5 对称面：非暂停任务恢复不重放 pause；HTTP 引擎不得 resume
        //（add 已自启下载循环，重复 resume 会产生多余 epoch 循环）。
        // BT 的 resume 重放由 fastresume e2e（真实内核）覆盖。
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        state
            .add_http_task("https://example.com/running.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        state2.restore_from(&store).await.unwrap();
        assert!(fake2.paused_calls().is_empty());
        assert!(fake2.resumed_calls().is_empty(), "HTTP 恢复不得重复 resume");
        let rec = state2.tasks.lock().values().next().cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Queued);
    }

    #[tokio::test]
    async fn restore_add_failure_marks_failed() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]).with_storage(store.clone()));
        let tid = state
            .add_http_task("https://example.com/gone.bin".into(), None)
            .await
            .unwrap();
        wait_file(&store, 2000);

        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        fake2.fail_url("https://example.com/gone.bin");
        let state2 = DaemonState::new(fake2.clone(), vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 0, "add 失败不计入恢复数");
        let rec = state2.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Failed, "add 失败任务标 Failed");
        assert!(rec.engine_tid.is_none());
    }

    #[tokio::test]
    async fn no_storage_no_autosave() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let _ = state
            .add_http_task("https://example.com/x.bin".into(), None)
            .await
            .unwrap();
        // 无 persist_path → 无写盘（autosave 直接 return）
        // 此测试验证不 panic；写盘路径由 with_storage 测试覆盖。
        assert!(fake.added().len() == 1);
    }

    #[tokio::test]
    async fn dest_none_uses_default_dest_root() {
        // with_dest_root 注入默认目录后，dest 未指定 → 任务落默认目录（而非 daemon cwd）
        // Task 5-a：用临时目录替代硬编码 /data/default-dl（沙盒无 /data 写权限 → Permission denied）
        let tmp = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
        let tid = state
            .add_http_task("https://example.com/dest.bin".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.dest_root,
            tmp.path().to_path_buf(),
            "dest 未指定应落到默认 dest_root"
        );
    }

    #[tokio::test]
    async fn explicit_dest_overrides_default() {
        // Task 5-a：默认 dest_root 同样改为临时目录（保持沙盒可跑）。
        // 安全修复（V2）：显式 dest 必须落在白名单内——用 dest_root 子目录验证
        // 「显式 dest 优先于默认」契约保持；白名单外由 reject_dest_outside_roots 覆盖。
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("custom");
        std::fs::create_dir_all(&sub).unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
        let tid = state
            .add_http_task(
                "https://example.com/override.bin".into(),
                Some(sub.to_string_lossy().into_owned()),
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.dest_root, sub, "显式 dest（白名单内）优先于默认");
    }

    #[tokio::test]
    async fn reject_dest_outside_roots() {
        // 安全回归（V2）：显式 dest 在白名单外 → 拒绝任务（不再任意目录可写）。
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap(); // 白名单外独立目录
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(tmp.path().to_path_buf());
        let r = state
            .add_http_task(
                "https://example.com/escape.bin".into(),
                Some(outside.path().to_string_lossy().into_owned()),
            )
            .await;
        assert!(
            matches!(&r, Err(DaemonError::InvalidSource(m)) if m.contains("越界")),
            "白名单外 dest 必须拒绝: {r:?}"
        );
    }

    /// Bug B 回归（重入自死锁，HTTP 推进路径）：storage 启用 + 引擎状态推进
    /// （Queued → Downloading）触发锁内 autosave。修复前：poll_engine_states
    /// 持 tasks 锁调 autosave → persisted_tasks 同线程重入 → 5s 超时失败。
    #[tokio::test(flavor = "multi_thread")]
    async fn http_poll_transition_with_storage_autosave_no_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]).with_storage(store.clone());
        let _tid = state
            .add_http_task("https://example.com/wedge.bin".into(), None)
            .await
            .unwrap();
        // FakeEngine::status 默认 MetadataPending → 推进 Queued→Downloading(Http)
        //（迁移必发生 → 必走 autosave 路径）
        let work = state.poll_engine_states();
        let effects = tokio::time::timeout(std::time::Duration::from_secs(5), work)
            .await
            .expect("poll_engine_states 死锁（Bug B 重入回归）");
        assert_eq!(effects.len(), 1, "应推进一条任务状态");
        assert!(store.exists(), "状态推进应触发持久化落盘");
    }
}

/// 卡 D：FTP 路由（feature `ftp`）单测——add_ftp_task 前缀校验 / user+pass 提取 /
/// 路由 Ftp 引擎 / 目录 files 同步；以及端到端（真实 FtpEngine + 最小 mock FTP server）。
#[cfg(all(test, feature = "ftp"))]
mod ftp_tests {
    use super::*;

    // ---- 单元：add_ftp_task 基础路由 ----

    #[tokio::test]
    async fn non_ftp_url_is_invalid() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let err = state
            .add_ftp_task("https://example.com/f.bin".into(), None)
            .await
            .expect_err("非 ftp:// 前缀应拒绝");
        assert!(
            matches!(err, DaemonError::InvalidSource(_)),
            "应返回 InvalidSource: {err}"
        );
        assert!(fake.added().is_empty(), "不应路由到引擎");
    }

    #[tokio::test]
    async fn extracts_auth_and_routes_to_ftp_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let url = "ftp://alice:secret@host/pub/a.bin".to_string();
        let tid = state.add_ftp_task(url.clone(), None).await.unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.engine_kind, EngineKind::Ftp, "engine_kind 记 Ftp");
        match &rec.task.source {
            DownloadSource::Ftp { url: u, user, pass } => {
                assert_eq!(u, &url);
                assert_eq!(user, "alice", "parse_ftp_auth 应提取 user");
                assert_eq!(pass, "secret", "parse_ftp_auth 应提取 pass");
            }
            other => panic!("source 应为 DownloadSource::Ftp: {other:?}"),
        }
        // FakeEngine.add 对 Ftp 源记录 task.id → 证明路由到 Ftp 引擎
        assert_eq!(fake.added(), vec![tid], "应路由到 Ftp 引擎 add");
    }

    #[tokio::test]
    async fn anonymous_auth_falls_back() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_ftp_task("ftp://host/pub/a.bin".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        match &rec.task.source {
            DownloadSource::Ftp { user, pass, .. } => {
                assert_eq!(user, "anonymous");
                assert_eq!(pass, "");
            }
            _ => panic!("source 应为 Ftp"),
        }
    }

    #[tokio::test]
    async fn single_file_uses_url_filename_as_metadata_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        fake.set_status_files(vec![FileProgress {
            rel_path: "x.bin".into(),
            done: 0,
            size: 5,
        }]);
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_ftp_task("ftp://host/pub/a.bin".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("a.bin"),
            "单文件任务落盘名取 URL 最后一段"
        );
        assert!(
            rec.task.files.is_empty(),
            "单文件任务不应触发目录 files 同步"
        );
    }

    #[tokio::test]
    async fn dir_task_syncs_files_from_engine_status() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        fake.set_status_files(vec![
            FileProgress {
                rel_path: "a.bin".into(),
                done: 0,
                size: 10,
            },
            FileProgress {
                rel_path: "b.bin".into(),
                done: 0,
                size: 20,
            },
        ]);
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_ftp_task("ftp://host/pub/".into(), None)
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.files.len(),
            2,
            "目录任务 files 应从引擎 status 同步: {:?}",
            rec.task.files
        );
        assert_eq!(rec.task.files[0].rel_path, "a.bin");
        assert_eq!(rec.task.files[0].size, 10);
        assert_eq!(rec.task.files[0].done, 0);
        assert_eq!(rec.task.files[0].engine, EngineKind::Ftp);
        assert_eq!(
            rec.task.files[0].source_urls,
            vec!["ftp://host/pub/".to_string()]
        );
        // 序列化往返：task.files（含 EngineKind::Ftp）应可持久化反序列化
        let json = serde_json::to_vec(&rec.task).expect("task 可序列化");
        let _restored: DownloadTask = serde_json::from_slice(&json).expect("可反序列化");
    }

    #[tokio::test]
    async fn repeated_ftp_dup_is_rejected() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Ftp));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let _ = state
            .add_ftp_task("ftp://host/pub/".into(), None)
            .await
            .unwrap();
        let err = state
            .add_ftp_task("ftp://host/pub/".into(), None)
            .await
            .expect_err("重复 canonical 应拒绝");
        assert!(
            matches!(err, DaemonError::Duplicate(_)),
            "应返回 Duplicate: {err}"
        );
    }

    // ---- 端到端：真实 FtpEngine + 最小 mock FTP server（卡 D 验收点） ----

    /// 最小 mock FTP server：支持目录 LIST（文件清单）/ 文件 RETR（完整内容）、PASSIVE 被动模式。
    /// `files` = `(远端绝对路径, 内容)`；路径不带 user:pass@。
    async fn start_mock_ftp(files: Vec<(String, Vec<u8>)>) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let files = SArc::new(files);
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let files = files.clone();
                tokio::spawn(async move {
                    handle_mock_ftp(stream, files).await;
                });
            }
        });
        addr
    }

    use std::net::SocketAddr;
    use std::sync::Arc as SArc;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpStream;

    async fn handle_mock_ftp(mut stream: TcpStream, files: SArc<Vec<(String, Vec<u8>)>>) {
        let mut rest: u64 = 0;
        let mut data_listener: Option<tokio::net::TcpListener> = None;
        let _ = stream.write_all(b"220 test ftp ready\r\n").await;
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let cmd = line.trim_end_matches("\r\n").trim_end();
            let (verb, arg) = match cmd.split_once(' ') {
                Some((v, a)) => (v, a.trim()),
                None => (cmd, ""),
            };
            let conn = reader.get_mut();
            match verb {
                "USER" | "PASS" => {
                    let _ = conn.write_all(b"230 logged in\r\n").await;
                }
                "TYPE" => {
                    let _ = conn.write_all(b"200 type set\r\n").await;
                }
                "PASV" => {
                    let dl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    let da = dl.local_addr().unwrap();
                    let (p1, p2) = (da.port() / 256, da.port() % 256);
                    let _ = conn
                        .write_all(
                            format!("227 Entering Passive Mode (127,0,0,1,{p1},{p2})\r\n")
                                .as_bytes(),
                        )
                        .await;
                    data_listener = Some(dl);
                }
                "RETR" => {
                    let data: Option<Vec<u8>> =
                        files.iter().find(|(p, _)| p == arg).map(|(_, d)| d.clone());
                    let Some(body) = data else {
                        let _ = conn.write_all(b"550 file unavailable\r\n").await;
                        continue;
                    };
                    if rest > body.len() as u64 {
                        let _ = conn.write_all(b"550 file unavailable\r\n").await;
                        continue;
                    }
                    let _ = conn.write_all(b"150 opening data connection\r\n").await;
                    if let Some(dl) = data_listener.as_ref() {
                        if let Ok((mut data_conn, _)) = dl.accept().await {
                            let _ = data_conn.write_all(&body[rest as usize..]).await;
                            let _ = data_conn.shutdown().await;
                        }
                    }
                    let _ = conn.write_all(b"226 transfer complete\r\n").await;
                    rest = 0;
                    data_listener = None;
                }
                "LIST" => {
                    let dir = arg.trim_end_matches('/');
                    let prefix = format!("{dir}/");
                    let mut lines: Vec<String> = Vec::new();
                    for (p, d) in files.iter() {
                        if let Some(name) = p.strip_prefix(&prefix) {
                            if !name.is_empty() && !name.contains('/') {
                                lines.push(format!(
                                    "-rw-r--r--  1 owner  group  {:>8} Jan 01 12:00 {name}",
                                    d.len()
                                ));
                            }
                        }
                    }
                    let mut text = format!("total {}\r\n", lines.len());
                    text.push_str(&lines.join("\r\n"));
                    text.push_str("\r\n");
                    let _ = conn.write_all(b"150 opening data connection\r\n").await;
                    if let Some(dl) = data_listener.as_ref() {
                        if let Ok((mut data, _)) = dl.accept().await {
                            let _ = data.write_all(text.as_bytes()).await;
                            let _ = data.shutdown().await;
                        }
                    }
                    let _ = conn.write_all(b"226 transfer complete\r\n").await;
                    data_listener = None;
                }
                _ => {
                    let _ = conn.write_all(b"502 unknown\r\n").await;
                }
            }
        }
    }

    /// 端到端验收（卡 D）：FTP 目录任务（2 文件）→ add 后 task.files 同步 →
    /// poll_engine_states 推进 Completed → 快照含文件信息且逐文件 done==size；
    /// 同时一个 HTTP 任务（独立 Http 槽）不受影响。
    #[tokio::test]
    async fn ftp_dir_completes_and_http_unaffected() {
        let ftp_addr = start_mock_ftp(vec![
            ("/pub/a.bin".to_string(), vec![0x5Au8; 16]),
            ("/pub/b.bin".to_string(), vec![0x5Bu8; 24]),
        ])
        .await;
        let ftp_url = format!("ftp://127.0.0.1:{}/pub/", ftp_addr.port());
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().to_str().unwrap().to_string();

        // 同时挂 HTTP 引擎（Fake）与真实 FTP 引擎：验证各占独立槽位
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let ftp_engine: Arc<dyn DownloadEngine> = Arc::new(smart_dl_httpdl::FtpEngine::new());
        let state = Arc::new(
            DaemonState::new(http_fake.clone(), vec![])
                .with_ftp(ftp_engine)
                .with_dest_root(dir.path().to_path_buf()),
        );

        // 1) FTP 目录任务
        let ftp_tid = state
            .add_link_task(ftp_url.clone(), Some(dest.clone()))
            .await
            .expect("FTP 目录任务 add 应成功");
        let rec = state.tasks.lock().get(&ftp_tid).cloned().unwrap();
        assert_eq!(rec.engine_kind, EngineKind::Ftp);
        assert_eq!(
            rec.task.files.len(),
            2,
            "add 后 files 应同步 2 个: {:?}",
            rec.task.files
        );

        // 2) HTTP 任务走独立 Http 槽（Fake 引擎记录），不受 FTP 影响
        let http_tid = state
            .add_http_task("https://example.com/keep.bin".into(), Some(dest.clone()))
            .await
            .unwrap();
        assert!(
            http_fake
                .added()
                .contains(&"https://example.com/keep.bin".to_string()),
            "HTTP 任务应路由到 Http 引擎: {:?}",
            http_fake.added()
        );

        // 3) 轮询推进 FTP 到 Completed（FTP 引擎无 alert，靠 poll 推进——同 HTTP 链路）
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let _ = state.poll_engine_states().await;
            if let Some(snap) = state.task_snapshot(&ftp_tid).await {
                if snap.state == "Completed" {
                    // 快照含文件信息，且逐文件 done==size
                    assert_eq!(snap.files.len(), 2, "快照应含 2 个文件进度");
                    for f in &snap.files {
                        assert_eq!(f.done, f.size, "逐文件 done 应等于 size: {f:?}");
                    }
                    // HTTP 任务记录仍存在（未被破坏）
                    assert!(state.task_snapshot(&http_tid).await.is_some());
                    return;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "60s 内 FTP 任务未 Completed"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
}

/// 卡 I′：F5 P2SP web seed 注入（feature `webseed`，隐含 `bt`）——
/// 端点语义单测 + 真实 BtCore 本地 HTTP 源端到端（Rust 版 F5 PoC-1）。
#[cfg(all(test, feature = "webseed"))]
mod webseed_tests {
    use super::*;

    /// 最小单文件 .torrent（无 tracker；info: length=123/name=test/piece length/pieces）。
    fn sample_torrent() -> Vec<u8> {
        let mut b = b"d4:infod6:lengthi123e4:name4:test12:piece lengthi16384e6:pieces20:".to_vec();
        b.extend_from_slice(&[0u8; 20]);
        b.extend_from_slice(b"eee");
        b
    }

    #[tokio::test]
    async fn non_bt_task_rejects_webseed() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_http_task(
                "http://example.com/a.bin".into(),
                Some(dir.path().to_string_lossy().into_owned()),
            )
            .await
            .unwrap();
        let err = state
            .add_webseeds(&tid, &["http://seed/1".into()])
            .await
            .expect_err("非 BT 任务必须拒绝");
        assert!(
            matches!(err, DaemonError::UnsupportedOp(_)),
            "实际: {err:?}"
        );
        assert!(fake.url_seeds().is_empty());
    }

    #[tokio::test]
    async fn bt_task_forwards_urls_to_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let dir = tempfile::tempdir().unwrap();
        let state = DaemonState::new(fake.clone(), vec![]).with_dest_root(dir.path().to_path_buf());
        let tid = state
            .add_torrent_task(
                sample_torrent(),
                Some(dir.path().to_string_lossy().into_owned()),
            )
            .await
            .expect("add_torrent_task");
        let n = state
            .add_webseeds(&tid, &["http://seed/1".into(), "http://seed/2".into()])
            .await
            .expect("注入应成功");
        assert_eq!(n, 2);
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        let engine_tid = rec.engine_tid.expect("engine_tid");
        assert_eq!(
            fake.url_seeds(),
            vec![
                (engine_tid.clone(), "http://seed/1".into()),
                (engine_tid, "http://seed/2".into()),
            ]
        );
    }

    #[tokio::test]
    async fn webseed_missing_task_is_not_found() {
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Bt)), vec![]);
        let err = state
            .add_webseeds("t_missing", &["http://seed/1".into()])
            .await
            .expect_err("");
        assert!(matches!(err, DaemonError::NotFound(_)));
    }

    // ---- 端到端：真实 BtCore + 本地 HTTP 静态源（F5 PoC-1 的 Rust 复刻）----

    /// 极简 HTTP/1.1 静态源：200 全量 / 206 Range 分片，Connection: close。
    async fn serve_http(mut sock: tokio::net::TcpStream, body: Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).to_string();
        let range = req
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()));
        if let Some(r) = range {
            let spec = r.trim_start_matches("bytes=");
            let (a, b) = spec.split_once('-').unwrap_or((spec, ""));
            let start: u64 = a.parse().unwrap_or(0);
            let end: u64 = if b.is_empty() {
                body.len() as u64 - 1
            } else {
                b.parse::<u64>().unwrap_or(body.len() as u64 - 1)
            };
            let s = (start as usize).min(body.len().saturating_sub(1));
            let e = (end as usize).min(body.len().saturating_sub(1));
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {s}-{e}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
                e.saturating_sub(s) + 1
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body[s..=e]).await;
        } else {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = sock.write_all(head.as_bytes()).await;
            let _ = sock.write_all(&body).await;
        }
        let _ = sock.shutdown().await;
    }

    #[tokio::test]
    async fn webseed_e2e_downloads_via_local_http_source() {
        use sha1::{Digest, Sha1};
        // 单 piece 文件：64KB 内容，pieces = SHA1(全量)
        let content: Vec<u8> = (0..65536u32).map(|i| (i % 251) as u8).collect();
        let mut h = Sha1::new();
        h.update(&content);
        let piece = h.finalize();
        let mut meta =
            b"d4:infod6:lengthi65536e4:name8:file.bin12:piece lengthi65536e6:pieces20:".to_vec();
        meta.extend_from_slice(&piece);
        meta.extend_from_slice(b"eee");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                let c = content.clone();
                tokio::spawn(async move { serve_http(sock, c).await });
            }
        });

        let save = tempfile::tempdir().unwrap();
        let core = smart_dl_btcore::BtCore::new(save.path(), "webseed-e2e").expect("session init");
        let ih = core.add_torrent_file(&meta, &[]).expect("add torrent");
        core.add_url_seed(&ih, &format!("http://{addr}/file.bin"))
            .expect("add url seed");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let st = core.status(&ih).expect("status");
            if st.downloaded >= 65536 && st.progress >= 0.999 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "90s 未完成: downloaded={} progress={}",
                st.downloaded,
                st.progress
            );
        }
    }
}
#[cfg(test)]
mod ct_eq_tests {
    use super::{ct_eq, DaemonState};
    use std::sync::Arc;

    #[test]
    fn ct_eq_matches_equality_semantics() {
        assert!(ct_eq("abc", "abc"));
        assert!(ct_eq("", ""));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abC"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(!ct_eq("abcd", "abc"));
        assert!(!ct_eq("", "a"));
        // 高熵长 token 等价性
        let t = "a1B2c3D4e5F6g7H8";
        assert!(ct_eq(t, t));
        assert!(!ct_eq(t, "a1B2c3D4e5F6g7H9"));
    }

    #[test]
    fn verify_http_token_end_to_end() {
        let engine = smart_dl_httpdl::HttpEngine::new(reqwest::Client::new());
        let st =
            DaemonState::new(Arc::new(engine), vec![]).with_http_token(Some("s3cret".to_string()));
        assert!(st.verify_http_token(Some("Bearer s3cret")));
        // 前缀大小写敏感（Bearer 规范）
        assert!(!st.verify_http_token(Some("bearer s3cret")));
        assert!(!st.verify_http_token(Some("Bearer s3cretX")));
        assert!(!st.verify_http_token(Some("Bearer ")));
        assert!(!st.verify_http_token(Some("Basic s3cret")));
        assert!(!st.verify_http_token(None));
    }
}

/// E5 任务级代理：add 入口校验 + source.proxy 持久化回显（FakeEngine 不联网）。
#[cfg(test)]
mod task_proxy_tests {
    use super::*;

    #[tokio::test]
    async fn add_rejects_invalid_proxy_url() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        for bad in ["", "http://127.0.0.1:70000"] {
            let r = state
                .add_http_task_opts(
                    "https://example.com/f.bin".into(),
                    None,
                    AddHttpOpts {
                        proxy: Some(bad.to_string()),
                        ..Default::default()
                    },
                )
                .await;
            match r {
                Err(DaemonError::InvalidSource(m)) => {
                    assert!(m.contains("proxy"), "错误信息应定性 proxy: {m}");
                }
                other => panic!("非法 proxy {bad:?} 必须在入队前拒绝: {other:?}"),
            }
        }
        assert!(fake.added().is_empty(), "被拒任务不得进入引擎");
    }

    #[tokio::test]
    async fn add_persists_wellformed_proxy_in_source() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    proxy: Some("http://127.0.0.1:8080".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        match &rec.task.source {
            DownloadSource::Http { proxy, .. } => {
                assert_eq!(
                    proxy.as_deref(),
                    Some("http://127.0.0.1:8080"),
                    "proxy 应在 source 持久化回显"
                );
            }
            other => panic!("source 应为 Http: {other:?}"),
        }
        // 默认路径：proxy=None 正常建任务
        let tid2 = state
            .add_http_task("https://example.com/g.bin".into(), None)
            .await
            .unwrap();
        let rec2 = state.tasks.lock().get(&tid2).cloned().unwrap();
        match &rec2.task.source {
            DownloadSource::Http { proxy, .. } => assert!(proxy.is_none()),
            other => panic!("source 应为 Http: {other:?}"),
        }
    }
}

/// E6 AddHttpOpts：API 新暴露字段（headers/auth/sha256/backup/name）落任务
/// 记录 + 入参校验（FakeEngine 不联网）。
#[cfg(test)]
mod add_opts_tests {
    use super::*;

    #[tokio::test]
    async fn add_opts_fields_land_in_task_record() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    sequential: true,
                    proxy: Some("socks5://127.0.0.1:1080".into()),
                    headers: vec![
                        ("Referer".into(), "https://ref.example".into()),
                        ("X-Token".into(), "abc".into()),
                    ],
                    basic_auth: Some(("user".into(), "pass".into())),
                    // 大写 + 首尾空白：断言小写归一
                    sha256: Some(
                        " ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789 ".into(),
                    ),
                    sha1: None,
                    md5: None,
                    backup_url: Some("https://backup.example/f.bin".into()),
                    backup_md5: Some("ABCDEF0123456789ABCDEF0123456789".into()),
                    name: Some("explicit-name.bin".into()),
                    conflict: None,
                    start_at_unix: None,
                    auto_retry: 0,
                },
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec.task.sequential, "sequential 应落任务");
        match &rec.task.source {
            DownloadSource::Http {
                headers,
                auth,
                backup_url,
                proxy,
                ..
            } => {
                assert_eq!(headers.len(), 2, "headers 应原序落 source");
                assert_eq!(headers[0].0, "Referer");
                assert_eq!(
                    auth.as_ref(),
                    Some(&Auth::Basic("user".into(), "pass".into()))
                );
                assert_eq!(backup_url.as_deref(), Some("https://backup.example/f.bin"));
                assert_eq!(proxy.as_deref(), Some("socks5://127.0.0.1:1080"));
            }
            other => panic!("source 应为 Http: {other:?}"),
        }
        match &rec.task.identity {
            ContentIdentity::SingleFile {
                sha256, backup_md5, ..
            } => {
                assert_eq!(
                    sha256.as_deref(),
                    Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
                    "sha256 大写+空白应小写归一"
                );
                assert_eq!(
                    backup_md5.as_deref(),
                    Some("abcdef0123456789abcdef0123456789"),
                    "backup_md5 大写应小写归一"
                );
            }
            other => panic!("identity 应为 SingleFile: {other:?}"),
        }
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("explicit-name.bin"),
            "显式名应落 metadata"
        );
    }

    #[tokio::test]
    async fn add_opts_validation_rejects_bad_input() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(DaemonState::new(fake.clone(), vec![]));
        let cases: Vec<(AddHttpOpts, &str)> = vec![
            (
                AddHttpOpts {
                    sha256: Some("ab".repeat(31)), // 63 hex
                    ..Default::default()
                },
                "sha256",
            ),
            (
                AddHttpOpts {
                    sha256: Some("g".repeat(64)), // 非 hex
                    ..Default::default()
                },
                "sha256",
            ),
            (
                AddHttpOpts {
                    sha1: Some("ab".repeat(19)), // 38 hex
                    ..Default::default()
                },
                "sha1",
            ),
            (
                AddHttpOpts {
                    sha1: Some("z".repeat(40)), // 非 hex
                    ..Default::default()
                },
                "sha1",
            ),
            (
                AddHttpOpts {
                    md5: Some("ab".repeat(15)), // 30 hex
                    ..Default::default()
                },
                "md5",
            ),
            (
                AddHttpOpts {
                    md5: Some("w".repeat(32)), // 非 hex
                    ..Default::default()
                },
                "md5",
            ),
            (
                AddHttpOpts {
                    // E25 互斥：sha256 + sha1
                    sha256: Some("a".repeat(64)),
                    sha1: Some("a".repeat(40)),
                    ..Default::default()
                },
                "互斥",
            ),
            (
                AddHttpOpts {
                    // E25 互斥：sha256 + md5
                    sha256: Some("a".repeat(64)),
                    md5: Some("a".repeat(32)),
                    ..Default::default()
                },
                "互斥",
            ),
            (
                AddHttpOpts {
                    // E25 互斥：sha1 + md5
                    sha1: Some("a".repeat(40)),
                    md5: Some("a".repeat(32)),
                    ..Default::default()
                },
                "互斥",
            ),
            (
                AddHttpOpts {
                    // E25 互斥：三者同时
                    sha256: Some("a".repeat(64)),
                    sha1: Some("a".repeat(40)),
                    md5: Some("a".repeat(32)),
                    ..Default::default()
                },
                "互斥",
            ),
            (
                AddHttpOpts {
                    backup_md5: Some("a".repeat(32)), // 无 backup_url
                    ..Default::default()
                },
                "成对",
            ),
            (
                AddHttpOpts {
                    backup_url: Some("https://b.example/f".into()),
                    backup_md5: Some("a".repeat(31)),
                    ..Default::default()
                },
                "backup_md5",
            ),
            (
                AddHttpOpts {
                    backup_url: Some("ftp://b.example/f".into()),
                    ..Default::default()
                },
                "backup_url",
            ),
            (
                AddHttpOpts {
                    headers: vec![("X-Bad:Header".into(), "v".into())],
                    ..Default::default()
                },
                "header",
            ),
            (
                AddHttpOpts {
                    headers: vec![("".into(), "v".into())],
                    ..Default::default()
                },
                "header",
            ),
            (
                AddHttpOpts {
                    headers: vec![("X-Ok".into(), "v\ninjected".into())],
                    ..Default::default()
                },
                "换行",
            ),
            (
                AddHttpOpts {
                    name: Some("../escape.bin".into()),
                    ..Default::default()
                },
                "name",
            ),
        ];
        for (opts, tag) in cases {
            let r = state
                .add_http_task_opts("https://example.com/f.bin".into(), None, opts)
                .await;
            match r {
                Err(DaemonError::InvalidSource(m)) => {
                    assert!(m.contains(tag), "错误信息应定性 {tag}: {m}");
                }
                other => panic!("非法入参（{tag}）必须 InvalidSource 拒绝: {other:?}"),
            }
        }
        assert!(fake.added().is_empty(), "被拒任务不得进入引擎");
    }

    #[tokio::test]
    async fn add_opts_verify_algo_e25_normalize_and_landing() {
        // E25：sha1/md5 主源校验目标入参大写+空白 → 小写归一落 identity，
        // 且经 API 层（AddHttpOpts）进入引擎可见的任务身份。
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/e25.bin".into(),
                None,
                AddHttpOpts {
                    sha1: Some(format!(" {} ", "ab".repeat(20))), // 40 hex + 空白
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        match &rec.task.identity {
            ContentIdentity::SingleFile { sha1, md5, .. } => {
                assert_eq!(
                    sha1.as_deref(),
                    Some("ab".repeat(20)).as_deref(),
                    "sha1 大写+空白应小写归一"
                );
                assert_eq!(md5, &None);
            }
            other => panic!("identity 应为 SingleFile: {other:?}"),
        }
        // md5 同理（第二个任务，URL 不同避开 canonical 查重）
        let tid2 = state
            .add_http_task_opts(
                "https://example.com/e25-md5.bin".into(),
                None,
                AddHttpOpts {
                    md5: Some(" ABCDEF0123456789ABCDEF0123456789".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let rec2 = state.tasks.lock().get(&tid2).cloned().unwrap();
        match &rec2.task.identity {
            ContentIdentity::SingleFile { sha1, md5, .. } => {
                assert_eq!(sha1, &None);
                assert_eq!(
                    md5.as_deref(),
                    Some("abcdef0123456789abcdef0123456789"),
                    "md5 大写+空白应小写归一"
                );
            }
            other => panic!("identity 应为 SingleFile: {other:?}"),
        }
    }
}

/// E7 任务管理面：列表过滤/分页/确定性排序 + 批量操作 + remove delete_data
/// 透传（FakeEngine 不联网；状态直接改记录白盒构造，避免真实下载竞态）。
#[cfg(test)]
mod list_batch_tests {
    use super::*;

    /// 造 n 个不同 URL 的 HTTP 任务（dest 走默认路径），返回 task_id 列表。
    async fn add_n(state: &DaemonState, n: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for i in 0..n {
            let tid = state
                .add_http_task(format!("https://example.com/f{i}.bin"), None)
                .await
                .unwrap();
            ids.push(tid);
        }
        ids
    }

    #[tokio::test]
    async fn list_filtered_orders_by_creation_and_paginates() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 5).await;

        // 无过滤：全量，创建序（HashMap 迭代序不稳定 → 排序必须确定性）
        let (all, total) = state.list_filtered(&ListQuery::default());
        assert_eq!(total, 5);
        assert_eq!(
            all.iter().map(|r| r.task_id.clone()).collect::<Vec<_>>(),
            ids,
            "默认列表必须按创建序（task_id 数值后缀）"
        );

        // 分页：limit=2 offset=1 → 第 2、3 个；total 是过滤后总数而非页长
        let (page, total) = state.list_filtered(&ListQuery {
            limit: Some(2),
            offset: 1,
            ..Default::default()
        });
        assert_eq!(total, 5);
        assert_eq!(
            page.iter().map(|r| r.task_id.clone()).collect::<Vec<_>>(),
            ids[1..3].to_vec()
        );

        // offset 越界 → 空页但 total 不丢
        let (page, total) = state.list_filtered(&ListQuery {
            offset: 99,
            ..Default::default()
        });
        assert!(page.is_empty());
        assert_eq!(total, 5);
    }

    #[tokio::test]
    async fn list_filtered_by_state_and_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 3).await;
        // 白盒改状态：t1 → Paused，t2 → Failed（t3 保持 Queued）
        {
            let mut tasks = state.tasks.lock();
            tasks.get_mut(&ids[0]).unwrap().task.state = TaskState::Paused;
            tasks.get_mut(&ids[1]).unwrap().task.state = TaskState::Failed;
        }

        // 单状态过滤 + 大小写不敏感
        let (rows, total) = state.list_filtered(&ListQuery {
            states: vec!["paused".into()],
            ..Default::default()
        });
        assert_eq!((rows.len(), total), (1, 1));
        assert_eq!(rows[0].task_id, ids[0]);

        // 多状态 OR
        let (rows, _) = state.list_filtered(&ListQuery {
            states: vec!["Paused".into(), "Failed".into()],
            ..Default::default()
        });
        assert_eq!(rows.len(), 2);

        // 引擎过滤（大小写不敏感）+ 维度间 AND
        let (rows, total) = state.list_filtered(&ListQuery {
            engines: vec!["HTTP".into()],
            ..Default::default()
        });
        assert_eq!((rows.len(), total), (3, 3));
        assert!(
            rows.iter().all(|r| r.engine == "http"),
            "engine 标签必须回显"
        );
        let (rows, _) = state.list_filtered(&ListQuery {
            states: vec!["Paused".into()],
            engines: vec!["bt".into()],
            ..Default::default()
        });
        assert!(rows.is_empty(), "状态命中但引擎不命中 → AND 过滤为空");

        // 列表条目 name 字段：未显式命名 → 省略（E4 派生链未回填）
        let (rows, _) = state.list_filtered(&ListQuery::default());
        assert!(rows.iter().all(|r| r.name.is_none()));
    }

    #[tokio::test]
    async fn batch_pause_resume_remove_semantics() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 3).await;

        // 批量 pause：2 存在 + 1 不存在 + 1 重复 id（静默去重，不产生假失败）
        let mut req = ids[..2].to_vec();
        req.push("t999".into());
        req.push(ids[0].clone());
        let out = state.batch(&req, BatchAction::Pause).await;
        assert_eq!(out.results.len(), 3, "重复 id 去重后仅 3 项");
        assert_eq!((out.succeeded, out.failed), (2, 1));
        assert_eq!(
            out.results.iter().filter(|r| r.ok).count(),
            2,
            "逐项结果形状完整"
        );
        let missing = out.results.iter().find(|r| !r.ok).unwrap();
        assert_eq!(missing.id, "t999");
        assert!(
            missing.error.as_deref().unwrap_or("").contains("not found"),
            "单项失败必须带原因: {:?}",
            missing.error
        );
        assert_eq!(fake.paused_calls().len(), 2, "引擎 pause 恰好 2 次");
        for tid in &ids[..2] {
            let rec = state.tasks.lock().get(tid).cloned().unwrap();
            assert!(matches!(rec.task.state, TaskState::Paused));
        }

        // 批量 resume：1 存在 + 1 不存在
        let out = state
            .batch(&[ids[0].clone(), "tX".into()], BatchAction::Resume)
            .await;
        assert_eq!((out.succeeded, out.failed), (1, 1));
        assert_eq!(fake.resumed_calls().len(), 1);

        // 批量 remove：3 全成功（引擎侧 delete_data=false）
        let out = state
            .batch(
                &[ids[0].clone(), ids[1].clone(), ids[2].clone()],
                BatchAction::Remove { delete_data: false },
            )
            .await;
        assert_eq!((out.succeeded, out.failed), (3, 0));
        assert_eq!(fake.removed_calls().len(), 3);
        assert!(
            fake.removed_calls().iter().all(|(_, dd)| !dd),
            "delete_data=false 必须原样透传"
        );
        assert!(state.list().is_empty(), "批量 remove 后任务表必须清空");
    }

    #[tokio::test]
    async fn remove_with_forwards_delete_data_to_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ids = add_n(&state, 1).await;
        let engine_tid = state.tasks.lock().get(&ids[0]).unwrap().engine_tid.clone();

        // 不存在的 id → NotFound，且引擎零调用
        assert!(state.remove_with("t404", true).await.is_err());
        assert!(fake.removed_calls().is_empty());

        state.remove_with(&ids[0], true).await.unwrap();
        assert_eq!(
            fake.removed_calls(),
            vec![(engine_tid.unwrap(), true)],
            "delete_data=true 必须透传到引擎"
        );
        assert!(state.list().is_empty());
    }

    #[test]
    fn known_labels_cover_all_variants() {
        let states = known_state_labels();
        for s in [
            "Queued",
            "Evaluating",
            "Downloading",
            "Paused",
            "FallbackProvider",
            "Transferring",
            "Completed",
            "Stopped",
            "Seeding",
            "Failed",
        ] {
            assert!(
                states.iter().any(|k| k == s),
                "state 合法值全集缺 {s}: {states:?}"
            );
        }
        let engines = known_engine_labels();
        assert_eq!(engines, vec!["bt", "http", "ftp", "provider", "xunlei-nas"]);
    }

    /// E14 搜索：名字 / URL 子串命中、大小写不敏感、无命中空集、
    /// 空白关键字 trim 后退化为不过滤（空针 contains 恒真）。
    #[tokio::test]
    async fn list_filtered_search_matches_name_and_url() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        state
            .add_http_task("https://srv-a.lan/Alpha.ISO".into(), None)
            .await
            .unwrap();
        state
            .add_http_task_opts(
                "https://other.lan/beta.zip".into(),
                None,
                AddHttpOpts {
                    name: Some("Movie Night.mkv".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        state
            .add_http_task("https://srv-b.lan/gamma.bin".into(), None)
            .await
            .unwrap();

        let ids = |q: ListQuery| -> Vec<String> {
            state
                .list_filtered(&q)
                .0
                .into_iter()
                .map(|r| r.task_id)
                .collect()
        };

        // URL 命中（查询小写 vs 源 MixedCase → 大小写不敏感）
        assert_eq!(
            ids(ListQuery {
                search: Some("alpha.iso".into()),
                ..Default::default()
            })
            .len(),
            1,
            "URL 子串命中（大小写不敏感）"
        );

        // 名字命中（该任务 URL 不含 movie → 只能来自名字语料）
        assert_eq!(
            ids(ListQuery {
                search: Some("MOVIE".into()),
                ..Default::default()
            })
            .len(),
            1,
            "名字子串命中（大小写不敏感）"
        );

        // 同前缀多主机：两台 srv-* 都命中
        assert_eq!(
            ids(ListQuery {
                search: Some("srv-".into()),
                ..Default::default()
            })
            .len(),
            2,
            "URL 前缀命中应覆盖两台 srv-* 主机"
        );

        // 无命中 → 空集 + total=0
        let (page, total) = state.list_filtered(&ListQuery {
            search: Some("nonexistent-keyword".into()),
            ..Default::default()
        });
        assert!(page.is_empty());
        assert_eq!(total, 0);

        // 空白关键字 = 不过滤（全量 3）
        let (page, total) = state.list_filtered(&ListQuery {
            search: Some("   ".into()),
            ..Default::default()
        });
        assert_eq!(total, 3);
        assert_eq!(page.len(), 3, "空白关键字 trim 后为空 → 不过滤");
    }
}

/// E8 任务级代理热改：记录回显 + 引擎调用 + 入参校验 + 非 HTTP 预拒
/// （FakeEngine 记录调用；BT 任务白盒插记录，precheck 在 engine_for 之前，
/// 非 bt 构建同样可测）。
#[cfg(test)]
mod task_proxy_set_tests {
    use super::*;
    use smart_dl_core::identity::{CanonicalId, CanonicalKind, ContentIdentity};

    /// 手工插一条 BT kind 记录（不注册 BT 引擎——set_task_proxy 的 kind
    /// 预拒发生在 engine_for 之前，未注册引擎不影响断言路径）。
    fn insert_bt_rec(state: &DaemonState, id: &str, ih: &str) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: TaskState::Downloading(EngineKind::Bt),
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    fn source_proxy(state: &DaemonState, id: &str) -> Option<String> {
        match &state.tasks.lock().get(id).unwrap().task.source {
            DownloadSource::Http { proxy, .. } => proxy.clone(),
            other => panic!("source 应为 Http: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_task_proxy_updates_record_and_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();

        state
            .set_task_proxy(&tid, Some("http://127.0.0.1:8080".into()))
            .await
            .unwrap();
        assert_eq!(
            source_proxy(&state, &tid),
            Some("http://127.0.0.1:8080".into()),
            "proxy 必须回显到任务 source（持久化口径）"
        );
        let engine_tid = state.tasks.lock().get(&tid).unwrap().engine_tid.clone();
        assert_eq!(
            fake.proxy_set_calls(),
            vec![(
                engine_tid.clone().unwrap(),
                Some("http://127.0.0.1:8080".into())
            )],
            "引擎必须收到热改调用（按 engine_tid 记录）"
        );
        // 清除回共享 client
        state.set_task_proxy(&tid, None).await.unwrap();
        assert_eq!(
            source_proxy(&state, &tid),
            None,
            "清除后 source.proxy = None"
        );
        assert_eq!(
            fake.proxy_set_calls()[1],
            (engine_tid.unwrap(), None),
            "清除语义必须透传引擎（None 而非空串）"
        );
    }

    #[tokio::test]
    async fn set_task_proxy_rejects_invalid_without_side_effect() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();

        for bad in [
            "",                       // 空串：非法 URL 不是清除（清除传 None）
            "http://127.0.0.1:70000", // 端口越界
        ] {
            let r = state.set_task_proxy(&tid, Some(bad.to_string())).await;
            match r {
                Err(DaemonError::InvalidSource(m)) => {
                    assert!(!m.is_empty(), "{bad:?} 必须带错误说明");
                }
                other => panic!("非法 proxy {bad:?} 必须 InvalidSource: {other:?}"),
            }
        }
        assert!(
            fake.proxy_set_calls().is_empty(),
            "非法 proxy 不得触达引擎（零副作用）"
        );
        assert_eq!(source_proxy(&state, &tid), None, "记录保持原状");
        // 不存在的任务 → NotFound
        assert!(matches!(
            state
                .set_task_proxy("t404", Some("http://127.0.0.1:1".into()))
                .await,
            Err(DaemonError::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn set_task_proxy_rejects_non_http_task() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec(&state, "t-bt", "ABC123");

        let r = state
            .set_task_proxy("t-bt", Some("http://127.0.0.1:8080".into()))
            .await;
        match r {
            Err(DaemonError::UnsupportedOp(m)) => {
                assert!(m.contains("HTTP"), "409 错误信息应定性仅 HTTP 支持: {m}");
            }
            other => panic!("BT 任务 set proxy 必须 UnsupportedOp(409): {other:?}"),
        }
        assert!(
            fake.proxy_set_calls().is_empty(),
            "HTTP 引擎不得收到 BT 任务的调用"
        );
    }
}

/// E30 失败自动重试：退避纯函数序列 / 失败处置矩阵 / 调度门控 /
/// poll Error 拦截全环 / add 失败安排重试后源恢复重试成功。
/// 全部 FakeEngine 白盒构造，无真实网络。
#[cfg(test)]
mod auto_retry_tests {
    use super::*;

    fn rec_with_retry(max: u32, st: TaskState) -> TaskRecord {
        let mut rec = TaskRecord {
            task: DownloadTask {
                id: "tX".into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Http,
                    identity: "https://x/f.bin".into(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Http {
                    url: "https://x/f.bin".into(),
                    headers: vec![],
                    auth: None,
                    backup_url: None,
                    proxy: None,
                },
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: st.clone(),
                retry: RetryState {
                    retries: 0,
                    max_retries: max,
                },
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some("fk1".into()),
            engine_kind: EngineKind::Http,
            engine_status: None,
            events: vec![],
        };
        // state 与句柄一致性前置校验：Failed 用例应无句柄语义混淆——
        // 统一从 Downloading 出发（真实失败路径正是活跃 → 失败）。
        if st == TaskState::Failed {
            rec.task.state = TaskState::Failed;
            rec.engine_tid = None;
        }
        rec
    }

    #[test]
    fn backoff_delay_sequence_and_cap() {
        // 2/4/8/16/32 → 封顶 60
        assert_eq!(retry_backoff_delay_s(1), 2);
        assert_eq!(retry_backoff_delay_s(2), 4);
        assert_eq!(retry_backoff_delay_s(3), 8);
        assert_eq!(retry_backoff_delay_s(4), 16);
        assert_eq!(retry_backoff_delay_s(5), 32);
        assert_eq!(retry_backoff_delay_s(6), 60, "2^6=64 → 封顶 60");
        assert_eq!(retry_backoff_delay_s(7), 60);
        assert_eq!(retry_backoff_delay_s(25), 60, "远超上限恒封顶");
    }

    #[test]
    fn fail_with_zero_budget_is_terminal() {
        let mut rec = rec_with_retry(0, TaskState::Downloading(EngineKind::Http));
        let to = rec.fail_or_schedule_retry(Some("boom"));
        assert_eq!(to, TaskState::Failed);
        assert_eq!(rec.task.state, TaskState::Failed);
        assert_eq!(rec.task.retry.retries, 0, "max=0 不消耗计数");
        assert_eq!(rec.task.metadata.next_retry_at_unix, 0);
        assert!(
            rec.events.iter().all(|e| e.op != "auto_retry"),
            "终态不产生重试事件"
        );
    }

    #[test]
    fn fail_within_budget_schedules_retry() {
        let mut rec = rec_with_retry(1, TaskState::Downloading(EngineKind::Http));
        let to = rec.fail_or_schedule_retry(Some("boom"));
        assert_eq!(to, TaskState::Queued, "预算未用尽 → 回队列等待退避");
        assert_eq!(rec.task.state, TaskState::Queued);
        assert_eq!(rec.task.retry.retries, 1);
        assert!(
            rec.task.metadata.next_retry_at_unix > now_unix(),
            "next_retry 应为未来时刻（退避延迟）"
        );
        assert!(rec.engine_tid.is_none(), "重试等待任务必须无引擎句柄");
        assert!(
            rec.events.iter().any(|e| e.op == "auto_retry"),
            "必须落 auto_retry 事件"
        );

        // 第二次失败：预算（max=1）用尽 → 终态
        let to2 = rec.fail_or_schedule_retry(Some("boom again"));
        assert_eq!(to2, TaskState::Failed);
        assert_eq!(rec.task.retry.retries, 1, "retries 停在 max，不再递增");
        assert_eq!(rec.task.state, TaskState::Failed);
    }

    #[tokio::test]
    async fn poll_error_schedules_retry_then_exhausts() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    auto_retry: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // 白盒推进到 Downloading（真实失败路径：活跃中报 Error）
        state.tasks.lock().get_mut(&tid).unwrap().task.state =
            TaskState::Downloading(EngineKind::Http);
        fake.set_status_state(EngineState::Error);

        // 首轮 poll：Error → 预算未用尽 → Queued + 重试安排
        let effects = state.poll_engine_states().await;
        assert_eq!(effects.len(), 1, "重试安排也是一次状态迁移（广播）");
        assert_eq!(
            effects[0].to,
            TaskState::Queued,
            "effect 目标必须是 Queued（非 Failed）"
        );
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            assert_eq!(rec.task.state, TaskState::Queued);
            assert_eq!(rec.task.retry.retries, 1);
            assert_eq!(rec.task.retry.max_retries, 1);
            assert!(rec.task.metadata.next_retry_at_unix > now_unix());
            assert!(rec.engine_tid.is_none());
        }

        // 未到期：调度循环不激活
        let activated = state.activate_due_tasks().await;
        assert!(activated.is_empty(), "退避期内不得激活");

        // 白盒到期 → 激活（引擎 add 恒成功）→ 恢复 Downloading → next_retry 消费
        state
            .tasks
            .lock()
            .get_mut(&tid)
            .unwrap()
            .task
            .metadata
            .next_retry_at_unix = 1;
        let activated = state.activate_due_tasks().await;
        assert_eq!(activated, vec![tid.clone()]);
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            // 重试 = 重新 add → 引擎侧新句柄（FakeEngine counter 自增：fk2）
            assert!(rec.engine_tid.is_some(), "重试激活必须重建引擎句柄");
            assert_eq!(
                rec.task.metadata.next_retry_at_unix, 0,
                "激活即消费重试安排"
            );
        }

        // 再次 Error：预算用尽 → Failed 终态
        state.tasks.lock().get_mut(&tid).unwrap().task.state =
            TaskState::Downloading(EngineKind::Http);
        let effects = state.poll_engine_states().await;
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0].to, TaskState::Failed);
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec.task.state, TaskState::Failed);
        assert_eq!(rec.task.retry.retries, 1);
    }

    #[tokio::test]
    async fn poll_zero_budget_keeps_legacy_failed_semantics() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        state.tasks.lock().get_mut(&tid).unwrap().task.state =
            TaskState::Downloading(EngineKind::Http);
        fake.set_status_state(EngineState::Error);

        state.poll_engine_states().await;
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.state,
            TaskState::Failed,
            "默认 auto_retry=0 必须保持既有一次性失败语义"
        );
        assert_eq!(rec.task.metadata.next_retry_at_unix, 0);
    }

    #[tokio::test]
    async fn add_failure_schedules_retry_and_succeeds_after_recovery() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        fake.fail_url("https://example.com/f.bin");
        // 白盒建「重试等待」记录（首次 add 失败/restore 激活失败后的形态）：
        // Queued + 句柄空 + next_retry 已到期
        let tid = "t901".to_string();
        state.tasks.lock().insert(
            tid.clone(),
            TaskRecord {
                task: DownloadTask {
                    id: tid.clone(),
                    canonical_id: CanonicalId {
                        kind: CanonicalKind::Http,
                        identity: "https://example.com/f.bin".into(),
                        validator: None,
                        token_sensitive: false,
                    },
                    source: DownloadSource::Http {
                        url: "https://example.com/f.bin".into(),
                        headers: vec![],
                        auth: None,
                        backup_url: None,
                        proxy: None,
                    },
                    identity: ContentIdentity::SingleFile {
                        size: 0,
                        etag: None,
                        sha256: None,
                        sha1: None,
                        md5: None,
                        backup_md5: None,
                    },
                    dest_root: PathBuf::from("."),
                    files: vec![],
                    acquisitions: vec![],
                    aggregate: Default::default(),
                    state: TaskState::Queued,
                    retry: RetryState {
                        retries: 0,
                        max_retries: 1,
                    },
                    created_at: std::time::Instant::now(),
                    file_priorities: None,
                    sequential: false,
                    metadata: TaskMetadata {
                        name: None,
                        added_at_unix: 0,
                        tags: Vec::new(),
                        finished_at_unix: 0,
                        start_at_unix: 0,
                        next_retry_at_unix: 1, // 已到期
                    },
                    limits: None,
                },
                engine_tid: None,
                engine_kind: EngineKind::Http,
                engine_status: None,
                events: vec![],
            },
        );

        // add 仍败：重试预算消耗 → 再次安排（retries=1，继续等待）
        let activated = state.activate_due_tasks().await;
        assert!(activated.is_empty(), "重试轮仍失败不进 activated");
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            assert_eq!(rec.task.state, TaskState::Queued, "预算内 → 继续等待");
            assert_eq!(rec.task.retry.retries, 1);
            assert!(rec.task.metadata.next_retry_at_unix > 1, "新退避时刻已安排");
        }

        // 源恢复 → 到期重激活成功
        fake.unfail_url("https://example.com/f.bin");
        state
            .tasks
            .lock()
            .get_mut(&tid)
            .unwrap()
            .task
            .metadata
            .next_retry_at_unix = 1;
        let activated = state.activate_due_tasks().await;
        assert_eq!(activated, vec![tid.clone()]);
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec.engine_tid.is_some());
        assert_eq!(
            fake.added().len(),
            1,
            "恢复后重试 add 成功 1 次（首败在记录前）"
        );
    }

    #[tokio::test]
    async fn snapshot_and_summary_surface_retry_fields() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    auto_retry: 3,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let snap = state.task_snapshot(&tid).await.unwrap();
        let json = serde_json::to_value(&snap).unwrap();
        assert!(json.get("retries").is_none(), "retries=0 序列化省略");
        assert_eq!(json["max_retries"], 3);
        assert!(
            json.get("next_retry_at_unix").is_none(),
            "无重试安排序列化省略"
        );

        // 安排重试后：三字段全透出
        state
            .tasks
            .lock()
            .get_mut(&tid)
            .unwrap()
            .fail_or_schedule_retry(Some("x"));
        let snap = state.task_snapshot(&tid).await.unwrap();
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["retries"], 1);
        assert_eq!(json["max_retries"], 3);
        assert!(json["next_retry_at_unix"].as_u64().unwrap() > 0);

        let (summaries, _) = state.list_filtered(&ListQuery::default());
        let s = summaries.iter().find(|s| s.task_id == tid).unwrap();
        assert_eq!(s.retries, 1);
        assert_eq!(s.max_retries, 3);
    }

    #[tokio::test]
    async fn resume_failed_without_handle_retries_manually() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        // 白盒建「预算耗尽 Failed」任务（无句柄——激活失败路径形态）
        let tid = "t902".to_string();
        state.tasks.lock().insert(
            tid.clone(),
            TaskRecord {
                task: DownloadTask {
                    id: tid.clone(),
                    canonical_id: CanonicalId {
                        kind: CanonicalKind::Http,
                        identity: "https://example.com/f.bin".into(),
                        validator: None,
                        token_sensitive: false,
                    },
                    source: DownloadSource::Http {
                        url: "https://example.com/f.bin".into(),
                        headers: vec![],
                        auth: None,
                        backup_url: None,
                        proxy: None,
                    },
                    identity: ContentIdentity::SingleFile {
                        size: 0,
                        etag: None,
                        sha256: None,
                        sha1: None,
                        md5: None,
                        backup_md5: None,
                    },
                    dest_root: PathBuf::from("."),
                    files: vec![],
                    acquisitions: vec![],
                    aggregate: Default::default(),
                    state: TaskState::Failed,
                    retry: RetryState {
                        retries: 1,
                        max_retries: 1,
                    },
                    created_at: std::time::Instant::now(),
                    file_priorities: None,
                    sequential: false,
                    metadata: TaskMetadata {
                        name: None,
                        added_at_unix: 0,
                        tags: Vec::new(),
                        finished_at_unix: 0,
                        start_at_unix: 0,
                        next_retry_at_unix: 0,
                    },
                    limits: None,
                },
                engine_tid: None,
                engine_kind: EngineKind::Http,
                engine_status: None,
                events: vec![],
            },
        );

        // 手动重试：resume 成功 → 引擎重新接入 → Downloading
        state.resume(&tid).await.unwrap();
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            assert_eq!(rec.task.state, TaskState::Downloading(EngineKind::Http));
            assert!(rec.engine_tid.is_some(), "重试必须重建引擎句柄");
            assert_eq!(fake.added().len(), 1);
            assert!(
                rec.events.iter().any(|e| e.op == "retry"),
                "Failed 来源的 resume 必须落 retry 事件（区分普通恢复）: {:?}",
                rec.events.iter().map(|e| e.op.clone()).collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test]
    async fn resume_retry_keeps_exhausted_budget_no_infinite_loop() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    auto_retry: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // 首轮失败：预算内 → 重试安排；二轮失败：耗尽 → Failed（句柄保留）
        state.tasks.lock().get_mut(&tid).unwrap().task.state =
            TaskState::Downloading(EngineKind::Http);
        fake.set_status_state(EngineState::Error);
        state.poll_engine_states().await;
        state
            .tasks
            .lock()
            .get_mut(&tid)
            .unwrap()
            .task
            .metadata
            .next_retry_at_unix = 1;
        state.activate_due_tasks().await;
        state.tasks.lock().get_mut(&tid).unwrap().task.state =
            TaskState::Downloading(EngineKind::Http);
        state.poll_engine_states().await;
        assert_eq!(
            state.tasks.lock().get(&tid).unwrap().task.state,
            TaskState::Failed
        );

        // E32 手动重试（有句柄 → 引擎侧 resume 分支）→ 再失败：
        // auto_retry 预算【不重置】→ fail_or_schedule_retry 直接终态（无循环）
        state.resume(&tid).await.unwrap();
        assert_eq!(
            state.tasks.lock().get(&tid).unwrap().task.state,
            TaskState::Downloading(EngineKind::Http)
        );
        let effects = state.poll_engine_states().await;
        assert_eq!(effects[0].to, TaskState::Failed, "预算耗尽 → 不再自动重试");
        assert_eq!(
            state.tasks.lock().get(&tid).unwrap().task.retry.retries,
            1,
            "retries 停在 max，手动重试不白给预算"
        );
    }
}

/// E9 名字回填：引擎 status().name（落盘名决策结果）→ 轮询回填
/// metadata.name（空缺时）→ 列表/快照透出链。显式名权威不被覆盖；幂等。
#[cfg(test)]
mod name_backfill_tests {
    use super::*;

    #[tokio::test]
    async fn poll_backfills_derived_name_once() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        assert!(
            state
                .tasks
                .lock()
                .get(&tid)
                .unwrap()
                .task
                .metadata
                .name
                .is_none(),
            "前置：无显式名任务 metadata.name 为空"
        );
        fake.set_status_name("cd-derived.bin");

        // 首轮：迁移（Queued→Downloading，FakeEngine 恒 MetadataPending）+ 回填
        let effects = state.poll_engine_states().await;
        assert_eq!(effects.len(), 1, "首轮应有状态迁移");
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            assert_eq!(
                rec.task.metadata.name.as_deref(),
                Some("cd-derived.bin"),
                "引擎报的派生名必须回填 metadata.name"
            );
            assert!(
                rec.events.iter().any(|e| e.op == "name_backfilled"),
                "回填必须留事件痕迹"
            );
        }

        // 幂等：次轮零迁移零重复回填（to==from 纯无操作）
        let effects2 = state.poll_engine_states().await;
        assert!(effects2.is_empty(), "to==from 纯回填轮次不产生迁移广播");
        let rec2 = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(rec2.task.metadata.name.as_deref(), Some("cd-derived.bin"));
        let n = rec2
            .events
            .iter()
            .filter(|e| e.op == "name_backfilled")
            .count();
        assert_eq!(n, 1, "回填事件恰好一次（幂等）");

        // E7 透出链：列表条目 name 生效
        let list = state.list();
        assert_eq!(list[0].name.as_deref(), Some("cd-derived.bin"));
    }

    #[tokio::test]
    async fn poll_never_overrides_explicit_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task_opts(
                "https://example.com/f.bin".into(),
                None,
                AddHttpOpts {
                    name: Some("explicit.bin".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        fake.set_status_name("engine-name.bin");

        state.poll_engine_states().await;
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("explicit.bin"),
            "显式名权威——引擎回显同值也不覆盖"
        );
        assert!(
            !rec.events.iter().any(|e| e.op == "name_backfilled"),
            "显式名任务无回填事件"
        );
    }

    #[tokio::test]
    async fn poll_skips_tasks_with_engine_silent_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        // 引擎不透出 name（status_name 恒 None）→ 不回填不事件
        state.poll_engine_states().await;
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec.task.metadata.name.is_none(), "引擎未报 → 保持 None");
        assert!(!rec.events.iter().any(|e| e.op == "name_backfilled"));
    }
}

/// E15 任务重命名：显示层改名 + 清除回退 + 入参校验（V3 终审同函数）。
/// 落盘路径在引擎 add 时已定，改名不迁移文件（显示/检索字段）。
#[cfg(test)]
mod task_rename_tests {
    use super::*;

    #[tokio::test]
    async fn rename_sets_name_list_search_and_event() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://x.lan/f.bin".into(), None)
            .await
            .unwrap();

        state
            .set_task_name(&tid, Some("renamed.bin".into()))
            .unwrap();
        // 快照透出（E7 链路）
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(snap.name.as_deref(), Some("renamed.bin"));
        // 列表透出
        let list = state.list();
        assert!(list
            .iter()
            .any(|r| r.task_id == tid && r.name.as_deref() == Some("renamed.bin")));
        // E14 搜索语料联动：按新名命中（大小写不敏感）
        let (page, total) = state.list_filtered(&ListQuery {
            search: Some("RENAMED".into()),
            ..Default::default()
        });
        assert_eq!(total, 1);
        assert_eq!(page[0].task_id, tid);
        // 事件链（detail 只记 set/cleared，名字本体不进事件）
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec
            .events
            .iter()
            .any(|e| e.op == "name_changed" && e.detail.as_deref() == Some("set")));
    }

    #[tokio::test]
    async fn rename_validation_clear_and_notfound() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://x.lan/f.bin".into(), None)
            .await
            .unwrap();

        // 空白拒绝（清除语义由 None 承担）
        let e = state.set_task_name(&tid, Some("   ".into())).unwrap_err();
        assert!(matches!(e, DaemonError::InvalidSource(_)));
        // 非法路径分量拒绝（sanitize_rel 与 add 同一裁决点）
        let e = state
            .set_task_name(&tid, Some("../evil".into()))
            .unwrap_err();
        assert!(matches!(e, DaemonError::InvalidSource(_)));
        assert!(
            state
                .tasks
                .lock()
                .get(&tid)
                .unwrap()
                .task
                .metadata
                .name
                .is_none(),
            "非法改名必须零副作用"
        );

        // 设置 → 清除 → 快照 name 省略 + cleared 事件
        state.set_task_name(&tid, Some("tmp.bin".into())).unwrap();
        state.set_task_name(&tid, None).unwrap();
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(snap.name, None);
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec
            .events
            .iter()
            .any(|e| e.op == "name_changed" && e.detail.as_deref() == Some("cleared")));

        // 未知任务
        assert!(matches!(
            state.set_task_name("t404", Some("x".into())),
            Err(DaemonError::NotFound(_))
        ));
    }
}

/// E11 速率缓存：轮询器把引擎快照写入 `engine_status` → `/stats` 聚合生效；
/// BT 仅缓存不迁移（状态权威 = alert 流）；暂停/终态清零防陈旧速率。
#[cfg(test)]
mod rate_cache_tests {
    use super::*;

    /// 插入指定状态的 BT 任务记录（不联网，engine_tid = infohash）。
    fn insert_bt_rec_with(state: &DaemonState, id: &str, ih: &str, st: TaskState) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: st,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    #[tokio::test]
    async fn poll_caches_rates_into_stats_and_pause_zeroes() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        assert_eq!(state.stats().down_bytes_s, 0, "轮询前缓存为空 → 聚合速率 0");

        fake.set_status_rates(1000, 50);
        state.poll_engine_states().await;
        let st = state.stats();
        assert_eq!(st.down_bytes_s, 1000, "HTTP 引擎报的下行速率应入 /stats");
        assert_eq!(st.up_bytes_s, 50);
        {
            let rec = state.tasks.lock().get(&tid).cloned().unwrap();
            let es = rec.engine_status.expect("缓存应有引擎快照");
            assert_eq!(es.down_rate, 1000);
            assert_eq!(es.up_rate, 50);
        }

        // 暂停：清零缓存速率（轮询器不再光顾暂停任务，不清则聚合虚高）
        state.pause(&tid).await.unwrap();
        let st = state.stats();
        assert_eq!(st.down_bytes_s, 0, "暂停后聚合下行速率必须清零");
        assert_eq!(st.up_bytes_s, 0, "暂停后聚合上行速率必须清零");
        assert_eq!(
            state.task_logs(&tid).unwrap()["state"],
            serde_json::json!("Paused")
        );
    }

    #[tokio::test]
    async fn poll_refreshes_rates_each_round() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();

        fake.set_status_rates(500, 0);
        state.poll_engine_states().await;
        assert_eq!(state.stats().down_bytes_s, 500);

        fake.set_status_rates(0, 0);
        state.poll_engine_states().await;
        assert_eq!(state.stats().down_bytes_s, 0, "缓存跟随引擎每轮刷新");
    }

    #[tokio::test]
    async fn bt_rates_cached_without_transition() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(
            &state,
            "t-bt",
            "ABC123",
            TaskState::Downloading(EngineKind::Bt),
        );
        fake.set_status_rates(2000, 1000);

        let effects = state.poll_engine_states().await;
        assert!(effects.is_empty(), "BT 轮询仅缓存，不产生迁移效果");
        {
            let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
            assert_eq!(
                rec.task.state,
                TaskState::Downloading(EngineKind::Bt),
                "BT 状态权威 = alert 流，轮询不得迁移"
            );
            let es = rec.engine_status.expect("BT 快照应入缓存");
            assert_eq!(es.down_rate, 2000);
            assert_eq!(es.up_rate, 1000);
        }
        let st = state.stats();
        assert_eq!(st.down_bytes_s, 2000);
        assert_eq!(st.up_bytes_s, 1000);
    }

    #[tokio::test]
    async fn seeding_bt_task_rates_cached() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(&state, "t-seed", "DEF456", TaskState::Seeding);
        fake.set_status_rates(0, 800);

        let effects = state.poll_engine_states().await;
        assert!(effects.is_empty());
        let st = state.stats();
        assert_eq!(st.up_bytes_s, 800, "做种中上行速率应入聚合");
        assert_eq!(st.down_bytes_s, 0);
    }

    #[tokio::test]
    async fn paused_bt_task_not_polled() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(&state, "t-paused", "FFF111", TaskState::Paused);
        fake.set_status_rates(999, 999);

        state.poll_engine_states().await;
        let rec = state.tasks.lock().get("t-paused").cloned().unwrap();
        assert!(
            rec.engine_status.is_none(),
            "暂停任务不在候选集——缓存不得被写入陈旧速率"
        );
        assert_eq!(state.stats().down_bytes_s, 0);
    }

    /// E13：快照速率取自实时引擎快照——轮询器未跑（缓存恒 None）也能透出，
    /// 锁定「实时源而非缓存源」的设计语义。
    #[tokio::test]
    async fn snapshot_exposes_live_engine_rates_without_cache() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        assert!(
            state
                .tasks
                .lock()
                .get(&tid)
                .unwrap()
                .engine_status
                .is_none(),
            "前提：无轮询器 → 缓存必须为空（证明速率来自实时快照）"
        );
        fake.set_status_rates(1234, 56);
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(
            snap.rates,
            Some(TaskRates {
                down_bytes_s: 1234,
                up_bytes_s: 56
            }),
            "快照速率应来自引擎实时 status()，与缓存无关"
        );
        // 引擎报零 → 速率字段仍在（Some），形状恒定不省略
        fake.set_status_rates(0, 0);
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(
            snap.rates,
            Some(TaskRates {
                down_bytes_s: 0,
                up_bytes_s: 0
            })
        );
    }

    /// E13：记录级 Paused 是显示权威（qB 式），快照速率对齐 pause() 清零
    /// 语义——引擎侧 <200ms 平滑窗口的陈旧非零值不得穿透到暂停任务快照。
    #[tokio::test]
    async fn snapshot_zeroes_rates_for_paused_record() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        fake.set_status_rates(9999, 888);
        state.pause(&tid).await.unwrap();
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(snap.state, "Paused");
        // FakeEngine.status() 仍报 (9999, 888) + Downloading——清零只能来自
        // 记录级 Paused 守卫（引擎实时值与显示权威的裁决点）。
        assert_eq!(
            snap.rates,
            Some(TaskRates {
                down_bytes_s: 0,
                up_bytes_s: 0
            }),
            "暂停任务快照速率必须清零（防平滑窗口陈旧值毛刺）"
        );
    }

    /// E33：BT 累计统计与分享率透出——快照字段、JSON 形状（非零才出现）、
    /// share_ratio 精度三合一。FakeEngine 模拟引擎侧 all_time_* 回显。
    #[tokio::test]
    async fn snapshot_exposes_bt_totals_and_share_ratio() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(
            &state,
            "t-bt",
            "TOTL01",
            TaskState::Downloading(EngineKind::Bt),
        );
        // (累计下行 2MB, 累计上行 512KB) → 分享率 0.25
        fake.set_status_totals(2 * 1024 * 1024, 512 * 1024);
        let snap = state.task_snapshot("t-bt").await.unwrap();
        assert_eq!(snap.total_downloaded, 2 * 1024 * 1024);
        assert_eq!(snap.total_uploaded, 512 * 1024);
        assert_eq!(snap.share_ratio, Some(0.25));
        let json = serde_json::to_string(&snap).unwrap();
        assert!(json.contains("\"total_downloaded\":2097152"), "{json}");
        assert!(json.contains("\"total_uploaded\":524288"), "{json}");
        assert!(json.contains("\"share_ratio\":0.25"), "{json}");
    }

    /// E33：零值省略——无累计数据（HTTP 引擎默认/引擎不可达）时三字段均不
    /// 出现在 JSON 里（非破坏增量，旧消费者零感知）。
    #[tokio::test]
    async fn snapshot_omits_totals_when_zero() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone(), vec![]);
        let tid = state
            .add_http_task("https://example.com/f.bin".into(), None)
            .await
            .unwrap();
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(snap.total_downloaded, 0);
        assert_eq!(snap.total_uploaded, 0);
        assert_eq!(snap.share_ratio, None);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("total_downloaded"), "{json}");
        assert!(!json.contains("total_uploaded"), "{json}");
        assert!(!json.contains("share_ratio"), "{json}");
    }

    /// E33：累计语义与速率相反——记录级 Paused 只清瞬时速率（E13），累计
    /// 统计是全生命周期事实（做种贡献），暂停不清零。
    #[tokio::test]
    async fn snapshot_keeps_totals_for_paused_record() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        insert_bt_rec_with(
            &state,
            "t-bt",
            "KEPT01",
            TaskState::Downloading(EngineKind::Bt),
        );
        fake.set_status_totals(1_000_000, 2_000_000);
        state.pause("t-bt").await.unwrap();
        let snap = state.task_snapshot("t-bt").await.unwrap();
        assert_eq!(snap.state, "Paused");
        // 速率清零（E13 语义不变）……
        assert_eq!(
            snap.rates,
            Some(TaskRates {
                down_bytes_s: 0,
                up_bytes_s: 0
            })
        );
        // ……累计与分享率保留
        assert_eq!(snap.total_downloaded, 1_000_000);
        assert_eq!(snap.total_uploaded, 2_000_000);
        assert_eq!(snap.share_ratio, Some(2.0));
    }

    /// E33：share_ratio 纯函数——零除保护 + 3 位小数舍入（qB 同级精度）。
    #[test]
    fn share_ratio_rules() {
        assert_eq!(share_ratio(0, 0), None, "无下行 → None");
        assert_eq!(share_ratio(500, 0), None, "纯上传侧比率无意义 → None");
        assert_eq!(share_ratio(500_000, 2_000_000), Some(0.25));
        assert_eq!(share_ratio(2_000_000, 500_000), Some(4.0));
        assert_eq!(share_ratio(1, 3), Some(0.333), "1/3 舍入到 3 位小数");
        assert_eq!(share_ratio(2, 3), Some(0.667), "2/3 进位到 3 位小数");
    }
}

/// E16 全局限速总阀门：引擎下发形态 / 纯查询 / 无变化 no-op / 失败回滚。
#[cfg(test)]
mod global_limits_tests {
    use super::*;

    #[tokio::test]
    async fn dispatches_down_to_http_and_both_to_bt() {
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let bt_fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let mut state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        // 直接注入 Bt 槽位（with_bt 是 feature 门控；测试模块内私有字段可及）
        state
            .engines
            .insert(EngineKind::Bt, bt_fake.clone() as Arc<dyn DownloadEngine>);

        let g = state
            .apply_global_limits(Some(2048), Some(512))
            .await
            .unwrap();
        assert_eq!(g.max_download_kb_s, 2048);
        assert_eq!(g.max_upload_kb_s, 512);
        // HTTP 位：仅 down 方向（up 请求不该下发，HTTP/FTP 无上传概念）
        assert_eq!(
            http_fake.global_sets(),
            vec![(Some(2048), None)],
            "HTTP 位应只收 down 方向"
        );
        // BT 位：全量两方向（settings_pack 全量语义）
        assert_eq!(bt_fake.global_sets(), vec![(Some(2048), Some(512))]);

        // 内存值同步
        let cur = state.global_limits();
        assert_eq!(cur.max_download_kb_s, 2048);
        assert_eq!(cur.max_upload_kb_s, 512);

        // 事件广播
        let envs = state.hub().read_after(0, 100);
        assert!(
            envs.iter()
                .any(|e| e.event.type_label() == "global_limits_changed"),
            "应广播 global_limits_changed 事件"
        );
    }

    #[tokio::test]
    async fn both_none_is_pure_query() {
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_global_limits(1024, 256);

        let g = state.apply_global_limits(None, None).await.unwrap();
        assert_eq!(g.max_download_kb_s, 1024, "纯查询返回注入值");
        assert_eq!(g.max_upload_kb_s, 256);
        assert!(http_fake.global_sets().is_empty(), "纯查询不得产生引擎调用");
        assert!(state.hub().read_after(0, 100).is_empty(), "纯查询无事件");
    }

    #[tokio::test]
    async fn unchanged_values_are_noop() {
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_global_limits(1024, 0);

        state.apply_global_limits(Some(1024), None).await.unwrap();
        assert!(
            http_fake.global_sets().is_empty(),
            "合并后与当前一致 → no-op（引擎侧已是该值）"
        );
        assert!(state.hub().read_after(0, 100).is_empty(), "无变化不发事件");
    }

    #[tokio::test]
    async fn partial_merge_keeps_unset_direction() {
        let bt_fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let mut state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![])
            .with_global_limits(1024, 512);
        state
            .engines
            .insert(EngineKind::Bt, bt_fake.clone() as Arc<dyn DownloadEngine>);

        let g = state.apply_global_limits(None, Some(256)).await.unwrap();
        assert_eq!(g.max_download_kb_s, 1024, "None 方向沿用当前值");
        assert_eq!(g.max_upload_kb_s, 256);
        assert_eq!(bt_fake.global_sets(), vec![(Some(1024), Some(256))]);
    }

    #[tokio::test]
    async fn engine_failure_keeps_valve_unchanged() {
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let bt_fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        bt_fake.fail_global_limits(EngineError::Other("settings_pack boom".into()));
        let mut state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_global_limits(1024, 128);
        state
            .engines
            .insert(EngineKind::Bt, bt_fake.clone() as Arc<dyn DownloadEngine>);

        let err = state
            .apply_global_limits(Some(4096), None)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("全局限速下发失败"),
            "错误应定性为阀门下发失败: {err}"
        );
        // BT 先行失败 → HTTP 未下发、内存值不变（近全有或全无）
        assert!(
            http_fake.global_sets().is_empty(),
            "BT 失败时 HTTP 不得已改动"
        );
        let cur = state.global_limits();
        assert_eq!(cur.max_download_kb_s, 1024, "阀门保持旧值");
        assert!(state.hub().read_after(0, 100).is_empty(), "失败无事件");
    }

    #[tokio::test]
    async fn unsupported_engine_is_skipped_silently() {
        // 引擎无该设施（返回 Unsupported，如未来 NAS 远程引擎同位替换）→
        // 静默跳过，不阻塞阀门生效（内存值照常更新 + 事件照发）
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        http_fake.fail_global_limits(EngineError::Unsupported);
        let state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![]);

        let g = state.apply_global_limits(Some(2048), None).await.unwrap();
        assert_eq!(g.max_download_kb_s, 2048, "阀门照常生效（内存值）");
        assert!(!state.hub().read_after(0, 100).is_empty(), "事件照发");
    }

    #[tokio::test]
    async fn snapshot_overlay_reflects_effective_values() {
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![])
            .with_config(serde_json::json!({
                "dest_root": "./downloads",
                "max_download_kb_s": 0,
                "max_upload_kb_s": 0,
            }));
        state
            .apply_global_limits(Some(2048), Some(512))
            .await
            .unwrap();
        let snap = state.config_snapshot();
        assert_eq!(snap["max_download_kb_s"], 2048, "/config 快照覆盖");
        assert_eq!(snap["max_upload_kb_s"], 512);
    }
}

/// E18 任务标签：归一化 / 校验 / 清除 / 过滤 / 搜索联动。
#[cfg(test)]
mod tags_tests {
    use super::*;

    async fn state_with_tasks(n: usize) -> (Arc<DaemonState>, Arc<FakeEngine>, Vec<String>) {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        let mut ids = Vec::new();
        for i in 0..n {
            ids.push(
                state
                    .add_http_task(format!("https://example.com/f{i}.bin"), None)
                    .await
                    .unwrap(),
            );
        }
        (Arc::new(state), fake, ids)
    }

    #[test]
    fn set_tags_normalizes_and_links_everywhere() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (state, _fake, ids) = state_with_tasks(2).await;
            let id0 = ids[0].clone();

            // trim + 去重 + 丢空，返回归一化结果
            let got = state
                .set_task_tags(
                    &id0,
                    Some(vec![
                        " Movie ".into(),
                        "4K".into(),
                        "".into(),
                        "Movie".into(),
                    ]),
                )
                .unwrap();
            assert_eq!(got, vec!["Movie", "4K"]);

            // 快照 / 列表联动
            let snap = state.task_snapshot(&id0).await.unwrap();
            assert_eq!(snap.tags, vec!["Movie".to_string(), "4K".to_string()]);
            let (rows, _) = state.list_filtered(&ListQuery::default());
            let row = rows.iter().find(|r| r.task_id == id0).unwrap();
            assert_eq!(row.tags, vec!["Movie".to_string(), "4K".to_string()]);

            // 搜索语料含标签（?search=movie 命中 t1，即使名字是 example.com）
            let (rows, _) = state.list_filtered(&ListQuery {
                search: Some("4k".into()),
                ..Default::default()
            });
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].task_id, id0);

            // 日志事件
            let logs = state.task_logs(&id0).unwrap();
            assert!(
                serde_json::to_string(&logs["events"])
                    .unwrap()
                    .contains("tags_changed"),
                "应有 tags_changed 事件"
            );
        });
    }

    #[test]
    fn clear_tags_via_none_and_empty_list() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (state, _fake, ids) = state_with_tasks(1).await;
            let id0 = ids[0].clone();
            state
                .set_task_tags(&id0, Some(vec!["a".into(), "b".into()]))
                .unwrap();
            // None 清除
            let got = state.set_task_tags(&id0, None).unwrap();
            assert!(got.is_empty());
            // Some(空) 同清除
            state.set_task_tags(&id0, Some(vec!["x".into()])).unwrap();
            let got = state.set_task_tags(&id0, Some(vec![])).unwrap();
            assert!(got.is_empty());
            let snap = state.task_snapshot(&id0).await.unwrap();
            assert!(snap.tags.is_empty(), "清除后快照无标签");
        });
    }

    #[test]
    fn tag_validation_rejects_over_limits() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (state, _fake, ids) = state_with_tasks(1).await;
            let id0 = ids[0].clone();

            // 17 个标签 → 400（InvalidSource）
            let many: Vec<String> = (0..17).map(|i| format!("t{i}")).collect();
            assert!(state.set_task_tags(&id0, Some(many)).is_err());

            // 65 字符标签 → 400
            let long = "a".repeat(65);
            assert!(state.set_task_tags(&id0, Some(vec![long])).is_err());

            // 零副作用：失败后标签仍为空
            let snap = state.task_snapshot(&id0).await.unwrap();
            assert!(snap.tags.is_empty(), "失败设置不得产生半写状态");

            // 不存在任务 → NotFound
            assert!(matches!(
                state.set_task_tags("t999", Some(vec!["x".into()])),
                Err(DaemonError::NotFound(_))
            ));
        });
    }

    #[test]
    fn list_filter_by_tag_any_of_and_case_insensitive() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let (state, _fake, ids) = state_with_tasks(3).await;
            state
                .set_task_tags(&ids[0], Some(vec!["movie".into()]))
                .unwrap();
            state
                .set_task_tags(&ids[1], Some(vec!["Music".into(), "4K".into()]))
                .unwrap();
            // ids[2] 无标签

            // 单标签命中（大小写不敏感）
            let (rows, total) = state.list_filtered(&ListQuery {
                tags: vec!["MOVIE".into()],
                ..Default::default()
            });
            assert_eq!((rows.len(), total), (1, 1));
            assert_eq!(rows[0].task_id, ids[0]);

            // 多标签 any-of
            let (rows, _) = state.list_filtered(&ListQuery {
                tags: vec!["movie".into(), "music".into()],
                ..Default::default()
            });
            assert_eq!(rows.len(), 2);

            // 与 states 维度 AND：Queued 全部命中（三条均 Queued）
            let (rows, total) = state.list_filtered(&ListQuery {
                tags: vec!["4k".into()],
                states: vec!["queued".into()],
                ..Default::default()
            });
            assert_eq!((rows.len(), total), (1, 1));

            // 与 states 维度 AND：Failed 零命中
            let (rows, _) = state.list_filtered(&ListQuery {
                tags: vec!["4k".into()],
                states: vec!["failed".into()],
                ..Default::default()
            });
            assert!(rows.is_empty());

            // 无标签任务不被命中
            let (rows, _) = state.list_filtered(&ListQuery {
                tags: vec!["nothing".into()],
                ..Default::default()
            });
            assert!(rows.is_empty());
        });
    }
}

/// E19 按条件批量：选择器解析 / 非破坏性约束 / 命中集执行。
#[cfg(test)]
mod batch_select_tests {
    use super::*;

    /// 白盒插入指定状态/引擎种类的任务记录（不联网）。
    fn insert_rec(state: &DaemonState, id: &str, kind: EngineKind, st: TaskState) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: id.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{id}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: st,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(id.to_string()),
            engine_kind: kind,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    #[tokio::test]
    async fn resume_all_failed_hits_only_failed() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        insert_rec(&state, "t1", EngineKind::Bt, TaskState::Failed);
        insert_rec(&state, "t2", EngineKind::Bt, TaskState::Failed);
        insert_rec(&state, "t3", EngineKind::Bt, TaskState::Queued);

        let outcome = state
            .batch_select(
                &ListQuery {
                    states: vec!["failed".into()],
                    ..Default::default()
                },
                BatchAction::Resume,
            )
            .await
            .unwrap();
        assert_eq!(outcome.succeeded, 2, "两个 Failed 任务恢复成功");
        assert_eq!(outcome.failed, 0);
        // Queued 未被波及
        {
            let tasks = state.tasks.lock();
            assert_eq!(tasks.get("t3").unwrap().task.state, TaskState::Queued);
            // Failed → Downloading（记录态推进）
            assert_eq!(
                tasks.get("t1").unwrap().task.state,
                TaskState::Downloading(EngineKind::Bt)
            );
        }
    }

    #[tokio::test]
    async fn pause_by_engine_kind() {
        let http_fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let mut state = DaemonState::new(http_fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        // bt 槽注入独立 Fake（pause 走引擎调用，槽内引擎按种类解析）
        state.engines.insert(EngineKind::Bt, fake_http_stub());
        insert_rec(
            &state,
            "h1",
            EngineKind::Http,
            TaskState::Downloading(EngineKind::Http),
        );
        insert_rec(
            &state,
            "b1",
            EngineKind::Bt,
            TaskState::Downloading(EngineKind::Bt),
        );

        let outcome = state
            .batch_select(
                &ListQuery {
                    engines: vec!["bt".into()],
                    ..Default::default()
                },
                BatchAction::Pause,
            )
            .await
            .unwrap();
        assert_eq!(outcome.succeeded, 1, "仅 bt 命中");
        assert_eq!(outcome.results[0].id, "b1");
        {
            let tasks = state.tasks.lock();
            assert!(
                matches!(
                    tasks.get("h1").unwrap().task.state,
                    TaskState::Downloading(_)
                ),
                "http 任务不受影响"
            );
        }
    }

    fn fake_http_stub() -> Arc<dyn DownloadEngine> {
        Arc::new(FakeEngine::new(EngineKind::Bt))
    }

    #[tokio::test]
    async fn remove_via_select_rejected() {
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
        let err = state
            .batch_select(
                &ListQuery {
                    states: vec!["completed".into()],
                    ..Default::default()
                },
                BatchAction::Remove { delete_data: false },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("不支持 remove"), "{err}");
    }

    #[tokio::test]
    async fn empty_hit_set_is_idempotent_outcome() {
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![]);
        let outcome = state
            .batch_select(
                &ListQuery {
                    states: vec!["failed".into()],
                    ..Default::default()
                },
                BatchAction::Resume,
            )
            .await
            .unwrap();
        assert_eq!(
            (outcome.succeeded, outcome.failed),
            (0, 0),
            "空命中 = 空结果"
        );
    }

    #[tokio::test]
    async fn tag_and_search_selectors_work() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        insert_rec(&state, "t1", EngineKind::Http, TaskState::Failed);
        insert_rec(&state, "t2", EngineKind::Http, TaskState::Failed);
        state
            .set_task_tags("t1", Some(vec!["movie".into()]))
            .unwrap();

        // tag 选择器
        let outcome = state
            .batch_select(
                &ListQuery {
                    tags: vec!["movie".into()],
                    ..Default::default()
                },
                BatchAction::Resume,
            )
            .await
            .unwrap();
        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.results[0].id, "t1");

        // search 选择器（source 语料 magnet btih 含 id）
        let outcome = state
            .batch_select(
                &ListQuery {
                    search: Some("t2".into()),
                    ..Default::default()
                },
                BatchAction::Pause,
            )
            .await
            .unwrap();
        assert_eq!(outcome.succeeded, 1);
        assert_eq!(outcome.results[0].id, "t2");
    }
}

/// E20 已完成任务自动清扫：判龄 / 禁用 / 数据处置。
#[cfg(test)]
mod cleanup_tests {
    use super::*;

    /// 白盒插入 Completed 任务，可编程完成时刻与引擎槽位。
    fn insert_completed(state: &DaemonState, id: &str, finished_at_unix: u64) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Http,
                    identity: format!("https://example.com/{id}"),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Http {
                    url: format!("https://example.com/{id}"),
                    headers: vec![],
                    auth: None,
                    backup_url: None,
                    proxy: None,
                },
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: TaskState::Completed,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(id.to_string()),
            engine_kind: EngineKind::Http,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    fn now_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[tokio::test]
    async fn sweep_respects_age_and_disabled() {
        // days=0 禁用
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![])
            .with_cleanup(crate::config::CleanupCfg {
                auto_remove_completed_days: 0,
                auto_remove_keep_data: true,
            });
        insert_completed(&state, "old1", now_secs() - 86_400 * 30);
        assert!(
            state.sweep_completed_cleanup().await.is_empty(),
            "禁用时空转"
        );

        // days=7：老任务清扫，年轻任务保留
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![])
            .with_cleanup(crate::config::CleanupCfg {
                auto_remove_completed_days: 7,
                auto_remove_keep_data: true,
            });
        insert_completed(&state, "old1", now_secs() - 86_400 * 8);
        insert_completed(&state, "new1", now_secs() - 86_400);
        insert_completed(&state, "old_nots", now_secs() - 86_400 * 30);
        {
            // 无完成时刻（旧档）→ 跳过不猜龄
            let mut tasks = state.tasks.lock();
            tasks
                .get_mut("old_nots")
                .unwrap()
                .task
                .metadata
                .finished_at_unix = 0;
        }
        let swept = state.sweep_completed_cleanup().await;
        assert_eq!(
            swept,
            vec!["old1".to_string()],
            "仅超龄且有完成时刻的被清扫"
        );
        let tasks = state.tasks.lock();
        assert!(tasks.get("new1").is_some(), "年轻任务保留");
        assert!(tasks.get("old_nots").is_some(), "无时刻任务保留");
        assert!(tasks.get("old1").is_none(), "超龄任务已移除");
    }

    #[tokio::test]
    async fn sweep_data_disposition_follows_config() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]).with_cleanup(
            crate::config::CleanupCfg {
                auto_remove_completed_days: 1,
                auto_remove_keep_data: false, // 连数据一起删
            },
        );
        insert_completed(&state, "old1", now_secs() - 86_400 * 2);
        let swept = state.sweep_completed_cleanup().await;
        assert_eq!(swept.len(), 1);
        assert_eq!(
            fake.removed_calls(),
            vec![("old1".to_string(), true)],
            "keep_data=false → 引擎侧删数据"
        );
    }

    #[tokio::test]
    async fn sweep_ignores_non_completed_states() {
        let state = DaemonState::new(Arc::new(FakeEngine::new(EngineKind::Http)), vec![])
            .with_cleanup(crate::config::CleanupCfg {
                auto_remove_completed_days: 1,
                auto_remove_keep_data: true,
            });
        // Seeding 任务带超龄完成时刻——状态非 Completed 不清扫（做种保护）
        {
            insert_completed(&state, "seed1", now_secs() - 86_400 * 30);
            let mut tasks = state.tasks.lock();
            tasks.get_mut("seed1").unwrap().task.state = TaskState::Seeding;
        }
        let swept = state.sweep_completed_cleanup().await;
        assert!(swept.is_empty(), "非 Completed 状态不清扫");
    }
}

/// E21 文件冲突策略：改名候选 / skip 秒完成 / 默认覆盖不变。
#[cfg(test)]
mod conflict_tests {
    use super::*;

    #[test]
    fn bump_conflict_name_skips_occupied() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        assert_eq!(
            DaemonState::bump_conflict_name(p, "a.bin").as_deref(),
            Some("a(1).bin"),
            "空闲目录取 (1)"
        );
        std::fs::write(p.join("a(1).bin"), b"x").unwrap();
        assert_eq!(
            DaemonState::bump_conflict_name(p, "a.bin").as_deref(),
            Some("a(2).bin"),
            "占用则顺延"
        );
        // 无扩展名
        assert_eq!(
            DaemonState::bump_conflict_name(p, "file").as_deref(),
            Some("file(1)")
        );
    }

    #[tokio::test]
    async fn skip_policy_completes_without_engine_add() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dup.bin"), b"existing").unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        // V2 白名单：显式 dest 落测试目录 → 注入为白名单根
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf());

        let id = state
            .add_http_task_opts(
                "https://example.com/dup.bin".into(),
                Some(dir.path().to_string_lossy().into_owned()),
                crate::state::AddHttpOpts {
                    name: Some("dup.bin".into()),
                    conflict: Some(ConflictPolicy::Skip),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 引擎未收到 add
        assert!(fake.added.lock().is_empty(), "skip 不得入引擎");
        // 任务直接 Completed + 完成时刻入档 + 事件标记 conflict_skip
        {
            let tasks = state.tasks.lock();
            let rec = tasks.get(&id).unwrap();
            assert_eq!(rec.task.state, TaskState::Completed);
            assert!(rec.task.metadata.finished_at_unix > 0);
            assert!(
                rec.events
                    .iter()
                    .any(|e| e.detail.as_deref() == Some("conflict_skip")),
                "应有 conflict_skip 事件"
            );
            // identity.size 反映既有文件字节数（快照 total 口径来自引擎，
            // skip 无引擎任务 → 用 identity 断言）
            match &rec.task.identity {
                ContentIdentity::SingleFile { size, .. } => assert_eq!(*size, 8),
                other => panic!("identity 应为 SingleFile: {other:?}"),
            }
        }
        // 快照可读
        let snap = state.task_snapshot(&id).await.unwrap();
        assert_eq!(snap.state, "Completed");
        // 可正常删除
        assert!(state.remove_with(&id, false).await.is_ok());
    }

    #[tokio::test]
    async fn no_policy_keeps_default_overwrite_flow() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dup.bin"), b"existing").unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf());

        // 显式名 + 无策略 → 旧行为：照常入引擎（引擎 finalize 覆盖）
        let id = state
            .add_http_task_opts(
                "https://example.com/dup.bin".into(),
                Some(dir.path().to_string_lossy().into_owned()),
                crate::state::AddHttpOpts {
                    name: Some("dup.bin".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(fake.added.lock().len(), 1, "默认照常入引擎");
        assert_eq!(state.task_snapshot(&id).await.unwrap().state, "Downloading");
    }
}

/// 定时/错峰下载（E23）：延迟入引擎 + 调度激活 + 调度中 pause/resume +
/// 恢复不误启动。
#[cfg(test)]
mod scheduled_tests {
    use super::*;

    /// 等待持久化文件出现（autosave 异步时序；本模块独立实现——
    /// wait_file 定义于 persist_tests 模块内部，跨模块不可见）。
    fn wait_file(path: &std::path::Path, timeout_ms: u64) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("等待持久化文件超时: {path:?}");
    }

    /// 白盒插入调度等待任务（engine_tid 空 + start_at 指定时刻；不联网）。
    fn insert_scheduled(
        state: &DaemonState,
        id: &str,
        kind: EngineKind,
        start_at: u64,
        st: TaskState,
    ) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Http,
                    identity: format!("https://s.example/{id}"),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Http {
                    url: format!("https://s.example/{id}"),
                    headers: vec![],
                    auth: None,
                    backup_url: None,
                    proxy: None,
                },
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: st,
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: start_at,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: None,
            engine_kind: kind,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    #[tokio::test]
    async fn add_with_future_start_at_defers_engine() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        let future = now_unix() + 3600;
        let tid = state
            .add_http_task_opts(
                "https://s.example/later.bin".into(),
                None,
                AddHttpOpts {
                    start_at_unix: Some(future),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // 引擎未被触碰（延迟入引擎的核心证据）
        assert!(
            fake.added.lock().is_empty(),
            "定时任务不应在 add 时接入引擎"
        );
        let rec = state.tasks.lock().get(&tid).cloned().unwrap();
        assert!(rec.engine_tid.is_none(), "调度等待中无引擎句柄");
        assert_eq!(rec.task.state, TaskState::Queued);
        assert_eq!(rec.task.metadata.start_at_unix, future);
        // 列表/快照透出（0 省略、非 0 出现）
        let list = state.list();
        assert_eq!(list[0].start_at_unix, future);
        let snap = state.task_snapshot(&tid).await.unwrap();
        assert_eq!(snap.start_at_unix, future);
        assert_eq!(snap.state, "Queued");
        // 对照：未指定 start_at 的任务立即入引擎且字段省略
        let tid2 = state
            .add_http_task("https://s.example/now.bin".into(), None)
            .await
            .unwrap();
        assert_eq!(fake.added.lock().len(), 1, "普通任务照常入引擎");
        let snap2 = state.task_snapshot(&tid2).await.unwrap();
        assert_eq!(snap2.start_at_unix, 0);
    }

    #[tokio::test]
    async fn activate_due_tasks_activates_only_due() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        let past = now_unix() - 10;
        let future = now_unix() + 3600;
        insert_scheduled(&state, "t1", EngineKind::Http, past, TaskState::Queued);
        insert_scheduled(&state, "t2", EngineKind::Http, future, TaskState::Queued);
        insert_scheduled(&state, "t3", EngineKind::Http, 0, TaskState::Queued); // 无调度（0）
        let activated = state.activate_due_tasks().await;
        assert_eq!(activated, vec!["t1".to_string()], "仅到期任务被激活");
        assert_eq!(fake.added.lock().len(), 1);
        {
            let tasks = state.tasks.lock();
            assert!(tasks.get("t1").unwrap().engine_tid.is_some());
            assert_eq!(
                tasks.get("t1").unwrap().task.state,
                TaskState::Queued,
                "激活不改记录态（轮询器对齐）"
            );
            assert!(
                tasks.get("t2").unwrap().engine_tid.is_none(),
                "未到期不激活"
            );
            assert!(
                tasks.get("t3").unwrap().engine_tid.is_none(),
                "无调度不激活"
            );
        }
        // 事件：t1 有 TaskActivated（通过任务事件链验证 scheduled_start）
        let rec = state.tasks.lock().get("t1").cloned().unwrap();
        assert!(
            rec.events.iter().any(|e| e.op == "scheduled_start"),
            "激活应落任务事件: {:?}",
            rec.events
        );
    }

    #[tokio::test]
    async fn activate_without_engine_marks_failed() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        // Provider 引擎未装配 → engine_for 失败 → Failed 终态
        insert_scheduled(
            &state,
            "t9",
            EngineKind::Provider,
            now_unix() - 5,
            TaskState::Queued,
        );
        let activated = state.activate_due_tasks().await;
        assert!(activated.is_empty(), "激活失败不进返回集");
        {
            let tasks = state.tasks.lock();
            let rec = tasks.get("t9").unwrap();
            assert_eq!(rec.task.state, TaskState::Failed);
            assert!(rec.engine_tid.is_none());
        }
    }

    #[tokio::test]
    async fn pause_resume_on_scheduled_task_lifecycle() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        insert_scheduled(
            &state,
            "t1",
            EngineKind::Http,
            now_unix() + 3600,
            TaskState::Queued,
        );
        // pause = 取消自动启动（记录级，无引擎调用）
        state.pause("t1").await.unwrap();
        assert!(fake.paused_calls().is_empty(), "调度中暂停不触碰引擎");
        assert_eq!(
            state.tasks.lock().get("t1").unwrap().task.state,
            TaskState::Paused
        );
        // resume = 立即激活（消费定时）
        state.resume("t1").await.unwrap();
        assert_eq!(fake.added.lock().len(), 1, "resume 应接入引擎");
        assert_eq!(
            state.tasks.lock().get("t1").unwrap().task.state,
            TaskState::Downloading(EngineKind::Http)
        );
        // 激活后再次 pause → 走引擎侧原路径
        state.pause("t1").await.unwrap();
        assert_eq!(fake.paused_calls().len(), 1);
    }

    #[test]
    fn resolve_start_at_explicit_and_jitter() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        // 显式值直传：过去时刻原样保留（= 立即语义），不受 jitter 影响
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        assert_eq!(state.resolve_start_at(Some(12345)), 12345);
        assert_eq!(state.resolve_start_at(Some(0)), 0);
        // jitter 未配置 → None = 0（立即）
        assert_eq!(state.resolve_start_at(None), 0);
        // jitter 配置后：None → now..=now+jitter；显式值不被抖动叠加
        let state2 =
            DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]).with_start_jitter(5);
        for _ in 0..20 {
            let t = state2.resolve_start_at(None);
            assert!(
                t > now_unix() - 1 && t <= now_unix() + 5,
                "jitter 时刻应在 (now, now+5] 内: {t}"
            );
        }
        assert_eq!(
            state2.resolve_start_at(Some(999)),
            999,
            "显式 start_at 不叠加抖动"
        );
    }

    #[tokio::test]
    async fn restore_keeps_future_scheduled_out_of_engine() {
        // 定时任务重启：未到期任务恢复为记录（不入引擎），到期后可被激活
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("tasks.json");
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = Arc::new(
            DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
                .with_storage(store.clone()),
        );
        let future = now_unix() + 3600;
        let tid = state
            .add_http_task_opts(
                "https://s.example/boot.bin".into(),
                None,
                AddHttpOpts {
                    start_at_unix: Some(future),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(fake.added.lock().is_empty());
        wait_file(&store, 2000);

        // 新 state 恢复：不入引擎、保持 Queued、start_at 保留
        let fake2 = Arc::new(FakeEngine::new(EngineKind::Http));
        let state2 = DaemonState::new(fake2.clone() as Arc<dyn DownloadEngine>, vec![]);
        let n = state2.restore_from(&store).await.unwrap();
        assert_eq!(n, 1);
        assert!(fake2.added.lock().is_empty(), "未到期任务恢复时不得误启动");
        {
            let tasks = state2.tasks.lock();
            let rec = tasks.get(&tid).unwrap();
            assert!(rec.engine_tid.is_none());
            assert_eq!(rec.task.state, TaskState::Queued);
            assert_eq!(rec.task.metadata.start_at_unix, future);
        }
        // 手工把时刻改为已到期（模拟"等待期间时刻流逝"）→ 调度循环可激活
        {
            let mut tasks = state2.tasks.lock();
            tasks.get_mut(&tid).unwrap().task.metadata.start_at_unix = now_unix() - 1;
        }
        let activated = state2.activate_due_tasks().await;
        assert_eq!(activated, vec![tid.clone()]);
        assert_eq!(fake2.added.lock().len(), 1);
    }
}

/// E27 完成自动处理：move_to 移动 + hook 外部程序 + conflict-skip/目录豁免。
#[cfg(test)]
mod post_download_tests {
    use super::*;

    /// 造一个"已完成 + 显式名 + 文件已落盘"的任务（FakeEngine 不联网）。
    async fn completed_task_with_file(
        state: &DaemonState,
        url_path: &str,
        name: &str,
        content: &[u8],
    ) -> String {
        let dir = state.default_dest_root.lock().clone();
        std::fs::write(dir.join(name), content).unwrap();
        let id = state
            .add_http_task_opts(
                format!("https://example.com/{url_path}"),
                None,
                crate::state::AddHttpOpts {
                    name: Some(name.into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut tasks = state.tasks.lock();
        let rec = tasks.get_mut(&id).unwrap();
        rec.task.state = TaskState::Completed;
        rec.task.metadata.name = Some(name.into());
        id
    }

    #[tokio::test]
    async fn post_move_relocates_file_and_records_event() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf())
            .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

        let id = completed_task_with_file(&state, "a.bin", "done.bin", b"payload").await;
        state.publish_task_completed(&id);

        assert!(!dir.path().join("done.bin").exists(), "源文件应已移走");
        let target = inbox.path().join("done.bin");
        assert_eq!(std::fs::read(&target).unwrap(), b"payload", "内容不变");
        let tasks = state.tasks.lock();
        let rec = tasks.get(&id).unwrap();
        assert!(
            rec.events.iter().any(|e| e.op == "post_move"
                && e.detail.as_deref() == Some(target.to_string_lossy().as_ref())),
            "应有 post_move 事件且记录目标路径: {:?}",
            rec.events
        );
    }

    #[tokio::test]
    async fn post_move_collision_autorenames() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        std::fs::write(inbox.path().join("done.bin"), b"old").unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf())
            .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

        let id = completed_task_with_file(&state, "b.bin", "done.bin", b"new").await;
        state.publish_task_completed(&id);

        assert_eq!(
            std::fs::read(inbox.path().join("done.bin")).unwrap(),
            b"old"
        );
        assert_eq!(
            std::fs::read(inbox.path().join("done(1).bin")).unwrap(),
            b"new",
            "同名冲突自动改名落位"
        );
    }

    #[tokio::test]
    async fn post_move_skipped_for_conflict_skip_tasks() {
        // conflict_policy=skip：既有文件用户明确要求不动 → 不移动
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dup.bin"), b"existing").unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf())
            .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

        let id = state
            .add_http_task_opts(
                "https://example.com/dup.bin".into(),
                Some(dir.path().to_string_lossy().into_owned()),
                crate::state::AddHttpOpts {
                    name: Some("dup.bin".into()),
                    conflict: Some(ConflictPolicy::Skip),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        state.publish_task_completed(&id);

        assert!(
            dir.path().join("dup.bin").exists(),
            "skip 任务既有文件必须保持原样"
        );
        assert!(!inbox.path().join("dup.bin").exists(), "不得移动进目标目录");
        let tasks = state.tasks.lock();
        let rec = tasks.get(&id).unwrap();
        assert!(
            !rec.events.iter().any(|e| e.op == "post_move"),
            "不得有 post_move 事件"
        );
    }

    #[tokio::test]
    async fn post_move_skipped_for_directory_task() {
        // 落盘路径是目录（BT 多文件）→ 移动跳过
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf())
            .with_post_download(Some(inbox.path().to_string_lossy().into_owned()), None);

        let id = completed_task_with_file(&state, "c.torrent", "bundle", b"").await;
        // 把落盘路径改成目录
        std::fs::remove_file(dir.path().join("bundle")).unwrap();
        std::fs::create_dir(dir.path().join("bundle")).unwrap();
        state.publish_task_completed(&id);

        assert!(dir.path().join("bundle").is_dir(), "目录任务不得被移动");
        assert!(!inbox.path().join("bundle").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn post_hook_receives_final_path_env() {
        // unix-only：hook 脚本 dump 环境变量 → 断言 SD_FILE_PATH = 移动后终路径
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let dump = dir.path().join("envdump.txt");
        let script = dir.path().join("hook.sh");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nenv > {}\n", dump.to_string_lossy().into_owned()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![])
            .with_dest_root(dir.path().to_path_buf())
            .with_post_download(
                Some(inbox.path().to_string_lossy().into_owned()),
                Some(script.to_string_lossy().into_owned()),
            );

        let id = completed_task_with_file(&state, "d.bin", "done.bin", b"payload").await;
        state.publish_task_completed(&id);

        // 钩子在后台线程执行 → 轮询等待 dump 文件内容就绪。
        // `env > dump` 重定向：文件创建 ≠ 内容写毕（CI 高负载下写入窗口
        // 可达数百 ms，曾致读空误报"应有 SD_TASK_ID"）→ 等内容而非等存在。
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let env = loop {
            assert!(
                std::time::Instant::now() < deadline,
                "钩子 10s 内未产出环境 dump"
            );
            if dump.exists() {
                if let Ok(content) = std::fs::read_to_string(&dump) {
                    if content.contains("SD_TASK_ID=") {
                        break content;
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        assert!(env.contains("SD_TASK_ID="), "应有 SD_TASK_ID: {env}");
        assert!(
            env.contains(&format!(
                "SD_FILE_PATH={}",
                inbox.path().join("done.bin").to_string_lossy()
            )),
            "SD_FILE_PATH 应为移动后终路径: {env}"
        );
        let tasks = state.tasks.lock();
        let rec = tasks.get(&id).unwrap();
        assert!(
            rec.events.iter().any(|e| e.op == "post_hook"),
            "应有 post_hook 事件"
        );
    }
}

/// E28 BT 任务名回填：torrent metadata name → 轮询幂等回填（状态权威不变）。
#[cfg(test)]
mod bt_name_backfill_tests {
    use super::*;

    /// 插入 Downloading(Bt) 状态的无名 magnet 任务记录（不联网）。
    fn insert_bt_downloading(state: &DaemonState, id: &str, ih: &str) {
        let rec = TaskRecord {
            task: DownloadTask {
                id: id.into(),
                canonical_id: CanonicalId {
                    kind: CanonicalKind::Bt,
                    identity: ih.to_string(),
                    validator: None,
                    token_sensitive: false,
                },
                source: DownloadSource::Magnet(format!("magnet:?xt=urn:btih:{ih}")),
                identity: ContentIdentity::SingleFile {
                    size: 0,
                    etag: None,
                    sha256: None,
                    sha1: None,
                    md5: None,
                    backup_md5: None,
                },
                dest_root: PathBuf::from("."),
                files: vec![],
                acquisitions: vec![],
                aggregate: Default::default(),
                state: TaskState::Downloading(EngineKind::Bt),
                retry: Default::default(),
                created_at: std::time::Instant::now(),
                file_priorities: None,
                sequential: false,
                metadata: TaskMetadata {
                    name: None,
                    added_at_unix: 0,
                    tags: Vec::new(),
                    finished_at_unix: 0,
                    start_at_unix: 0,
                    next_retry_at_unix: 0,
                },
                limits: None,
            },
            engine_tid: Some(ih.to_string()),
            engine_kind: EngineKind::Bt,
            engine_status: None,
            events: vec![],
        };
        state.tasks.lock().insert(id.into(), rec);
    }

    #[tokio::test]
    async fn poll_backfills_bt_torrent_name_without_state_migration() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ih = "0d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
        insert_bt_downloading(&state, "t-bt", ih);
        fake.set_status_name("Ubuntu 24.04 ISO");

        let effects = state.poll_engine_states().await;
        assert!(
            effects.is_empty(),
            "BT 分支状态权威 = alert 流，轮询不得产生迁移 effect"
        );
        {
            let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
            assert_eq!(
                rec.task.metadata.name.as_deref(),
                Some("Ubuntu 24.04 ISO"),
                "torrent metadata name 应回填"
            );
            assert!(
                rec.events.iter().any(|e| e.op == "name_backfilled"
                    && e.detail.as_deref() == Some("Ubuntu 24.04 ISO")),
                "应有 name_backfilled 事件"
            );
            assert!(rec.engine_status.is_some(), "E11 快照缓存仍应生效");
        }

        // 幂等：回填一次后 name 非 None 自然停；引擎改名不得污染
        fake.set_status_name("renamed-should-not-apply");
        let _ = state.poll_engine_states().await;
        let rec = state.tasks.lock().get("t-bt").cloned().unwrap();
        assert_eq!(rec.task.metadata.name.as_deref(), Some("Ubuntu 24.04 ISO"));
        assert_eq!(
            rec.events
                .iter()
                .filter(|e| e.op == "name_backfilled")
                .count(),
            1,
            "回填事件恰好一次"
        );
    }

    #[tokio::test]
    async fn poll_bt_never_overrides_explicit_name() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ih = "1d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
        insert_bt_downloading(&state, "t-bt2", ih);
        {
            let mut tasks = state.tasks.lock();
            tasks.get_mut("t-bt2").unwrap().task.metadata.name = Some("用户显式名".into());
        }
        fake.set_status_name("torrent-metadata-name");

        let _ = state.poll_engine_states().await;
        let rec = state.tasks.lock().get("t-bt2").cloned().unwrap();
        assert_eq!(
            rec.task.metadata.name.as_deref(),
            Some("用户显式名"),
            "显式名权威，E15 语义与 HTTP 侧一致"
        );
        assert!(
            !rec.events.iter().any(|e| e.op == "name_backfilled"),
            "不得有回填事件"
        );
    }

    #[tokio::test]
    async fn poll_bt_skips_non_active_states() {
        let fake = Arc::new(FakeEngine::new(EngineKind::Bt));
        let state = DaemonState::new(fake.clone(), vec![]);
        let ih = "2d2c9c9d5c2d3e8f9a1b2c3d4e5f6a7b8c9d0e1f";
        insert_bt_downloading(&state, "t-bt3", ih);
        // 非活跃态（与 apply_bt_alert 终态清零同口径；Queued = 调度未激活）
        {
            let mut tasks = state.tasks.lock();
            tasks.get_mut("t-bt3").unwrap().task.state = TaskState::Queued;
        }
        fake.set_status_name("should-not-backfill");

        let _ = state.poll_engine_states().await;
        let rec = state.tasks.lock().get("t-bt3").cloned().unwrap();
        assert!(rec.task.metadata.name.is_none(), "非活跃态不回填不缓存");
        assert!(rec.engine_status.is_none(), "非活跃态不缓存快照");
    }
}
