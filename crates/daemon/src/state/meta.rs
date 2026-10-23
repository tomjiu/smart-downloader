//! 元数据域：任务重命名与标签。

use super::*;
impl DaemonState {
    /// 任务重命名（E15）：`POST /tasks/:id/name`。显示层改名——落盘路径在
    /// 引擎 add 时即已决定（httpdl `resolved_name` 决策链），改名不迁移已
    /// 落盘/在传文件；名字是列表/快照透出与 E14 搜索语料（name 分量）。
    /// `None` = 清除显式名（E9 回填仅在"名字为空且引擎报名"的轮询点发生，
    /// 活跃任务下一轮可能自动补回派生名——清除语义即"交还派生链"）。
    /// 事件 detail 只记 set/cleared（与 proxy_changed 同口径，名字本体走
    /// 快照/列表查询，不进事件链路）。
    pub fn set_task_name(&self, id: &str, name: Option<String>) -> Result<(), DaemonError> {
        if let Some(n) = &name {
            if n.trim().is_empty() {
                return Err(DaemonError::InvalidSource(
                    "name 不能为空白（清除语义请传 null）".into(),
                ));
            }
            // V3 终审同函数：与 add 入参同一裁决点（非法路径分量即拒）
            smart_dl_storage::sanitize_rel(n)
                .map_err(|e| DaemonError::InvalidSource(format!("name 非法: {e}")))?;
        }
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let detail = if name.is_some() { "set" } else { "cleared" };
            rec.task.metadata.name = name;
            rec.push_event("name_changed", Some(detail.into()));
        }
        self.autosave();
        Ok(())
    }

    /// 任务标签设置（E18）：**替换式**全量覆盖（请求携带的标签列表即为最终权威，
    /// 语义可预测）；`None`/空表 = 清除全部。显示/分组元数据——引擎无关零副作用
    /// （对齐 set_task_name 边界），持久化随 tasks.json（TaskMetadata serde default
    /// 兼容旧档案），入 `?tag=` 过滤与 `?search=` 语料。
    ///
    /// 归一化：逐个 trim → 丢空串 → 去重（保留首次出现序，大小写敏感——
    /// 标签匹配大小写不敏感但显示保留原样）→ 上限 16 个/单个 1..=64 字符
    ///（超限 400 InvalidSource，调用方可先归一化再展示）。
    pub fn set_task_tags(
        &self,
        id: &str,
        tags: Option<Vec<String>>,
    ) -> Result<Vec<String>, DaemonError> {
        let normalized = match tags {
            None => Vec::new(),
            Some(list) => {
                if list.len() > 16 {
                    return Err(DaemonError::InvalidSource(format!(
                        "标签数量超上限 16（实际 {}）",
                        list.len()
                    )));
                }
                let mut out: Vec<String> = Vec::new();
                for t in list {
                    let t = t.trim();
                    if t.is_empty() {
                        continue;
                    }
                    if t.chars().count() > 64 {
                        return Err(DaemonError::InvalidSource(format!("标签超 64 字符: {t:?}")));
                    }
                    if !out.iter().any(|e| e == t) {
                        out.push(t.to_string());
                    }
                }
                out
            }
        };
        {
            let mut tasks = self.tasks.lock();
            let rec = tasks
                .get_mut(id)
                .ok_or_else(|| DaemonError::NotFound(id.to_string()))?;
            let detail = if normalized.is_empty() {
                "cleared"
            } else {
                "set"
            };
            rec.task.metadata.tags = normalized.clone();
            rec.push_event("tags_changed", Some(detail.into()));
        }
        self.autosave();
        Ok(normalized)
    }
}
