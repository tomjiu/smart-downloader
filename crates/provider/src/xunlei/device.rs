//! OAuth 2.0 设备码流程（RFC 8628）状态机。

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
