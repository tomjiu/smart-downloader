//! 本地登录页服务（Task 5-b，Task 25 补全）。
//!
//! - 页面：`include_str!("login_page.html")`，深蓝渐变 + 白卡片 + 四 Tab
//!   （扫码 / 密码 / 短信 / 第三方），扫码二维码由本地 [`crate::xunlei::login_flow`]
//!   构造的官方授权 URL 生成（SVG，qrcode crate，无图片依赖）。
//! - API：
//!   | 端点 | 说明 |
//!   |------|------|
//!   | GET  /                  | 登录页 HTML |
//!   | GET  /api/qr.svg        | 当前设备码会话的二维码 SVG |
//!   | POST /api/start         | 发起设备码会话（返回 web_auth_url） |
//!   | GET  /api/status        | {state: pending/authorized/expired/error, web_auth_url, ...} |
//!   | POST /api/login/pwd     | 账密登录（captcha/init + signin 全链） |
//!   | POST /api/login/sms/send | 发送短信验证码（返回 verification_id，服务端同时记住） |
//!   | POST /api/login/sms/verify | 验证短信验证码（verification_id 缺省时服务端兜底） |
//! - 第三方（微信/QQ/微博）：页面跳官方 /yc/ 授权页（appid 回调白名单绑死
//!   迅雷域，本地无法自托管），同一 user_code 确认后轮询自动截 token。
//! - 成功后登录态写入 `token_path`（0600），页面展示成功状态。
//! - 只绑定 `127.0.0.1`，不对外监听。
//!
//! 测试：`mock_upstream` 集成测试通过 [`Client::with_bases`] 把迅雷端点
//! 指向本地 mock，全流程离线可跑。

use crate::xunlei::auth::AuthState;
use crate::xunlei::client::{Client, ClientError};
use crate::xunlei::login_flow::{
    self, poll_device_session, start_device_session, DeviceSession,
};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use qrcode::render::svg;
use qrcode::QrCode;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Mutex;

/// 登录页会话状态（axum State 共享）。
pub struct LoginSession {
    client: Client,
    token_path: std::path::PathBuf,
    scope: &'static str,
    device: Mutex<Option<DeviceSession>>,
    last_error: Mutex<Option<String>>,
    /// 短信会话兜底：send 时记住 verification_id（前端断链/页面刷新丢失时
    /// verify 可不带，服务端补上）。client.rs 实证会话必须绑 id，手机号无效。
    sms_verification_id: Mutex<Option<String>>,
    /// 已完成的登录态（authorized 后可读，测试断言用）。
    pub done: Mutex<Option<AuthState>>,
}

impl LoginSession {
    pub fn new(client: Client, token_path: std::path::PathBuf) -> Arc<Self> {
        Self::new_with_client(client, token_path, None)
    }

    /// 带预置设备码会话构造（Browser 模式：CLI 先拿设备码，本地备用页
    /// 直接展示同一张二维码，避免重复会话）。
    pub fn new_with_client(
        client: Client,
        token_path: std::path::PathBuf,
        preset: Option<DeviceSession>,
    ) -> Arc<Self> {
        Arc::new(LoginSession {
            client,
            token_path,
            scope: login_flow::DEVICE_SCOPE,
            device: Mutex::new(preset),
            last_error: Mutex::new(None),
            sms_verification_id: Mutex::new(None),
            done: Mutex::new(None),
        })
    }
}

/// 构建登录页路由（测试与 CLI 共用）。
pub fn login_router(session: Arc<LoginSession>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/qr.svg", get(qr_svg))
        .route("/api/start", post(start))
        .route("/api/status", get(status))
        .route("/api/login/pwd", post(pwd_login))
        .route("/api/login/sms/send", post(sms_send))
        .route("/api/login/sms/verify", post(sms_verify))
        .with_state(session)
}

/// 启动本地登录页服务：绑定 `127.0.0.1:<port>`（0 = 随机）。
/// 返回实际监听地址。
pub async fn serve_login_page(
    session: Arc<LoginSession>,
    port: u16,
) -> std::io::Result<std::net::SocketAddr> {
    let app = login_router(session);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(addr)
}

// ---------- handlers ----------

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        include_str!("login_page.html"),
    )
}

async fn qr_svg(State(sess): State<Arc<LoginSession>>) -> axum::response::Response {
    let dev = sess.device.lock().await.clone();
    let Some(d) = dev else {
        return (StatusCode::PRECONDITION_FAILED, "no active session").into_response();
    };
    match QrCode::new(d.qr_url.as_bytes()) {
        Ok(code) => {
            let svg = code
                .render::<svg::Color>()
                .dark_color(svg::Color("#101828"))
                .light_color(svg::Color("#ffffff"))
                .min_dimensions(196, 196)
                .build();
            (StatusCode::OK, [(header::CONTENT_TYPE, "image/svg+xml")], svg).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("qr: {e}"),
        )
            .into_response(),
    }
}

async fn start(State(sess): State<Arc<LoginSession>>) -> axum::response::Response {
    match start_device_session(&sess.client, sess.scope).await {
        Ok(d) => {
            *sess.device.lock().await = Some(d.clone());
            Json(json!({
                "ok": true,
                "user_code": d.user_code,
                "expires_at": d.expires_at,
                "web_auth_url": d.web_auth_url,
            }))
            .into_response()
        }
        Err(e) => {
            *sess.last_error.lock().await = Some(e.to_string());
            (StatusCode::BAD_GATEWAY, Json(json!({ "ok": false, "message": e.to_string() }))).into_response()
        }
    }
}

/// 前端轮询状态：pending / authorized / expired / error。
async fn status(State(sess): State<Arc<LoginSession>>) -> impl IntoResponse {
    let dev = sess.device.lock().await.clone();
    let Some(d) = dev else {
        return Json(json!({ "state": "error", "message": "会话未初始化，请刷新页面" }));
    };
    match poll_device_session(&sess.client, &d).await {
        Ok(Some(auth)) => {
            let uid = auth.user_id.clone();
            match login_flow::store_auth_state(&sess.token_path, &auth) {
                Ok(()) => {
                    *sess.done.lock().await = Some(auth);
                    Json(json!({ "state": "authorized", "user_id": uid }))
                }
                Err(e) => {
                    let msg = format!("登录态保存失败: {e}");
                    *sess.last_error.lock().await = Some(msg.clone());
                    Json(json!({ "state": "error", "message": msg }))
                }
            }
        }
        Ok(None) => {
            let state = if crate::xunlei::client::now_unix() >= d.expires_at {
                "expired"
            } else {
                "pending"
            };
            Json(json!({
                "state": state,
                "user_code": d.user_code,
                "web_auth_url": d.web_auth_url,
            }))
        }
        Err(e) => {
            let s = e.to_string();
            let state = if s.contains("过期") { "expired" } else { "error" };
            Json(json!({ "state": state, "message": s }))
        }
    }
}

#[derive(Deserialize)]
struct PwdReq {
    username: String,
    password: String,
}

async fn pwd_login(State(sess): State<Arc<LoginSession>>, Json(req): Json<PwdReq>) -> impl IntoResponse {
    match login_flow::login_with_password(&sess.client, &req.username, &req.password).await {
        Ok(auth) => finalize(sess, auth).await,
        Err(e) => err_resp(&e),
    }
}

#[derive(Deserialize)]
struct SmsSendReq {
    phone: String,
}

async fn sms_send(State(sess): State<Arc<LoginSession>>, Json(req): Json<SmsSendReq>) -> axum::response::Response {
    match login_flow::send_sms_code(&sess.client, &req.phone).await {
        Ok(vid) => {
            // 服务端记住会话 id：前端断链/刷新丢失时 verify 兜底（Task 25）。
            *sess.sms_verification_id.lock().await = Some(vid.clone());
            Json(json!({ "ok": true, "verification_id": vid })).into_response()
        }
        Err(e) => err_resp(&e),
    }
}

#[derive(Deserialize)]
struct SmsVerifyReq {
    phone: String,
    code: String,
    /// 发送时返回的验证码会话 id（前端透传；为空则尝试不带）。
    #[serde(default)]
    verification_id: String,
}

async fn sms_verify(State(sess): State<Arc<LoginSession>>, Json(req): Json<SmsVerifyReq>) -> axum::response::Response {
    // 兜底链：前端没带 verification_id（旧版页面/刷新丢失）时用 send 时记住的。
    let mut vid = req.verification_id;
    if vid.is_empty() {
        vid = sess.sms_verification_id.lock().await.clone().unwrap_or_default();
    }
    match login_flow::verify_sms_code(&sess.client, &req.phone, &req.code, &vid).await {
        Ok(auth) => finalize(sess, auth).await,
        Err(e) => err_resp(&e),
    }
}

/// 登录成功收口：落盘 + 返回 user_id（不回传 token，凭证只落盘）。
async fn finalize(sess: Arc<LoginSession>, auth: AuthState) -> axum::response::Response {
    match login_flow::store_auth_state(&sess.token_path, &auth) {
        Ok(()) => {
            *sess.done.lock().await = Some(auth.clone());
            Json(json!({ "ok": true, "user_id": auth.user_id })).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "ok": false, "message": format!("登录态保存失败: {e}") })),
        )
            .into_response(),
    }
}

fn err_resp(e: &ClientError) -> axum::response::Response {
    // 不回显任何 token/敏感字段，只透出业务可读信息。
    let (status, msg) = match e {
        ClientError::Http(re) if re.status().map(|s| s.as_u16()) == Some(400) => (
            StatusCode::UNAUTHORIZED,
            "账号或密码错误（或触发风控，请改用扫码）".to_string(),
        ),
        ClientError::Http(re) if re.status().map(|s| s.as_u16()) == Some(401) => (
            StatusCode::UNAUTHORIZED,
            "验证码错误或已过期".to_string(),
        ),
        other => (StatusCode::BAD_GATEWAY, other.to_string()),
    };
    (status, Json(json!({ "ok": false, "message": msg }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本地 mock 迅雷端点（xluser + pan 一体）。
    /// 行为：/v1/auth/device/code 发固定 device code；
    /// /v1/auth/token 前 N 次返回 authorization_pending，之后返回 token。
    async fn spawn_mock_upstream() -> String {
        let pending = Arc::new(std::sync::atomic::AtomicUsize::new(2));
        let app = Router::new()
            .route(
                "/v1/auth/device/code",
                post(|| async {
                    axum::Json(serde_json::json!({
                        "device_code": "DC123", "user_code": "UC5678",
                        "verification_url": "https://pan.xunlei.com/yc/",
                        "expires_in": 300, "interval": 1
                    }))
                }),
            )
            .route(
                "/v1/auth/token",
                post(move || {
                    let pending = pending.clone();
                    async move {
                        if pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                            axum::Json(serde_json::json!({"error": "authorization_pending"}))
                        } else {
                            axum::Json(serde_json::json!({
                                "access_token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiI5OTkiLCJleHAiOjk5OTk5OTk5OTl9.sig",
                                "refresh_token": "rt_x", "expires_in": 7200
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/shield/captcha/init",
                post(|| async {
                    axum::Json(serde_json::json!({"captcha_token": "ck_mock", "expires_in": 300}))
                }),
            )
            .route(
                "/v1/auth/signin",
                post(|| async {
                    axum::Json(serde_json::json!({
                        "access_token": "eyJhbGciOiJub25lIn0.eyJzdWIiOiI3NzciLCJleHAiOjk5OTk5OTk5OTl9.sig",
                        "refresh_token": "rt_pwd", "expires_in": 7200
                    }))
                }),
            )
            // 短信两步（Task 25 e2e）：send 下发会话 id；verify 返
            // verification_token（SDK 口径），第二段走 /v1/auth/signin 换 token。
            .route(
                "/v1/auth/verification",
                post(|| async { axum::Json(serde_json::json!({ "verification_id": "VID9" })) }),
            )
            .route(
                "/v1/auth/verification/verify",
                post(|| async { axum::Json(serde_json::json!({ "verification_token": "VT9" })) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { let _ = axum::serve(listener, app).await; });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn login_page_e2e_device_flow() {
        let upstream = spawn_mock_upstream().await;
        let client = Client::with_bases(upstream.clone(), upstream.clone());
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("auth.json");
        let sess = LoginSession::new(client, token_path.clone());

        let app = login_router(sess.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { let _ = axum::serve(listener, app).await; });
        let base = format!("http://{addr}");

        let http = reqwest::Client::new();
        // 1) 页面可访问且是 App 风格（含迅雷品牌与三 Tab）
        let html = http.get(format!("{base}/")).send().await.unwrap();
        assert!(html.status().is_success());
        let body = html.text().await.unwrap();
        assert!(body.contains("迅雷"));
        assert!(body.contains("扫码登录") && body.contains("密码登录") && body.contains("短信登录"));
        assert!(body.contains("第三方") && body.contains("微信") && body.contains("微博"));

        // 2) start → pending → （mock 第 3 次 poll 放行）→ authorized
        let r = http.post(format!("{base}/api/start")).send().await.unwrap();
        assert!(r.status().is_success());
        let j: serde_json::Value = r.json().await.unwrap();
        assert_eq!(j["user_code"], "UC5678");
        // Task 25：start 必须回传带 scope 的官方授权页链接（第三方 Tab 用）。
        assert_eq!(j["web_auth_url"],
            "https://pan.xunlei.com/yc/?client_id=Xqp0kJBXWhwaTpB6&user_code=UC5678&scope=profile%20offline%20pan%20sso%20user");

        // 二维码 SVG 端点
        let svg = http.get(format!("{base}/api/qr.svg")).send().await.unwrap();
        assert_eq!(svg.headers()["Content-Type"], "image/svg+xml");
        assert!(svg.text().await.unwrap().contains("<svg"));

        let mut authorized = false;
        for _ in 0..6 {
            let st: serde_json::Value =
                http.get(format!("{base}/api/status")).send().await.unwrap().json().await.unwrap();
            if st["state"] == "authorized" {
                assert_eq!(st["user_id"], "999");
                authorized = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        assert!(authorized, "设备码流程未在轮询窗口内授权");

        // 3) 登录态已落盘且可读回
        let stored = crate::xunlei::auth::load(&token_path);
        assert!(stored.is_some(), "token 未落盘");
        assert_eq!(stored.unwrap().user_id, "999");
    }

    #[tokio::test]
    async fn login_page_password_flow() {
        let upstream = spawn_mock_upstream().await;
        let client = Client::with_bases(upstream.clone(), upstream.clone());
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("auth.json");
        let sess = LoginSession::new(client, token_path.clone());
        let app = login_router(sess);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { let _ = axum::serve(listener, app).await; });
        let base = format!("http://{addr}");

        let http = reqwest::Client::new();
        let r = http
            .post(format!("{base}/api/login/pwd"))
            .json(&json!({ "username": "+8613800000000", "password": "pw" }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
        let j: serde_json::Value = r.json().await.unwrap();
        assert_eq!(j["ok"], true);
        assert_eq!(j["user_id"], "777");
        assert_eq!(crate::xunlei::auth::load(&token_path).unwrap().user_id, "777");
    }

    #[tokio::test]
    async fn login_page_sms_flow_with_fallback() {
        // Task 25 短信链路 e2e：send 返回 verification_id；verify 故意不带
        // verification_id（模拟旧前端断链/页面刷新丢失），服务端兜底应生效。
        let upstream = spawn_mock_upstream().await;
        let client = Client::with_bases(upstream.clone(), upstream.clone());
        let dir = tempfile::tempdir().unwrap();
        let token_path = dir.path().join("auth.json");
        let sess = LoginSession::new(client, token_path.clone());
        let app = login_router(sess);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { let _ = axum::serve(listener, app).await; });
        let base = format!("http://{addr}");

        let http = reqwest::Client::new();
        let r = http
            .post(format!("{base}/api/login/sms/send"))
            .json(&json!({ "phone": "13800000000" }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
        let j: serde_json::Value = r.json().await.unwrap();
        assert_eq!(j["verification_id"], "VID9");

        // 故意不带 verification_id —— 服务端记住的会话 id 应兜底。
        let r = http
            .post(format!("{base}/api/login/sms/verify"))
            .json(&json!({ "phone": "13800000000", "code": "123456" }))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "verify 兜底失败: {}", r.status());
        let j: serde_json::Value = r.json().await.unwrap();
        assert_eq!(j["ok"], true);
        assert!(crate::xunlei::auth::load(&token_path).is_some(), "短信登录态未落盘");
    }
}
