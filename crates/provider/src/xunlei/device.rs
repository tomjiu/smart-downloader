//! OAuth 2.0 设备码流程（RFC 8628）状态机 + 编排。

use crate::xunlei::client::{Client, ClientError, DeviceCode};

/// 设备码流程状态。
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceFlowState {
    AwaitingScan {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_at: u64,
    },
    Done {
        access_token: String,
        refresh_token: String,
    },
    Failed {
        reason: String,
    },
}

impl DeviceFlowState {
    /// 轮询一次：error_code None=成功，Some("authorization_pending")=继续等，
    /// Some("slow_down")=降速继续等，Some("expired_token")=过期失败。
    pub fn on_poll(&self, error_code: Option<&str>, now: u64) -> DeviceFlowState {
        match error_code {
            None => DeviceFlowState::Done { access_token: String::new(), refresh_token: String::new() },
            Some("expired_token") => DeviceFlowState::Failed { reason: "device code expired".into() },
            Some(_) => match self {
                DeviceFlowState::AwaitingScan { expires_at, .. } => {
                    if now >= *expires_at {
                        DeviceFlowState::Failed { reason: "timeout".into() }
                    } else {
                        self.clone()
                    }
                }
                _ => self.clone(),
            },
        }
    }
}

/// 设备码登录编排：请求 device code → 展示二维码 → 轮询 token。
/// 网络调用委托给 `Client`；本结构只负责把状态机串起来。
pub struct DeviceAuthFlow {
    client: Client,
}

impl DeviceAuthFlow {
    pub fn new(client: Client) -> Self {
        DeviceAuthFlow { client }
    }

    /// 发起设备码登录：请求 device code，返回包含二维码 URL 的 `AwaitingScan` 状态。
    pub async fn start(&self, scope: &str) -> Result<DeviceFlowState, ClientError> {
        let code: DeviceCode = self.client.request_device_code(scope).await?;
        let now = crate::xunlei::client::now_unix();
        Ok(DeviceFlowState::AwaitingScan {
            device_code: code.device_code,
            user_code: code.user_code,
            verification_uri: code.verification_uri,
            expires_at: now + code.expires_in,
        })
    }

    /// 轮询一次：若成功返回 `Done`（含 token），否则返回更新后的状态。
    /// 需要当前状态里的 device_code。
    pub async fn poll_once(&self, state: &DeviceFlowState) -> Result<DeviceFlowState, ClientError> {
        let device_code = match state {
            DeviceFlowState::AwaitingScan { device_code, .. } => device_code.clone(),
            _ => return Ok(state.clone()),
        };
        match self.client.poll_device_token(&device_code).await? {
            Some(token) => Ok(DeviceFlowState::Done {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
            }),
            None => Ok(state.on_poll(Some("authorization_pending"), crate::xunlei::client::now_unix())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn awaiting(expires_at: u64) -> DeviceFlowState {
        DeviceFlowState::AwaitingScan {
            device_code: "dc".into(), user_code: "uc".into(),
            verification_uri: "https://example.com".into(), expires_at,
        }
    }
    #[test]
    fn pending_keeps_waiting() {
        assert!(matches!(awaiting(1000).on_poll(Some("authorization_pending"), 500), DeviceFlowState::AwaitingScan { .. }));
    }
    #[test]
    fn slow_down_keeps_waiting() {
        assert!(matches!(awaiting(1000).on_poll(Some("slow_down"), 500), DeviceFlowState::AwaitingScan { .. }));
    }
    #[test]
    fn expired_token_fails() {
        assert!(matches!(awaiting(1000).on_poll(Some("expired_token"), 500), DeviceFlowState::Failed { .. }));
    }
    #[test]
    fn timeout_fails() {
        assert!(matches!(awaiting(1000).on_poll(Some("authorization_pending"), 1500), DeviceFlowState::Failed { .. }));
    }
    #[test]
    fn success_returns_done() {
        assert!(matches!(awaiting(1000).on_poll(None, 500), DeviceFlowState::Done { .. }));
    }
}
