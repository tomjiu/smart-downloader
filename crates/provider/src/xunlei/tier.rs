//! 平台身份档位（P1-1 多 profile）：同一套登录协议，按档位换参数集。
//!
//! # 背景（2026-08-31 收尾计划 §P1-1）
//!
//! 服务端视角的「平台」不是硬件属性，而是一组**身份参数**：
//! `client_id`（OAuth 流程 + `x-client-id` 头）+ `package_name`/`client_version`
//! （captcha/init meta）。同一账号用不同参数集登录，云端即视为不同客户端档位，
//! 风控与权限面按档位下发（A4 实证：docker 档任务对象携带
//! `client_id=X9ibIS… / package_name=pan.xunlei.cli.docker / platform=docker`）。
//!
//! 因此跨平台身份可移植：client_id 是编译期常量、device_id 软件生成并随
//! AuthState 落盘、token 走 RFC 8628 设备码流——无需任何硬件证明。
//!
//! # 已注册档位
//!
//! | 档位  | client_id          | package_name           | 证据                          |
//! |-------|--------------------|------------------------|-------------------------------|
//! | `web` | `Xqp0kJBXWhwaTpB6` | `pan.xunlei.com`       | A 级（web 页一手 dump+实测）  |
//! | `nas` | `X9ibISwpIp8jQ4Ya` | `pan.xunlei.cli.docker`| A 级（a4_run3.json 任务对象） |
//!
//! `nas` 档 client_version 取引擎二进制版本 3.23.5（B 级：xunlei-pan-cli.3.23.5
//! 文件名 + A2 校准对象）；captcha_sign 盐链沿用 web 链（假设区：该档未在
//! Rust 侧实弹验证，NAS 实弹路径仍是引擎二进制托管，见 docs/nas/NAS_REMOTE_ENGINE.md）。
//!
//! # 三条纪律（防互踢/防风控）
//!
//! 1. **每档独立 device_id**：登录态按档分文件（`xunlei_auth_<tier>.json`），
//!    同账号多档并存时不互踢会话；
//! 2. **参数与档位严格匹配**：client_version/package_name 随档位下发，禁止
//!    混搭（错档 = 服务端可见的异常指纹）；
//! 3. **L3 永不入表**：私有加速（RSA-1024 每请求随机密钥）不属于身份档位，
//!    永不收录（收尾计划红线）。
//!
//! # 归属边界
//!
//! `share.rs` / `cloud_search.rs` 是网页端专有功能，**显式钉死 web 档**
//! （直接引用 client 模块常量，不随 Client 档位漂移）；`vip_speedup.rs`
//! 属 L3 邻接面，其 `X_CLIENT_ID` 独立维护，不进档位表。

use crate::xunlei::client::QR_AUTHORIZE_URL_TEMPLATE;

/// 单个身份档位的参数集。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tier {
    /// 档位名（配置 `provider_xunlei.tier` / CLI `--tier` / 登录态分文件后缀）。
    pub name: &'static str,
    /// OAuth 全流程 client_id（设备码请求/轮询/refresh + `x-client-id` 头）。
    pub client_id: &'static str,
    /// captcha/init meta.package_name。
    pub package_name: &'static str,
    /// captcha/init meta.client_version（同时是 captcha_sign base 的 version 段）。
    pub client_version: &'static str,
    /// captcha_sign base 的 host 段（web 页固定 pan.xunlei.com）。
    pub host: &'static str,
    /// 该档授权页说明（CLI 打印；/yc/ 统一授权页按 client_id 出对应客户端文案）。
    pub authorize_note: &'static str,
}

/// web 档：pan.xunlei.com 网页版（本仓默认档，95% 能力面已实测）。
pub const TIER_WEB: Tier = Tier {
    name: "web",
    client_id: crate::xunlei::client::CLIENT_ID,
    package_name: crate::xunlei::sign::PACKAGE_NAME,
    client_version: crate::xunlei::client::CLIENT_VERSION,
    host: "pan.xunlei.com",
    authorize_note: "网页版身份（pan.xunlei.com 同款）；官方授权页内扫码/账密/短信/第三方均可",
};

/// nas 档：群晖套件 pan-cli/docker 身份（A2-A5 校准对象；未实弹验证，
/// 每日提交策略与 90120 属该档云端裁剪，见 A6_PREP §8/§10）。
pub const TIER_NAS: Tier = Tier {
    name: "nas",
    client_id: "X9ibISwpIp8jQ4Ya",
    package_name: "pan.xunlei.cli.docker",
    client_version: "3.23.5",
    host: "pan.xunlei.com",
    authorize_note: "群晖套件/docker 身份（pan-cli 同款）；授权页按套件客户端出确认文案",
};

/// 全量已注册档位（未知档一律拒绝启动，防错档静默运行）。
pub const ALL_TIERS: [&Tier; 2] = [&TIER_WEB, &TIER_NAS];

impl Tier {
    /// 按名查档；未知名字返回 None（调用方拒绝启动）。
    pub fn by_name(name: &str) -> Option<&'static Tier> {
        ALL_TIERS.into_iter().find(|t| t.name == name)
    }
}

impl Default for &'static Tier {
    fn default() -> Self {
        &TIER_WEB
    }
}

/// 构造该档的 `/yc/` 统一授权页 URL（client_id 随档位；scope 显式透传）。
///
/// 与 [`crate::xunlei::client::web_auth_url`] 同构，但 client_id 取自档位表
/// 而非 web 常量——`web` 档两者逐字节一致（单测守护）。
pub fn tier_authorize_url(tier: &Tier, user_code: &str, scope: &str) -> String {
    QR_AUTHORIZE_URL_TEMPLATE
        .replace("{client_id}", tier.client_id)
        .replace("{user_code}", user_code)
        + "&scope="
        + &scope.replace(' ', "%20")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_finds_registered_tiers() {
        assert_eq!(Tier::by_name("web"), Some(&TIER_WEB));
        assert_eq!(Tier::by_name("nas"), Some(&TIER_NAS));
        assert_eq!(Tier::by_name("android"), None, "未注册档拒绝");
        assert_eq!(Tier::by_name(""), None);
        assert_eq!(Tier::by_name("WEB"), None, "大小写敏感，防手滑错档");
    }

    #[test]
    fn web_tier_matches_verified_constants() {
        // 防回归：web 档必须与实测常量逐字节一致（client.rs / sign.rs 权威源）。
        assert_eq!(TIER_WEB.client_id, "Xqp0kJBXWhwaTpB6");
        assert_eq!(TIER_WEB.client_id, crate::xunlei::client::CLIENT_ID);
        assert_eq!(TIER_WEB.package_name, "pan.xunlei.com");
        assert_eq!(TIER_WEB.package_name, crate::xunlei::sign::PACKAGE_NAME);
        assert_eq!(TIER_WEB.client_version, "1.92.91");
        assert_eq!(TIER_WEB.host, "pan.xunlei.com");
    }

    #[test]
    fn nas_tier_matches_calibration_evidence() {
        // 防回归：nas 档参数 = A4 任务对象实测值（a4_run3.json）。
        assert_eq!(TIER_NAS.client_id, "X9ibISwpIp8jQ4Ya");
        assert_eq!(TIER_NAS.package_name, "pan.xunlei.cli.docker");
        assert_eq!(TIER_NAS.client_version, "3.23.5");
    }

    #[test]
    fn tiers_differ_in_identity_params() {
        // 多档的意义：参数集必须可区分（否则「切档」是空操作）。
        assert_ne!(TIER_WEB.client_id, TIER_NAS.client_id);
        assert_ne!(TIER_WEB.package_name, TIER_NAS.package_name);
        assert_ne!(TIER_WEB.client_version, TIER_NAS.client_version);
    }

    #[test]
    fn web_authorize_url_identical_to_client_free_fn() {
        // web 档授权页必须与既有 free fn 逐字节一致（老用户零回归）。
        let scope = crate::xunlei::login_flow::DEVICE_SCOPE;
        let via_tier = tier_authorize_url(&TIER_WEB, "UC1234", scope);
        let via_free = crate::xunlei::client::web_auth_url("UC1234", scope);
        assert_eq!(via_tier, via_free);
        assert!(via_tier.contains("client_id=Xqp0kJBXWhwaTpB6"));
        assert!(via_tier.contains("user_code=UC1234"));
        assert!(via_tier.ends_with("&scope=profile%20offline%20pan%20sso%20user"));
    }

    #[test]
    fn nas_authorize_url_carries_nas_client_id() {
        let url = tier_authorize_url(&TIER_NAS, "AB12-CD34", "profile offline pan sso user");
        assert!(url.contains("client_id=X9ibISwpIp8jQ4Ya"));
        assert!(url.contains("user_code=AB12-CD34"));
    }

    #[test]
    fn default_tier_is_web() {
        let d: &'static Tier = Default::default();
        assert_eq!(d.name, "web");
    }
}
