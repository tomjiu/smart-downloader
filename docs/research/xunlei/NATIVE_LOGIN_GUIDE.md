# 迅雷原生登录用户手册（smart-dl xunlei-login）

> 更新：2026-08-30（Task 5-b）
> 适用版本：provider/login_flow + login_page + daemon/xunlei_login（本仓库）
> 结论先行：**三种原生登录模式全部可用，均不依赖官方客户端二进制**；
> 登录态（access_token/refresh_token/captcha_token）只保存在本机文件。

---

## 一、三种登录模式总览

| 模式 | 命令 | 用户体验 | 对应需求 |
|------|------|----------|----------|
| 本地登录页（**默认**） | `smart-dl-daemon xunlei-login` | 浏览器打开 `http://127.0.0.1:<随机端口>`，看到**与迅雷 App 一致风格的登录页**（深蓝渐变 + 白色圆角卡片 + "迅雷"品牌标志 + 三个 Tab：扫码登录 / 密码登录 / 短信登录） | "本地渲染的和 app 一样的页面" |
| 官方页跳转 | `smart-dl-daemon xunlei-login --browser` | 命令执行后**系统浏览器直接跳转迅雷官方授权页** `pan.xunlei.com/yc/?client_id=…&user_code=…`，在官方页面扫码或登录确认；本地同时保留一个备用登录页（浏览器被拦截时手动打开） | "点击直接跳转官方页面" |
| 终端二维码 | `smart-dl-daemon xunlei-login --qr` | 命令行直接打印二维码（unicode 字符画），手机迅雷 App「扫一扫」即可，无需浏览器 | 无图形环境的服务器场景 |

通用参数：

```
--token <path>   登录态保存路径（默认 ./xunlei_auth.json，权限 0600）
--port <n>       本地登录页端口（默认 0 = 随机端口）
```

登录成功后控制台输出 `user_id` 与落盘路径；**token 本身不会打印**。

## 二、登录页视觉复刻清单（与 App 一致性说明）

本地登录页（`crates/provider/src/xunlei/login_page.html`）复刻的迅雷 App 登录页视觉元素：

| 元素 | 复刻实现 |
|------|----------|
| 背景 | 深蓝渐变（`#0b1c3f → #16345f → #1d4a7a`，135°） |
| 卡片 | 白色圆角（16px）+ 大投影，居中布局 |
| 品牌标志 | 蓝色渐变圆角方块 + 白色闪电三角 + "迅雷"字标（品牌蓝 `#2468f2`） |
| Tab 三选 | 扫码登录 / 密码登录 / 短信登录，激活态蓝色下划线 |
| 扫码区 | 白底方框内嵌二维码（SVG 实时生成）+ "打开手机迅雷 App → 右上角扫一扫" 提示 + 实时状态行（等待扫码/授权成功/已过期） |
| 输入框 | 浅灰底圆角输入框，聚焦变白描蓝边；短信 Tab 带"获取验证码"后缀按钮（60s 倒计时） |
| 主按钮 | 蓝色渐变（`#2468f2 → #3b82f6`）通栏圆角按钮 |
| 行为 | 扫码区 1.5s 轮询授权状态；成功后显示 ✓ 并展示 user_id；失败/过期给出"重新生成二维码"按钮 |

## 三、三种模式的技术流程

### 3.1 扫码（设备码流程，RFC 8628）

```text
smart-dl                      迅雷服务端
  │ POST /v1/auth/device/code    │  (client_id = Xqp0kJBXWhwaTpB6)
  │──────────────────────────────▶
  │◀──── device_code + user_code │
  │                              │
  │ 本地构造授权页 URL：            │
  │ https://pan.xunlei.com/yc/?client_id=Xqp0…&user_code=UCxxxx
  │ （跳转官方页 / 渲染二维码）      │
  │                              │  用户在官方页确认（App 扫码/网页登录）
  │ POST /v1/auth/token          │  (grant_type=device_code, 轮询)
  │──────────────────────────────▶
  │◀──── access_token(+JWT.sub=user_id) + refresh_token
  │                              │
  │ POST /v1/shield/captcha/init │  (captcha_sign 9 盐套件)
  │──────────────────────────────▶
  │◀──── captcha_token           │
  │                              │
  │ 落盘 xunlei_auth.json (0600) │
```

- 端点实测依据：`docs/research/2026-08-22-xunlei-login-reverse-status.md`、`docs/PROJECT_STATUS.md §三`（2026-08-25 端到端通过）。
- client_id 说明：设备码流程原代码使用 `XW5SkOhLDjnOZP7J`（已知失败值），本次对齐为实测通过的 web 端 `Xqp0kJBXWhwaTpB6`（见 client.rs 常量注释）。

### 3.2 密码登录

1. `POST /v1/shield/captcha/init`（action=`POST:/v1/auth/signin`，meta 带完整签名套件：timestamp + captcha_sign + client_version + package_name；按用户名类型附 phone_number/email/username）
2. `POST /v1/auth/signin`（`{username, password, client_id}`，HTTPS 明文密码——逆向确认无客户端加密；带 `X-Captcha-Token` 头）
3. user_id 优先取响应体，缺省从 access_token JWT `sub` 声明解析。

注意：账密登录可能触发风控（实测出现 `result:review` / captcha_invalid 4002）。**推荐优先扫码**；页面密码 Tab 失败时会提示改用扫码。

### 3.3 短信验证码登录

1. `POST /v1/auth/verification`（发码，返回 verification_id）
2. `POST /v1/auth/verification/verify`（verify，返回 token 三件套）

页面短信 Tab 自动透传 verification_id。

## 四、登录态的保存与复用

| 项 | 说明 |
|----|------|
| 保存路径 | `--token` 指定，默认 `./xunlei_auth.json` |
| 文件权限 | POSIX 下设为 0600；Windows 继承用户目录 ACL |
| 内容 | AuthState JSON：access_token / refresh_token / device_id / captcha_token / user_id / 两个过期时间戳 |
| 复用 | `XunleiProvider::new("xunlei", token_path)` 直接加载；也兼容网页版 localStorage 导出的 `credentials_Xqp0…` 形状 |
| 自动续期 | provider 轮询时 access_token 剩余 <5min 自动 refresh；captcha_token 过期自动重签 |
| 安全约定 | 活体 token 严禁入库（.gitignore 已排除 `xunlei_auth*.json`）；日志/页面均不回显完整 token |

## 五、错误与降级

| 现象 | 原因 | 处理 |
|------|------|------|
| 扫码后一直 pending | 设备码过期前未确认 | 页面/CLI 显示过期后点"重新生成二维码"或重跑命令 |
| 密码登录 4002 captcha_invalid | 风控判定签名/环境异常 | 改用扫码（设备码流程不触发滑块） |
| `error_code 11`（下载时出现） | 当日配额耗尽 | 非登录问题；provider 自动冷却 1h 后重试（免费档离线 3 次/日） |
| 本地页打不开 | 端口被防火墙拦截 | `--port 18080` 指定固定端口重试 |

## 六、第三方登录（微信扫码）与两条设备码流辨析（2026-08-31 补充）

### 6.1 微信扫码：能用，但只在官方授权页上

- 官方 `/yc/` 授权页（web_auth_url）内含**账密/短信/扫码/微信/QQ/微博全方式**
  （Task 22/23 实证，2026-08-25）。
- 微信在流程中的角色 = **登录迅雷账号的身份源**；授权主体是**账号**而非某台
  已登录设备——浏览器里用微信登进账号后确认 user_code，与 App 扫码授权等价，
  设备码轮询照常截获 token。
- 操作路径：`xunlei-login --browser` → 官方页 → 选「微信」→ 手机微信确认 →
  页面确认 user_code 授权 → token 落盘。
- 前提：迅雷账号绑定过微信（未绑则首次扫码按提示绑定）。

### 6.2 本地页与终端二维码为什么做不了微信

- 微信 OAuth 的 **appid 回调白名单绑死迅雷官方域名**（微信开放平台机制，
  绕不开）→ 本地自托管页收不到回调，故本地页只承载扫码（迅雷 App）/
  密码/短信三个 Tab。
- 终端二维码（`--qr`）指向迅雷 App「扫一扫」通道，微信扫不开。

### 6.3 两条设备码流辨析（易混淆点）

存在两张形状相同的 `/yc/` 授权页，分属两条独立的设备码流：

| | **Win/Mac 主线（本模块）** | **NAS 引擎线（Task 31，已归档）** |
|---|---|---|
| client_id | `Xqp0kJBXWhwaTpB6`（web/PC 档） | `X9ibISwpIp8jQ4Ya`（pan-cli docker 档） |
| yc 页参数 | `scope`（空格转 %20） | `platform=docker&privilege=PLATFORM_DOCKER&space=device_id#…` |
| 授权对象 | 本客户端的云 API 访问 | NAS 引擎设备绑定 |
| 全方式登录（含微信） | ✅ 官方页 Task 22/23 实证 | 页面同源；NAS 线已归档不再维护 |
| 代码位置 | `crates/provider/src/xunlei/login_flow.rs` | `scripts/nas/a2_device_flow.py` |

## 七、测试与验证状态

| 项 | 状态 |
|----|------|
| 设备码流程端到端（真机 App 扫码） | ✅ 2026-08-22 / 2026-08-25 两轮真机验证（历史） |
| 本地 QR URL 构造 | ✅ 单测 `qr_url_uses_local_template` + 形状与实测 URL 一致 |
| 登录页设备码全链（mock 上游） | ✅ 集成测试 `login_page_e2e_device_flow`（start→pending→authorized→落盘） |
| 登录页密码链（mock 上游） | ✅ 集成测试 `login_page_password_flow`（captcha+signin→JWT 解 user_id→落盘） |
| 终端二维码渲染 | ✅ 单测 `print_qr_terminal_renders_ascii` |
| 账密真实账号验证 | ⏳ 待用户提供真实账号（历史遗留项，端点/请求形状已逆向确认） |
| 短信真实发送验证 | ⏳ 同上（端点 B 级证据） |

## 八、合规声明

- 本模块为**互操作性研究实现**：仅调用迅雷公开 Web/移动端登录端点，不接触、不分发迅雷专有代码，不绕过任何加密或鉴权机制。
- 二维码授权由**迅雷官方授权页**完成，用户凭证（密码/验证码）仅经 HTTPS 直连迅雷服务端，smart-dl 不存储密码、不上传任何数据。
- 登录态文件仅存本机；请勿分享 `xunlei_auth.json`。
- 仅供个人学习与合法互操作用途，请遵守当地法律法规与服务条款。
