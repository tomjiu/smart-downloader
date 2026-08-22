//! 迅雷 HTTP 客户端：三要素头 + OAuth refresh + captcha 刷新。

use crate::xunlei::auth::AuthState;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

/// pan 网盘场景的 client_id（取链/captcha/refresh 用）。
pub const CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";
/// 网页登录（设备码流程）的 app_id（扫码登录用）。
pub const DEVICE_CLIENT_ID: &str = "XW5SkOhLDjnOZP7J";
pub const XLUSER_BASE: &str = "https://xluser-ssl.xunlei.com";
pub const PAN_BASE: &str = "https://api-pan.xunlei.com";

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("auth missing")]
    NoAuth,
    #[error("device flow: {0}")]
    DeviceFlow(String),
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Self {
        Client { http: reqwest::Client::new() }
    }

    /// 构造 drive API 的三要素请求头。
    pub(crate) fn auth_headers(state: &AuthState) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", state.access_token)).unwrap());
        h.insert("x-device-id", HeaderValue::from_str(&state.device_id).unwrap());
        h.insert("x-captcha-token", HeaderValue::from_str(&state.captcha_token).unwrap());
        h.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h
    }

    /// refresh_token 换新 access_token（已验证可行）。
    pub async fn refresh(&self, state: &mut AuthState) -> Result<(), ClientError> {
        let resp: TokenResp = self.http
            .post(format!("{}/v1/auth/token", XLUSER_BASE))
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": state.refresh_token,
                "client_id": CLIENT_ID,
            }))
            .send().await?.error_for_status()?.json().await?;
        state.access_token = resp.access_token;
        state.refresh_token = resp.refresh_token;
        state.access_token_expires_at = now_unix() + resp.expires_in;
        Ok(())
    }

    /// 匿名获取/刷新 captcha_token（已验证可行，300s）。
    pub async fn refresh_captcha(&self, state: &mut AuthState) -> Result<(), ClientError> {
        #[derive(Deserialize)]
        struct CaptchaResp { captcha_token: String, expires_in: u64 }
        let resp: CaptchaResp = self.http
            .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
            .json(&serde_json::json!({
                "action": "POST:/drive/v1/files",
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": state.device_id,
                "meta": {},
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
            }))
            .send().await?.error_for_status()?.json().await?;
        state.captcha_token = resp.captcha_token;
        state.captcha_token_expires_at = now_unix() + resp.expires_in;
        Ok(())
    }

    /// 请求设备码（RFC 8628 设备码流程第一步，已实测端点）。
    pub async fn request_device_code(&self, scope: &str) -> Result<DeviceCode, ClientError> {
        #[derive(Deserialize)]
        struct Resp {
            device_code: String,
            user_code: String,
            #[serde(default)] verification_uri_complete: String,
            #[serde(default)] verification_url: String,
            expires_in: u64,
            #[serde(default)] interval: u64,
        }
        let resp: Resp = self.http
            .post(format!("{}/v1/auth/device/code", XLUSER_BASE))
            .form(&[("scope", scope), ("client_id", DEVICE_CLIENT_ID)])
            .send().await?.error_for_status()?.json().await?;
        Ok(DeviceCode {
            device_code: resp.device_code,
            user_code: resp.user_code,
            verification_uri: if resp.verification_uri_complete.is_empty() { resp.verification_url } else { resp.verification_uri_complete },
            expires_in: resp.expires_in,
            interval: resp.interval,
        })
    }

    /// 轮询设备码是否已被扫码授权（RFC 8628 第二步，已实测端点）。
    /// 返回 `Ok(Some(TokenResp))` = 授权成功；`Ok(None)` = 未授权（authorization_pending/slow_down）。
    pub async fn poll_device_token(&self, device_code: &str) -> Result<Option<TokenResp>, ClientError> {
        #[derive(Deserialize)]
        struct ErrResp { error: String }
        let resp = self.http
            .post(format!("{}/v1/auth/token", XLUSER_BASE))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", DEVICE_CLIENT_ID),
            ])
            .send().await?;
        let status = resp.status();
        if status.is_success() {
            let token: TokenResp = resp.json().await?;
            return Ok(Some(token));
        }
        // 非 2xx：解析 error 字段，authorization_pending/slow_down = 未授权（继续等）
        let body: Result<ErrResp, _> = resp.json().await;
        match body {
            Ok(e) if e.error == "authorization_pending" || e.error == "slow_down" => Ok(None),
            Ok(e) => Err(ClientError::DeviceFlow(e.error)),
            Err(_) => Err(ClientError::DeviceFlow(format!("poll failed with status {}", status))),
        }
    }

    /// 取直链：调 PLAY API 拿 web_content_link（F2/F3 已验证端点）。
    /// 返回 (name, web_content_link)。size/expires 由调用方从 URL 参数解析（f=/e=）。
    pub async fn resolve_link(&self, state: &AuthState, file_id: &str) -> Result<PlayResp, ClientError> {
        let url = format!("{}/drive/v1/files/{}?space=&usage=PLAY", PAN_BASE, file_id);
        let resp: PlayResp = self.http
            .get(url)
            .headers(Self::auth_headers(state))
            .send().await?
            .error_for_status()?
            .json().await?;
        Ok(resp)
    }
}

/// 设备码响应（request_device_code 的返回）。
#[derive(Clone, Debug)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// 二维码内容（verification_uri_complete，前端把它转成二维码图片）。
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// token 端点成功响应（refresh / 设备码轮询成功共用结构）。
#[derive(Clone, Debug, Deserialize)]
pub struct TokenResp {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// PLAY API 响应（files/{id}?usage=PLAY）。
#[derive(Clone, Debug, Deserialize)]
pub struct PlayResp {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub web_content_link: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AuthState {
        AuthState {
            access_token: "at123".into(), refresh_token: "rt".into(),
            device_id: "dev456".into(), captcha_token: "ck789".into(),
            access_token_expires_at: 0, captcha_token_expires_at: 0,
        }
    }
    #[test]
    fn auth_headers_has_three_elements() {
        let h = Client::auth_headers(&state());
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer at123");
        assert_eq!(h.get("x-device-id").unwrap(), "dev456");
        assert_eq!(h.get("x-captcha-token").unwrap(), "ck789");
        assert_eq!(h.get("x-client-id").unwrap(), CLIENT_ID);
    }
}
