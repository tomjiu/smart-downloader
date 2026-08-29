//! 登录态（OAuth 设备码流程的产物）+ token 持久化。

use serde::{Deserialize, Serialize};

/// 登录态三要素 + OAuth token。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthState {
    pub access_token: String,
    pub refresh_token: String,
    pub device_id: String,
    pub captcha_token: String,
    /// 用户 ID（数字字符串，如 "860599297"）。来自 access_token JWT 的 `sub` 声明。
    /// captcha/init 的 meta.user_id 需要它。
    #[serde(default)]
    pub user_id: String,
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

    /// 从 access_token（JWT）解析 user_id（`sub` 声明），填充 `self.user_id`。
    /// 若解析失败或 user_id 已存在则不动。
    pub fn fill_user_id_from_token(&mut self) {
        if !self.user_id.is_empty() {
            return;
        }
        if let Some(sub) = jwt_sub(&self.access_token) {
            self.user_id = sub;
        }
    }
}

/// 从 JWT 的 payload 段解析 `sub` 声明（不校验签名，仅取 user_id）。
///
/// JWT 格式：`header.payload.signature`（base64url）。payload 是 JSON，含 `sub`。
/// 迅雷 access_token 的 `sub` 即 user_id（如 "860599297"）。
pub fn jwt_sub(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64_url_decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("sub")?.as_str().map(|s| s.to_string())
}

/// base64url 解码（JWT 用，无 padding，`-`→`+`，`_`→`/`）。
fn base64_url_decode(s: &str) -> Option<Vec<u8>> {
    let mut b64 = s.replace('-', "+").replace('_', "/");
    while !b64.len().is_multiple_of(4) {
        b64.push('=');
    }
    // 依赖 base64 crate（Cargo.toml 需引入）或手写。这里用最小实现避免新依赖：
    decode_base64(&b64)
}

/// 极简 base64 解码（仅标准字母表 + padding，供 JWT 使用）。
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut buf: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let idx = TABLE.iter().position(|&t| t == c)?;
        buf = (buf << 6) | idx as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// 从磁盘加载；不存在返回 None。
///
/// 兼容两种格式：
/// 1. 标准 AuthState JSON（设备码流程产物）
/// 2. 网页版 localStorage 导出的凭证（credentials_Xqp0… 形状：只有
///    access_token/refresh_token/user_id，无 device_id/expires_at）
pub fn load(path: &std::path::Path) -> Option<AuthState> {
    let s = std::fs::read_to_string(path).ok()?;
    if let Ok(st) = serde_json::from_str::<AuthState>(&s) {
        return Some(st);
    }
    from_web_credentials_str(&s)
}

/// 从 JWT 的 payload 段解析 `exp` 声明（unix 秒）。
pub fn jwt_exp(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64_url_decode(payload)?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    json.get("exp")?.as_u64()
}

/// 本地随机 32 位 hex device_id。
/// 服务端不校验来源（README_captcha_sign §1.5 实测），本地随机即可。
fn random_device_id_32() -> String {
    use md5::{Digest as Md5Digest, Md5};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut h = Md5::new();
    h.update(format!("{nanos}-{pid}-{n}").as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 网页版 localStorage 导出凭证（credentials_Xqp0kJBXWhwaTpB6 形状）→ 登录态。
/// 必需字段 access_token/refresh_token；device_id 随机生成、expires_at 取 JWT exp、
/// user_id 缺省时从 JWT sub 解析。
pub fn from_web_credentials_str(s: &str) -> Option<AuthState> {
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    let access_token = v.get("access_token")?.as_str()?.to_string();
    let refresh_token = v.get("refresh_token")?.as_str()?.to_string();
    if access_token.is_empty() || refresh_token.is_empty() {
        return None;
    }
    let mut state = AuthState {
        access_token,
        refresh_token,
        device_id: random_device_id_32(),
        captcha_token: String::new(),
        user_id: v
            .get("user_id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        access_token_expires_at: 0,
        captcha_token_expires_at: 0,
    };
    state.access_token_expires_at = jwt_exp(&state.access_token)
        .unwrap_or_else(|| crate::xunlei::client::now_unix() + 3600);
    state.fill_user_id_from_token();
    Some(state)
}

/// 原子写（临时文件 + rename）。
pub fn save(path: &std::path::Path, state: &AuthState) -> std::io::Result<()> {
    // Bug B 修复：内容不变 → 跳过写盘。poll_ready 高频轮询下 refresh_auth 每次
    // 都落盘，同步 fs 写被 Defender/句柄卡住时会阻塞 tokio worker（运行时饿死）。
    let serialized = serde_json::to_string(state)?;
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing.trim_end() == serialized {
            return Ok(());
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &serialized)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AuthState {
        AuthState {
            access_token: "at".into(), refresh_token: "rt".into(),
            device_id: "dev".into(), captcha_token: "ck".into(),
            user_id: "123".into(),
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

    #[test]
    fn jwt_sub_parses_user_id() {
        // header.payload.signature，payload = {"sub":"860599297"}
        let header = "eyJhbGciOiJSUzI1NiJ9"; // {"alg":"RS256"}
        let payload = "eyJzdWIiOiI4NjA1OTkyOTcifQ"; // {"sub":"860599297"}
        let token = format!("{}.{}.sig", header, payload);
        assert_eq!(jwt_sub(&token).as_deref(), Some("860599297"));
    }

    #[test]
    fn fill_user_id_from_token_sets_empty() {
        let mut a = state();
        a.user_id.clear();
        let payload = "eyJzdWIiOiI4NjA1OTkyOTcifQ"; // {"sub":"860599297"}
        a.access_token = format!("h.{}.s", payload);
        a.fill_user_id_from_token();
        assert_eq!(a.user_id, "860599297");
    }

    #[test]
    fn fill_user_id_keeps_existing() {
        let mut a = state();
        a.user_id = "keep".into();
        a.fill_user_id_from_token();
        assert_eq!(a.user_id, "keep");
    }
}
