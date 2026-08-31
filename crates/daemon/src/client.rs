//! CLI 执行层：CliCommand → serve HTTP API 调用 + 输出（--json 全局标志）。
//! 未映射到 API 的命令（logs/config/fallback）给出明确提示（v1 无对应端点）。

use crate::cli::{CliCommand, CliError};

pub struct CliClient {
    base: String,
    client: reqwest::Client,
}

/// 非 2xx → 提取响应体 {error} 转 Err；否则返回原响应。
async fn ensure_ok(resp: reqwest::Response) -> Result<reqwest::Response, CliError> {
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"].as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("HTTP {}", status));
        return Err(CliError::Http(msg));
    }
    Ok(resp)
}

impl CliClient {
    /// 安全修复（V1 配套）：`token` Some 时经 reqwest default_headers 给全部请求
    /// 注入 `Authorization: Bearer <token>`（与 daemon 的 auth_mw 配对）；
    /// None = 未认证模式（回环未配置 token 的兼容行为）。
    pub fn new(base: &str, token: Option<&str>) -> Self {
        let mut builder = reqwest::Client::builder();
        if let Some(t) = token.filter(|t| !t.is_empty()) {
            let mut headers = reqwest::header::HeaderMap::new();
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {t}")) {
                headers.insert(reqwest::header::AUTHORIZATION, v);
            }
            builder = builder.default_headers(headers);
        }
        CliClient {
            base: base.trim_end_matches('/').to_string(),
            client: builder.build().expect("reqwest client 构建失败"),
        }
    }

    /// 执行命令；输出到 stdout。错误（含 HTTP 非 2xx）→ Err。
    pub async fn run(&self, cmd: &CliCommand, json: bool) -> Result<(), CliError> {
        match cmd {
            CliCommand::Add { url, dest } => self.add(url, dest.as_deref(), json).await,
            CliCommand::Pause { task_id } => self.action("pause", task_id, json).await,
            CliCommand::Resume { task_id } => self.action("resume", task_id, json).await,
            CliCommand::Remove { task_id } => {
                let resp = self
                    .client
                    .delete(format!("{}/tasks/{}", self.base, task_id))
                    .send()
                    .await
                    .map_err(|e| CliError::Http(e.to_string()))?;
                self.check(resp, json).await?;
                if !json {
                    println!("已删除: {task_id}");
                }
                Ok(())
            }
            CliCommand::List => self.list(json).await,
            CliCommand::Status { task_id } => self.status(task_id, json).await,
            CliCommand::Logs { task_id } => {
                let resp = self
                    .client
                    .get(format!("{}/tasks/{}/logs", self.base, task_id))
                    .send()
                    .await
                    .map_err(|e| CliError::Http(e.to_string()))?;
                self.check(resp, json).await?;
                Ok(())
            }
            CliCommand::Config => {
                let resp = self
                    .client
                    .get(format!("{}/config", self.base))
                    .send()
                    .await
                    .map_err(|e| CliError::Http(e.to_string()))?;
                self.check(resp, json).await?;
                Ok(())
            }
            CliCommand::Fallback { task_id } => {
                let resp = self
                    .client
                    .post(format!("{}/tasks/{}/fallback", self.base, task_id))
                    .send()
                    .await
                    .map_err(|e| CliError::Http(e.to_string()))?;
                self.check(resp, json).await?;
                Ok(())
            }
            #[cfg(feature = "xunlei-import")]
            CliCommand::ImportXunlei {
                torrent,
                cfg,
                xltds,
                dest,
            } => {
                use base64::Engine;
                let mut xltd_b64s = Vec::with_capacity(xltds.len());
                for xltd in xltds {
                    let data = std::fs::read(xltd)
                        .map_err(|e| CliError::Http(format!("读取 xltd 失败: {e}")))?;
                    xltd_b64s.push(base64::engine::general_purpose::STANDARD.encode(data));
                }
                let body = serde_json::json!({
                    "torrent_b64": base64::engine::general_purpose::STANDARD.encode(std::fs::read(torrent).map_err(|e| CliError::Http(format!("读取 torrent 失败: {e}")))?),
                    "cfg_b64": base64::engine::general_purpose::STANDARD.encode(std::fs::read(cfg).map_err(|e| CliError::Http(format!("读取 cfg 失败: {e}")))?),
                    "xltd_b64s": xltd_b64s,
                    "dest": dest,
                });
                let resp = self
                    .client
                    .post(format!("{}/tasks/xunlei-import", self.base))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| CliError::Http(e.to_string()))?;
                self.check(resp, json).await?;
                if !json {
                    println!("迅雷任务已导入（结果见上，或 --json 查看 task_id）");
                }
                Ok(())
            }
            // xunlei-login 在 main.rs 分发前本地拦截执行，理论上不会到这里；
            // 保留防御分支保证穷尽性（提示用户直接运行命令而非经 --server 转发）。
            CliCommand::XunleiLogin { .. } => Err(CliError::Unknown(
                "xunlei-login 为本地命令，不经过 daemon：请直接运行 smart-dl-daemon xunlei-login"
                    .to_string(),
            )),
        }
    }

    async fn add(&self, url: &str, dest: Option<&str>, json: bool) -> Result<(), CliError> {
        let mut body = serde_json::Map::new();
        body.insert("url".into(), serde_json::Value::String(url.to_string()));
        if let Some(d) = dest {
            body.insert("dest".into(), serde_json::Value::String(d.to_string()));
        }
        let resp = self
            .client
            .post(format!("{}/tasks", self.base))
            .json(&serde_json::Value::Object(body))
            .send()
            .await
            .map_err(|e| CliError::Http(e.to_string()))?;
        self.check(resp, json).await?;
        // 201: {"task_id": ...}；由 check 输出（json 模式）或这里打印
        if !json {
            // check 已消费 201 响应体到 json 打印；此处补人读输出
            println!("任务已添加（结果见上，或 --json 查看 task_id）");
        }
        Ok(())
    }

    async fn action(&self, action: &str, task_id: &str, json: bool) -> Result<(), CliError> {
        let resp = self
            .client
            .post(format!("{}/tasks/{}/{}", self.base, task_id, action))
            .send()
            .await
            .map_err(|e| CliError::Http(e.to_string()))?;
        self.check(resp, json).await?;
        if !json {
            println!("已{action}: {task_id}");
        }
        Ok(())
    }

    async fn list(&self, json: bool) -> Result<(), CliError> {
        let resp = ensure_ok(
            self.client
                .get(format!("{}/tasks", self.base))
                .send()
                .await
                .map_err(|e| CliError::Http(e.to_string()))?,
        )
        .await?;
        if json {
            let text = resp
                .text()
                .await
                .map_err(|e| CliError::Http(e.to_string()))?;
            println!("{text}");
            return Ok(());
        }
        let tasks: Vec<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| CliError::Http(e.to_string()))?;
        for t in tasks {
            println!("{}", list_line(&t));
        }
        Ok(())
    }

    async fn status(&self, task_id: &str, json: bool) -> Result<(), CliError> {
        let resp = ensure_ok(
            self.client
                .get(format!("{}/tasks/{}", self.base, task_id))
                .send()
                .await
                .map_err(|e| CliError::Http(e.to_string()))?,
        )
        .await?;
        if json {
            let text = resp
                .text()
                .await
                .map_err(|e| CliError::Http(e.to_string()))?;
            println!("{text}");
            return Ok(());
        }
        let t: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| CliError::Http(e.to_string()))?;
        let mut lines: Vec<(String, String)> = vec![
            (
                "task_id".into(),
                t["task_id"].as_str().unwrap_or("?").into(),
            ),
            ("state".into(), t["state"].as_str().unwrap_or("?").into()),
            ("engine".into(), t["engine"].as_str().unwrap_or("?").into()),
            ("source".into(), summarize(&t)),
        ];
        if let Some(e) = t["engine_status"]["error"].as_str() {
            lines.push(("error".into(), e.to_string()));
        }
        let w = lines.iter().map(|(k, _)| k.len()).max().unwrap_or(4);
        for (k, v) in lines {
            println!("{k:<w$}  {v}");
        }
        Ok(())
    }

    /// 非 2xx → 提取 {error} 转 Err；body 非空即打印（json 模式原样输出，人读模式
    /// 也可见——config/logs 的可读结果）。`_json` 保留签名（未来格式化用）。
    async fn check(&self, resp: reqwest::Response, _json: bool) -> Result<(), CliError> {
        let resp = ensure_ok(resp).await?;
        let text = resp
            .text()
            .await
            .map_err(|e| CliError::Http(e.to_string()))?;
        // json 模式原样输出；人读模式 body 非空也打印（config/logs 的可读结果——
        // 冒烟发现：此前仅 json 有输出，非 json 静默成功）
        if !text.is_empty() {
            println!("{text}");
        }
        Ok(())
    }
}

/// 人读摘要：从快照提取源信息（url/magnet/torrent）。
fn summarize(t: &serde_json::Value) -> String {
    if let Some(s) = t["source"].as_str() {
        return s.chars().take(72).collect();
    }
    if let Some(s) = t["summary"].as_str() {
        return s.chars().take(72).collect();
    }
    "?".into()
}

// —— 测试辅助：纯格式化逻辑（无网络） ——

/// 人读的任务列表行（供测试：不依赖 HTTP）。
pub fn list_line(t: &serde_json::Value) -> String {
    format!(
        "{}  {}  {}",
        t["task_id"].as_str().unwrap_or("?"),
        t["state"].as_str().unwrap_or("?"),
        summarize(t)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_url_truncated() {
        let v = serde_json::json!({
            "task_id": "t1",
            "state": "Downloading",
            "source": "Http { url: \"https://example.com/very/long/path/file.bin\", headers: [], auth: None }"
        });
        let line = list_line(&v);
        assert!(line.starts_with("t1  Downloading  Http { url"));
        assert!(line.len() <= 72 + 20, "摘要应截断: {line}");
    }

    #[test]
    fn summarize_missing_source() {
        let v = serde_json::json!({ "task_id": "t2" });
        assert!(list_line(&v).contains('?'));
    }

    #[test]
    fn base_url_trims_slash() {
        let c = CliClient::new("http://x:1/", None);
        assert_eq!(c.base, "http://x:1");
    }
}
