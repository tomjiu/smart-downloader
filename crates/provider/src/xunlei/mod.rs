//! 迅雷云盘（pan.xunlei.com）Provider 的算法地基：
//! captcha_sign / device_sign（sign.rs）、GCID / CID（hash.rs）。
//! 纯函数，无 I/O，算法移植自 alist（MIT）与 xunlei-lixian（公开）。

pub mod auth;
pub mod client;
pub mod device;
pub mod hash;
pub mod provider;
pub mod sign;

pub use hash::{cid, gcid};
pub use sign::{captcha_sign, device_sign};
