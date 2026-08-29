//! 迅雷 HTTP 客户端：三要素头 + OAuth refresh + captcha 刷新。

use crate::xunlei::auth::AuthState;
use crate::xunlei::sign::{captcha_sign, device_id_32, PACKAGE_NAME};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use serde::Deserialize;
use sha1::Digest as _;

/// pan 网盘场景的 client_id（取链/captcha/refresh 用）。
pub const CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";
/// 网页登录（设备码流程）的 app_id（扫码登录用）。
pub const DEVICE_CLIENT_ID: &str = "XW5SkOhLDjnOZP7J";
pub const XLUSER_BASE: &str = "https://xluser-ssl.xunlei.com";
pub const PAN_BASE: &str = "https://api-pan.xunlei.com";
/// Web 端 client_version（captcha/init meta 用）。
pub const CLIENT_VERSION: &str = "1.92.91";

pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

/// 当前毫秒时间戳（captcha_sign 的 payload）。
pub(crate) fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

/// 手机号归一化：不以 `+` 开头则补 `+86`（中国大陆默认区号）。
///
/// 与 web 登录页逻辑一致：`capture_raw.json` 的 `handleSignIn` 中
/// `Ri(o)?(n="+86 ".concat(o)):n=o`，即本地号自动加 `+86`。
/// A级：行为来自一手 dump（非推断）。
pub(crate) fn normalize_phone(phone: &str) -> String {
    if phone.starts_with('+') {
        phone.to_string()
    } else {
        format!("+86{}", phone)
    }
}

/// 极简 bencode 解析器：取出 `info` 字典的字节切片，返回其 SHA-1（即 torrent 的 info-hash）。
///
/// 规范性：info-hash 的算法是对原始 .torrent 里的 `info` 字典字节（未经重编码）做 SHA-1。
/// 本实现直接定位 bencode 中 `4:info` 的起始与对应闭合括号，避免对整文件重编码带来的歧义。
/// dump 考古中迅雷 web 端依赖 `@xunlei/bencode-worker` + `@xunlei/gcid-worker`
///（package.json dump 见 `m_180.js` / `mod180_source.js`）来做 bencode/gcid 计算，
/// 此处以通用纯函数实现等价逻辑，便于单测，不属于任何新依赖。
///
/// 返回 40 位小写十六进制 info-hash；解析失败返回 Err。
pub fn bencode_info_hash(torrent: &[u8]) -> Result<String, String> {
    let info = find_info_dict(torrent).ok_or_else(|| "torrent 中未找到 info 字典".to_string())?;
    let digest = sha1::Sha1::digest(info);
    Ok(hex_encode(&digest))
}

/// 在 bencode 字节流中定位 `info` 字典字节区间 [start,end)（含 `d` 不含对应 `e`）。
fn find_info_dict(torrent: &[u8]) -> Option<&[u8]> {
    // 找 "4:info" 标记
    let marker = b"4:info";
    let mut i = 0;
    while i + marker.len() <= torrent.len() {
        if &torrent[i..i + marker.len()] == marker {
            let mut p = i + marker.len();
            // 跳过前导空白
            while p < torrent.len() && (torrent[p] == b' ' || torrent[p] == b'\n' || torrent[p] == b'\r' || torrent[p] == b'\t') {
                p += 1;
            }
            if p < torrent.len() && torrent[p] == b'd' {
                if let Some(end) = matching_end(torrent, p) {
                    return Some(&torrent[p..=end]);
                }
            }
        }
        i += 1;
    }
    None
}

/// 从 `b'd'` 起始位置找到配对的 `e`，返回其下标（含）。
fn matching_end(buf: &[u8], start: usize) -> Option<usize> {
    if buf.get(start)? != &b'd' { return None; }
    let mut depth = 0i32;
    let mut i = start;
    while i < buf.len() {
        match buf[i] {
            b'd' | b'l' => depth += 1,
            b'e' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            // 字符串以 "<len>:" 表达，跳过 "<len>:" 后整段
            c if c.is_ascii_digit() => {
                let mut j = i;
                while j < buf.len() && buf[j].is_ascii_digit() { j += 1; }
                if j < buf.len() && buf[j] == b':' {
                    let mut len: usize = 0;
                    let mut ok = true;
                    for &digit in &buf[i..j] {
                        len = match len.checked_mul(10).and_then(|v| v.checked_add((digit - b'0') as usize)) {
                            Some(v) => v,
                            None => { ok = false; break; }
                        };
                    }
                    if !ok || j + 1 + len > buf.len() {
                        return None;
                    }
                    i = j + 1 + len;
                    continue;
                } else {
                    return None;
                }
            }
            // 整数 'i...e'：跳到配对的 'e' 后继续（不计入字典/列表深度）
            b'i' => {
                let mut j = i + 1;
                while j < buf.len() && buf[j] != b'e' { j += 1; }
                if j < buf.len() && buf[j] == b'e' {
                    i = j + 1;
                    continue;
                } else {
                    return None;
                }
            }
            // 其余字节视为非法 bencode
            _ => return None,
        }
        i += 1;
    }
    None
}

/// 字节转小写十六进制（无依赖，便于单测）。
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        s.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
    }
    s
}

/// RFC 3986 百分号编码（仅编码非 `unreserved` 字符），用于磁力链接的 `dn` 参数。
/// 无依赖纯函数，便于单测。
pub fn url_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap());
            }
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("auth missing")]
    NoAuth,
    #[error("device flow: {0}")]
    DeviceFlow(String),
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
}

impl Default for Client {
    fn default() -> Self { Self::new() }
}

impl Client {
    pub fn new() -> Self {
        Client { http: reqwest::Client::new() }
    }

    /// 构造 drive API 的三要素请求头。
    /// 注意：x-device-id 用 32 位 device_id（与 captcha/init 的 device_id 一致）。
    pub(crate) fn auth_headers(state: &AuthState) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", state.access_token)).unwrap());
        h.insert("x-device-id", HeaderValue::from_str(device_id_32(&state.device_id)).unwrap());
        h.insert("x-captcha-token", HeaderValue::from_str(&state.captcha_token).unwrap());
        h.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h
    }

    /// refresh_token 换新 access_token（已验证可行）。
    pub async fn refresh(&self, state: &mut AuthState) -> Result<(), ClientError> {
        let resp: TokenResp = self.http
            .post(format!("{}/v1/auth/token", XLUSER_BASE))
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": state.refresh_token,
                "client_id": CLIENT_ID,
            }))
            .send().await?.error_for_status()?.json().await?;
        state.access_token = resp.access_token;
        state.refresh_token = resp.refresh_token;
        state.access_token_expires_at = now_unix() + resp.expires_in;
        Ok(())
    }

    /// 账密登录：`/v1/auth/signin`（web 端 SDK signIn，已逆向验证端点/请求体）。
    ///
    /// 流程：先 captcha/init（action=`POST:/v1/auth/signin`，meta 用 `phone_number`/`email`/`username`）
    /// 拿 captcha_token，再带 `X-Captcha-Token` 头 POST signin。
    /// 成功后返回完整登录态（access_token/refresh_token/device_id/captcha_token/user_id）。
    ///
    /// `username` 规则（与 SDK 一致）：
    /// - 以 `+` 开头 → 手机号（meta 用 `phone_number`）
    /// - 含 `@` → 邮箱（meta 用 `email`）
    /// - 其他 → 用户名（meta 用 `username`）
    ///
    /// 密码为明文（HTTPS 加密，无客户端 md5/sha，逆向确认）。
    pub async fn signin(
        &self,
        username: &str,
        password: &str,
        device_id: &str,
    ) -> Result<AuthState, ClientError> {
        // 1) 登录前 captcha/init：全量 meta（账号标识 + timestamp/captcha_sign 套件）。
        //    实测（2026-08-25）：极简 meta 会被风控打回 result:review → signin 报
        //    captcha_invalid(4002)；带签名套件后风控才可能直接 pass。
        let did32 = device_id_32(device_id);
        let timestamp = now_millis().to_string();
        let sign = captcha_sign(did32, &timestamp);
        let mut meta = serde_json::json!({
            "client_version": CLIENT_VERSION,
            "package_name": PACKAGE_NAME,
            "user_id": "",
            "timestamp": timestamp,
            "captcha_sign": sign,
        });
        let action = "POST:/v1/auth/signin";
        if username.starts_with('+') {
            meta["phone_number"] = serde_json::Value::String(username.to_string());
        } else if username.contains('@') {
            meta["email"] = serde_json::Value::String(username.to_string());
        } else {
            meta["username"] = serde_json::Value::String(username.to_string());
        }

        #[derive(Deserialize)]
        struct CaptchaResp {
            #[serde(default)] captcha_token: String,
        }
        let cap_resp = self.http
            .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
            .json(&serde_json::json!({
                "action": action,
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": did32,
                "meta": meta,
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
            }))
            .send().await?;
        let cap_status = cap_resp.status();
        let cap_body = cap_resp.text().await.unwrap_or_default();
        if !cap_status.is_success() {
            return Err(ClientError::DeviceFlow(format!(
                "signin captcha/init -> {cap_status}: {cap_body}"
            )));
        }
        let captcha_resp: CaptchaResp = serde_json::from_str(&cap_body)
            .map_err(|e| ClientError::DeviceFlow(format!("captcha 解析失败: {e}: {cap_body}")))?;
        if captcha_resp.captcha_token.is_empty() {
            return Err(ClientError::DeviceFlow(format!(
                "signin captcha/init 未返回 token: {cap_body}"
            )));
        }
        let captcha_token = captcha_resp.captcha_token;

        // 2) 账密登录。
        #[derive(Deserialize)]
        struct SigninResp {
            access_token: String,
            refresh_token: String,
            #[serde(default)] expires_in: u64,
            #[serde(default)] user_id: String,
            #[serde(default)] sub: String,
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-captcha-token", HeaderValue::from_str(&captcha_token).unwrap());
        headers.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        headers.insert("x-device-id", HeaderValue::from_str(did32).unwrap());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let resp = self.http
            .post(format!("{}/v1/auth/signin", XLUSER_BASE))
            .headers(headers)
            .json(&serde_json::json!({
                "username": username,
                "password": password,
                "client_id": CLIENT_ID,
            }))
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "signin -> {status}: {body}"
            )));
        }
        let resp: SigninResp = resp.json().await?;

        let mut user_id = resp.user_id;
        if user_id.is_empty() {
            user_id = resp.sub;
        }
        let now = now_unix();
        Ok(AuthState {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            device_id: device_id.to_string(),
            captcha_token,
            user_id,
            access_token_expires_at: now + resp.expires_in,
            captcha_token_expires_at: now + 300,
        })
    }

    /// 匿名获取/刷新 captcha_token。
    ///
    /// 需要真实 meta（timestamp + captcha_sign + user_id + client_version + package_name），
    /// 否则服务端返回 `invalid captcha_sign` / `no client info found`。
    pub async fn refresh_captcha(&self, state: &mut AuthState) -> Result<(), ClientError> {
        #[derive(Deserialize)]
        struct CaptchaResp { captcha_token: String, expires_in: u64 }

        let timestamp = now_millis().to_string();
        // captcha_sign 用 32 位 device_id（wdi10. 前缀去掉 + 取前 32 位）。
        let did32 = device_id_32(&state.device_id);
        let sign = captcha_sign(did32, &timestamp);

        let resp: CaptchaResp = self.http
            .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
            .json(&serde_json::json!({
                "action": "POST:/drive/v1/files",
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": did32,
                "meta": {
                    "timestamp": timestamp,
                    "captcha_sign": sign,
                    "user_id": state.user_id,
                    "client_version": CLIENT_VERSION,
                    "package_name": PACKAGE_NAME,
                },
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
            }))
            .send().await?.error_for_status()?.json().await?;
        state.captcha_token = resp.captcha_token;
        state.captcha_token_expires_at = now_unix() + resp.expires_in;
        Ok(())
    }

    /// 请求设备码（RFC 8628 设备码流程第一步，已实测端点）。
    pub async fn request_device_code(&self, scope: &str) -> Result<DeviceCode, ClientError> {
        #[derive(Deserialize)]
        struct Resp {
            device_code: String,
            user_code: String,
            #[serde(default)] verification_uri_complete: String,
            #[serde(default)] verification_url: String,
            expires_in: u64,
            #[serde(default)] interval: u64,
        }
        let resp: Resp = self.http
            .post(format!("{}/v1/auth/device/code", XLUSER_BASE))
            .form(&[("scope", scope), ("client_id", DEVICE_CLIENT_ID)])
            .send().await?.error_for_status()?.json().await?;
        Ok(DeviceCode {
            device_code: resp.device_code,
            user_code: resp.user_code,
            verification_uri: if resp.verification_uri_complete.is_empty() { resp.verification_url } else { resp.verification_uri_complete },
            expires_in: resp.expires_in,
            interval: resp.interval,
        })
    }

    /// 轮询设备码是否已被扫码授权（RFC 8628 第二步，已实测端点）。
    /// 返回 `Ok(Some(TokenResp))` = 授权成功；`Ok(None)` = 未授权（authorization_pending/slow_down）。
    pub async fn poll_device_token(&self, device_code: &str) -> Result<Option<TokenResp>, ClientError> {
        #[derive(Deserialize)]
        struct ErrResp { error: String }
        let resp = self.http
            .post(format!("{}/v1/auth/token", XLUSER_BASE))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device_code),
                ("client_id", DEVICE_CLIENT_ID),
            ])
            .send().await?;
        let status = resp.status();
        if status.is_success() {
            let token: TokenResp = resp.json().await?;
            return Ok(Some(token));
        }
        // 非 2xx：解析 error 字段，authorization_pending/slow_down = 未授权（继续等）
        let body: Result<ErrResp, _> = resp.json().await;
        match body {
            Ok(e) if e.error == "authorization_pending" || e.error == "slow_down" => Ok(None),
            Ok(e) => Err(ClientError::DeviceFlow(e.error)),
            Err(_) => Err(ClientError::DeviceFlow(format!("poll failed with status {}", status))),
        }
    }

    /// 取直链：调 PLAY API 拿 web_content_link（F2/F3 已验证端点）。
    /// 返回 (name, web_content_link)。size/expires 由调用方从 URL 参数解析（f=/e=）。
    pub async fn resolve_link(&self, state: &AuthState, file_id: &str) -> Result<PlayResp, ClientError> {
        let url = format!("{}/drive/v1/files/{}?space=&usage=PLAY", PAN_BASE, file_id);
        let resp: PlayResp = self.http
            .get(url)
            .headers(Self::auth_headers(state))
            .send().await?
            .error_for_status()?
            .json().await?;
        Ok(resp)
    }

    /// 列云盘目录（`GET /drive/v1/files`，已实测 200）。
    ///
    /// `parent_id` 空 = 根目录；返回条目含 id/name/kind(folder|file)/size。
    pub async fn list_files(&self, state: &AuthState, parent_id: &str) -> Result<FilesResp, ClientError> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)] files: Vec<RawFile>,
            #[serde(default)] next_page_token: String,
        }
        #[derive(Deserialize)]
        struct RawFile {
            #[serde(default)] id: String,
            #[serde(default)] name: String,
            /// `drive#folder` / `drive#file`
            #[serde(default, rename = "kind")] kind: String,
            #[serde(default)] size: String,
            #[serde(default, rename = "mime_type")] mime_type: String,
        }
        let url = format!("{PAN_BASE}/drive/v1/files?parent_id={parent_id}&usage=DISPLAY&with_audit=true&limit=100");
        let resp = self.http
            .get(&url)
            .headers(Self::auth_headers(state))
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "list_files -> {status}: {body}"
            )));
        }
        let resp: Raw = resp.json().await?;
        Ok(FilesResp {
            next_page_token: resp.next_page_token,
            files: resp.files.into_iter().map(|f| DriveEntry {
                id: f.id,
                name: f.name,
                is_folder: f.kind.ends_with("folder"),
                size: f.size.parse().unwrap_or(0),
                mime_type: f.mime_type,
            }).collect(),
        })
    }

    /// 短信验证码登录 —— 第一步：发送验证码（`POST /v1/auth/verification`）。
    ///
    /// 请求体字段（A级：一手 dump `capture_raw.json` 登录页 `getVerification` 调用）：
    /// - `phone_number`：归一化后的手机号（`+` 开头）
    /// - `client_id`：沿用 web 端 `CLIENT_ID`
    /// - `target`：`"ANY"`（不限定已注册/未注册用户，A级）
    /// - `usage`：`"SIGN_IN"`（短信登录场景，A级）
    ///
    /// 发送走 web 端 SDK 的 `getVerification`，其内部会先 `captcha/init`
    /// （`VERIFICATION_URL` 对应的 `withCaptcha:true`，meta 用 `phone_number`）再 POST，
    /// 并可能触发滑块（不做滑块处理，直接透传服务端错误）。
    ///
    /// 成功时响应含 `verification_id`（B级：字段名来自 `capture_raw.json`
    /// 的 `o.verification_id` 响应取值，用于第二步 `verify`），本方法透传给调用方。
    pub async fn send_sms_code(&self, phone: &str, device_id: &str) -> Result<String, ClientError> {
        let phone = normalize_phone(phone);
        let did32 = device_id_32(device_id);
        #[derive(Deserialize)]
        struct SendResp {
            /// 下发成功后服务端返回的验证码会话 id（B级：字段名来自 `capture_raw.json` 的
            /// `o.verification_id`）。第二步 verify 需要它做会话关联。
            #[serde(default)]
            verification_id: String,
        }

        // 0) 本 action 的 captcha/init：实测必须携带（400 captcha_required 否则）。
        //    全量签名套件与 signin/drive 同构；风控 verdict 由服务端定。
        let timestamp = now_millis().to_string();
        let sign = captcha_sign(did32, &timestamp);
        #[derive(Deserialize)]
        struct CapResp {
            #[serde(default)] captcha_token: String,
        }
        let cap_resp = self.http
            .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
            .json(&serde_json::json!({
                "action": "POST:/v1/auth/verification",
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": did32,
                "meta": {
                    "phone_number": phone,
                    "client_version": CLIENT_VERSION,
                    "package_name": PACKAGE_NAME,
                    "user_id": "",
                    "timestamp": timestamp,
                    "captcha_sign": sign,
                },
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
            }))
            .send().await?;
        let cap_status = cap_resp.status();
        let cap_body = cap_resp.text().await.unwrap_or_default();
        if !cap_status.is_success() {
            return Err(ClientError::DeviceFlow(format!(
                "send_sms_code captcha/init -> {cap_status}: {cap_body}"
            )));
        }
        let cap: CapResp = serde_json::from_str(&cap_body)
            .map_err(|e| ClientError::DeviceFlow(format!("captcha 解析失败: {e}: {cap_body}")))?;
        if cap.captcha_token.is_empty() {
            return Err(ClientError::DeviceFlow(format!(
                "send_sms_code captcha/init 未返回 token: {cap_body}"
            )));
        }

        let mut headers = HeaderMap::new();
        headers.insert("x-captcha-token", HeaderValue::from_str(&cap.captcha_token).unwrap());
        headers.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        headers.insert("x-device-id", HeaderValue::from_str(did32).unwrap());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let resp = self.http
            .post(format!("{}/v1/auth/verification", XLUSER_BASE))
            .headers(headers)
            .json(&serde_json::json!({
                "phone_number": phone,
                "client_id": CLIENT_ID,
                "target": "ANY",
                "usage": "SIGN_IN",
                "captcha_token": cap.captcha_token,
            }))
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "send_sms_code -> {status}: {body}"
            )));
        }
        let send: SendResp = resp.json().await?;
        if send.verification_id.is_empty() {
            return Err(ClientError::DeviceFlow(
                "send_sms_code: 响应未含 verification_id".into(),
            ));
        }
        Ok(send.verification_id)
    }

    /// 短信验证码登录 —— 第二步：校验验证码换登录态（`POST /v1/auth/verification/verify`）。
    ///
    /// 请求体（A级：`capture_raw.json` 的 `verify({verification_id, verification_code})`；
    /// 另带 `phone_number`/`client_id` 冗余兼容）。
    /// `verification_id` 来自第一步 `send_sms_code` 的返回——**必须传**，
    /// 这是服务端的会话关联键（此前用手机号关联的 B 级推断已被否定：会话必须绑定 id）。
    ///
    /// 响应处理双分支兼容：
    /// - A级（SDK `signInOrSignUpByVerification`）：返回 `verification_token`
    ///   → 再 `POST /v1/auth/signin {username:手机号, verification_code, verification_token}` 换 token
    /// - 直接返回 `access_token`/`refresh_token` → 直接用
    pub async fn verify_sms_code(
        &self,
        phone: &str,
        code: &str,
        verification_id: &str,
        device_id: &str,
    ) -> Result<AuthState, ClientError> {
        let phone = normalize_phone(phone);
        let did32 = device_id_32(device_id);
        if verification_id.is_empty() {
            return Err(ClientError::DeviceFlow(
                "verify_sms_code: verification_id 为空（需先 send_sms_code 取得）".into(),
            ));
        }

        #[derive(Deserialize)]
        struct VerifyResp {
            #[serde(default)] verification_token: String,
            #[serde(default)] access_token: String,
            #[serde(default)] refresh_token: String,
            #[serde(default)] expires_in: u64,
            #[serde(default)] user_id: String,
            #[serde(default)] sub: String,
        }

        // 1) 校验验证码。
        let verify = self.http
            .post(format!("{}/v1/auth/verification/verify", XLUSER_BASE))
            .json(&serde_json::json!({
                "client_id": CLIENT_ID,
                "phone_number": phone,
                "verification_id": verification_id,
                "verification_code": code,
            }))
            .send().await?;
        let status = verify.status();
        if !status.is_success() {
            let body = verify.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "verify_sms_code -> {status}: {body}"
            )));
        }
        let verify: VerifyResp = verify.json().await?;

        // 2) 若 /verify 直接返回 token（任务描述口径），直接组装登录态。
        if !verify.access_token.is_empty() {
            let mut user_id = verify.user_id;
            if user_id.is_empty() {
                user_id = verify.sub;
            }
            let now = now_unix();
            return Ok(AuthState {
                access_token: verify.access_token,
                refresh_token: verify.refresh_token,
                device_id: String::new(),
                captcha_token: String::new(),
                user_id,
                access_token_expires_at: now + verify.expires_in,
                captcha_token_expires_at: 0,
            });
        }

        // 3) 否则走 SDK 口径：用 verification_token 调 signin 换 token（A级）。
        let token = verify.verification_token;
        if token.is_empty() {
            return Err(ClientError::DeviceFlow(
                "verify_sms_code: /verify 响应既无 access_token 也无 verification_token".into(),
            ));
        }

        #[derive(Deserialize)]
        struct SigninTokenResp {
            access_token: String,
            refresh_token: String,
            #[serde(default)] expires_in: u64,
            #[serde(default)] user_id: String,
            #[serde(default)] sub: String,
        }

        // 3a) signin 动作的 captcha/init（实测 4001 captcha_required 否则）。
        //     SMS 已验身会话，风控大概率直接 pass（区别于密码路径的 review）。
        let ts2 = now_millis().to_string();
        let sign2 = captcha_sign(did32, &ts2);
        #[derive(Deserialize)]
        struct CapResp2 {
            #[serde(default)] captcha_token: String,
        }
        let cap2_resp = self.http
            .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
            .json(&serde_json::json!({
                "action": "POST:/v1/auth/signin",
                "captcha_token": "",
                "client_id": CLIENT_ID,
                "device_id": did32,
                "meta": {
                    "phone_number": phone,
                    "client_version": CLIENT_VERSION,
                    "package_name": PACKAGE_NAME,
                    "user_id": "",
                    "timestamp": ts2,
                    "captcha_sign": sign2,
                },
                "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
            }))
            .send().await?;
        let c2_status = cap2_resp.status();
        let c2_body = cap2_resp.text().await.unwrap_or_default();
        if !c2_status.is_success() {
            return Err(ClientError::DeviceFlow(format!(
                "verify_sms_code signin-captcha -> {c2_status}: {c2_body}"
            )));
        }
        let cap2: CapResp2 = serde_json::from_str(&c2_body)
            .map_err(|e| ClientError::DeviceFlow(format!("captcha 解析失败: {e}: {c2_body}")))?;
        if cap2.captcha_token.is_empty() {
            return Err(ClientError::DeviceFlow(format!(
                "verify_sms_code signin-captcha 未返回 token: {c2_body}"
            )));
        }

        let mut headers = HeaderMap::new();
        headers.insert("x-captcha-token", HeaderValue::from_str(&cap2.captcha_token).unwrap());
        headers.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        headers.insert("x-device-id", HeaderValue::from_str(did32).unwrap());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let resp = self.http
            .post(format!("{}/v1/auth/signin", XLUSER_BASE))
            .headers(headers)
            .json(&serde_json::json!({
                "username": phone,
                "verification_code": code,
                "verification_token": token,
                "client_id": CLIENT_ID,
            }))
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "verify_sms_code(signin) -> {status}: {body}"
            )));
        }
        let resp: SigninTokenResp = resp.json().await?;

        let mut user_id = resp.user_id;
        if user_id.is_empty() {
            user_id = resp.sub;
        }
        let now = now_unix();
        Ok(AuthState {
            access_token: resp.access_token,
            refresh_token: resp.refresh_token,
            device_id: String::new(),
            captcha_token: String::new(),
            user_id,
            access_token_expires_at: now + resp.expires_in,
            captcha_token_expires_at: 0,
        })
    }
}

/// 目录列表项。
#[derive(Clone, Debug)]
pub struct DriveEntry {
    pub id: String,
    pub name: String,
    pub is_folder: bool,
    pub size: u64,
    pub mime_type: String,
}

/// 列目录响应。
#[derive(Clone, Debug)]
pub struct FilesResp {
    pub files: Vec<DriveEntry>,
    pub next_page_token: String,
}

/// 离线提交响应（POST /drive/v1/files, upload_type=UPLOAD_TYPE_URL）。
#[derive(Clone, Debug, Default)]
pub struct SubmitResp {
    /// 云端离线任务 id（轮询 /drive/v1/tasks 用；可能为空）。
    pub task_id: String,
    /// 云盘文件 id（resolve 取直链用）。
    pub file_id: String,
}

/// 云端离线任务条目（GET /drive/v1/tasks?type=offline）。
#[derive(Clone, Debug, Default)]
pub struct OfflineTask {
    pub id: String,
    pub name: String,
    /// 形如 `PHASE_TYPE_RUNNING` / `PHASE_TYPE_COMPLETE` / `PHASE_TYPE_FAILED`。
    pub phase: String,
    pub file_id: String,
}

impl Client {
    /// 离线提交磁力/HTTP 直链到迅雷云端（端点格式已由 verify_offline_submit.py 实测）。
    pub async fn offline_submit(
        &self,
        state: &AuthState,
        url: &str,
        name: &str,
    ) -> Result<SubmitResp, ClientError> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)] task: serde_json::Value,
            #[serde(default)] file: serde_json::Value,
        }
        let body = serde_json::json!({
            "kind": "drive#file",
            "name": name,
            "parent_id": "",
            "upload_type": "UPLOAD_TYPE_URL",
            "url": { "url": url },
        });
        let resp = self.http
            .post(format!("{PAN_BASE}/drive/v1/files"))
            .headers(Self::auth_headers(state))
            .json(&body)
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "offline_submit -> {status}: {body}"
            )));
        }
        let raw: Raw = resp.json().await?;
        let s = |v: &serde_json::Value, k: &str| {
            v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
        };
        Ok(SubmitResp {
            task_id: s(&raw.task, "id"),
            file_id: if raw.file.is_object() { s(&raw.file, "id") } else { raw.file.as_str().unwrap_or("").to_string() },
        })
    }

    /// 拉取云端离线任务列表（轮询 submit 进度用）。
    pub async fn offline_tasks(&self, state: &AuthState) -> Result<Vec<OfflineTask>, ClientError> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)] tasks: Vec<RawTask>,
        }
        #[derive(Deserialize)]
        struct RawTask {
            #[serde(default)] id: String,
            #[serde(default)] name: String,
            #[serde(default)] phase: String,
            #[serde(default)] file_id: String,
        }
        let resp = self.http
            .get(format!("{PAN_BASE}/drive/v1/tasks?type=offline&limit=50"))
            .headers(Self::auth_headers(state))
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "offline_tasks -> {status}: {body}"
            )));
        }
        let raw: Raw = resp.json().await?;
        Ok(raw.tasks.into_iter().map(|t| OfflineTask {
            id: t.id,
            name: t.name,
            phase: t.phase,
            file_id: t.file_id,
        }).collect())
    }

    /// 把 .torrent 字节流上传到迅雷云端离线（torrent 字节直传通道）。
    ///
    /// dump 考古证据（A级：`node_modules_dump/m_60.js`、`js_utils-initial.*.js.js`）：
    /// 离线提交的统一入口是 `POST /drive/v1/files`，请求体用 `upload_type` 区分通道。
    /// 已实现的 `UPLOAD_TYPE_URL`（磁力/HTTP 直链）经 `verify_offline_submit.py` 实测通过；
    /// 同一端点、同一 `upload_type` 字段在 dump 中还出现 `UPLOAD_TYPE_FORM` / `UPLOAD_TYPE_RESUMABLE`
    /// （`m_89.js`、`m_12.js`、`js_utils-initial.*.js.js` 的枚举定义），说明 `drive/v1/files`
    /// 是多通道入口，torrent 字节也是经此入口提交。
    ///
    /// 证据强度：端点 `POST /drive/v1/files` 与 `upload_type` 字段形状均为 A 级（真实 fetch 构造体 +
    /// 已实测同款请求成功）；`UPLOAD_TYPE_FORM`/`UPLOAD_TYPE_RESUMABLE` 为 B 级常量（枚举定义），
    /// 未见 `upload_type="TORRENT"` 的显式枚举，故本方法不直接投递 `UPLOAD_TYPE_FORM`，而是把
    /// 用户手里的 .torrent 字节解析出 info-hash（即磁力 `xt=urn:btih:<hash>`），再以已验证可行的
    /// `UPLOAD_TYPE_URL` 通道提交，复用 `offline_submit` —— 这是证据最扎实、最可落地的路径。
    ///
    /// 若 `enable_form_upload` 为 true，则额外按 B 级证据把原始 .torrent 以 multipart/form-data
    /// 直传（字段 `file`，Content-Type `multipart/form-data`，见 `m_60.js` / `js_utils-initial.*.js.js`
    /// 的 `multipart/form-data` + `form.multi_parts` 提交形状），返回 filename 供调用方定位任务。
    /// 两种下发通道并行：magnet 走 url 通道（实测稳），multipart 走 form 直传（B 级，未实测）。
    ///
    /// 错误处理沿用 `offline_submit` 的「非 2xx 带响应体进 DeviceFlow」模式。
    pub async fn torrent_upload(
        &self,
        state: &AuthState,
        torrent: &[u8],
        name: &str,
        enable_form_upload: bool,
    ) -> Result<TorrentUploadResp, ClientError> {
        // 1) 解析 torrent 字节 -> 磁力链接（B级：@xunlei/bencode-worker + @xunlei/gcid-worker
        //    依赖见 m_180.js / mod180_source.js 的 package.json dump；解析逻辑为通用 bencode，
        //    此处纯函数实现，便于单测）。
        let info_hash = bencode_info_hash(torrent)
            .map_err(|e| ClientError::DeviceFlow(format!("torrent 解析失败: {e}")))?;
        let magnet = format!("magnet:?xt=urn:btih:{info_hash}&dn={}", url_encode(name));

        // 2) 经已验证的 UPLOAD_TYPE_URL 通道提交磁力（A级：offline_submit 实测）。
        let submit = self.offline_submit(state, &magnet, name).await?;

        // 3) B级：multipart/form-data 原始字节直传（可选、默认关闭，未实测）。
        //
        // 【证据现状】详见 docs/research/xunlei/dump_mining_upload_hash.md：
        // - 已知（B级常量/提交形状）：
        //   · drive/v1/files 是多通道入口，dump 中 upload_type 枚举全集 =
        //     FORM / RESUMABLE / UNKNOWN / URL（m_89.js、m_12.js、js_utils-initial.*.js.js）；
        //   · 本地文件上传走 multipart/form-data、字段名 `file`（m_60.js:11469、
        //     js_utils-initial.*.js.js:205012 的 form.multi_parts 提交形状）；
        //   · 小文件（≤1GB）归 FORM、大文件归 RESUMABLE（m_60.js:10004 阈值判断）。
        // - 未知/自相矛盾（本分支尚不可信的原因）：
        //   a) 端点存疑——dump 显示 web 端 multipart 提交挂在某 form.url（疑似 OSS 预签名
        //      直传地址）上，「非 drive/v1/files 直投」；而本实现 POST 的是 drive/v1/files，
        //      端点本身就可能不对；
        //   b) 字段集未知——是否需随附 upload_type=UPLOAD_TYPE_FORM / parent_id / name 等
        //      元数据字段无 A 级证据；本实现只发单个 `file` part；
        //   c) 响应形状未知——task.id / file.id 属尽力猜测解析；
        //   d) 枚举里无 UPLOAD_TYPE_TORRENT，torrent 字节是否存在独立直传通道未证实
        //      （web 端实际是前端 bencode-worker 解析后走磁力/URL 通道，即上面第 2 步）。
        // - 激活条件（转正前置）：抓一次真实网页端「本地上传」流量（HAR/代理），确认
        //   端点 + 完整字段集 + 响应形状后重写 torrent_form_upload，并带登录态实测通过，
        //   才可转正或放开默认开关；在此之前保持 default-off，调用方不应依赖 form_task_id。
        let mut form_task_id = String::new();
        if enable_form_upload {
            form_task_id = self
                .torrent_form_upload(state, torrent, name)
                .await
                .map_err(|e| ClientError::DeviceFlow(format!("torrent form 上传失败: {e}")))?;
        }

        Ok(TorrentUploadResp {
            info_hash,
            magnet,
            task_id: submit.task_id,
            file_id: submit.file_id,
            form_task_id,
        })
    }

    /// B级：把原始 .torrent 以 multipart/form-data 直传到 `POST /drive/v1/files`
    /// （字段 `file`，见 `m_60.js` / `js_utils-initial.*.js.js` 的 `multipart/form-data` 提交形状）。
    /// 返回响应里可能携带的任务/文件 id（尽力解析，未实测，故该通道默认不开启）。
    async fn torrent_form_upload(
        &self,
        state: &AuthState,
        torrent: &[u8],
        name: &str,
    ) -> Result<String, ClientError> {
        let part = reqwest::multipart::Part::bytes(torrent.to_vec())
            .file_name(format!("{}.torrent", name))
            .mime_str("application/x-bittorrent")
            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(torrent.to_vec()));
        let form = reqwest::multipart::Form::new().part("file", part);
        let resp = self.http
            .post(format!("{PAN_BASE}/drive/v1/files"))
            .headers(Self::auth_headers(state))
            .multipart(form)
            .send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClientError::DeviceFlow(format!(
                "torrent_form_upload -> {status}: {body}"
            )));
        }
        // 尽力解析 task.id / file.id（与 offline_submit 同构）。
        let raw: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        let task_id = raw.get("task").and_then(|t| t.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let file_id = raw.get("file").and_then(|t| t.get("id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
        Ok(if !task_id.is_empty() { task_id } else { file_id })
    }
}

/// torrent 上传响应（torrent 字节直传云端离线的返回聚合）。
#[derive(Clone, Debug, Default)]
pub struct TorrentUploadResp {
    /// 从 .torrent 解析出的 40 位 info-hash（小写十六进制，即 btih）。
    pub info_hash: String,
    /// 由 info-hash 拼出的磁力链接（已提交到 UPLOAD_TYPE_URL 通道）。
    pub magnet: String,
    /// UPLOAD_TYPE_URL 通道返回的离线任务 id。
    pub task_id: String,
    /// UPLOAD_TYPE_URL 通道返回的云盘文件 id（resolve 取直链用）。
    pub file_id: String,
    /// 若开启 multipart 直传，返回其任务/文件 id（B级通道，未实测）。
    pub form_task_id: String,
}

/// 设备码响应（request_device_code 的返回）。
#[derive(Clone, Debug)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    /// 二维码内容（verification_uri_complete，前端把它转成二维码图片）。
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// token 端点成功响应（refresh / 设备码轮询成功共用结构）。
#[derive(Clone, Debug, Deserialize)]
pub struct TokenResp {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

/// PLAY API 响应（files/{id}?usage=PLAY）。
#[derive(Clone, Debug, Deserialize)]
pub struct PlayResp {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub web_content_link: String,
    /// 服务端返回字符串形态的数字（如 "67026810"），柔性解析。
    #[serde(default, deserialize_with = "de_size_flexible")]
    pub size: Option<u64>,
}

/// 尺寸字段柔性反序列化：数字或字符串均可（F3 PoC 已知坑）。
fn de_size_flexible<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<serde_json::Value>::deserialize(d)?;
    Ok(match v {
        Some(serde_json::Value::Number(n)) => n.as_u64(),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> AuthState {
        AuthState {
            access_token: "at123".into(), refresh_token: "rt".into(),
            device_id: "dev456".into(), captcha_token: "ck789".into(),
            user_id: "860599297".into(),
            access_token_expires_at: 0, captcha_token_expires_at: 0,
        }
    }
    #[test]
    fn auth_headers_has_three_elements() {
        let h = Client::auth_headers(&state());
        assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer at123");
        assert_eq!(h.get("x-device-id").unwrap(), "dev456");
        assert_eq!(h.get("x-captcha-token").unwrap(), "ck789");
        assert_eq!(h.get("x-client-id").unwrap(), CLIENT_ID);
    }

    #[test]
    fn normalize_phone_adds_plus_86_when_missing() {
        assert_eq!(normalize_phone("13012345678"), "+8613012345678");
        assert_eq!(normalize_phone("13800001111"), "+8613800001111");
    }

    #[test]
    fn normalize_phone_keeps_existing_plus() {
        assert_eq!(normalize_phone("+8613012345678"), "+8613012345678");
        assert_eq!(normalize_phone("+8613800001111"), "+8613800001111");
    }

    #[test]
    fn normalize_phone_keeps_foreign_country_code() {
        assert_eq!(normalize_phone("+85261234567"), "+85261234567");
    }

    #[test]
    fn hex_encode_lowercase_hex() {
        // 0x0a 0xff 0x00 -> "0aff00"
        assert_eq!(hex_encode(&[0x0a, 0xff, 0x00]), "0aff00");
        assert_eq!(hex_encode(&[]), "");
        assert_eq!(hex_encode(&[0x00, 0x11, 0xab]), "0011ab");
    }

    #[test]
    fn bencode_info_hash_known_vector() {
        // 构造一个最小合法 torrent：一个 info 字典，内容稳定可复算。
        // d8:announce0:4:infod4:name3:abc12:piece lengthi1e6:pieces0:ee
        // 取 "4:info" 之后的 d4:name3:abc12:piece lengthi1e6:pieces0:e 做 SHA-1
        let torrent = b"d8:announce0:4:infod4:name3:abc12:piece lengthi1e6:pieces0:ee";
        // 独立计算期望值（用 sha1 直接对 info 字节求摘要）
        let info_bytes = b"d4:name3:abc12:piece lengthi1e6:pieces0:e";
        let expected = hex_encode(&sha1::Sha1::digest(info_bytes));
        let got = bencode_info_hash(torrent).expect("应能解析 info 字典");
        assert_eq!(got, expected);
        assert_eq!(got.len(), 40);
    }

    #[test]
    fn bencode_info_hash_rejects_missing_info() {
        let not_torrent = b"d4:name3:abce";
        assert!(bencode_info_hash(not_torrent).is_err());
    }

    #[test]
    fn url_encode_percent_encodes_special_chars() {
        assert_eq!(url_encode("abc"), "abc");
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("中文"), "%e4%b8%ad%e6%96%87");
        // 单引号/空格等需编码，连字符/下划线保留
        assert_eq!(url_encode("a-b_c.d~"), "a-b_c.d~");
    }
}
