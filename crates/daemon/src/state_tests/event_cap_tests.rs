//! 审计修复 B1（2026-10-21）回归：任务操作日志环形上限。
//! 长寿命任务 + 高频 pause/resume 循环 → events Vec 曾无限增长
//! （内存 + `/tasks/:id/logs` 全量序列化响应双重无界）。
//! 修复后保留最近 TASK_EVENT_CAP（500）条，时间序保留尾部最新。
#![cfg(test)]

use super::*;

#[test]
fn task_events_capped_at_500() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let fake = Arc::new(FakeEngine::new(EngineKind::Http));
        let state = DaemonState::new(fake.clone() as Arc<dyn DownloadEngine>, vec![]);
        let tid = state
            .add_http_task("https://example.com/cap.bin".to_string(), None)
            .await
            .unwrap();

        // 600 轮 pause/resume：每次各入账 1 条事件 → 1200+ 条原始事件
        for _ in 0..600 {
            state.pause(&tid).await.unwrap();
            state.resume(&tid).await.unwrap();
        }

        let logs = state.task_logs(&tid).unwrap();
        let events = logs["events"].as_array().unwrap();
        assert!(
            events.len() <= 500,
            "events 必须封顶 500，实际 {}",
            events.len()
        );
        // 尾部保留最新：resume 必须是最后一条（第 600 轮的 resume）
        let last = events.last().unwrap();
        assert_eq!(last["op"].as_str().unwrap(), "resume", "尾部应为最新事件");
        // 首条应是早期事件（add 已被挤出，存在中期 pause）
        let first_op = events[0]["op"].as_str().unwrap();
        assert!(
            first_op == "pause" || first_op == "resume",
            "首条应为挤出边界后的操作事件，实际 {first_op}"
        );
    });
}
