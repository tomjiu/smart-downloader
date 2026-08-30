//! 迅雷云盘「分享链接解析」库模块。
//!
//! 移植自 `scripts/research/cloud_delivery/share_dl/` 下的一手实测脚本。
//! 本模块含两条链路：
//!
//! 1. **匿名链路**（`list`/`resolve`）：**实测结论是这条链路无法完整跑通**
//!    （`share/detail` 返回 `400 no client info found`，见各端点注释里的「失败结论」），
//!    保留作对照与调试基线。
//! 2. **登录态链路**（`list_with_auth`/`resolve_with_auth`，2026-08-30 新增）：
//!    与 `client.rs` 的 `list_files`/`resolve_link`（实测验证过的同族端点）共用
//!    三要素登录态头（Bearer + x-device-id + x-captcha-token），仅在 pass_code_token
//!    的获取端点上按 B 级证据推断（`POST /drive/v1/share/verify`）。【B级待验】：
//!    端点形状按一手实测脚本还原，网络验证待登录态会话。

use crate::xunlei::auth::AuthState;
use crate::xunlei::client::{Client, CLIENT_ID, CLIENT_VERSION, PAN_BASE, XLUSER_BASE};
use crate::xunlei::sign::{captcha_sign, device_id_32, PACKAGE_NAME};
use md5::{Digest as Md5Digest, Md5};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde::Deserialize;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 解析后的分享链接。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedLink {
    /// 分享 ID（URL 中 `/s/` 后面的那段，如 `VP-cAuy04PiKRmKkFAvDLJgqA1`）。
    pub share_id: String,
    /// 提取码（URL `?pwd=` 参数）。无提取码分享为 `None`。
    pub pass_code: Option<String>,
}

/// 分享内的单个文件/文件夹条目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFile {
    pub id: String,
    pub name: String,
    /// 字节大小（文件夹通常为 0）。
    pub size: u64,
    pub is_folder: bool,
}

/// 取到的直链结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLink {
    /// 可下载直链（web_content_link）。
    pub url: String,
    /// 文件大小（来自响应 size 或 URL 的 `f=` 参数）。
    pub size: u64,
    /// 直链过期 Unix 时间戳（来自 URL 的 `e=` 参数；无限期则为 None）。
    pub expires_at: Option<u64>,
}

/// 分享解析错误。
///
/// 风格对齐 `client.rs` 的 `ClientError`：非 2xx 一律把响应体文本带进错误详情，
/// 便于后续真机调试（参考 `list_files` 的写法）。
#[derive(Debug, thiserror::Error)]
pub enum ShareError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),

    /// captcha/init 非 2xx。
    #[error("captcha/init 失败 ({status}): {body}")]
    CaptchaInit { status: u16, body: String },

    /// 提取码校验（pwd → pass_code_token）非 2xx。
    #[error("提取码校验失败 ({status}): {body}")]
    PassCodeVerify { status: u16, body: String },

    /// 分享详情（share/detail）非 2xx。
    #[error("分享详情失败 ({status}): {body}")]
    ShareDetail { status: u16, body: String },

    /// 分享文件信息（share/file_info）非 2xx。
    #[error("分享文件信息失败 ({status}): {body}")]
    ShareFileInfo { status: u16, body: String },

    /// 分享下载（share/download）非 2xx。
    #[error("分享下载失败 ({status}): {body}")]
    ShareDownload { status: u16, body: String },

    /// 直链取链（files/{id}?usage=PLAY）非 2xx。
    #[error("直链取链失败 ({status}): {body}")]
    ResolvePlay { status: u16, body: String },

    /// 响应里没有 web_content_link。
    #[error("响应未包含 web_content_link：{body}")]
    NoLink { body: String },

    /// 需要 pass_code_token 但未能取得（提取码校验未成功 / 链路需要登录态）。
    #[error("缺少 pass_code_token：提取码 → pass_code_token 的转换在匿名链路下未验证可用（研究结论：需登录态或浏览器上下文）")]
    MissingPassCodeToken,
}

/// 解析分享 URL。
///
/// 支持形态：
/// - `https://pan.xunlei.com/s/{id}`
/// - `https://pan.xunlei.com/s/{id}?pwd={code}`
/// - 带 `http://`、尾部 `/`、`.html` 等变体
///
/// 非 `pan.xunlei.com` 域名、或路径不是 `/s/{id}` 形态 → 返回 `None`。
/// 纯函数，无 I/O。
pub fn parse_share_link(url: &str) -> Option<SharedLink> {
    // 1) 拆 host 与 路径+query（不依赖第三方 URL 库，自己最小解析）。
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let host_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let host = &rest[..host_end];
    let path_query = &rest[host_end..]; // 含前导 '/' 或 '?'

    // 2) 必须是 pan.xunlei.com 域名（含作为后缀，排除其他域名）。
    if !host.ends_with("pan.xunlei.com") {
        return None;
    }

    // 3) 路径必须形如 /s/{id}（或 /s/{id}/...）。
    let path = path_query.split('?').next().unwrap_or(path_query);
    let segments: Vec<&str> = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    // segments[0] == "s" 且 segments[1] 为 share_id。
    let share_id = match segments.as_slice() {
        ["s", id, ..] => id.to_string(),
        _ => return None,
    };
    if share_id.is_empty() {
        return None;
    }

    // 4) 提取 ?pwd= 参数（大小写不敏感）。
    let pass_code = path_query
        .split('?')
        .nth(1)
        .and_then(|q| {
            q.split('&').find_map(|kv| {
                let mut it = kv.splitn(2, '=');
                let k = it.next()?;
                let v = it.next().unwrap_or("");
                if k.eq_ignore_ascii_case("pwd") {
                    Some(v.to_string())
                } else {
                    None
                }
            })
        })
        .filter(|s| !s.is_empty());

    Some(SharedLink { share_id, pass_code })
}

/// 分享解析器。
///
/// 设计取舍：需求允许「Sharer 内部状态」或「显式传入」，这里选**内部状态**——
/// captcha_token 与 pass_code_token 在 `list` 取得后缓存在内部，
/// `resolve` 直接复用，保证 list→resolve 同一会话。
///
/// 为何不直接持有 `client::Client`：其低层 `http` 字段是私有的（pub(crate) 仅限
/// client 模块），且任务要求「不动 client.rs」。本模块自行持有一个 reqwest 客户端，
/// 仅复用 client 模块导出的 `CLIENT_ID` 等常量与 `sign` 模块的导出函数。
pub struct Sharer {
    http: reqwest::Client,
    /// 匿名 captcha_token（captcha/init 取得，300s 有效）。
    captcha: Mutex<Option<String>>,
    /// 提取码校验得到的 pass_code_token（跨 list→resolve 复用）。
    pass_token: Mutex<Option<String>>,
}

impl Sharer {
    pub fn new() -> Self {
        Sharer {
            http: reqwest::Client::new(),
            captcha: Mutex::new(None),
            pass_token: Mutex::new(None),
        }
    }

    /// 构造 share API 的三要素请求头（参考 client.rs::auth_headers，但 x-device-id 用 32 位匿名设备号）。
    fn share_headers(&self, device_id: &str, captcha_token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-device-id", HeaderValue::from_str(device_id).unwrap());
        h.insert(
            "x-captcha-token",
            HeaderValue::from_str(captcha_token).unwrap(),
        );
        // 部分接口同时读大写的 X-Captcha-Token（脚本实测里两种都带过）。
        h.insert(
            "X-Captcha-Token",
            HeaderValue::from_str(captcha_token).unwrap(),
        );
        h.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        h.insert(
            "Referer",
            HeaderValue::from_static("https://pan.xunlei.com/"),
        );
        h.insert(
            "Origin",
            HeaderValue::from_static("https://pan.xunlei.com"),
        );
        h.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        h
    }

    /// 匿名获取/刷新 captcha_token。
    ///
    /// **【A级 · 一手实测】** 来源 `CAPTCHA_APP_RESULT.md` 步骤 1：
    /// `POST https://xluser-ssl.xunlei.com/v1/shield/captcha/init`，
    /// 匿名（不带 captcha_sign）即可 200 返回 `{"captcha_token":"ck0....","expires_in":300}`。
    /// 该结论在 App 端 client_id 下验证；此处复用 Web 端 `CLIENT_ID` + Web 端 `captcha_sign`
    /// （与 client.rs::refresh_captcha 一致），并按需求用本地生成的匿名设备号。
    async fn ensure_captcha(&self) -> Result<String, ShareError> {
        // 已有则直接复用。
        if let Some(t) = self.captcha.lock().unwrap().clone() {
            return Ok(t);
        }
        let full_dev = anonymous_device_id();
        let did32 = device_id_32(&full_dev); // 复用 sign::device_id_32
        let ts = now_millis().to_string();
        let sign = captcha_sign(did32, &ts); // 复用 sign::captcha_sign（Web 端盐链）

        #[derive(Deserialize)]
        struct CaptchaResp {
            captcha_token: String,
            #[serde(default)]
            #[allow(dead_code)]
            expires_in: u64,
        }

        let resp = self
            .http
            .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
            .json(&serde_json::json!({
                "action": "POST:/drive/v1/files",
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": did32,
                "meta": {
                    "timestamp": ts,
                    "captcha_sign": sign,
                    "user_id": "",              // 匿名无 user_id
                    "client_version": CLIENT_VERSION,
                    "package_name": PACKAGE_NAME,
                },
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::CaptchaInit { status: s, body: b });
        }
        let body: CaptchaResp = resp.json().await?;
        // 缓存（忽略 expires，下次 list/resolve 若失败可重试刷新）。
        *self.captcha.lock().unwrap() = Some(body.captcha_token.clone());
        Ok(body.captcha_token)
    }

    /// 提取码校验（登录态版）：pwd → pass_code_token。【B级待验】
    ///
    /// 与匿名版同端点（`POST /drive/v1/share/verify`），差异仅在鉴权头：
    /// 登录态下带 `Client::auth_headers`（Bearer + 会话 device_id + 会话 captcha_token），
    /// 这与桌面 App 登录态打开带提取码分享的行为一致（web 前端在登录态下走同一 drive API）。
    async fn verify_pass_code_authed(
        &self,
        link: &SharedLink,
        state: &AuthState,
    ) -> Result<String, ShareError> {
        #[derive(Deserialize)]
        struct VerifyResp {
            #[serde(default)]
            pass_code_token: String,
            #[serde(default)]
            data: serde_json::Value,
        }

        let resp = self
            .http
            .post(format!("{}/drive/v1/share/verify", PAN_BASE))
            .headers(authed_share_headers(state))
            .json(&serde_json::json!({
                "share_id": link.share_id,
                "pass_code": link.pass_code.clone().unwrap_or_default(),
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::PassCodeVerify { status: s, body: b });
        }
        let body: VerifyResp = resp.json().await?;
        let token = if !body.pass_code_token.is_empty() {
            body.pass_code_token
        } else if let Some(t) = body.data.get("pass_code_token").and_then(|v| v.as_str()) {
            t.to_string()
        } else {
            return Err(ShareError::MissingPassCodeToken);
        };
        Ok(token)
    }

    /// 登录态全流程：会话三要素头 → 分享详情 →（需要时）提码校验 → 文件列表。
    ///
    /// 【B级待验】与 `client.rs::list_files`（实测验证过的同族 drive API）同一套登录态头；
    /// 与匿名版 `list` 的差异：
    /// 1. 鉴权头换成 `authed_share_headers(state)`（Bearer + 会话 device_id + 会话 captcha_token）——
    ///    匿名链路实测死于 `400 no client info found`（api-pan 认登录态不认匿名 xluser token），
    ///    登录态正是该错误的对症解；
    /// 2. device_id 用会话的（`device_id_32(&state.device_id)`），与 captcha/init 口径一致；
    /// 3. 提取码校验走 `verify_pass_code_authed`（带登录态重试同端点）。
    /// 网络验证待登录态会话（同 VIP 通道 UNTESTED 模式）。
    pub async fn list_with_auth(
        &self,
        link: &SharedLink,
        state: &AuthState,
    ) -> Result<Vec<SharedFile>, ShareError> {
        // 若带提取码，先校验拿到 pass_code_token 并缓存（list→resolve 复用）。
        if link.pass_code.is_some() {
            let token = self.verify_pass_code_authed(link, state).await?;
            *self.pass_token.lock().unwrap() = Some(token);
        }
        let pass_token = self.pass_token.lock().unwrap().clone();

        let url = build_share_detail_url(&link.share_id, pass_token.as_deref());
        let resp = self
            .http
            .get(&url)
            .headers(authed_share_headers(state))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::ShareDetail { status: s, body: b });
        }
        let detail: Detail = resp.json().await?;
        Ok(detail
            .files
            .into_iter()
            .map(|f| SharedFile {
                id: f.id,
                name: f.name,
                size: parse_size(&f.size),
                is_folder: f.kind.ends_with("folder"),
            })
            .collect())
    }

    /// 登录态对单个分享文件取直链。【B级待验】同 `list_with_auth`。
    ///
    /// 直链 URL 形状沿用匿名版实测的最可能路径（一手实测 line 325，A 级形状）：
    /// `GET /drive/v1/files/{fid}?space=&usage=PLAY&share_id=..&pass_code_token=..`，
    /// 差异仅在鉴权头换登录态三件套（`files` API 实测需有效 Bearer，见
    /// CAPTCHA_APP_RESULT 步骤 2 的 401——登录态正是 401 的对症解）。
    pub async fn resolve_with_auth(
        &self,
        link: &SharedLink,
        file_id: &str,
        state: &AuthState,
    ) -> Result<ResolvedLink, ShareError> {
        let pass_token = self.pass_token.lock().unwrap().clone();
        let url = build_share_play_url(file_id, &link.share_id, pass_token.as_deref());

        let resp = self
            .http
            .get(&url)
            .headers(authed_share_headers(state))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::ResolvePlay { status: s, body: b });
        }
        let body_text = resp.text().await.unwrap_or_default();
        parse_play_body(body_text)
    }

    /// 提取码校验：pwd → pass_code_token。
    ///
    /// **【B级 · 推断】** 来源 `verify_share_nologin.py` step_4 的候选端点之一
    /// （`POST /drive/v1/share/verify` 带 `{share_id, pass_code}`），响应预期
    /// `{"pass_code_token":"..."}` 或 `{"data":{"pass_code_token":"..."}}`。
    ///
    /// **【失败结论 · 一手实测】** `SHARE_NOLOGIN_RESULT.md` 明确记录：pwd→pass_code_token
    /// 的转换接口**从未在实测中找到**（可能隐藏在未逆向的前端 JS / 浏览器上下文里）。
    /// 因此本方法「按形态调用」，成功与否以真机为准；失败则上层会拿到明确的错误体便于调试。
    async fn verify_pass_code(
        &self,
        link: &SharedLink,
        device_id: &str,
        captcha_token: &str,
    ) -> Result<String, ShareError> {
        #[derive(Deserialize)]
        struct VerifyResp {
            #[serde(default)]
            pass_code_token: String,
            #[serde(default)]
            data: serde_json::Value,
        }

        let resp = self
            .http
            .post(format!("{}/drive/v1/share/verify", PAN_BASE))
            .headers(self.share_headers(device_id, captcha_token))
            .json(&serde_json::json!({
                "share_id": link.share_id,
                "pass_code": link.pass_code.clone().unwrap_or_default(),
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::PassCodeVerify { status: s, body: b });
        }
        let body: VerifyResp = resp.json().await?;
        let token = if !body.pass_code_token.is_empty() {
            body.pass_code_token
        } else if let Some(t) = body.data.get("pass_code_token").and_then(|v| v.as_str()) {
            t.to_string()
        } else {
            return Err(ShareError::MissingPassCodeToken);
        };
        Ok(token)
    }

    /// 全流程：匿名 captcha → 分享详情 →（需要时）提码校验 → 文件列表。
    pub async fn list(&self, link: &SharedLink) -> Result<Vec<SharedFile>, ShareError> {
        let captcha_token = self.ensure_captcha().await?;
        let full_dev = anonymous_device_id();
        let did32 = device_id_32(&full_dev);
        let device_id = did32; // 与 captcha/init 一致

        // 若带提取码，先校验拿到 pass_code_token 并缓存。
        if let Some(_pwd) = &link.pass_code {
            let token = self
                .verify_pass_code(link, device_id, &captcha_token)
                .await?;
            *self.pass_token.lock().unwrap() = Some(token);
        }

        // **【B级 · 推断】** 分享详情端点形态来自 `verify_share_nologin.py` 文件头注释
        // （line 24）：`GET /drive/v1/share/detail?share_id=...&pass_code_token=...&parent_id=...&usage=CONSUME`。
        //
        // **【失败结论 · 一手实测】** `SHARE_NOLOGIN_RESULT.md` 2.1 记录：
        // - 不带任何 token → `400 captcha_token is empty`
        // - 带 xluser-ssl 的 captcha_token → `400 no client info found`（api-pan 不认 xluser 的 token）
        // 即 `share/detail` 在匿名链路下**实测必然失败**。此处仍按形态实现，错误体会透传给调用方；
        // 登录态对症版本见 `list_with_auth`。
        let pass_token = self.pass_token.lock().unwrap().clone();

        let url = build_share_detail_url(&link.share_id, pass_token.as_deref());

        let resp = self
            .http
            .get(&url)
            .headers(self.share_headers(device_id, &captcha_token))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::ShareDetail { status: s, body: b });
        }
        let detail: Detail = resp.json().await?;
        Ok(detail
            .files
            .into_iter()
            .map(|f| SharedFile {
                id: f.id,
                name: f.name,
                size: parse_size(&f.size),
                is_folder: f.kind.ends_with("folder"),
            })
            .collect())
    }

    /// 对单个分享文件取直链（usage=PLAY + share_id + pass_code_token）。
    ///
    /// **【A级 · 一手实测】** 直链 URL 形状来自 `verify_share_nologin.py` line 325
    /// （步骤 8，已实测为最可能拿到 web_content_link 的路径）：
    /// `GET /drive/v1/files/{fid}?space=&usage=PLAY&share_id=..&pass_code_token=..`
    /// 响应含 `web_content_link`（取链字段本身在 `client.rs::PlayResp` 已实测验证）。
    ///
    /// **【失败结论 · 一手实测】** 同上，`files/{id}?usage=PLAY` 在匿名（无 Bearer 登录态）
    /// 下会被 `share/detail` 同一套 captcha 校验挡在门外；且 `files` API 实测需
    /// `xluser-ssl` 的 captcha_token + 有效 Bearer（见 CAPTCHA_APP_RESULT 步骤 2 的 401）。
    /// 故本方法「按形态实现」，匿名调用预期失败，错误体便于真机调试。
    pub async fn resolve(
        &self,
        link: &SharedLink,
        file_id: &str,
    ) -> Result<ResolvedLink, ShareError> {
        let captcha_token = self.ensure_captcha().await?;
        let full_dev = anonymous_device_id();
        let device_id = device_id_32(&full_dev);
        let pass_token = self.pass_token.lock().unwrap().clone();

        let url = build_share_play_url(file_id, &link.share_id, pass_token.as_deref());

        let resp = self
            .http
            .get(&url)
            .headers(self.share_headers(device_id, &captcha_token))
            .send()
            .await?;
        if !resp.status().is_success() {
            let (s, b) = status_body(resp).await;
            return Err(ShareError::ResolvePlay { status: s, body: b });
        }
        let body_text = resp.text().await.unwrap_or_default();
        parse_play_body(body_text)
    }
}

/// 纯函数：分享详情 URL（匿名/登录态共用，B 级形状还原自 verify_share_nologin.py line 24）。
pub(crate) fn build_share_detail_url(share_id: &str, pass_code_token: Option<&str>) -> String {
    let mut url = format!(
        "{}/drive/v1/share/detail?share_id={}&parent_id=&usage=CONSUME&limit=100",
        PAN_BASE,
        urlencode(share_id)
    );
    if let Some(t) = pass_code_token {
        url.push_str(&format!("&pass_code_token={}", urlencode(t)));
    }
    url
}

/// 纯函数：分享取直链 URL（匿名/登录态共用，A 级形状还原自 verify_share_nologin.py line 325）。
pub(crate) fn build_share_play_url(file_id: &str, share_id: &str, pass_code_token: Option<&str>) -> String {
    let mut url = format!(
        "{}/drive/v1/files/{}?space=&usage=PLAY&share_id={}",
        PAN_BASE,
        urlencode(file_id),
        urlencode(share_id)
    );
    if let Some(t) = pass_code_token {
        url.push_str(&format!("&pass_code_token={}", urlencode(t)));
    }
    url
}

/// 登录态 share 请求头：`client.rs::auth_headers`（实测验证过的三要素）+ web 分享页 Referer/Origin。
///
/// 三要素口径与 `list_files`/`resolve_link` 完全一致（Bearer + device_id_32(会话设备号) +
/// 会话 captcha_token），Referer/Origin 沿用匿名版 `share_headers` 的 web 形状。
pub(crate) fn authed_share_headers(state: &AuthState) -> reqwest::header::HeaderMap {
    let mut h = Client::auth_headers(state);
    h.insert(
        reqwest::header::REFERER,
        reqwest::header::HeaderValue::from_static("https://pan.xunlei.com/"),
    );
    h.insert(
        reqwest::header::ORIGIN,
        reqwest::header::HeaderValue::from_static("https://pan.xunlei.com"),
    );
    h
}

/// share/detail 响应的文件条目（匿名/登录态共用）。
#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    /// `drive#folder` / `drive#file`
    #[serde(default, rename = "kind")]
    kind: String,
    /// size 实测为字符串（与 list_files 一致），也兼容数字。
    #[serde(default)]
    size: serde_json::Value,
}

/// share/detail 响应体（匿名/登录态共用）。
#[derive(Deserialize)]
struct Detail {
    #[serde(default)]
    files: Vec<RawFile>,
}

/// 解析 `files/{id}?usage=PLAY` 响应体 → 直链（匿名/登录态共用）。
fn parse_play_body(body_text: String) -> Result<ResolvedLink, ShareError> {
    #[derive(Deserialize)]
    struct Play {
        #[serde(default)]
        web_content_link: String,
        #[serde(default)]
        size: Option<u64>,
    }
    let play: Play = serde_json::from_str(&body_text)
        .map_err(|_| ShareError::NoLink { body: body_text.clone() })?;
    if play.web_content_link.is_empty() {
        return Err(ShareError::NoLink { body: body_text });
    }
    let size = play
        .size
        .or_else(|| url_query_u64(&play.web_content_link, "f"))
        .unwrap_or(0);
    let expires_at = url_query_u64(&play.web_content_link, "e");
    Ok(ResolvedLink {
        url: play.web_content_link,
        size,
        expires_at,
    })
}

impl Default for Sharer {
    fn default() -> Self {
        Self::new()
    }
}

/// 取 (状态码, 响应体文本) —— 非 2xx 错误时把响应体一并带出，便于真机调试。
async fn status_body(resp: reqwest::Response) -> (u16, String) {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    (status, body)
}

/// 解析 size：兼容字符串与数字两种形态（实测为字符串）。
fn parse_size(v: &serde_json::Value) -> u64 {
    match v {
        serde_json::Value::String(s) => s.parse().unwrap_or(0),
        serde_json::Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// 极简 URL 编码（覆盖分享 ID / token 里的特殊字符）。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 从 URL 查询串取 u64 参数（如 f=size, e=expires）。
fn url_query_u64(url: &str, key: &str) -> Option<u64> {
    let query = url.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some(key) {
            if let Some(v) = kv.next() {
                if let Ok(n) = v.parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// 当前毫秒时间戳（captcha_sign 的 payload）。
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// 本地生成匿名 32 位 hex 设备号前缀（`wdi10.` + 64 位 hex）。
///
/// 思路参考 `provider.rs::generate_device_id`：服务端对 device_id 不做来源校验，
/// 任意 32 位 hex 即可。这里用时间戳 + 固定盐做两次 Md5 拼成 64 位 hex，
/// 再交给 `sign::device_id_32` 取前 32 位（保持与登录取链同一套设备号口径）。
fn anonymous_device_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut h1 = Md5::new();
    h1.update(ts.to_le_bytes());
    h1.update(b"xl-share-device-1");
    let mut h2 = Md5::new();
    h2.update(ts.to_be_bytes());
    h2.update(b"xl-share-device-2");
    let mut s = String::with_capacity(64);
    for b in h1.finalize().iter().chain(h2.finalize().iter()) {
        s.push_str(&format!("{:02x}", b));
    }
    format!("wdi10.{}", s)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============ parse_share_link 单元测试 ============

    #[test]
    fn parses_with_pwd() {
        let link = parse_share_link("https://pan.xunlei.com/s/VP-cAuy04PiKRmKkFAvDLJgqA1?pwd=wfaa");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "VP-cAuy04PiKRmKkFAvDLJgqA1".into(),
                pass_code: Some("wfaa".into()),
            })
        );
    }

    #[test]
    fn parses_without_pwd() {
        let link = parse_share_link("https://pan.xunlei.com/s/VP-cAuy04PiKRmKkFAvDLJgqA1");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "VP-cAuy04PiKRmKkFAvDLJgqA1".into(),
                pass_code: None,
            })
        );
    }

    #[test]
    fn parses_http_and_trailing_slash() {
        let link = parse_share_link("http://pan.xunlei.com/s/ABC123/");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "ABC123".into(),
                pass_code: None,
            })
        );
    }

    #[test]
    fn parses_pwd_uppercase_param() {
        let link = parse_share_link("https://pan.xunlei.com/s/xyz?PWD=secret");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "xyz".into(),
                pass_code: Some("secret".into()),
            })
        );
    }

    #[test]
    fn parses_pwd_with_extra_query() {
        let link = parse_share_link("https://pan.xunlei.com/s/ID123?a=1&pwd=code&b=2");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "ID123".into(),
                pass_code: Some("code".into()),
            })
        );
    }

    #[test]
    fn parses_trailing_path_segments() {
        // 分享 URL 可能带额外路径（如分享内子目录深链），share_id 取 /s/ 后第一段即可。
        let link = parse_share_link("https://pan.xunlei.com/s/ID123/folder/sub");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "ID123".into(),
                pass_code: None,
            })
        );
    }

    #[test]
    fn rejects_non_pan_domain() {
        assert_eq!(parse_share_link("https://www.baidu.com/s/abc?pwd=x"), None);
        assert_eq!(parse_share_link("https://xunlei.com/s/abc"), None);
        assert_eq!(parse_share_link("https://pan.xunlei.com.evil.com/s/abc"), None);
    }

    #[test]
    fn rejects_non_s_path() {
        assert_eq!(parse_share_link("https://pan.xunlei.com/t/abc"), None);
        assert_eq!(parse_share_link("https://pan.xunlei.com/"), None);
        assert_eq!(parse_share_link("https://pan.xunlei.com"), None);
    }

    #[test]
    fn rejects_empty_id() {
        assert_eq!(parse_share_link("https://pan.xunlei.com/s/"), None);
        assert_eq!(parse_share_link("https://pan.xunlei.com/s//"), None);
    }

    #[test]
    fn rejects_empty_pwd() {
        // ?pwd= 为空 → pass_code 视为 None
        let link = parse_share_link("https://pan.xunlei.com/s/ID?pwd=");
        assert_eq!(
            link,
            Some(SharedLink {
                share_id: "ID".into(),
                pass_code: None,
            })
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_share_link("not a url"), None);
        assert_eq!(parse_share_link(""), None);
        assert_eq!(parse_share_link("ftp://pan.xunlei.com/s/x"), None);
    }

    // ============ 纯函数工具测试（不触网） ============

    #[test]
    fn urlencode_escapes_special() {
        assert_eq!(urlencode("abc-_.~"), "abc-_.~");
        assert!(urlencode(" ").contains('%'));
    }

    #[test]
    fn url_query_u64_parses() {
        let u = "https://vod.xunlei.com/dl?fid=x&f=27471387&e=1787156621";
        assert_eq!(url_query_u64(u, "f"), Some(27471387));
        assert_eq!(url_query_u64(u, "e"), Some(1787156621));
        assert_eq!(url_query_u64(u, "nope"), None);
    }

    #[test]
    fn parse_size_both_shapes() {
        assert_eq!(parse_size(&serde_json::json!("12345")), 12345);
        assert_eq!(parse_size(&serde_json::json!(678)), 678);
        assert_eq!(parse_size(&serde_json::json!("notnum")), 0);
        assert_eq!(parse_size(&serde_json::json!(null)), 0);
    }

    // ============ 登录态链路（list_with_auth / resolve_with_auth）纯函数测试 ============
    // 网络方法 UNTESTED（待登录态会话），此处先锁定 URL 与请求头的还原形状。

    fn test_auth_state() -> AuthState {
        AuthState {
            access_token: "at.x".into(),
            refresh_token: "rt.x".into(),
            device_id: "wdi10.0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
            captcha_token: "ck0.t".into(),
            user_id: "860599297".into(),
            access_token_expires_at: u64::MAX,
            captcha_token_expires_at: u64::MAX,
        }
    }

    #[test]
    fn share_detail_url_shape_with_and_without_pass_token() {
        let u0 = build_share_detail_url("VP-cAuy04PiKRmKkFAvDLJgqA1", None);
        assert!(u0.starts_with(
            "https://api-pan.xunlei.com/drive/v1/share/detail?share_id=VP-cAuy04PiKRmKkFAvDLJgqA1&parent_id=&usage=CONSUME&limit=100"
        ));
        assert!(!u0.contains("pass_code_token"));

        let u1 = build_share_detail_url("VP-cAuy04PiKRmKkFAvDLJgqA1", Some("pct.abc"));
        assert!(u1.contains("&pass_code_token=pct.abc"));
    }

    #[test]
    fn share_play_url_shape_matches_field_evidence() {
        // A 级形状（verify_share_nologin.py line 325）：
        // /drive/v1/files/{fid}?space=&usage=PLAY&share_id=..&pass_code_token=..
        let u = build_share_play_url("file-1", "share-1", Some("tok"));
        assert!(u.starts_with(
            "https://api-pan.xunlei.com/drive/v1/files/file-1?space=&usage=PLAY&share_id=share-1"
        ));
        assert!(u.ends_with("&pass_code_token=tok"));
        // 无提码 token 时不得出现空参
        let u0 = build_share_play_url("file-1", "share-1", None);
        assert!(!u0.contains("pass_code_token"));
    }

    #[test]
    fn authed_share_headers_match_client_three_elements_plus_web() {
        let state = test_auth_state();
        let h = authed_share_headers(&state);
        assert_eq!(
            h.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer at.x")
        );
        let expected_device: String = device_id_32(&state.device_id).to_string();
        assert_eq!(
            h.get("x-device-id").and_then(|v| v.to_str().ok()),
            Some(expected_device.as_str())
        );
        assert_eq!(
            h.get("x-captcha-token").and_then(|v| v.to_str().ok()),
            Some("ck0.t")
        );
        assert_eq!(
            h.get("referer").and_then(|v| v.to_str().ok()),
            Some("https://pan.xunlei.com/")
        );
        assert_eq!(
            h.get("origin").and_then(|v| v.to_str().ok()),
            Some("https://pan.xunlei.com")
        );
    }

    #[test]
    fn parse_play_body_resolves_size_and_expiry() {
        let body = r#"{"web_content_link":"https://dl.xunlei.com/f?e=1700000000&f=12345"}"#.to_string();
        let r = parse_play_body(body).expect("应解析出直链");
        assert_eq!(r.size, 12345);
        assert_eq!(r.expires_at, Some(1700000000));
        assert!(r.url.contains("dl.xunlei.com"));
    }

    #[test]
    fn parse_play_body_missing_link_is_no_link_error() {
        let r = parse_play_body(r#"{"error":"nothing"}"#.to_string());
        assert!(matches!(r, Err(ShareError::NoLink { .. })));
        let r2 = parse_play_body("not json".to_string());
        assert!(matches!(r2, Err(ShareError::NoLink { .. })));
    }
}
