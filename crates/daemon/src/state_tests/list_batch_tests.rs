//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E7 任务管理面：列表过滤/分页/确定性排序 + 批量操作 + remove delete_data
//! 透传（FakeEngine 不联网；状态直接改记录白盒构造，避免真实下载竞态）。
#![cfg(test)]

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
