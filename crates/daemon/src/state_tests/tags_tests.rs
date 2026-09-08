//! 拆分自 state_tests.rs（技术债 #2 第三步，纯移动零语义改动）。
//! E18 任务标签：归一化 / 校验 / 清除 / 过滤 / 搜索联动。
#![cfg(test)]

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
