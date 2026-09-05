//! 百度网盘 Provider（B3-a：分享免登录解析链）。
//!
//! 模块结构：
//! - [`types`]：错误分类（WrongPasscode/NeedVerify/…）+ 端点常量（实测 UA/APP_ID）
//! - [`share`]：分享链接解析（`/s/1xxx` 与 `/share/init?surl=` 双形态统一规约）
//! - [`client`]：`BaiduClient`——verify（POST）→ BDCLND → 分享页 meta 提取
//!   → `share/list` 目录清单；协议形状 2026-09-05 真实链接实测
//!   （`docs/research/baidu/share_protocol.md`，A 级证据）
//!
//! 范围边界：dlink 直链转换需要登录态（BDUSS；免登录 `/api/download`
//! 实测 errno -6），属 B3-b（登录态 + 转存/直链链真机校准后接入
//! `RemoteProvider` 契约）。
//!
//! 「112 链接」备注：BACKLOG E 段用户术语「百度网盘（112 链接）」的格式
//! 定义至今未获取（2026-08-30 调研 + 2026-09-05 复核均无公开资料）；
//! 本模块输入面为标准分享链接。若后续拿到 112 链接真实样本，在其上
//! 增补解析规则（`share.rs` 单点扩展）。

pub mod client;
pub mod share;
pub mod types;

pub use client::{BaiduClient, BaiduShareFile, BaiduShareMeta};
pub use share::{parse_share_link, BaiduShareLink};
pub use types::BaiduError;
