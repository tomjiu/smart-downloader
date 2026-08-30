//! 迅雷云盘（pan.xunlei.com）Provider 的算法地基：
//! captcha_sign / device_sign（sign.rs）、GCID / CID（hash.rs）。
//! 纯函数，无 I/O，算法移植自 alist（MIT）与 xunlei-lixian（公开）。

pub mod auth;
pub mod client;
pub mod cloud_search;
pub mod device;
pub mod hash;
pub mod login_flow;
pub mod login_page;
pub mod provider;
pub mod share;
pub mod sign;
pub mod url_class;

pub use client::{device_code_qr_url, DEVICE_CLIENT_ID};
pub use login_flow::{LoginMode, DeviceSession};

pub use hash::{bcid, cid, gcid};
pub use share::{parse_share_link, ResolvedLink, SharedFile, SharedLink, Sharer, ShareError};
pub use sign::{captcha_sign, device_id_32, device_sign};
pub use url_class::{classify_url, cdn_hosts_by_region, LinkClass};
