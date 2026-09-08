//! VIP 加速通道客户端（附录 A #3/#4 · 2026-08-30）。
//!
//! 端点族与证据（docs/research/xunlei/SPEEDUP_SYSTEM.md）：
//! - `check_status`（✅ 响应形状已实测验证，SPEEDUP_SYSTEM §三）：
//!   `POST {speedup_base}/v1/check_status`，body `{"user_id": u64}`，
//!   鉴权 = Xqp0 Bearer 票（设备码流程 access_token）；
//!   返回 `vas_id/is_vip/is_exp/probation/speed_open/basic_rate_down/basic_rate_up`。
//! - 下载试用加速 TrySpeed（🔶 形状假设·UNTESTED）：官方桌面 inner-api 路由
//!   `/device/v1/try_speed/{get_info,get_config,apply}`（allow_inner_api_paths 白名单）；
//!   远端真身 = HostHighSpeedFlow(=api-pan) 的 VipSpeedUpUrl（**完整路径待抓包**，
//!   `inner_base` 可注入，指向官方客户端本地 inner-api 或未来确认的远端）。
//!   配额字段：trial_left_times / trial_used_times / trial_key / total_sec /
//!   timeout_sec / speed_res_status / is_speed_trial_queried。
//! - 经典引擎速度认证（🔶 形状假设·UNTESTED）：
//!   `speed.auth.vip.xunlei.com/speed/{speedup,res_status}` → 产出 certification
//!   字符串 → 喂给 `xunlei-ffi::identity::set_accelerate_certification`（A 级已封装）。
//!
//! 合规边界（CROSS_PLATFORM_UNIVERSAL_SOLUTION §5）：
//! - 只消费**用户自有账号**的授权面（Bearer 票由调用方提供，通常来自
//!   `login_flow` 设备码登录）；不伪造配额、不对抗风控；
//! - 风控/滑块错误原样透传为 `SpeedupError::Rejected`，不做重试轰炸；
//! - 「提取成免 VIP 能力」不在目标内（附录 A #3 终判）。
//!
//! 状态标记：`check_status` = 形状已验；其余端点 = 代码就位、等待有
//! 试用/会员票据的真机会话校准（用户 2026-08-30 指示：先落未测试代码）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Xqp0 设备票对应的 client-id（与 pan API 登录态同源，SPEEDUP_SYSTEM §三）。
pub const X_CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";

/// speedup.xunlei.com 生产基址。
pub const SPEEDUP_BASE: &str = "https://speedup.xunlei.com";
/// 经典引擎速度认证基址（形状假设，UNTESTED）。
pub const SPEED_CERT_BASE: &str = "https://speed.auth.vip.xunlei.com";
/// 官方桌面 inner-api 基址占位（端口因版本而异，调用方必须注入或抓包校准）。
pub const INNER_API_PLACEHOLDER: &str = "http://127.0.0.1:8000";

/// VIP 通道错误。
#[derive(Debug, thiserror::Error)]
pub enum SpeedupError {
    /// 调用方未提供 Bearer 票（ticket 返回 None）。
    #[error("no bearer ticket available (login first via device-code flow)")]
    NoTicket,
    /// HTTP 层错误。
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// 非 2xx 状态。
    #[error("http status {0}")]
    Status(u16),
    /// 响应不是合法 JSON。
    #[error("bad json: {0}")]
    Json(String),
    /// 服务端风控/拒绝（ret/errcode 非 0），原样透传不重试。
    #[error("rejected by server: ret={ret} err={err} msg={msg:?}")]
    Rejected {
        ret: i64,
        err: i64,
        msg: Option<String>,
    },
}

/// Bearer 票提供者（通常闭包内读 `AuthState` access_token）。
pub type TicketProvider = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// `check_status` 响应（形状已实测验证，SPEEDUP_SYSTEM §三）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SpeedupStatus {
    #[serde(default)]
    pub vas_id: Option<u64>,
    #[serde(default)]
    pub is_vip: bool,
    #[serde(default)]
    pub is_exp: bool,
    #[serde(default)]
    pub probation: Option<u64>,
    #[serde(default)]
    pub speed_open: bool,
    #[serde(default)]
    pub basic_rate_down: Option<u64>,
    #[serde(default)]
    pub basic_rate_up: Option<u64>,
    /// 兼容包裹：部分版本把业务体放在 data 里（待验证，两端都试）。
    #[serde(default)]
    pub data: Option<Box<SpeedupStatus>>,
    #[serde(default)]
    pub ret: Option<i64>,
    #[serde(default)]
    pub err: Option<i64>,
}

impl SpeedupStatus {
    /// 解包 data 包裹（若服务端把业务体嵌在 data 下）。
    pub fn effective(self) -> Self {
        match self.data {
            Some(inner) => *inner,
            None => self,
        }
    }
}

/// TrySpeed 试用加速信息（🔶 形状假设·UNTESTED，字段名来自 Go struct json tags）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrySpeedInfo {
    #[serde(default)]
    pub trial_left_times: Option<u64>,
    #[serde(default)]
    pub trial_used_times: Option<u64>,
    #[serde(default)]
    pub trial_key: Option<String>,
    #[serde(default)]
    pub total_sec: Option<u64>,
    #[serde(default)]
    pub timeout_sec: Option<u64>,
    #[serde(default)]
    pub speed_res_status: Option<String>,
    #[serde(default)]
    pub is_speed_trial_queried: Option<bool>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// TrySpeed apply 结果（🔶 形状假设·UNTESTED）。
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrySpeedApply {
    #[serde(default)]
    pub speed_res_status: Option<String>,
    #[serde(default)]
    pub trial_key: Option<String>,
    #[serde(default)]
    pub timeout_sec: Option<u64>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// VIP 加速通道客户端。
///
/// 三个基址都可注入（测试用 axum mock；生产用默认常量）。
#[derive(Clone)]
pub struct VipSpeedupClient {
    http: reqwest::Client,
    speedup_base: String,
    cert_base: String,
    inner_base: String,
    ticket: TicketProvider,
}

impl VipSpeedupClient {
    /// 用默认生产基址构造。
    pub fn new(ticket: TicketProvider) -> Self {
        Self::with_bases(ticket, SPEEDUP_BASE, SPEED_CERT_BASE, INNER_API_PLACEHOLDER)
    }

    /// 注入基址（测试 / inner-api 端口未确认期间由调用方指定）。
    pub fn with_bases(
        ticket: TicketProvider,
        speedup_base: impl Into<String>,
        cert_base: impl Into<String>,
        inner_base: impl Into<String>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            speedup_base: speedup_base.into().trim_end_matches('/').to_string(),
            cert_base: cert_base.into().trim_end_matches('/').to_string(),
            inner_base: inner_base.into().trim_end_matches('/').to_string(),
            ticket,
        }
    }

    fn bearer(&self) -> Result<String, SpeedupError> {
        (self.ticket)().ok_or(SpeedupError::NoTicket)
    }

    /// 会员/试用资格状态查询（✅ 形状已验）。
    ///
    /// 这是 VIP 通道的**入口探测**：先确认 `is_vip/is_exp/probation/trial` 可用性，
    /// 再决定是否走 apply（消耗配额前必须先查，避免无谓的配额扣减）。
    pub async fn check_status(&self, user_id: u64) -> Result<SpeedupStatus, SpeedupError> {
        let url = format!("{}/v1/check_status", self.speedup_base);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.bearer()?))
            .header("x-client-id", X_CLIENT_ID)
            .json(&serde_json::json!({ "user_id": user_id }))
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.map_err(SpeedupError::Http)?;
        if !status.is_success() {
            return Err(SpeedupError::Status(status.as_u16()));
        }
        let parsed: SpeedupStatus = serde_json::from_str(&body)
            .map_err(|e| SpeedupError::Json(format!("{e}; body head: {}", head(&body))))?;
        if parsed.ret.unwrap_or(0) != 0 {
            return Err(SpeedupError::Rejected {
                ret: parsed.ret.unwrap_or(0),
                err: parsed.err.unwrap_or(0),
                msg: None,
            });
        }
        Ok(parsed.effective())
    }

    /// 试用配额查询（🔶 UNTESTED，形状假设）。
    pub async fn try_speed_get_info(&self) -> Result<TrySpeedInfo, SpeedupError> {
        let url = format!("{}/device/v1/try_speed/get_info", self.inner_base);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.bearer()?))
            .header("x-client-id", X_CLIENT_ID)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.map_err(SpeedupError::Http)?;
        if !status.is_success() {
            return Err(SpeedupError::Status(status.as_u16()));
        }
        serde_json::from_str(&body)
            .map_err(|e| SpeedupError::Json(format!("{e}; body head: {}", head(&body))))
    }

    /// 对任务列表套用试用加速体验单（🔶 UNTESTED，消耗配额的操作）。
    ///
    /// 请求体形状假设 `{"taskIDList": [...]}`（SPEEDUP_SYSTEM §二
    /// `superSpeedTaskIDListRef` 的 json tag 同构）——真机会话校准前不要在生产路径调用。
    pub async fn try_speed_apply(&self, task_ids: &[u64]) -> Result<TrySpeedApply, SpeedupError> {
        let url = format!("{}/device/v1/try_speed/apply", self.inner_base);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.bearer()?))
            .header("x-client-id", X_CLIENT_ID)
            .json(&serde_json::json!({ "taskIDList": task_ids }))
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.map_err(SpeedupError::Http)?;
        if !status.is_success() {
            return Err(SpeedupError::Status(status.as_u16()));
        }
        serde_json::from_str(&body)
            .map_err(|e| SpeedupError::Json(format!("{e}; body head: {}", head(&body))))
    }

    /// 经典引擎速度认证状态（🔶 UNTESTED，形状假设：POST /speed/res_status）。
    ///
    /// 成功路径产出的 certification 字符串喂给
    /// `xunlei-ffi::identity::set_accelerate_certification`（A 级已封装）。
    /// 证书下发流程哪个接口产出 certification 仍未知（SPEEDUP_SYSTEM §四-3）。
    pub async fn speed_cert_res_status(
        &self,
        user_id: u64,
    ) -> Result<serde_json::Value, SpeedupError> {
        let url = format!("{}/speed/res_status", self.cert_base);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.bearer()?))
            .header("x-client-id", X_CLIENT_ID)
            .json(&serde_json::json!({ "user_id": user_id }))
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await.map_err(SpeedupError::Http)?;
        if !status.is_success() {
            return Err(SpeedupError::Status(status.as_u16()));
        }
        serde_json::from_str(&body)
            .map_err(|e| SpeedupError::Json(format!("{e}; body head: {}", head(&body))))
    }
}

fn head(s: &str) -> String {
    s.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::Router;

    fn ticket_of(tok: &'static str) -> TicketProvider {
        Arc::new(move || Some(tok.to_string()))
    }

    async fn spawn_mock(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), handle)
    }

    #[tokio::test]
    async fn check_status_parses_verified_shape() {
        // 响应逐字段取自 SPEEDUP_SYSTEM.md §三 实测记录
        let app = Router::new().route(
            "/v1/check_status",
            post(|| async {
                axum::Json(serde_json::json!({
                    "vas_id": 14, "is_vip": false, "is_exp": false,
                    "probation": 0, "speed_open": false,
                    "basic_rate_down": 1024, "basic_rate_up": 256
                }))
            }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("tok"), &base, &base, &base);
        let s = c.check_status(42).await.unwrap();
        assert!(!s.is_vip);
        assert!(!s.speed_open);
        assert_eq!(s.vas_id, Some(14));
        assert_eq!(s.basic_rate_down, Some(1024));
    }

    #[tokio::test]
    async fn check_status_unwraps_data_wrapper() {
        let app = Router::new().route(
            "/v1/check_status",
            post(|| async {
                axum::Json(serde_json::json!({
                    "ret": 0,
                    "data": { "is_vip": true, "speed_open": true }
                }))
            }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("tok"), &base, &base, &base);
        let s = c.check_status(42).await.unwrap();
        assert!(s.is_vip);
        assert!(s.speed_open);
    }

    #[tokio::test]
    async fn check_status_sends_bearer_and_client_id() {
        use axum::http::HeaderMap;
        let app = Router::new().route(
            "/v1/check_status",
            post(|headers: HeaderMap| async move {
                assert_eq!(
                    headers.get("authorization").unwrap(),
                    "Bearer live-token-abc"
                );
                assert_eq!(headers.get("x-client-id").unwrap(), X_CLIENT_ID);
                axum::Json(serde_json::json!({ "is_vip": true }))
            }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("live-token-abc"), &base, &base, &base);
        assert!(c.check_status(1).await.unwrap().is_vip);
    }

    #[tokio::test]
    async fn no_ticket_fails_before_http() {
        let none_ticket: TicketProvider = Arc::new(|| None);
        let c = VipSpeedupClient::with_bases(
            none_ticket,
            "http://127.0.0.1:1", // 不可达端口：若发出请求必然连接错误而非 NoTicket
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
        );
        let err = c.check_status(1).await.unwrap_err();
        assert!(matches!(err, SpeedupError::NoTicket), "err={err}");
    }

    #[tokio::test]
    async fn rejected_ret_is_passthrough() {
        let app = Router::new().route(
            "/v1/check_status",
            post(|| async { axum::Json(serde_json::json!({ "ret": 16, "err": 1101 })) }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("tok"), &base, &base, &base);
        let err = c.check_status(1).await.unwrap_err();
        assert!(
            matches!(
                err,
                SpeedupError::Rejected {
                    ret: 16,
                    err: 1101,
                    ..
                }
            ),
            "err={err}"
        );
    }

    #[tokio::test]
    async fn try_speed_apply_posts_task_list() {
        use axum::extract::Json as Ej;
        let app = Router::new().route(
            "/device/v1/try_speed/apply",
            post(|Ej(body): Ej<serde_json::Value>| async move {
                assert_eq!(body["taskIDList"], serde_json::json!([7, 9]));
                axum::Json(serde_json::json!({
                    "speed_res_status": "OK", "trial_key": "k1", "timeout_sec": 60
                }))
            }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("tok"), &base, &base, &base);
        let r = c.try_speed_apply(&[7, 9]).await.unwrap();
        assert_eq!(r.trial_key.as_deref(), Some("k1"));
        assert_eq!(r.timeout_sec, Some(60));
    }

    #[tokio::test]
    async fn try_speed_get_info_parses_quota_fields() {
        let app = Router::new().route(
            "/device/v1/try_speed/get_info",
            get(|| async {
                axum::Json(serde_json::json!({
                    "trial_left_times": 3, "trial_used_times": 1,
                    "trial_key": "abc", "total_sec": 300,
                    "timeout_sec": 60, "speed_res_status": "OK",
                    "is_speed_trial_queried": true
                }))
            }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("tok"), &base, &base, &base);
        let info = c.try_speed_get_info().await.unwrap();
        assert_eq!(info.trial_left_times, Some(3));
        assert_eq!(info.trial_key.as_deref(), Some("abc"));
    }

    #[tokio::test]
    async fn cert_res_status_passthrough_json() {
        let app = Router::new().route(
            "/speed/res_status",
            post(|| async { axum::Json(serde_json::json!({ "cert": "XYZ", "status": 1 })) }),
        );
        let (base, _h) = spawn_mock(app).await;
        let c = VipSpeedupClient::with_bases(ticket_of("tok"), &base, &base, &base);
        let v = c.speed_cert_res_status(42).await.unwrap();
        assert_eq!(v["cert"], "XYZ");
    }
}
