# 迅雷登录逆向 — 当前进度与现状存档

> **存档日期**: 2026-08-22
> **状态**: web 端登录逆向已收尾，账密登录已实现未真实验证，暂停继续（用户指示"web 端先不做了"）
> **目的**: 固化本次会话所有结论，避免上下文丢失后重新逆向

---

## 一、核心结论速览

### 1. 登录方式可行性（一手逆向，非二手资料）

| 登录方式 | 端点 | 是否扫码 | 依赖 | 可行性 |
|---------|------|---------|------|--------|
| **账密登录** | `/v1/auth/signin` | 否 | client_id + captcha_token | ✅ 已实现（未真实验证） |
| **短信验证码** | `/v1/auth/verification` + `/verify` | 否 | client_id | ✅ 可复现（可能触发滑块） |
| **迅雷 App 扫码** | `/v1/auth/device/code` | 是 | 无 | ✅ 已实现（设备码流程） |
| **微信/QQ/微博** | `/v1/auth/provider/uri` + `/provider/token` | 是（跳第三方） | 微信/QQ/微博开放平台 AppID+secret | ❌ 第三方拿不到 |

### 2. OAuth 回调（redirect_uri）—— 顶替不了，结论定论

服务端按 **client_id 维度** 校验 redirect_uri 的 host：

| client_id | 身份 | redirect_uri host 白名单 |
|-----------|------|------------------------|
| `Xqp0kJBXWhwaTpB6` | web 端 | `i.xunlei.com pan.xunlei.com sl-m-ssl.xunlei.com sj-m-ssl.xunlei.com admin.mobile.xunlei.com dev-f2e.xunlei.com test-pan.xunlei.com pre-pan.xunlei.com` |
| `Xp6vsxz_7IYVw2BB` | App 端（安卓） | `com.xunlei.downloadprovider`（**安卓包名**，非 scheme） |
| `XW5SkOhLDjnOZP7J` | web 登录 device-code | `i.xunlei.com` + Chrome 扩展 ID + ... |

**为什么"顶替 scheme"这条路卡住**（纠正：不是"Windows 没包名所以顶替不了"，那是错误表述）：

1. **`xlaccsdk01://xunlei.com/callback` 是 Windows 桌面迅雷的 scheme** —— **Windows 注册表能注册这个 scheme，顶替本身技术上可行**。这一点之前说错了，特此纠正。

2. **真正的卡点**：顶替 scheme 后，必须有 OAuth 授权流程**真的往这个 scheme 回调**。回调需要一个 **client_id，且其 redirect_uri 白名单里包含 `xunlei.com`**。

3. **已实测的三个 client_id 白名单都不含裸 `xunlei.com`**：
   - web 端 `Xqp0kJBXWhwaTpB6` → 8 个 `xxx.xunlei.com` 域名（无裸 `xunlei.com`）
   - 安卓 App 端 `Xp6vsxz_7IYVw2BB` → `com.xunlei.downloadprovider`（安卓包名，与 Windows 无关）
   - device-code `XW5SkOhLDjnOZP7J` → `i.xunlei.com` 等

4. **未解决的未知项**：允许回调到 `xlaccsdk01://xunlei.com/callback` 的那个 client_id 是**桌面迅雷自己的 client_id**，它在**桌面客户端**里（不在 web 端 dump 代码）。要坐实"能否顶替"，需从桌面迅雷客户端（如 `XDASKernel.dll`）逆向出它的 client_id + client_secret + redirect_uri 白名单。

**最终结论（当前）**：正常第三方软件登录迅雷 = **账密直登 + 设备码扫码 + 短信验证码**（均无需 redirect 回调）。"顶替桌面迅雷 scheme"是**未验证的潜在路径**，卡在"桌面客户端 client_id 未逆向"，留待后续。

---

## 二、已保存的登录态（供软件测试）

文件：`scripts/research/cloud_delivery/login_reverse/login_state_tokens.json`

| 字段 | 状态 | 说明 |
|------|------|------|
| `access_token` | ✅ 有效至 8/23 08:52 | 从浏览器 localStorage `credentials_Xqp0kJBXWhwaTpB6` 提取 |
| `device_id` | ✅ 有效 | `wdi10.adb1a76709f6584a13b58baaf6e1d871d02650159e5762f2299e41b38b017500` |
| `captcha_token` | ✅ 有效 | 也可随时用 captcha_sign 重新生成 |
| `user_id` | ✅ `860599297` | 手机号 `+86 130***963` |
| `refresh_token` | ⚠️ **已失效** | `invalid_grant`（曾用旧脚本触发过 refresh 导致轮换） |

**实测验证**：该登录态能正常 `captcha/init`(200) + `drive/v1/files`(200，列出 3 个文件夹)。

**注意**：refresh_token 已失效 → access_token 过期后（08:52）无法自动续期，需重新登录。这正是账密登录的价值。

---

## 三、账密登录实现状态

### 已实现（Rust）

`crates/provider/src/xunlei/client.rs` 新增 `signin(username, password, device_id)` 方法：

- 端点 `/v1/auth/signin`（web 端 SDK，非 App 端 CoreLogin）
- 请求体 `{username, password, client_id}`，密码**明文**（HTTPS，无客户端加密）
- 需 `X-Captcha-Token` 头（登录前 captcha/init，action=`POST:/v1/auth/signin`，meta 用 `phone_number`/`email`/`username`）
- username 规则：`+`开头→phone_number；含`@`→email；否则 username
- **编译通过，35 个测试全过**

### 未验证

- 用**真实账号密码**端到端验证 `signin()`（之前用假号测，返回"账号冻结"证明格式正确，但未验证真实成功）
- 需要用户提供真实账号密码才能完成闭环

---

## 四、逆向结论来源（一手）

全部来自 web 端 dump 的模块（非 alist/二手脚本）：

| 结论 | 来源 |
|------|------|
| captcha_sign 9盐算法 | `module_1428_source.js` + config 模块 23（见 README_captcha_sign.md） |
| 登录端点清单 | `node_modules_dump/m_1182.js`（`@xbase/sso` SDK） |
| signIn 账密登录 | `m_1182.js` 约 130525 字符处 |
| refresh 需要 client_secret | `m_1182.js` 约 78995 字符处（`_defaultRefreshTokenFunc`） |
| redirect_uri 白名单 | 实测 `/v1/user/authorize/info` 端点 |
| 账密端点验证 | `probe_signin.py`（假号返回业务错误，证明格式正确） |

---

## 五、待办 / 后续方向

1. **账密登录真实验证**（阻塞项：需真实账号密码）—— 路线 A，优先
2. **短信验证码登录**（`/v1/auth/verification`，未实现）
3. **离线提交端点**（`submit()`，仍为骨架，之前想逆向但未完成）
4. **web 端 client_secret**（refresh 需要，config 模块无，未逆向出）

5. **【最后做】桌面迅雷 scheme 顶替（路线 B）**
   - 注册表注册 `xlaccsdk01://` 技术可行（已纠正"Windows 不行"的错误结论）
   - 双卡点：① 桌面 client_id/secret 在 `XDASKernel.dll` 等客户端二进制里，未逆向；
     ② 桌面请求可能带额外设备签名，未逆向
   - 前置：逆向桌面 DLL → 成本高、收益不确定 → 用户指示标记为最后做

> 注：用户指示"先不做了，跑偏太远"，以上待办全部暂停。
