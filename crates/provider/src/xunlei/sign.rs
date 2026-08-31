//! 迅雷云盘签名算法（captcha_sign / device_sign），纯函数。
//!
//! ## captcha_sign（Web 端，pan.xunlei.com）
//!
//! 算法逆向来源见仓库内文档：
//! `scripts/research/cloud_delivery/login_reverse/README_captcha_sign.md`
//!
//! ```text
//! base = clientId + version + host + deviceId(32位) + timestamp(毫秒)
//! s = base
//! for salt in WEB_SALTS: s = md5(s + salt)   // 共 9 轮
//! captcha_sign = "1." + s
//! ```
//!
//! 关键配置（来自主应用 config 模块 module 23，`funFile = "code-res"` 走本地盐链路径）：
//! - clientId  = `Xqp0kJBXWhwaTpB6`
//! - version   = `1.92.91`（package.json 模块 module 180）
//! - host      = `pan.xunlei.com`
//! - algVersion= `1`
//!
//! ⚠️ deviceId 必须是 **32 位 hex**（`wdi10.` 前缀的 64 位 device_id 去掉前缀后取前 32 位）。

#![allow(unused_imports)]

use md5::{Digest as Md5Digest, Md5};
use sha1::{Digest as Sha1Digest, Sha1};

/// 手写 hex 编码（避免引入 hex crate）。
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Web 端 captcha_sign 的 9 个盐（从 module 51→37→26→17→29→26→39→60→29 链逐层逆向，
/// 已用真实接口验证）。区别于 App 端（alist 的 10 个盐）。
const WEB_SALTS: [&str; 9] = [
    "tkPbM0TLWT+eMvAdV2FbXEEQ/Qx5QrfO895+47hmDDPdRZ98xm",
    "7EBc6XKuI6YGw19anZHmnE4d8W18zjrJU+F",
    "stEQvsO6eeP93DdrX7mfYA7G",
    "edXgGCdIaqdZJZH5k",
    "J9SB6D864S1B",
    "xlAs2Oo28sr",
    "21+f+kgyrbIcwUUo+xaPD4GYHkpRGv5i4wOnyHrkH4ehKti",
    "08kltU1bp6eV5bEdlgSEU0GpzjD7/j5X3FwbiiraEzar",
    "hX6tf7kBT/DS",
];

/// Web 端 client_id（pan.xunlei.com 场景）。
const CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";
/// Web 端版本（package.json 里的 version）。
const CLIENT_VERSION: &str = "1.92.91";
/// host（captcha_sign base 里的 host，pan.xunlei.com 场景固定）。
const HOST: &str = "pan.xunlei.com";
/// package_name（captcha/init meta 里用）。
pub const PACKAGE_NAME: &str = "pan.xunlei.com";
/// algVersion 前缀。
const ALG_VERSION: &str = "1";

// App 端（com.xunlei.downloadprovider）参数，device_sign 用。
const APP_PACKAGE_NAME: &str = "com.xunlei.downloadprovider";
const APPID: &str = "40";
const APPKEY: &str = "34a062aaa22f906fca4fefe9fb3a3021";

/// 从完整 device_id（`wdi10.` + 64位hex）提取 captcha_sign 用的 32 位 device_id。
///
/// 规则：去掉 `wdi10.` 前缀，取前 32 个 hex 字符。
/// 已验证：`wdi10.adb1a76709f6584a13b58baaf6e1d871d02650159e5762f2299e41b38b017500`
///       → `adb1a76709f6584a13b58baaf6e1d871`
pub fn device_id_32(device_id: &str) -> &str {
    let s = device_id.strip_prefix("wdi10.").unwrap_or(device_id);
    &s[..s.len().min(32)]
}

/// 计算 Web 端 captcha_sign：
///   s = clientId + version + host + deviceId(32位) + timestamp(毫秒)
///   9 轮 md5(s + salt)
///   返回 "1." + s
pub fn captcha_sign(device_id_32: &str, timestamp_millis: &str) -> String {
    captcha_sign_with(
        CLIENT_ID,
        CLIENT_VERSION,
        HOST,
        device_id_32,
        timestamp_millis,
    )
}

/// 档位泛化版 captcha_sign（P1-1）：base 三段（clientId/version/host）由调用方
/// 按身份档位下发，盐链沿用 web 链（nas 档未实弹验证，见 tier.rs 假设区标注）。
pub fn captcha_sign_with(
    client_id: &str,
    client_version: &str,
    host: &str,
    device_id_32: &str,
    timestamp_millis: &str,
) -> String {
    let mut s = format!(
        "{}{}{}{}{}",
        client_id, client_version, host, device_id_32, timestamp_millis
    );
    for salt in WEB_SALTS {
        let mut h = Md5::new();
        h.update(s.as_bytes());
        h.update(salt.as_bytes());
        s = to_hex(&h.finalize());
    }
    format!("{}.{}", ALG_VERSION, s)
}

/// 计算 captcha_sign 的完整入参 base（调试/测试用，暴露 base 拼接逻辑）。
pub fn captcha_sign_base(device_id_32: &str, timestamp_millis: &str) -> String {
    format!(
        "{}{}{}{}{}",
        CLIENT_ID, CLIENT_VERSION, HOST, device_id_32, timestamp_millis
    )
}

/// device_sign = "div101." + deviceID + md5_hex(sha1_hex(deviceID+packageName+APPID+APPKey))
/// （App 端 device-sign 流程用，与 captcha_sign 无关，保留。）
pub fn device_sign(device_id: &str) -> String {
    let base = format!("{}{}{}{}", device_id, APP_PACKAGE_NAME, APPID, APPKEY);
    let sha1_hex = {
        let mut h = Sha1::new();
        h.update(base.as_bytes());
        to_hex(&h.finalize())
    };
    let md5_hex = {
        let mut h = Md5::new();
        h.update(sha1_hex.as_bytes());
        to_hex(&h.finalize())
    };
    format!("div101.{}{}", device_id, md5_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_encodes_correctly() {
        assert_eq!(to_hex(&[0x00, 0xff, 0x0a]), "00ff0a");
    }

    #[test]
    fn device_id_32_strips_prefix_and_truncates() {
        let full = "wdi10.adb1a76709f6584a13b58baaf6e1d871d02650159e5762f2299e41b38b017500";
        assert_eq!(device_id_32(full), "adb1a76709f6584a13b58baaf6e1d871");
    }

    #[test]
    fn device_id_32_handles_no_prefix() {
        assert_eq!(device_id_32("adb1a76709f6584a13b58baaf6e1d871"), "adb1a76709f6584a13b58baaf6e1d871");
    }

    #[test]
    fn captcha_sign_matches_verified_sample() {
        // 真实捕获样本（2026-01，已验证服务端接受）：
        //   device_id(32位) = adb1a76709f6584a13b58baaf6e1d871
        //   timestamp       = 1787409379387
        //   期望 captcha_sign = 1.2546227cbfbcf07eeba5df575fac2085
        let sign = captcha_sign("adb1a76709f6584a13b58baaf6e1d871", "1787409379387");
        assert_eq!(sign, "1.2546227cbfbcf07eeba5df575fac2085");
    }

    #[test]
    fn captcha_sign_starts_with_1_dot() {
        let sign = captcha_sign("device123", "1700000000000");
        assert!(sign.starts_with("1."));
    }

    #[test]
    fn captcha_sign_is_32_hex_after_prefix() {
        let sign = captcha_sign("device123", "1700000000000");
        let hex_part = sign.strip_prefix("1.").unwrap();
        assert_eq!(hex_part.len(), 32);
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn captcha_sign_is_deterministic() {
        let a = captcha_sign("dev", "1000");
        let b = captcha_sign("dev", "1000");
        assert_eq!(a, b);
    }

    #[test]
    fn captcha_sign_changes_with_timestamp() {
        let a = captcha_sign("dev", "1000");
        let b = captcha_sign("dev", "1001");
        assert_ne!(a, b);
    }

    #[test]
    fn device_sign_has_div101_prefix() {
        let s = device_sign("device123");
        assert!(s.starts_with("div101.device123"));
    }

    #[test]
    fn device_sign_is_deterministic() {
        assert_eq!(device_sign("dev"), device_sign("dev"));
    }

    #[test]
    fn device_sign_differs_by_device() {
        assert_ne!(device_sign("a"), device_sign("b"));
    }
}

/// 本地随机生成完整 device_id（`wdi10.` + 64 位 hex）。
///
/// 服务端不校验来源（README_captcha_sign §1.5 实测），本地随机即可。
/// device_id_32() 会自动剥前缀取前 32 位供 captcha_sign 使用。
pub fn random_device_id() -> String {
    use md5::{Digest as Md5Digest, Md5};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let mut h = Md5::new();
    h.update(format!("{nanos}-{pid}-{n}-a").as_bytes());
    let a: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let mut h2 = Md5::new();
    h2.update(format!("{nanos}-{pid}-{n}-b").as_bytes());
    let b: String = h2.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("wdi10.{a}{b}")
}
