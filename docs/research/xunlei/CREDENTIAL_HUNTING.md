# 迅雷凭证狩猎·交接文档 v2（反编译优先）

> 更新：2026-08-25。**第一优先参考 = 反编译代码与运行时日志实录**。
> 所有第三方来源（alist 等）推导的路径已证伪并废弃，见 §4。
>
> ## 🎉 §0.5 终局（2026-08-25 晚）：登录已解决
> 票源 = 用户浏览器 `pan.xunlei.com` 的 localStorage `credentials_Xqp0kJBXWhwaTpB6`
> （aud=Xqp0，白名单正主；refresh_token 为 `a1.` 前缀格式，12h 自动续期实测通过）。
> **此前所有 "no client info found" 的真正原因**：captcha/init 的 `meta` 必须携带
> `{user_id, captcha_sign, client_version:"1.92.91", package_name:"pan.xunlei.com", timestamp}`，
> 且请求本身要带 `Authorization: Bearer` 头。captcha_sign 用本仓 sign.rs 已实现的
> 9 轮盐链算法（base=client+ver+host+did32+tsMs）。完整可运行配方：
> `scripts/research/xunlei/web_token_validate.ps1`（token→captcha→list→PLAY→Range206 全链验证）。
> 凭证文件：`xunlei_auth_web.json`（含 refresh 自动回写）。

---

## 0. 现状一句话

本地桌面迅雷已登录；缓存中有有效 XW-G4 access_token（scope=user pan sync offline，
~43h 有效期，实测可提取），但 api-pan 按 captcha 签发 client 白名单校验，XW-G4/XW5Sk
均不在册 → 唯一前进路径 = **按 xllite 反编译实录的设备码流程，用白名单 client
重新走一次授权**（手机迅雷 App 扫码确认，该模式 8/22 已端到端成功过一次）。

## 1. 权威参考源

| 来源 | 位置 |
|------|------|
| xllite 运行时日志实录 | 本次会话捕获：启动日志含完整 config dump、`detect platform: pcxllite XW-G4v1H72tgfJym`、`drive/auth.go:69 GetQrControlUrl → resp: https://pan.xunlei.com/yc/?client_id=XW-G4…&user_code=…`、`oauth2client/client.go:575 POST /v1/auth/device/code` |
| Go 符号表 | xllite.exe 偏移 ~39706533（platformdetect 包全量方法名） |
| 服务端全局配置 | 启动时拉取 conf-m-ssl 成功，含 `allow_inner_api_paths` 25 条 |
| 归档文档 | docs/research/xunlei/{NEXT_ACTION,xllite_reverse,xllite_oauth_exchange}.md |

## 2. 登录体系（仅反编译/运行时实证）

### 2.1 设备码授权流（xllite auth.go 实录）
```
1. POST https://xluser-ssl.xunlei.com/v1/auth/device/code
   （oauth2client/client.go:575 发起；响应含 device_code/user_code/expires_in=120/interval=2）
2. 【关键】二维码 URL 由客户端本地构造，不使用服务端返回的 verification_url：
   https://pan.xunlei.com/yc/?client_id={cid}
       &noActionBar=true&noStatusBar=true
       &platform=pcxllite&plm=pcl&privilege=PLATFORM_PCXLLITE
       &runner_space=platform%23pcxllite
       &space=device_id%23{device_id_32hex}
       &user_code={user_code}
3. 手机迅雷 App 扫该二维码 → App 内确认授权
4. 轮询 POST https://xluser-ssl.xunlei.com/v1/auth/token
   grant_type=urn:ietf:params:oauth:grant-type:device_code（Client.startWatch）
```
注：浏览器直接打开 yc 页会显示"远程设备登录"页但报"授权已过期"
（实测×2，含零秒打开场景）——该页设计给 App 扫码，非浏览器确认。

### 2.2 api-pan 请求规则（实测）
- 三件套：`Authorization: Bearer <jwt>` + `x-client-id` + `x-device-id`(32hex)
- `x-captcha-token` 必带（缺省 400 captcha_token is empty）
- captcha 经 `POST xluser-ssl/v1/shield/captcha/init` 签发，签发时绑定 client_id
- api-pan 反查 captcha 上下文按白名单过滤：

| captcha 签发 client | api-pan 结果 |
|---|---|
| XW5SkOhLDjnOZP7J（登录页） | ❌ no client info found |
| XW-G4v1H72tgfJym（桌面/xllite） | ❌ no client info found |
| **白名单内（待测：Xqp0/X9ib/XVJV/Yd0*）** | 未测 ← 当前唯一缺口 |

### 2.3 client 白名单（api-pan 接受域，实测+路由表交叉确认）
`X9ibISwpIp8jQ4Ya` / `XVJVzaJv8vKHzVCk` / `Xqp0kJBXWhwaTpB6`(web pan) /
`Yd0*GrNJhCC2oX` 系列(电视盒)。不在册：XW5Sk、XW-G4。
设备码端点对白名单 client 发起【已验证】：Xqp0 发起成功返回四元组
（2026-08-25 实测，scope="user pan offline" 可接受）。

### 2.4 凭证存储位置（候选，标注证据等级）
| 位置 | 内容 | 等级 |
|---|---|---|
| `%APPDATA%\thunder\Cache\Cache_Data\data_1` | JWT 明文（HTTP 缓存），可提取，当前有 4 个未过期 XW-G4 票 | 实测✅ |
| `.drive\{cc7bb…,6c8497…}` Storm KV 库 | 会话状态（推断含 token）；共享读可复制，内容加密（0 明文命中）。日志泄露 `coreEncryptKey:eb5aa306672cab6116b3843eea276a71` | 复制实测✅/内容推断 |
| 活体 xllite 进程内存 | 运行时 token | 未验证 |

## 3. 我们侧登录代码地图
- `crates/provider/src/xunlei/client.rs`
  - :12 `DEVICE_CLIENT_ID = "XW5SkOhLDjnOZP7J"` ← 需改为白名单 client
  - :373-385 request_device_code（form 提交 scope+client_id；QR 渲染用服务端
    verification 字段 ← 需改为 §2.1 的本地 yc 模板构造）
  - :401 附近 token 轮询；:474 起 SMS 全套；signin/captcha/sign 各函数
- examples：`xunlei_qr_login.rs`（终端二维码渲染，8/22 扫码成功即此例）、
  `xunlei_desktop_probe.rs`（缓存票测试器）、`xunlei_sms_login.rs`

## 4. 已证伪路径（全部废弃，禁止再试）
| 路径 | 结果 |
|---|---|
| `xluser-ssl/__/auth/device/?...`（服务端返回的 verification 页） | nginx 404（页面不存在；官方客户端从不使用它） |
| `pan.xunlei.com/act-13158170504565/device?...`（第三方推导） | 空页/回落网盘首页 |
| `xluser-ssl/api/v1/reurl?action=scan`（short_uri_complete） | 404 |
| 浏览器直接确认 `pan.xunlei.com/yc/?...user_code=` | 结构可达但报"授权已过期"→ 该页面向 App 扫码设计 |
| 密码 signin | result:review 滑块风控 |
| DownloadSDK DLL 登录 | 无账号能力（导出表普查否定） |
| xllite 静态提取 secret | 多代理会战结构性不可行 |

## 5. 前进路径（唯一，按反编译配方执行）
改造 `xunlei_qr_login.rs`：
1. `DEVICE_CLIENT_ID` → `Xqp0kJBXWhwaTpB6`（或备选 X9ibISwpIp8jQ4Ya）
2. QR 内容不再用服务端 verification 字段，改按 §2.1 模板本地构造：
   `https://pan.xunlei.com/yc/?client_id=Xqp0kJBXWhwaTpB6&user_code={uc}`
   （先最小参数集；若 App 不认再逐步加 platform/privilege 参数）
3. 用户手机迅雷 App 扫码确认（8/22 同模式已成功过）
4. 拿到 aud=Xqp0 票后立即：captcha/init(Xqp0) → list_files 三件套同源验证
5. 若 Xqp0 被拒 → 换 X9ib 重跑 §5.1-4

## 6. 云 AI 任务书
1. 审阅 §2/§4/§5，指出遗漏或给出替代解读（须引用本文档编号）
2. 产出：`request_device_code` 与 QR 构造的具体 Rust 补丁建议（对照 client.rs 行号）
3. 若 §5 步骤 4 失败：根据回传错误体推演（对照 §2.2 规则表），给出下一假设
4. β 储备方案（仅在 α 全线失败后启用）：Storm KV 解密器 Python 脚本
   （boltdb 变体解析 + coreEncryptKey 尝试 AES 系列；先只回传键名清单不回传值）

## 7. 敏感信息约定
access_token/refresh_token 完整值只存本地 json；文档/回传仅允许
前 12 字符 + 长度 + exp。JWT payload（aud/scope/exp）可全量记录。

## 8. 云 AI 第一轮交付归档与对账（2026-08-25）
- 归档：`scripts/research/cloud_delivery/sdk_login_static/`
  （LOGIN_SOURCE_ANALYSIS.md、LOGIN_STATIC_ANALYSIS_FINAL.md、DownloadSDKServer_DECOMPILED.c 16539 行）
- 结论与我方地图一致：安装包 SDK（30 个 DLL/EXE）**零登录代码**（client_id/captcha/OAuth/
  PlatformConfig/APPKey 全 0 命中）；登录唯一在 xllite.exe；SDK 只消费 token
  （XL_SetAccelerateToken/XLSetEquityToken/XLSetTaskToken/XL_SetTokenMode 注入面，
  与本仓 xunlei-ffi/src/identity.rs 已封装的三个 setter 对应）
- 新增可用事实：`DownloadSDKServer_DECOMPILED.c` 可作 PHub/SHub token 消费侧参考（P2P 线，D28 排除内）
- 对凭证提取无增量——α（Xqp0 设备码）路线不受影响，仍是首选
