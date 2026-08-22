# XunleiProvider 登录模块实现计划（第二期：OAuth 设备码 + 取链 + 离线）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `crates/provider/src/xunlei/` 实现 XunleiProvider 的登录态管理（OAuth 2.0 设备码流程）+ 取链（PLAY API）+ 离线下载（磁链提交），实现 `RemoteProvider` trait，接入现有 FallbackCoordinator。

**Architecture:** 迅雷 = 下载渠道（channel），自己引擎 = libtorrent（btcore）。XunleiProvider 把"云盘直链"作为 web seed 喂给 btcore。

**Tech Stack:** Rust 2021、reqwest 0.12（json）、serde、tokio、thiserror、async-trait、base64、sha1、md-5（已加）。

---

## 背景（写给零上下文工程师）

### 已逆向确证的事实（全部自己验证，非第三方）

1. **迅雷登录 = 标准 OAuth 2.0 设备码流程（RFC 8628）**，证据：`i.xunlei.com/login/` 登录页 HTML 含 RFC 8628 错误码（`authorization_pending`/`slow_down`/`expired_token`）。
2. **client_id** = `Xqp0kJBXWhwaTpB6`（网页端，从 access_token JWT aud 确认）。
3. **refresh_token 离线刷新可行**（已验证 HTTP 200）：
   ```
   POST https://xluser-ssl.xunlei.com/v1/auth/token
   {"grant_type":"refresh_token","refresh_token":"a1.xxx","client_id":"Xqp0kJBXWhwaTpB6"}
   → {access_token(JWT 12h), refresh_token(轮转), expires_in:43200}
   ```
4. **captcha_token 可离线匿名获取**（已验证 HTTP 200）：
   ```
   POST https://xluser-ssl.xunlei.com/v1/shield/captcha/init
   → {captcha_token:"ck0.xxx", expires_in:300}，可无限刷新
   ```
5. **取链 API**（F3 已验证）：`GET api-pan.xunlei.com/drive/v1/files/{id}?usage=PLAY` → `web_content_link`，需三要素头（Authorization/x-device-id/x-captcha-token）。

### 尚未确证（写代码时用浏览器实测补齐）

- device code 请求端点的确切 URL（授权端点）
- user_code / verification_uri 格式
- 二维码内容生成方式

---

## 文件结构

```
crates/provider/src/xunlei/
├── mod.rs           # 已有：sign.rs + hash.rs 的 re-export
├── sign.rs          # 已有：captcha_sign + device_sign
├── hash.rs          # 已有：GCID + CID
├── auth.rs          # 新增：AuthState（登录态）+ token 持久化 + refresh
├── device.rs        # 新增：设备码流程（扫码登录）
├── client.rs        # 新增：HTTP 客户端（三要素头 + 取链 + 离线）
└── provider.rs      # 新增：XunleiProvider（实现 RemoteProvider）
```

---

## 依赖变更

`crates/provider/Cargo.toml` 加：
```toml
reqwest = { workspace = true, features = ["json"] }
serde_json = { workspace = true }
```
（确认 workspace 根 Cargo.toml 已有 reqwest/serde_json 定义；若无则加版本号）

---

## Task 1: AuthState 数据模型 + token 持久化（纯结构，无网络）

**Files:** Create `crates/provider/src/xunlei/auth.rs`

- [ ] **Step 1: 定义 AuthState**

```rust
//! 登录态（OAuth 设备码流程的产物）+ token 持久化。

use serde::{Deserialize, Serialize};

/// 登录态三要素 + OAuth token。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthState {
    /// OAuth access_token（JWT，12h 有效）。
    pub access_token: String,
    /// OAuth refresh_token（轮转，长期）。
    pub refresh_token: String,
    /// device_id（App 端 32-hex）。
    pub device_id: String,
    /// captcha_token（300s 有效，可刷新）。
    pub captcha_token: String,
    /// access_token 过期 unix 秒。
    pub access_token_expires_at: u64,
    /// captcha_token 过期 unix 秒。
    pub captcha_token_expires_at: u64,
}

impl AuthState {
    /// access_token 是否即将过期（<5min 缓冲）。
    pub fn access_token_expiring(&self, now: u64) -> bool {
        now + 300 >= self.access_token_expires_at
    }
    /// captcha_token 是否即将过期（<60s 缓冲）。
    pub fn captcha_token_expiring(&self, now: u64) -> bool {
        now + 60 >= self.captcha_token_expires_at
    }
}
```

- [ ] **Step 2: 写失败测试（序列化 + 过期判断）**

在 auth.rs 加 `#[cfg(test)] mod tests`：
```rust
#[test]
fn auth_state_roundtrip_json() {
    let a = AuthState {
        access_token: "at".into(), refresh_token: "rt".into(),
        device_id: "dev".into(), captcha_token: "ck".into(),
        access_token_expires_at: 1000, captcha_token_expires_at: 500,
    };
    let j = serde_json::to_string(&a).unwrap();
    let b: AuthState = serde_json::from_str(&j).unwrap();
    assert_eq!(a, b);
}

#[test]
fn access_token_expiring_detection() {
    let a = AuthState { access_token_expires_at: 1000, captcha_token_expires_at: 500, ..default_state() };
    assert!(!a.access_token_expiring(0));   // 1000 - 0 > 300
    assert!(a.access_token_expiring(800));  // 1000 - 800 = 200 < 300
}

#[test]
fn captcha_token_expiring_detection() {
    let a = AuthState { access_token_expires_at: 1000, captcha_token_expires_at: 500, ..default_state() };
    assert!(!a.captcha_token_expiring(0));   // 500 - 0 > 60
    assert!(a.captcha_token_expiring(450));  // 500 - 450 = 50 < 60
}
```

- [ ] **Step 3: 运行测试确认失败** → `cargo test -p smart-dl-provider xunlei::auth`

- [ ] **Step 4: 实现默认构造 + 持久化**

```rust
/// 从磁盘加载 AuthState；不存在返回 None。
pub fn load(path: &std::path::Path) -> Option<AuthState> {
    let s = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

/// 保存 AuthState 到磁盘（原子写：先写临时文件再 rename）。
pub fn save(path: &std::path::Path, state: &AuthState) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string(state).unwrap())?;
    std::fs::rename(&tmp, path)
}
```

- [ ] **Step 5: 运行测试通过 + Commit**

`cargo test -p smart-dl-provider xunlei::auth` → PASS
`git commit -m "feat(provider): xunlei AuthState 数据模型 + token 持久化"`

---

## Task 2: HTTP 客户端（三要素头 + refresh + captcha 刷新）

**Files:** Create `crates/provider/src/xunlei/client.rs`

- [ ] **Step 1: 定义 Client + 请求头构造**

```rust
//! 迅雷 HTTP 客户端：三要素头 + OAuth refresh + captcha 刷新。

use crate::xunlei::auth::AuthState;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

pub const CLIENT_ID: &str = "Xqp0kJBXWhwaTpB6";
pub const XLUSER_BASE: &str = "https://xluser-ssl.xunlei.com";
pub const PAN_BASE: &str = "https://api-pan.xunlei.com";

pub struct Client {
    http: reqwest::Client,
}

impl Client {
    pub fn new() -> Self {
        Client { http: reqwest::Client::new() }
    }

    /// 构造 drive API 的三要素请求头。
    fn auth_headers(state: &AuthState) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Bearer {}", state.access_token)).unwrap());
        h.insert("x-device-id", HeaderValue::from_str(&state.device_id).unwrap());
        h.insert("x-captcha-token", HeaderValue::from_str(&state.captcha_token).unwrap());
        h.insert("x-client-id", HeaderValue::from_static(CLIENT_ID));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h
    }
}
```

- [ ] **Step 2: 写失败测试（auth_headers 构造）**

```rust
#[test]
fn auth_headers_has_three_elements() {
    let state = AuthState {
        access_token: "at123".into(), refresh_token: "rt".into(),
        device_id: "dev456".into(), captcha_token: "ck789".into(),
        access_token_expires_at: 0, captcha_token_expires_at: 0,
    };
    let h = Client::auth_headers(&state);
    assert_eq!(h.get(AUTHORIZATION).unwrap(), "Bearer at123");
    assert_eq!(h.get("x-device-id").unwrap(), "dev456");
    assert_eq!(h.get("x-captcha-token").unwrap(), "ck789");
    assert_eq!(h.get("x-client-id").unwrap(), CLIENT_ID);
}
```

- [ ] **Step 3: 实现 refresh（OAuth refresh_token grant）**

```rust
/// 用 refresh_token 换新 access_token（已验证可行）。
pub async fn refresh(&self, state: &mut AuthState) -> Result<(), ClientError> {
    let resp: TokenResp = self.http
        .post(format!("{}/v1/auth/token", XLUSER_BASE))
        .json(&serde_json::json!({
            "grant_type": "refresh_token",
            "refresh_token": state.refresh_token,
            "client_id": CLIENT_ID,
        }))
        .send().await?
        .error_for_status()?
        .json().await?;
    state.access_token = resp.access_token;
    state.refresh_token = resp.refresh_token;  // 轮转
    state.access_token_expires_at = now_unix() + resp.expires_in;
    Ok(())
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}
```

- [ ] **Step 4: 实现 captcha 刷新**

```rust
/// 匿名获取/刷新 captcha_token（已验证可行，300s 有效）。
pub async fn refresh_captcha(&self, state: &mut AuthState) -> Result<(), ClientError> {
    let resp: CaptchaResp = self.http
        .post(format!("{}/v1/shield/captcha/init", XLUSER_BASE))
        .json(&serde_json::json!({
            "action": "POST:/drive/v1/files",
            "captcha_token": "",
            "client_id": CLIENT_ID,
            "device_id": state.device_id,
            "meta": {},
            "redirect_uri": "xlaccsdk01://xunlei.com/callback?state=harbor",
        }))
        .send().await?
        .error_for_status()?
        .json().await?;
    state.captcha_token = resp.captcha_token;
    state.captcha_token_expires_at = now_unix() + resp.expires_in;
    Ok(())
}

#[derive(Deserialize)]
struct CaptchaResp {
    captcha_token: String,
    expires_in: u64,
}
```

- [ ] **Step 5: 定义 ClientError + now_unix 复用**

```rust
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
}
```

注意 `now_unix()` 已有（`crate::types::now_unix`，但它是 `pub(crate)`，需确认可见性；若不可见就在 client.rs 内联一个）。

- [ ] **Step 6: 运行测试 + Commit**

`cargo test -p smart-dl-provider xunlei::client` → PASS
`git commit -m "feat(provider): xunlei HTTP 客户端（三要素头 + refresh + captcha）"`

---

## Task 3: 设备码流程（扫码登录）

**Files:** Create `crates/provider/src/xunlei/device.rs`

> **注意**：本 Task 的端点是"待浏览器实测"的占位。先实现**状态机结构**（等待扫码 → 轮询 → 成功），端点的确切 URL 用 `const` 占位并在写代码时实测替换。

- [ ] **Step 1: 定义设备码流程状态机（纯结构，可测试）**

```rust
//! OAuth 2.0 设备码流程（RFC 8628）：请求 device code → 扫码 → 轮询 token。

/// 设备码流程的状态。
#[derive(Clone, Debug, PartialEq)]
pub enum DeviceFlowState {
    /// 已请求 device code，等待用户扫码。
    AwaitingScan { device_code: String, user_code: String, verification_uri: String, expires_at: u64 },
    /// 扫码成功，拿到 token。
    Done { access_token: String, refresh_token: String },
    /// 用户拒绝 / 过期。
    Failed { reason: String },
}

impl DeviceFlowState {
    /// 轮询一次：根据服务端返回更新状态。
    /// `error_code`: None=成功，Some("authorization_pending")=继续等，Some("slow_down")=降速，Some("expired_token")=过期。
    pub fn on_poll(&self, error_code: Option<&str>, now: u64) -> DeviceFlowState {
        match (self, error_code) {
            (_, None) => DeviceFlowState::Done { /* token 由调用方填入 */ access_token: String::new(), refresh_token: String::new() },
            (DeviceFlowState::AwaitingScan { expires_at, .. }, Some("expired_token")) => {
                DeviceFlowState::Failed { reason: "device code expired".into() }
            }
            (DeviceFlowState::AwaitingScan { expires_at, .. }, Some(_)) => {
                if now >= *expires_at {
                    DeviceFlowState::Failed { reason: "timeout".into() }
                } else {
                    self.clone()  // 继续等待
                }
            }
            _ => self.clone(),
        }
    }
}
```

- [ ] **Step 2: 写测试（状态机转换）**

```rust
#[test]
fn awaiting_scan_stays_waiting_on_pending() {
    let s = DeviceFlowState::AwaitingScan {
        device_code: "dc".into(), user_code: "uc".into(),
        verification_uri: "https://example.com".into(), expires_at: 1000,
    };
    assert!(matches!(s.on_poll(Some("authorization_pending"), 500), DeviceFlowState::AwaitingScan { .. }));
}

#[test]
fn awaiting_scan_fails_on_expired_token() {
    let s = DeviceFlowState::AwaitingScan {
        device_code: "dc".into(), user_code: "uc".into(),
        verification_uri: "https://example.com".into(), expires_at: 1000,
    };
    assert!(matches!(s.on_poll(Some("expired_token"), 500), DeviceFlowState::Failed { .. }));
}

#[test]
fn awaiting_scan_times_out() {
    let s = DeviceFlowState::AwaitingScan {
        device_code: "dc".into(), user_code: "uc".into(),
        verification_uri: "https://example.com".into(), expires_at: 1000,
    };
    assert!(matches!(s.on_poll(Some("authorization_pending"), 1500), DeviceFlowState::Failed { .. }));
}
```

- [ ] **Step 3: 运行测试确认失败 → 实现 → 通过 + Commit**

`cargo test -p smart-dl-provider xunlei::device` → PASS
`git commit -m "feat(provider): xunlei 设备码流程状态机（扫码登录骨架）"`

---

## Task 4: XunleiProvider（实现 RemoteProvider）

**Files:** Create `crates/provider/src/xunlei/provider.rs` + 更新 mod.rs

- [ ] **Step 1: 定义 XunleiProvider 结构**

```rust
//! XunleiProvider：迅雷云盘渠道，实现 RemoteProvider。

use crate::types::{ProviderError, ProviderRuntime, ProviderStatus, ProviderTaskId, ResolvedRemoteFile};
use crate::xunlei::auth::AuthState;
use crate::xunlei::client::Client;
use smart_dl_core::types::{Capability, DownloadSource};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct XunleiProvider {
    name: String,
    client: Client,
    /// 登录态（Arc<Mutex> 供 refresh 更新）。
    auth: Arc<Mutex<Option<AuthState>>>,
    /// token 持久化路径。
    token_path: PathBuf,
    /// 任务表（submit 后跟踪）。
    tasks: Arc<Mutex<std::collections::HashMap<ProviderTaskId, TaskEntry>>>,
}

struct TaskEntry {
    file_id: String,          // 云端文件 id
    status: ProviderStatus,
}
```

- [ ] **Step 2: 实现 RemoteProvider trait 的方法签名（先骨架，返回 Ok/Err 占位）**

关键方法：
- `name() -> &str`：返回 "xunlei"
- `capabilities()`：`vec![Capability::OfflineCache, Capability::UrlRefresh]`
- `runtime()`：根据 auth 状态构造 ProviderRuntime（authenticated = auth 是否 Some 且未过期）
- `refresh_auth()`：调用 client.refresh（若 auth 存在）+ client.refresh_captcha，更新 auth + 持久化
- `submit(source)`：匹配 source → ① Magnet → 离线提交 ② XunleiShare → 分享解析（待定）→ 拿 file_id → 存任务表
- `status(id)`：查任务表（真实实现要轮询云端任务状态，这里先返回 Ready 占位）
- `resolve(id)`：调 PLAY API 拿 web_content_link → 构造 ResolvedRemoteFile
- `remove(id)`：删任务
- `refresh_links(id)`：重调 PLAY API 换新链

> **诚实标注**：submit/status/resolve 的**真实网络逻辑**依赖端点 URL（待 Task 3 的浏览器实测），本 Task 先实现**结构 + 空实现**，保证 trait 可编译、可被 coordinator 调用。真实取链逻辑在端点确认后补。

- [ ] **Step 3: 写一个最小测试（name/capabilities/runtime）**

```rust
#[test]
fn provider_reports_name_and_capabilities() {
    let p = XunleiProvider::new("xunlei", PathBuf::from("nonexistent.json"));
    assert_eq!(p.name(), "xunlei");
    assert!(p.capabilities().contains(&Capability::OfflineCache));
}
```

- [ ] **Step 4: 运行测试 + Commit**

`cargo test -p smart-dl-provider xunlei::provider` → PASS
`git commit -m "feat(provider): XunleiProvider 骨架（实现 RemoteProvider trait）"`

---

## Task 5: core 侧接入（XunleiShare 变体 + registry 路由）

**Files:** Modify `crates/core/src/types.rs` + `crates/core/src/registry.rs`

- [ ] **Step 1: types.rs 加 XunleiShare 变体**

```rust
pub enum DownloadSource {
    // ... 现有 ...
    Thunder(String),
    /// 迅雷网盘分享链接（pan.xunlei.com/s/xxx?pwd=yyy）。
    XunleiShare(String),
    Ed2k(String),
}
```

- [ ] **Step 2: registry.rs 加路由分支**

```rust
DownloadSource::XunleiShare(_) => self
    .first_with(Capability::OfflineCache)
    .ok_or(RoutingError::NoEngineForSource),
```

- [ ] **Step 3: 修复所有 match 的 exhaustive 编译错误**

`XunleiShare` 是新增变体，所有 `match source` 的地方都要加分支（router.rs、normalize.rs 等）。运行 `cargo build --workspace` 找出所有需要补的地方，逐个加 `XunleiShare` 分支（未实现的先返回 Unsupported 或类似）。

- [ ] **Step 4: 运行全量编译 + Commit**

`cargo build --workspace` → 无 error
`git commit -m "feat(core): DownloadSource::XunleiShare 变体 + registry 路由"`

---

## Self-Review 检查结果

1. **Spec coverage**：AuthState+持久化（Task1）、HTTP 客户端（Task2）、设备码状态机（Task3）、XunleiProvider（Task4）、core 接入（Task5）—— 覆盖"登录模块"的结构骨架。
2. **Placeholder scan**：设备码端点 URL 和取链端点是"待浏览器实测"的明确占位（在文档里标注，不在代码里写死错误 URL）；submit/resolve 的真实网络逻辑标注"端点确认后补"。
3. **Type consistency**：AuthState 字段、ClientError、DeviceFlowState 前后一致。
4. **诚实边界**：Task 4 的 submit/resolve 是骨架（可编译但不真取链），这是**有意为之**——因为端点 URL 还没实测确认，先搭好结构，端点确认后填真实逻辑。

## 遗留（后续）

- 设备码端点的确切 URL（浏览器实测，见 §背景"尚未确证"）
- 取链 PLAY API 的确切参数和响应结构（F3 已记录，需用真实 token 复测）
- 分享链接解析（XunleiShare 的 share API，之前验证"免登录走不通"，登录后待测）
- 离线下载任务轮询（磁链提交后轮询 cloud task 状态）
