//! 迅雷 HTTP 客户端：三要素头 + OAuth refresh + captcha 刷新。

use crate::xunlei::auth::AuthState;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;

pub const CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";
pub const XLUSER_BASE: &str = "https://xluser-ssl.xunlei.com";
pub const PAN_BASE: &str = "https://api-pan.xunlei.com";

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("auth missing")]
    NoAuth,
}

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Self {
        Client { http: reqwest::Client::new() }
    }

    /// 构造 drive API 的三要素请求头。
    fn auth_headers(state: &AuthState) -> HeaderMap {
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
        #[derive(Deserialize)]
        struct TokenResp { access_token: String, refresh_token: String, expires_in: u64 }
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
