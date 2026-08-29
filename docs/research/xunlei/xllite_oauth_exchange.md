# xllite.exe 第二段令牌交换链路还原报告

> 目标：从 `C:\Program Files\Thunder Network\Thunder\program\xllite.exe`（50MB Go 二进制，UTF-8 明文字符串）还原「第二段令牌交换」流程。
> 锚点来源：扫码登录拿到的 `access_token` 绑定登录页 client `XW5SkOhLDjnOZP7J`，调 `api-pan` 报 `no client info found`；网页版真实流程存在第二段——浏览器带授权码跳转 pan 域换 `Xqp0` 票。
> 范围声明：**本任务专属 OAuth/SSO 交换链路，未触碰 `platformdetect.PlatformConfig.GetClientSecret` 凭证表锚点（由另一代理负责）。**

---

## 0. 一句话结论（先看）

**xllite.exe 内不存在任何「用 XW5Sk 会话/refresh_token 去换其他 client 的 pan 票」的可调用 JSON 端点。** 所谓「第二段令牌交换」在 xllite 里并不以独立的 server-to-server token 交换形式存在：

- xllite 的 Xunlei 登录是 **OAuth2 设备码授权流（device_code grant）**，常量串 `urn:ietf:params:oauth:grant-type:device_code` 即为其 grant type；
- 浏览器里看到的 `pan.xunlei.com/yc/oauth-callback` 跳转，实际上是 **阿里云盘（Aliyundrive）OAuth** 的回调（`client_id=947331beffa84e718adbd66b1732e748`），xllite 通过 `xluser-ssl.xunlei.com/proxy/aliyundrive/oauth/access_token` 做反向代理；
- `Xqp0kJBXWhwaTpB6` 是 **`x-client-id` 路由表里的客户端 ID（h5/套件端生态版），不是票名**；
- 没有 `token-exchange`、`client_credentials`、`/oauth/token`、`authorization_code` 兑换端点、也没有 `sso_ticket`/`credentials_` 字段——**因此按任务规则 5，不改动任何 Rust 代码。**

> 结论分级：**B 级（仅有片段/推测，代码即真相但链路不完整）**。构成不了「端点+参数+调用顺序」完整链路的 A 级证据，因为第二段在 xllite 内根本不是 server 端点，而是前端(webview)跳转 + 网关代理。

---

## 1. 流程图（文本版）

```
                    ┌─────────────────────────────────────────────────────────┐
                    │  xllite.exe 内的「OAuth 交换」相关事实（来自静态 dump）   │
                    └─────────────────────────────────────────────────────────┘

[A] Xunlei 本域登录（设备码授权流 —— 唯一真实 grant）
    ──────────────────────────────────────────────────────────
    getAuthorize()  (webview 前端, 偏移 0x01943FAF)
        │
        ▼
    /v1/auth/device/code/        (登录页路径, 偏移 0x015E9BB5/0x015E9C71)
        │  grant_type = urn:ietf:params:oauth:grant-type:device_code   (0x01635E38)
        ▼
    返回 { device_code, user_code, verification_uri_complete }  (struct 0x014E5F5B/0x0152798C)
        │
        ▼
    轮询/用户扫码确认
        │
        ▼
    本地存储: key = "xllite:access_token" , "xllite:token_secret"   (0x015E96A1/0x015E96B4)
        │
        ▼
    后续请求头带 Bearer(access_token) → 走各业务网关
        (xluser-ssl / api-pan / speedup / pan.xunlei.com ...)

[B] 阿里云盘挂载（网页端 OAuth 跳转 —— 即「yc/oauth」/「o/oauth/authorize」真身）
    ──────────────────────────────────────────────────────────
    webview 内 JS (Vue, 偏移 0x01C58952 / 0x01E6897D / 0x01BC42EA):
      client_id = 947331beffa84e718adbd66b1732e748   (固定, 阿里云盘)
      redirect_uri = encodeURIComponent("https://pan.xunlei.com/yc/oauth-callback?broadcast=1")
      scope = "user:base,file:all:read"
      response_type = code
      authorize_url = https://www.aliyundrive.com/o/oauth/authorize?...
        │
        ▼ (浏览器重定向, 用户授权)
    pan.xunlei.com/yc/oauth-callback?broadcast=1   (回调落地 xllite 的 pan 域 webview)
        │
        ▼
    xllite 反向代理兑换:
      POST https://xluser-ssl.xunlei.com/proxy/aliyundrive/oauth/access_token   (0x015E9BB5 附近 / 0x0165B57E)
      GET  https://xluser-ssl.xunlei.com/proxy/aliyundrive/oauth/users/info      (0x01396E41)
        │
        ▼
    阿里云盘 token 仅用于「挂载阿里云盘」文件源, 与 Xunlei 本域 access_token 是两条独立链路

[C] 客户端路由表（x-client-id 分段返回配置, 偏移 0x016D5CC3 起）
    ──────────────────────────────────────────────────────────
    config JSON 按 x-client-id 分组返回不同「个人影院 / 下载列表」展示配置:
      - X9ibISwpIp8jQ4Ya, XW-G4v1H72tgfJym  → pc / 套件端生态版
      - XoL5lqbDWNW0e7QA, Xp6vsxz_7IYVw2BB, Yd0uSVGrNJhCC2oE,
        Yd00NFGrNJhCC2oP, Yd0zTVGrNJhCC2oL, Xqp0kJBXWhwaTpB6,
        Yd0zylGrNJhCC2oN, Yd0yklGrNJhCC2oH, Yd0y91GrNJhCC2oJ, Yd00e1GrNJhCC2oR
        → h5 / 套件端生态版
    注意: Xqp0kJBXWhwaTpB6 出现于此处, 是【客户端 ID】, 不是【票名/token】。
          XW5SkOhLDjnOZP7J 不在此路由表中(仅出现一次于标点表, 见证据表)。

[D] 扫码/设备码登录页 client = XW5SkOhLDjnOZP7J
    ──────────────────────────────────────────────────────────
    该 client 绑定的 access_token 调 api-pan 报 "no client info found"
      → api-pan 网关按 x-client-id 校验, XW5Sk 不在其白名单(白名单见 [C] 列表 + X9ib/XVJV)
      → 所以「扫码票能否换 pan 票」答案: 不能仅靠 XW5Sk token 直接调 api-pan;
        需要的是 [C] 中某个已登记 client(如 X9ibISwpIp8jQ4Ya / XVJVzaJv8vKHzVCk / 各 Yd0* / Xqp0*) 的票据或 device_code 登录态。
```

---

## 2. 证据表（偏移 | 片段 | 等级）

| 偏移 (hex) | 片段（去噪） | 等级 | 说明 |
|---|---|---|---|
| `0x01635E38` | `urn:ietf:params:oauth:grant-type:device_code` | **A·dump推断** | xllite 真实使用的 OAuth grant type = 设备码流 |
| `0x014E5F5B` `0x0152798C` `0x02620977` | `struct { DeviceCode; VerificationURL(verification_uri_complete); UserCode }` | **A·dump推断** | 设备码授权响应结构体（前端+后端一致） |
| `0x015E9BB5` `0x015E9C71` | `/v1/auth/device/code/` , `/auth/qrlogin/status` , `/device/code/:status` | **A·dump推断** | 设备码/扫码登录的路由路径 |
| `0x015E96A1` `0x015E96B4` | `xllite:access_token` , `xllite:token_secret` | **A·dump推断** | 本地令牌存储键（设备码流产出） |
| `0x01C58952` `0x01E6897D` | `https://www.aliyundrive.com/o/oauth/authorize?client_id=947331beffa84e718adbd66b1732e748&redirect_uri=...pan.xunlei.com/yc/oauth-callback?broadcast=1&scope=user:base,file:all:read&response_type=code` | **A·dump推断** | 「yc/oauth」「o/oauth/authorize」真身 = 阿里云盘 OAuth，非 Xunlei pan 第二段 |
| `0x0165B57E` `0x01396E41` | `https://xluser-ssl.xunlei.com/proxy/aliyundrive/oauth/access_token` , `.../proxy/aliyundrive/oauth/users/info` | **A·dump推断** | 阿里云盘 token 兑换由 xllite 网关反向代理完成 |
| `0x016D5CC3` `0x016D6251` `0x016D658D` `0x016D6A1A` | `x-client-id in [XoL5..., Xp6v..., Yd0u..., Yd00..., Yd0z..., Xqp0kJBXWhwaTpB6, ...]` 及 `x-client-id in [X9ibISwpIp8jQ4Ya, XW-G4v1H72tgfJym]` | **A·dump推断** | 客户端路由表；Xqp0 是 client id，非票 |
| `0x015DBDFB` | `...X9ibISwpIp8jQ4Ya...XVJVzaJv8vKHzVCk...XW-G4v1H72tgfJym...XW5SkOhLDjnOZP7J...XXX_unrecognized...` | **B** | 标点/关键字表，XW5Sk 仅此处出现一次，**不在路由表/不在 API 路径**，属登录页 client |
| `0x013A7AAD` `0x013A7AC2` | `ClientSecret json:"client_secret"` | **B** | 结构体字段（credential 结构），非调用点 |
| `0x012FE94B` `0x013804A5` | `RefreshToken json:"refresh_token"` | **B** | 结构体字段，无对应 refresh 兑换端点证据 |
| `0x012E730C` `0x0135E2D5` `0x014359E5` | `ClientID json:"client_id"` / `ClientIdU json:"client_id"` | **B** | 请求/响应结构体字段 |
| `0x013B4935` | `id_token_signing_alg_values_supported` | **B** | OIDC 元数据字段，无对应端点 |
| `0x015C97A0` | `x-client-id` `x-device-id` `x-lml/...` 头名 | **B** | 请求头名表 |
| `0x016D5969` `0x016D6F5A` | `api-pan.xunlei.com` 出现在 `custom_host_to_ip` 与带 `withBearerSessionID/withUserID` 标签的 URL 白名单 | **A·dump推断** | api-pan 是 Bearer 鉴权的业务网关，按 x-client-id 校验 → 解释 "no client info found" |
| `0x02726B55` `0x025F42A2` | `golang.org/x/oauth2` , `/pkg/oauth2client` , `gitlab.xunlei.cn/xlppc/pan-cli/pkg/oauth2client` | **B（非端点）** | Go 库符号/包路径，非运行时端点 |
| `0x015E3CFD` `0x015FFF3E` | `api-pan.xunlei.com` 裸 host 字符串 | **B** | host 出现但无完整交换路径 |

### 关键「反向证据」（证明第二段 JSON 交换不存在）
| 探针 | 命中 | 含义 |
|---|---|---|
| `token-exchange` | 0 | 无 OAuth token-exchange 扩展端点 |
| `client_credentials` | 0 | 无客户端凭证 grant |
| `/oauth/token` | 0 | 无统一 token 端点 |
| `xluser-ssl.xunlei.com/oauth` | 0 | 反向代理只代理 `aliyundrive/oauth/*`，无本域 oauth 兑换 |
| `pan.xunlei.com/oauth` | 0 | pan 域无 oauth 端点 |
| `Authorization: Bearer` | 0 | 字面量未出现（鉴权经由网关标签 `withBearerSessionID`，非代码常量） |
| `sso_ticket` / `credentials_` / `code_link` / `authorizePage` | 0 | 关联词全 0 命中 |

---

## 3. 结论：扫码票可否经此换取 pan 票？需要什么前提？

**短答：不能「直接换」。xllite 内没有把 XW5Sk 登录态 exchange 成 pan 票的 server 端点。**

1. **XW5Sk 是登录页/扫码 client**，其 `access_token` 调 `api-pan` 报 `no client info found`，因为 `api-pan` 网关按 `x-client-id` 白名单校验，而 XW5Sk 不在白名单（白名单见证据表 [C]：X9ibISwpIp8jQ4Ya、XVJVzaJv8vKHzVCk、Xqp0kJBXWhwaTpB6、各 Yd0* 等）。
2. **真正的「第二段」是阿里云盘 OAuth 跳转**（`/yc/oauth-callback` + `aliyundrive.com/o/oauth/authorize`），由 xllite 的 `xluser-ssl` 反向代理完成 `access_token` 兑换——这用于「挂载阿里云盘」文件源，**与 Xunlei 本域 pan 票是两回事**。
3. **若要让 api-pan 放行，前提不是「换票」，而是用已登记 client 的登录态**：
   - 要么走设备码授权流（`device_code` grant）以某个白名单 client 完成登录，拿到该 client 的 `access_token`；
   - 要么复用已在路由表中的 client（如 `X9ibISwpIp8jQ4Ya` / `XVJVzaJv8vKHzVCk` / `Xqp0kJBXWhwaTpB6`）对应的会话。
   - 注意：任务背景里另有一代理在挖 `GetClientSecret` 凭证表——那才是这些白名单 client 的 client_secret 来源；本任务未触及。

> ⚠️ 遗留：xllite 是**打包了前端 webview 的桌面客户端**，大量「第二段」逻辑存在于网页端（远程 `pan.xunlei.com` 的 JS），并不在本地二进制里。本静态考古只覆盖 xllite.exe 内的 UTF-8 明文，因此「浏览器带授权码跳转 pan 域换 Xqp0 票」的完整实现需对线上 `pan.xunlei.com` 网页/JS 做动态抓包，本地二进制无法给出 A 级完整链路。

---

## 4. Rust 代码改动

**未改动。** 按任务规则 5：未发现可直接调用的 JSON 交换端点（无 token-exchange / 无 /oauth/token / 无授权码兑换），故 **不** 在 `crates/provider/src/xunlei/client.rs` 追加任何方法。保持 `cargo test / cargo check` 现状不动。

---

## 附录：检索方法
- 工具：`scripts/dig_xllite_oauth.py`（主锚点 ±400/±200）、`dig_pass2.py`（全部 URL / host 频率 / XW5Sk / 参数字段）、`dig_pass3.py`（device_code 流 / xllite:token 键 / Xqp0 / xluser-ssl URL / login 路径）、`dig_pass4.py`（反向探针 token-exchange/client_credentials/...）。
- 实现：一次性 `open(BIN,'rb').read()` 全量读入，UTF-8 `errors='replace'` 解码后多次 `re.finditer`，避免 PowerShell 正则硬跑 50MB。
- 原始 dump：`scripts/oauth_dump.txt`、`oauth_pass2.txt`、`oauth_pass3.txt`、`oauth_pass4.txt`。
