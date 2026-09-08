//! NasRemoteEngine — 迅雷 NAS 引擎（xllite/pan-cli）远程 HTTP 托管适配层。
//!
//! # 校准依据（Task 31，本仓库 docs/nas/）
//! - A2：引擎无头拉起（pty + `launcher -pid`）、`DriveListen` 路由面、KV 热启动
//! - A3：pan-auth 自举（`GET /` 动态注入 `uiauth(value){return "JWT"}` → 请求头
//!   `pan-auth: <JWT>` → `/drive/v1/*` 全通）；AES-KV 不破（自举取代）
//! - A4：任务 API 定案（`POST /drive/v1/task` 单数 + `url` 对象形；DELETE 同步
//!   阻塞 >30s；filters 矩阵；每日 3 任务创建上限；`下载(90120)` 错误语义）
//! - A5：平台伪装最小配方（`BinDir/envconfig` YAML：`PLATFORM=群晖` +
//!   `ALLOW_CUSTOM_PLATFORM=true`）；PipeLimit env 旋钮（=10 → 256）
//!
//! # 远程语义
//! 引擎进程由 ops 层启动（可跨机：引擎监听 `DriveListen` 的地址即本引擎的
//! `base_url`）。本模块只实现 **pan-auth 协议面**，不含进程管理。
//!
//! # v1 范围
//! - 支持：Http/Thunder 源 URL 任务（创建/状态轮询/删除/超速申请）
//! - 不支持：Magnet/TorrentFile（需 bencode 上传面，未校准）、pause/resume
//!   （A2 路由面实测未挂载）、peer 注入/piece 读（BT 专属）

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};

use smart_dl_core::task::DownloadTask;
use smart_dl_core::types::{
    Capability, DownloadEngine, DownloadSource, EngineError, EngineKind, EngineState, EngineStatus,
    EngineTaskId, FileProgress,
};

/// 迅雷任务相位（引擎返回 `PHASE_TYPE_*`）→ 统一 `EngineState`。
pub fn map_phase(phase: &str) -> EngineState {
    match phase {
        "PHASE_TYPE_RUNNING" => EngineState::Downloading,
        "PHASE_TYPE_COMPLETE" | "PHASE_TYPE_FINISHED" => EngineState::Completed,
        "PHASE_TYPE_PAUSED" => EngineState::Paused,
        "PHASE_TYPE_ERROR" | "PHASE_TYPE_FAILED" => EngineState::Error,
        // PENDING / 未知相位一律视为等待元数据（对齐 A4 观测的 PENDING 起点）
        _ => EngineState::MetadataPending,
    }
}

/// 从 `GET /` 首页 HTML 提取引擎注入的 UIAuth JWT（A3 自举链）。
///
/// 引擎在首页动态写入 `<script>function uiauth(value){ return "eyJ…" }</script>`，
/// 返回体为 HS256 三段式 JWT（key=UIAuth, exp=iat+259200）。
pub fn extract_uiauth_jwt(html: &str) -> Option<String> {
    let marker = html.find("uiauth(")?;
    let tail = &html[marker..];
    let open = tail.find('{')?;
    let ret = tail[open..].find("return")? + open;
    let q1 = tail[ret..].find('"')? + ret;
    let q2 = tail[q1 + 1..].find('"')? + q1 + 1;
    let jwt = &tail[q1 + 1..q2];
    if jwt.is_empty() || jwt.split('.').count() != 3 {
        return None;
    }
    Some(jwt.to_string())
}

/// 由 URL 推导任务名（引擎 `name/file_name` 字段；取路径末段）。
pub fn task_name_from_url(url: &str) -> String {
    let raw = url.split(['?', '#']).next().unwrap_or(url);
    let raw = raw.trim_end_matches('/');
    let name = raw.rsplit('/').next().unwrap_or(raw);
    if name.is_empty() {
        "nas-remote-task".into()
    } else {
        name.to_string()
    }
}

/// 创建载荷（A4 定案：`url` 必须为对象形；`space/params.target` = device space）。
pub fn build_create_payload(space: &str, name: &str, url: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "user#download-url",
        "space": space,
        "file_size": "0",
        "name": name,
        "file_name": name,
        "url": {"url": url},
        "parent_folder_id": "",
        "params": {"target": space},
    })
}

/// 分类 403/业务错误体（A4：`task_create_count_limit`；A3：`permission_deny`）。
pub fn classify_error(status: u16, body: &str) -> EngineError {
    if body.contains("task_create_count_limit") {
        return EngineError::Other(
            "xunlei quota: task_create_count_limit（每日 3 次创建上限，失败也计数，北京时间 0 点重置）"
                .into(),
        );
    }
    if body.contains("invalid number of segments") || body.contains("permission_deny") {
        return EngineError::Other("xunlei auth: pan-auth JWT 失效（需重新自举 GET /）".into());
    }
    EngineError::Other(format!("xunlei api {status}: {}", truncate(body, 160)))
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

/// `params.speed` 形如 `"1.23MB/s"` / `"456KB/s"` / `"789"`（B/s）→ B/s。
/// 无法解析返回 0（引擎侧字段形态未完全定案，防御式处理）。
pub fn parse_speed_bps(raw: &str) -> u64 {
    let s = raw.trim();
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let Ok(v): Result<f64, _> = s[..num_end].parse() else {
        return 0;
    };
    let unit = s[num_end..].trim_start();
    let mult = if unit.starts_with("GB") {
        1 << 30
    } else if unit.starts_with("MB") {
        1 << 20
    } else if unit.starts_with("KB") {
        1 << 10
    } else {
        1
    };
    (v * mult as f64) as u64
}

/// 远程引擎配置。
#[derive(Clone, Debug)]
pub struct NasRemoteConfig {
    /// 引擎 `DriveListen` 地址（http://host:port；NAS 盒子可为远程 IP）。
    pub base_url: String,
    /// 设备空间（`device_id#<hex32>`；引擎 `info.file` 的 device_id，A2 定案）。
    pub device_space: String,
}

impl NasRemoteConfig {
    pub fn new(base_url: impl Into<String>, device_id: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            device_space: format!("device_id#{}", device_id.into()),
        }
    }
}

/// 迅雷 NAS 远程引擎（`EngineKind::XunleiNas`，附录 E 落地）。
pub struct NasRemoteEngine {
    cfg: NasRemoteConfig,
    http: reqwest::Client,
    jwt: RwLock<Option<String>>,
    /// EngineTaskId → 迅雷任务 id（本引擎生成 "nas-<xlid>"， xlid 原样保留）。
    tasks: Arc<Mutex<HashMap<String, String>>>,
}

impl NasRemoteEngine {
    pub fn new(cfg: NasRemoteConfig) -> Self {
        Self {
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            jwt: RwLock::new(None),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 自举 pan-auth JWT（A3）：`GET /` → uiauth 提取。每次进程生命周期取新
    /// （JWT exp=iat+259200，但引擎重启即重签，缓存失效成本低）。
    async fn ensure_jwt(&self, force: bool) -> Result<String, EngineError> {
        if !force {
            if let Some(j) = self.jwt.read().await.as_ref() {
                return Ok(j.clone());
            }
        }
        let html = self
            .http
            .get(format!("{}/", self.cfg.base_url))
            .timeout(Duration::from_secs(8))
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("engine GET / failed: {e}")))?
            .text()
            .await
            .map_err(|e| EngineError::Other(format!("engine GET / read failed: {e}")))?;
        let jwt = extract_uiauth_jwt(&html).ok_or_else(|| {
            EngineError::Other(
                "uiauth JWT not found in engine homepage（引擎未就绪或版本不符）".into(),
            )
        })?;
        *self.jwt.write().await = Some(jwt.clone());
        Ok(jwt)
    }

    async fn req_jwt(&self) -> Result<String, EngineError> {
        self.ensure_jwt(false).await
    }

    /// 查询单任务原始 JSON（filters id 精确匹配，A4 filters 矩阵）。
    async fn fetch_task_raw(&self, xlid: &str) -> Result<serde_json::Value, EngineError> {
        let jwt = self.req_jwt().await?;
        let url = format!(
            "{}/drive/v1/tasks?space={}&filters={}",
            self.cfg.base_url,
            urlencode(&self.cfg.device_space),
            urlencode(&format!("{{\"id\":{{\"in\":\"{xlid}\"}}}}")),
        );
        let (st, body) = self.send(reqwest::Method::GET, &url, &jwt, None, 8).await?;
        if st != 200 {
            return Err(classify_error(st, &body));
        }
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| EngineError::Other(format!("bad json: {e}")))?;
        v.get("tasks")
            .and_then(|t| t.as_array())
            .and_then(|a| a.first().cloned())
            .ok_or(EngineError::NotFound)
    }

    /// pan-auth 请求封装；403 时强制重自举重试一次（JWT 悬崖兜底）。
    async fn send(
        &self,
        method: reqwest::Method,
        url: &str,
        jwt: &str,
        json_body: Option<&serde_json::Value>,
        timeout_s: u64,
    ) -> Result<(u16, String), EngineError> {
        let mut req = self
            .http
            .request(method.clone(), url)
            .header("pan-auth", jwt)
            .header("Content-Type", "application/json")
            .timeout(Duration::from_secs(timeout_s));
        if let Some(b) = json_body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                EngineError::Other(format!(
                    "engine request timeout ({timeout_s}s)（DELETE 属同步阻塞，需长超时，见 A4）"
                ))
            } else {
                EngineError::Other(format!("engine request failed: {e}"))
            }
        })?;
        let st = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        Ok((st, body))
    }

    /// 带一次重自举重试的请求。
    async fn send_retry(
        &self,
        method: reqwest::Method,
        url: &str,
        json_body: Option<&serde_json::Value>,
        timeout_s: u64,
    ) -> Result<(u16, String), EngineError> {
        let jwt = self.req_jwt().await?;
        let (st, body) = self
            .send(method.clone(), url, &jwt, json_body, timeout_s)
            .await?;
        if st == 403 {
            let jwt2 = self.ensure_jwt(true).await?;
            return self.send(method, url, &jwt2, json_body, timeout_s).await;
        }
        Ok((st, body))
    }
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[async_trait::async_trait]
impl DownloadEngine for NasRemoteEngine {
    fn id(&self) -> &str {
        "nas-remote"
    }

    fn kind(&self) -> EngineKind {
        EngineKind::XunleiNas
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Http,
            Capability::Https,
            Capability::MultiConnection,
        ]
    }

    /// 创建 URL 任务（A4 定案载荷）。成功后以 `nas-<xlid>` 为引擎任务句柄。
    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError> {
        let url = match &task.source {
            DownloadSource::Http { url, .. } => url.clone(),
            DownloadSource::Thunder(u) => u.clone(),
            other => {
                return Err(EngineError::Other(format!(
                    "NasRemoteEngine v1 仅支持 Http/Thunder 源，收到 {other:?}"
                )))
            }
        };
        let name = task_name_from_url(&url);
        let payload = build_create_payload(&self.cfg.device_space, &name, &url);
        let url_api = format!("{}/drive/v1/task", self.cfg.base_url);
        let (st, body) = self
            .send_retry(reqwest::Method::POST, &url_api, Some(&payload), 15)
            .await?;
        if st != 200 {
            return Err(classify_error(st, &body));
        }
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| EngineError::Other(format!("bad json: {e}")))?;
        let xlid = v
            .get("task")
            .and_then(|t| t.get("id"))
            .or_else(|| v.get("id"))
            .and_then(|i| i.as_str())
            .ok_or_else(|| {
                EngineError::Other(format!("create 响应缺 task.id: {}", truncate(&body, 120)))
            })?
            .to_string();
        let handle = format!("nas-{xlid}");
        self.tasks.lock().await.insert(handle.clone(), xlid);
        Ok(handle)
    }

    async fn pause(&self, _id: &EngineTaskId) -> Result<(), EngineError> {
        // A2 路由面：/drive/v1/tasks 的 pause/resume 未挂载（404）
        Err(EngineError::Unsupported)
    }

    async fn resume(&self, _id: &EngineTaskId) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    /// 状态快照：相位映射 + `params.error`（如 `下载(90120)`）+ `params.speed`。
    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError> {
        let xlid = self
            .tasks
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or(EngineError::NotFound)?;
        let t = self.fetch_task_raw(&xlid).await?;
        let phase = t.get("phase").and_then(|p| p.as_str()).unwrap_or("");
        let name = t
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("task")
            .to_string();
        let params = t.get("params").cloned().unwrap_or_default();
        let speed = params
            .get("speed")
            .and_then(|s| s.as_str())
            .map(parse_speed_bps)
            .unwrap_or(0);
        let progress = t.get("progress").and_then(|p| p.as_f64()).unwrap_or(0.0);
        // progress 语义未定案（0-100 疑似百分比，A4 观测）：仅当落在 [0,100] 视为百分比
        let (done, total) = if (0.0..=100.0).contains(&progress) {
            (progress, 100.0)
        } else {
            (0.0, 0.0)
        };
        Ok(EngineStatus {
            state: map_phase(phase),
            metadata_received: !phase.is_empty(),
            files: vec![FileProgress {
                rel_path: name,
                done: done as u64,
                size: total as u64,
            }],
            total_done: done as u64,
            total: total as u64,
            down_rate: speed,
            up_rate: 0,
            num_peers: 0,
            num_seeds: 0,
            error: params
                .get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string()),
        })
    }

    /// 删除任务（引擎会同步清理本地文件并远端同步——**阻塞 >30s**，超时给 95s，见 A4）。
    async fn remove(&self, id: &EngineTaskId, _delete_data: bool) -> Result<(), EngineError> {
        let xlid = self
            .tasks
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or(EngineError::NotFound)?;
        let url = format!(
            "{}/drive/v1/tasks?space={}&task_ids={}",
            self.cfg.base_url,
            urlencode(&self.cfg.device_space),
            urlencode(&xlid),
        );
        let (st, body) = self
            .send_retry(reqwest::Method::DELETE, &url, None, 95)
            .await?;
        if st == 200 {
            self.tasks.lock().await.remove(id);
            return Ok(());
        }
        Err(classify_error(st, &body))
    }

    /// 超速申请（A4：仅 RUNNING 任务生效；body 空对象即可，配额 usage 独立）。
    async fn update_sources(
        &self,
        id: &EngineTaskId,
        _urls: Vec<String>,
    ) -> Result<(), EngineError> {
        let _xlid = self
            .tasks
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or(EngineError::NotFound)?;
        let url = format!("{}/device/v1/try_speed/apply", self.cfg.base_url);
        let payload = serde_json::json!({});
        let (st, body) = self
            .send_retry(reqwest::Method::POST, &url, Some(&payload), 10)
            .await?;
        if st == 200 {
            return Ok(());
        }
        Err(classify_error(st, &body))
    }

    async fn add_url_seed(&self, _id: &EngineTaskId, _url: &str) -> Result<(), EngineError> {
        // v1 不支持追加种子源（引擎侧未校准追加语义）
        Err(EngineError::Unsupported)
    }

    async fn peers(
        &self,
        _id: &EngineTaskId,
    ) -> Result<Vec<smart_dl_core::types::PeerInfo>, EngineError> {
        Ok(vec![]) // NAS 引擎无 peer 概念暴露
    }

    async fn ban_peer(
        &self,
        _id: &EngineTaskId,
        _peer: std::net::SocketAddr,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }

    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>, EngineError> {
        Err(EngineError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HTML: &str = r#"<html><body><script>
function uiauth(value){ return "eyJhbGciOiJIUzI1NiJ9.eyJrZXkiOiJVSUF1dGgifQ.sig" }
</script></body></html>"#;

    #[test]
    fn uiauth_extract_ok() {
        let jwt = extract_uiauth_jwt(SAMPLE_HTML).unwrap();
        assert_eq!(jwt.split('.').count(), 3);
        assert!(jwt.starts_with("eyJ"));
    }

    #[test]
    fn uiauth_extract_rejects_non_jwt() {
        let html = "function uiauth(value){ return \"not-a-jwt\" }";
        assert!(extract_uiauth_jwt(html).is_none());
        assert!(extract_uiauth_jwt("<html>no script</html>").is_none());
    }

    #[test]
    fn phase_mapping() {
        assert_eq!(
            map_phase("PHASE_TYPE_PENDING"),
            EngineState::MetadataPending
        );
        assert_eq!(map_phase("PHASE_TYPE_RUNNING"), EngineState::Downloading);
        assert_eq!(map_phase("PHASE_TYPE_COMPLETE"), EngineState::Completed);
        assert_eq!(map_phase("PHASE_TYPE_ERROR"), EngineState::Error);
        assert_eq!(map_phase("PHASE_TYPE_PAUSED"), EngineState::Paused);
        assert_eq!(
            map_phase("PHASE_TYPE_WHATEVER"),
            EngineState::MetadataPending
        );
    }

    #[test]
    fn create_payload_golden() {
        let p = build_create_payload(
            "device_id#c7d089aad73f7e2ddd2c263c2956b5a6",
            "a4r3-10Mb.dat",
            "https://proof.ovh.net/files/10Mb.dat",
        );
        // A4 定案：url 必须为对象形；space/params.target 同值
        assert_eq!(
            p["url"],
            serde_json::json!({"url": "https://proof.ovh.net/files/10Mb.dat"})
        );
        assert_eq!(p["space"], "device_id#c7d089aad73f7e2ddd2c263c2956b5a6");
        assert_eq!(p["params"]["target"], p["space"]);
        assert_eq!(p["type"], "user#download-url");
        assert_eq!(p["file_size"], "0");
        assert_eq!(p["name"], "a4r3-10Mb.dat");
        assert_eq!(p["parent_folder_id"], "");
    }

    #[test]
    fn name_from_url() {
        assert_eq!(
            task_name_from_url("https://x.com/a/b/file.zip?sig=1"),
            "file.zip"
        );
        assert_eq!(task_name_from_url("https://x.com/"), "x.com");
        assert_eq!(task_name_from_url("not-a-url"), "not-a-url");
    }

    #[test]
    fn quota_error_classified() {
        let e = classify_error(
            403,
            r#"{"error":"task_create_count_limit","error_code":11,"error_description":"任务创建次数达到上限"}"#,
        );
        assert!(e.to_string().contains("每日 3 次"));
        let e2 = classify_error(
            403,
            r#"{"error":"permission_deny: checkAuth failed:token contains an invalid number of segments"}"#,
        );
        assert!(e2.to_string().contains("重新自举"));
    }

    #[test]
    fn speed_parse() {
        assert_eq!(parse_speed_bps("1.5MB/s"), 1_572_864);
        assert_eq!(parse_speed_bps("456KB/s"), 466_944);
        assert_eq!(parse_speed_bps("789"), 789);
        assert_eq!(parse_speed_bps(""), 0);
        assert_eq!(parse_speed_bps("n/a"), 0);
    }

    #[test]
    fn url_encode_space() {
        assert_eq!(
            urlencode("device_id#c7d089…"),
            "device_id%23c7d089%E2%80%A6"
        );
    }

    #[test]
    fn config_new_formats_space() {
        let c = NasRemoteConfig::new("http://127.0.0.1:5050/", "c7d089aad73f7e2ddd2c263c2956b5a6");
        assert_eq!(c.base_url, "http://127.0.0.1:5050");
        assert_eq!(c.device_space, "device_id#c7d089aad73f7e2ddd2c263c2956b5a6");
    }
}
