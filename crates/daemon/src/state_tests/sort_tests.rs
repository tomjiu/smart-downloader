//! R13 服务端排序（`?sort=&order=`）状态级测试：各键升/降序、平局创建序
//! 决出、ETA 哨兵语义、排序先于分页、仅 order=desc 的最新优先便利语义。
//! 白盒构造 engine_status/显式名（FakeEngine 不联网，避免真实下载竞态）。
#![cfg(test)]

use super::*;

/// 造一个带显式名 + 可选引擎快照占位的任务记录（对齐 bt_rec 构造模式）。
fn rec_named(id: &str, name: Option<&str>, url: &str, state: TaskState) -> TaskRecord {
    TaskRecord {
        seeding_since: None,
        task: DownloadTask {
            id: id.to_string(),
            canonical_id: CanonicalId {
                kind: CanonicalKind::Http,
                identity: url.to_string(),
                validator: None,
                token_sensitive: false,
            },
            source: DownloadSource::Http {
                url: url.to_string(),
                headers: Default::default(),
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
            state,
            retry: Default::default(),
            created_at: std::time::Instant::now(),
            file_priorities: None,
            sequential: false,
            max_connections: None,
            max_uploads: None,
            queue_priority: 0,
            metadata: TaskMetadata {
                name: name.map(str::to_string),
                added_at_unix: 0,
                added_at_ms: 0,
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
    }
}

/// 直插记录（排序测试只关心记录面数据，跳过 add_http_task 引擎交互）。
fn state_with(recs: Vec<TaskRecord>) -> DaemonState {
    let fake = Arc::new(FakeEngine::new(EngineKind::Http));
    let state = DaemonState::new(fake, vec![]);
    {
        let mut tasks = state.tasks.lock();
        for r in recs {
            tasks.insert(r.task.id.clone(), r);
        }
    }
    state
}

fn ids(list: &[TaskSummary]) -> Vec<String> {
    list.iter().map(|r| r.task_id.clone()).collect()
}

fn q(sort: Option<ListSort>, order: ListOrder) -> ListQuery {
    ListQuery {
        sort,
        order,
        ..Default::default()
    }
}

#[test]
fn sort_by_name_asc_desc_with_creation_tiebreak() {
    let recs = vec![
        rec_named("t1", Some("banana"), "https://x/b.bin", TaskState::Queued),
        rec_named("t2", Some("Apple"), "https://x/a.bin", TaskState::Queued),
        // 与 t2 同键（大小写不敏感）——平局创建序决出
        rec_named("t3", Some("apple"), "https://x/dup.bin", TaskState::Queued),
        rec_named("t4", Some("Cherry"), "https://x/c.bin", TaskState::Queued),
    ];
    let state = state_with(recs);

    // 升序：apple(t2) < apple(t3)（平局创建序） < banana < cherry
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Name), ListOrder::Asc));
    assert_eq!(ids(&rows), vec!["t2", "t3", "t1", "t4"]);

    // 降序：主键反转、平局仍创建序（cherry > banana > apple(t2) > apple(t3)）
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Name), ListOrder::Desc));
    assert_eq!(ids(&rows), vec!["t4", "t1", "t2", "t3"]);
}

#[test]
fn sort_by_name_falls_back_to_url_last_segment() {
    // 无显式名：URL 末段派生（zzz.bin vs aaa.bin），与 UI displayName 同链
    let recs = vec![
        rec_named("t1", None, "https://x/zzz.bin", TaskState::Queued),
        rec_named(
            "t2",
            Some("aaa.bin"),
            "https://x/ignored.bin",
            TaskState::Queued,
        ),
    ];
    let state = state_with(recs);
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Name), ListOrder::Asc));
    assert_eq!(ids(&rows), vec!["t2", "t1"], "aaa.bin < zzz.bin");
}

#[test]
fn sort_by_progress_size_speed_uses_engine_status() {
    let mut a = rec_named("t1", Some("a"), "https://x/a", TaskState::Queued);
    let mut b = rec_named("t2", Some("b"), "https://x/b", TaskState::Queued);
    let c = rec_named("t3", Some("c"), "https://x/c", TaskState::Queued); // 无快照 = 0
    a.engine_status = Some(EngineStatus {
        total_done: 50,
        total: 100,
        down_rate: 10,
        ..Default::default()
    });
    b.engine_status = Some(EngineStatus {
        total_done: 90,
        total: 100,
        down_rate: 200,
        ..Default::default()
    });
    let state = state_with(vec![a, b, c]);

    // 进度：b(0.9) > a(0.5) > c(0)
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Progress), ListOrder::Desc));
    assert_eq!(ids(&rows), vec!["t2", "t1", "t3"]);

    // 速率：b(200) > a(10) > c(0)
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Speed), ListOrder::Desc));
    assert_eq!(ids(&rows), vec!["t2", "t1", "t3"]);

    // 大小降序：a(100) = b(100)（平局创建序）> c(0)
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Size), ListOrder::Desc));
    assert_eq!(ids(&rows), vec!["t1", "t2", "t3"]);

    // 大小升序：c(0) < a(t1) = b(t2)
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Size), ListOrder::Asc));
    assert_eq!(ids(&rows), vec!["t3", "t1", "t2"]);
}

#[test]
fn sort_by_eta_sentinel_last_in_asc() {
    let mut a = rec_named("t1", Some("a"), "https://x/a", TaskState::Queued);
    let mut b = rec_named("t2", Some("b"), "https://x/b", TaskState::Queued);
    let mut done = rec_named("t3", Some("done"), "https://x/c", TaskState::Completed);
    let norate = rec_named("t4", Some("norate"), "https://x/d", TaskState::Queued);
    a.engine_status = Some(EngineStatus {
        total_done: 0,
        total: 100,
        down_rate: 10, // eta 10s
        ..Default::default()
    });
    b.engine_status = Some(EngineStatus {
        total_done: 0,
        total: 100,
        down_rate: 100, // eta 1s
        ..Default::default()
    });
    done.engine_status = Some(EngineStatus {
        total_done: 100,
        total: 100,
        down_rate: 50, // 已完成 → 哨兵
        ..Default::default()
    });
    // norate 无快照 → 哨兵
    let state = state_with(vec![a, b, done, norate]);

    // 升序：b(1s) < a(10s) < 哨兵组（t3, t4 创建序）
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Eta), ListOrder::Asc));
    assert_eq!(ids(&rows), vec!["t2", "t1", "t3", "t4"]);

    // 降序：哨兵组到头部（t3, t4 创建序），随后 a(10s) > b(1s)
    let (rows, _) = state.list_filtered(&q(Some(ListSort::Eta), ListOrder::Desc));
    assert_eq!(ids(&rows), vec!["t3", "t4", "t1", "t2"]);
}

#[test]
fn sort_applies_before_pagination() {
    // 排序先于分页：limit/offset 作用于排序后的序列（分页语义成立的核心断言）
    let names = ["d", "b", "a", "c"];
    let recs: Vec<TaskRecord> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            rec_named(
                &format!("t{}", i + 1),
                Some(n),
                "https://x/f",
                TaskState::Queued,
            )
        })
        .collect();
    let state = state_with(recs);

    let (page, total) = state.list_filtered(&ListQuery {
        sort: Some(ListSort::Name),
        order: ListOrder::Asc,
        limit: Some(2),
        offset: 1,
        ..Default::default()
    });
    assert_eq!(total, 4, "total 是过滤后总数（与排序无关）");
    assert_eq!(
        ids(&page),
        vec!["t2", "t4"],
        "offset=1 跳过排序后第 1 项(a=t3)"
    );
}

#[test]
fn order_desc_alone_is_newest_first() {
    // 仅 order=desc（无 sort）：创建序倒序 = 最新优先（便利语义）
    let state = state_with(vec![
        rec_named("t1", Some("a"), "https://x/a", TaskState::Queued),
        rec_named("t2", Some("b"), "https://x/b", TaskState::Queued),
        rec_named("t3", Some("c"), "https://x/c", TaskState::Queued),
    ]);
    let (rows, _) = state.list_filtered(&q(None, ListOrder::Desc));
    assert_eq!(ids(&rows), vec!["t3", "t2", "t1"]);
    // Asc（默认）= 既有创建序
    let (rows, _) = state.list_filtered(&q(None, ListOrder::Asc));
    assert_eq!(ids(&rows), vec!["t1", "t2", "t3"]);
}

#[test]
fn sort_created_desc_matches_order_desc_alone() {
    // sort=created&order=desc 与仅 order=desc 等价（同一语义两条写法）
    let state = state_with(vec![
        rec_named("t1", Some("a"), "https://x/a", TaskState::Queued),
        rec_named("t2", Some("b"), "https://x/b", TaskState::Queued),
    ]);
    let (by_key, _) = state.list_filtered(&q(Some(ListSort::Created), ListOrder::Desc));
    let (by_order, _) = state.list_filtered(&q(None, ListOrder::Desc));
    assert_eq!(ids(&by_key), ids(&by_order));
}

#[test]
fn sort_by_state_uses_label_lexicographic() {
    let a = rec_named(
        "t1",
        Some("a"),
        "https://x/a",
        TaskState::Downloading(EngineKind::Http),
    );
    let b = rec_named("t2", Some("b"), "https://x/b", TaskState::Completed);
    let state = state_with(vec![a, b]);
    let (rows, _) = state.list_filtered(&q(Some(ListSort::State), ListOrder::Asc));
    assert_eq!(
        ids(&rows),
        vec!["t2", "t1"],
        "Completed < Downloading（字典序）"
    );
}

#[test]
fn known_sort_keys_covers_all_variants() {
    // 编译期漏项守卫：ListSort 新增变体时，known_sort_keys 必须同步补字符串
    let keys = known_sort_keys();
    assert_eq!(
        keys,
        vec!["created", "name", "state", "progress", "size", "speed", "eta"]
    );
}
