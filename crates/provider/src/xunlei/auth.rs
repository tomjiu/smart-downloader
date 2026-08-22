//! 登录态（OAuth 设备码流程的产物）+ token 持久化。

use serde::{Deserialize, Serialize};

/// 登录态三要素 + OAuth token。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthState {
    pub access_token: String,
    pub refresh_token: String,
    pub device_id: String,
    pub captcha_token: String,
    pub access_token_expires_at: u64,
    pub captcha_token_expires_at: u64,
}

impl AuthState {
    pub fn access_token_expiring(&self, now: u64) -> bool {
        now + 300 >= self.access_token_expires_at
    }
    pub fn captcha_token_expiring(&self, now: u64) -> bool {
        now + 60 >= self.captcha_token_expires_at
    }
}

/// 从磁盘加载；不存在返回 None。
pub fn load(path: &std::path::Path) -> Option<AuthState> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

/// 原子写（临时文件 + rename）。
pub fn save(path: &std::path::Path, state: &AuthState) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string(state).unwrap())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AuthState {
        AuthState {
            access_token: "at".into(), refresh_token: "rt".into(),
            device_id: "dev".into(), captcha_token: "ck".into(),
            access_token_expires_at: 1000, captcha_token_expires_at: 500,
        }
    }
    #[test]
    fn roundtrip_json() {
        let a = state();
        let j = serde_json::to_string(&a).unwrap();
        assert_eq!(serde_json::from_str::<AuthState>(&j).unwrap(), a);
    }
    #[test]
    fn access_token_expiring() {
        let a = state();
        assert!(!a.access_token_expiring(0));
        assert!(a.access_token_expiring(800));
    }
    #[test]
    fn captcha_token_expiring() {
        let a = state();
        assert!(!a.captcha_token_expiring(0));
        assert!(a.captcha_token_expiring(450));
    }
    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("auth.json");
        let a = state();
        save(&p, &a).unwrap();
        assert_eq!(load(&p), Some(a));
    }
    #[test]
    fn load_nonexistent_is_none() {
        assert_eq!(load(std::path::Path::new("nonexistent_xyz.json")), None);
    }
}
