//! 夸克网盘类型：登录态（Cookie）、错误分类、端点常量。
//!
//! 来源：`docs/research/clients/multi_downloader/analysis/05_quark/
//! quark_architecture.md`（installer 逆向：UA/Referer/阿里系组件清单）
//! 与通用网盘 REST 形状（`pr=ucpro&fr=pc` 公共参数与端点路径对齐
//! 夸克 PC Web API 的公开互操作实现，**端点形状待真机验证**——本任务
//! 的 05_quark 分析只覆盖 installer stub，未含分享 API 抓包）。

use serde::{Deserialize, Serialize};

/// 云端 API 基址（夸克 PC 端 drive 网关，1.0/clouddrive）。
pub const BASE: &str = "https://drive-pc.quark.cn/1.0/clouddrive";

/// PC 端 Referer（夸克网页/客户端域）。
pub const REFERER: &str = "https://pan.quark.cn";

/// User-Agent：对齐夸克 PC 客户端伪装 Chrome + QuarkPC 标记
/// （来源 quark_architecture.md §3.3：`Chrome/130 … QuarkPC/4.3.0.0`）。
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 QuarkPC/4.3.0.0";

/// 公共查询参数（夸克 PC 端请求统一携带 `pr=ucpro&fr=pc`）。
pub const PR_FR: &str = "pr=ucpro&fr=pc";

/// 夸克登录态：PC 端以 Cookie 承载（无 OAuth；用户从浏览器导出或登录流程注入）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct QuarkAuth {
    /// 完整 Cookie 串（`__pus`/`__puus` 等，原样透传）。
    #[serde(default)]
    pub cookie: String,
    /// 用户 id（可空；来自接口响应或用户填写）。
    #[serde(default)]
    pub user_id: String,
}

impl QuarkAuth {
    /// 是否具备可用的登录态（cookie 非空即认为已登录，有效性由服务端裁决）。
    pub fn is_valid(&self) -> bool {
        !self.cookie.trim().is_empty()
    }
}

/// 从磁盘加载登录态；不存在/解析失败返回 None。
pub fn load_auth(path: &std::path::Path) -> Option<QuarkAuth> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

/// 原子写登录态（临时文件 + rename；内容不变跳过写盘，
/// 语义对齐 `xunlei::auth::save` 的 Bug B 修复，避免高频落盘）。
pub fn save_auth(path: &std::path::Path, auth: &QuarkAuth) -> std::io::Result<()> {
    let serialized = serde_json::to_string(auth)?;
    if let Ok(existing) = std::fs::read_to_string(path) {
        if existing.trim_end() == serialized {
            return Ok(());
        }
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &serialized)?;
    std::fs::rename(&tmp, path)
}

/// 夸克渠道错误分类（决策依据：对齐 `xunlei::provider` 的失败冷却模式）。
///
/// 分类到动作：
/// - [`QuarkError::NotLogin`] → `ProviderError::Auth`（冷却 5 分钟）
/// - [`QuarkError::QuotaExhausted`] → `ProviderError::Quota`（冷却 1 小时）
/// - 其余 → `ProviderError::Other`（冷却 1 分钟）
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum QuarkError {
    #[error("夸克未登录或登录态失效")]
    NotLogin,
    #[error("夸克分享已失效/被取消/提取码错误")]
    ShareExpired,
    #[error("夸克配额不足（网盘容量/转存次数）")]
    QuotaExhausted,
    #[error("不是夸克网盘分享链接")]
    NotShareLink,
    #[error("夸克接口响应异常: {0}")]
    BadResponse(String),
    #[error("夸克网络错误: {0}")]
    Network(String),
}

impl From<QuarkError> for crate::types::ProviderError {
    fn from(e: QuarkError) -> Self {
        use crate::types::ProviderError;
        match e {
            QuarkError::NotLogin => ProviderError::Auth,
            QuarkError::QuotaExhausted => ProviderError::Quota,
            QuarkError::NotShareLink => {
                ProviderError::Other("source is not a quark share link".into())
            }
            QuarkError::ShareExpired => {
                ProviderError::Other("quark share expired or passcode wrong".into())
            }
            QuarkError::BadResponse(m) => ProviderError::Other(format!("quark bad response: {m}")),
            QuarkError::Network(m) => ProviderError::Other(format!("quark network: {m}")),
        }
    }
}

/// 夸克统一响应壳：`{"status":200,"code":0,"message":"","data":...}`。
///
/// 分类规则（**具体业务码待真机验证**，字符串匹配作为兜底路径）：
/// - HTTP 401/403 或 message 命中 login/登录 → NotLogin
/// - `code==32003`（公开互操作实现中的 cookie 失效码）→ NotLogin
/// - message 命中「分享失效/取消/不存在/提取码」→ ShareExpired
/// - message 命中「容量/空间不足/转存次数/会员」→ QuotaExhausted
pub(crate) fn classify_envelope(
    http_status: u16,
    code: Option<i64>,
    message: Option<String>,
) -> Option<QuarkError> {
    let msg = message.unwrap_or_default();
    if http_status == 401 || http_status == 403 || msg.contains("login") || msg.contains("登录") {
        return Some(QuarkError::NotLogin);
    }
    if code == Some(32003) {
        return Some(QuarkError::NotLogin);
    }
    for kw in [
        "分享已取消",
        "分享不存在",
        "分享已失效",
        "失效",
        "提取码",
        "passcode",
    ] {
        if msg.contains(kw) {
            return Some(QuarkError::ShareExpired);
        }
    }
    for kw in ["容量", "空间不足", "转存次数", "会员"] {
        if msg.contains(kw) {
            return Some(QuarkError::QuotaExhausted);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_roundtrip_and_skip_unchanged_write() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("quark_auth.json");
        let a = QuarkAuth {
            cookie: "__pus=x; __puus=y".into(),
            user_id: "u1".into(),
        };
        assert!(a.is_valid());
        save_auth(&p, &a).unwrap();
        assert_eq!(load_auth(&p), Some(a.clone()));
        // 内容不变 → 仍成功（内部跳过写盘）
        save_auth(&p, &a).unwrap();
        assert_eq!(load_auth(&p), Some(a));
        // 空登录态无效
        assert!(!QuarkAuth::default().is_valid());
    }

    #[test]
    fn load_missing_returns_none() {
        assert_eq!(
            load_auth(std::path::Path::new("nonexistent_quark.json")),
            None
        );
    }

    #[test]
    fn envelope_classification() {
        assert_eq!(
            classify_envelope(401, None, None),
            Some(QuarkError::NotLogin)
        );
        assert_eq!(
            classify_envelope(200, Some(32003), None),
            Some(QuarkError::NotLogin)
        );
        assert_eq!(
            classify_envelope(200, Some(1), Some("分享已失效".into())),
            Some(QuarkError::ShareExpired)
        );
        assert_eq!(
            classify_envelope(200, Some(1), Some("网盘容量不足".into())),
            Some(QuarkError::QuotaExhausted)
        );
        // 业务失败但无法归类 → None（由调用方给 BadResponse）
        assert_eq!(
            classify_envelope(200, Some(99999), Some("unknown".into())),
            None
        );
        // 成功壳不产生错误
        assert_eq!(classify_envelope(200, Some(0), None), None);
    }

    #[test]
    fn error_maps_to_provider_error() {
        use crate::types::ProviderError;
        assert_eq!(
            ProviderError::from(QuarkError::NotLogin),
            ProviderError::Auth
        );
        assert_eq!(
            ProviderError::from(QuarkError::QuotaExhausted),
            ProviderError::Quota
        );
        assert!(matches!(
            ProviderError::from(QuarkError::ShareExpired),
            ProviderError::Other(_)
        ));
    }
}
