# 迅雷离线下载 RemoteProvider 调研报告

> 面向 Rust + libtorrent FFI + 自研 HTTP/FTP 引擎 + RemoteProvider 云兜底层的代码级设计文档
> 调研对象：迅雷云盘离线下载（pan.xunlei.com 系）为主，迅雷远程下载/NAS（xware/pan-xunlei-com CGI）为辅
> 参考实现：alist `drivers/thunder/`（Go，最完整）、`lalaking666/pyxunlei`（Python，NAS CGI）、`iambus/xunlei-lixian`（Python，旧版 lixian）、`filecxx/FileCentipede`（thunder:// 解析）

---

## 结论速览（TL;DR）

1. **用户"无登录即可调用"的判断与事实不符**。迅雷所有"云离线 / 云盘取回直链"路径（api-pan.xunlei.com）都要求一个**迅雷账号**的 access_token。不存在"提交磁力直接返回直链"的公开匿名 API。
2. 用户大概率把两件事混淆了：
   - **(c) `thunder://` 是纯客户端可解析的 URL 包装**（base64(`AA`+真实URL+`ZZ`)），不需要任何服务端；
   - **(d) 需要 device-id + 一个"免费(非VIP)迅雷账号"**，而不是 VIP。免费账号即可走完整"云离线→直链"流程，但有限额。
3. **实际可用路径**：免费注册一个迅雷账号 → 用 device_id + 账号密码登录拿 access_token → 提交磁力/ed2k/http 到云盘离线 → 轮询任务完成 → 拿 `web_content_link` CDN 直链 → HttpEngine 多线程下载。**这条路全程不需要 VIP**。
4. 风险点：这是**逆向接口**，签名 `Algorithms` 数组随 App 版本更新会变；免费账号"云添加"每日 3 次、高速流量每日约 2GB；频繁换设备/异地登录会触发 `review_panel` 短信验证。

---

## A. "无登录路径"真伪验证

逐条核对用户给出的四种可能：

| 选项 | 结论 | 依据 |
|------|------|------|
| (a) 公开匿名 API，提交磁力返回直链 | **不存在** | alist `Thunder` / `ThunderExpert` 的 `Init()` 必走 `Login()` 或 `RefreshToken()`；`XunLeiCommon.Request` 每个请求都带 `Authorization: Bearer <access_token>` 与 `X-Captcha-Token`。无 token 必然 401/403。 |
| (b) 热门资源"公共缓存"接口 | **部分存在但需登录** | 迅雷云盘有"秒存"机制：若该 infohash/GCID 已被任意用户下过，提交后几乎瞬间 `PHASE_TYPE_COMPLETE`。但"提交"动作本身仍需 access_token。 |
| (c) `thunder://` 解析后是普通 HTTP | **正确，但只是 URL 包装** | 见下文 D 节，纯客户端 base64 解码，无服务端依赖。 |
| (d) 需特殊 device-id/cookie，但不需要 VIP | **正确，这是真正的可用路径** | 需要 `device_id`（客户端 MD5 生成即可）+ **免费迅雷账号**的 access_token。device_id 不需要调 device-register 接口，客户端本地生成 32 位 hex 即可。 |

### "无登录"到底指什么 —— 调研结论

**用户的判断有误**：迅雷云离线下载**必须有一个迅雷账号**（免费即可，无需 VIP）。证据链：

- `pan.xunlei.com` 首页只有"手机验证登录 / 账号密码登录 / 立即注册"，无"游客"入口；
- alist `drivers/thunder/driver.go` 的 `Login()` 必须先 `CoreLogin(user,pass)` 拿 sessionID，再 `auth/signin/token` 换 access_token；
- `pyxunlei` 连接 NAS 服务时，若设备未绑定迅雷账号，`/drive/v1/tasks?type=user#runner` 直接返回 HTTP 500 → 抛 `NotLoginXunLeiAccount`；
- 新浪众测对"玩物下载"的记录：旧版"知晓地址端口即可用任意账户登录"是**安全漏洞**而非匿名能力，新版已修复为必须绑定账号。

### device-id 是否需要 device-register 接口？

**不需要**。alist 代码里 `DeviceID` 是这样来的：

```go
DeviceID: func() string {
    if len(x.DeviceID) != 32 {
        return utils.GetMD5EncodeStr(x.DeviceID) // 任意字符串 MD5 成 32 hex
    }
    return x.DeviceID
}()
```

即：随便给一个种子字符串（或直接随机 32 hex），就是合法 device_id。它只是参与 `captcha_sign` 签名与请求头 `x-device-id`，**不是服务端签发的**。device_id 与账号绑定发生在登录时，服务端记录"这个 device_id 登录过这个账号"。

> 注意：device_id 一旦登录成功，**不要频繁更换**，否则容易触发风控 `review_panel`。alist 文档原话："触发登录验证后，不要频繁更换设备ID，否则可能导致验证结果无法复用。"

---

## B. API 端点 / 协议形态

下面所有端点都来自 alist `drivers/thunder/util.go` + `driver.go` 的实测代码。两条域名：

- **鉴权域** `XLUSER_API_BASE_URL = https://xluser-ssl.xunlei.com`（路径前缀 `/v1`）
- **云盘域** `API_URL = https://api-pan.xunlei.com/drive/v1`

> 注：文档里也出现过 `https://x-api-pan.xunlei.com`，与 `api-pan.xunlei.com` 是同一组接口的两个 host，行为一致；alist 用的是 `api-pan.xunlei.com`。

### B.0 公共请求头（每个业务请求都要带）

```
user-agent: ANDROID-com.xunlei.downloadprovider/8.31.0.9726 netWorkType/5G appid/40 deviceName/Xiaomi_M2004j7ac deviceModel/M2004J7AC OSVersion/12 protocolVersion/301 platformVersion/10 sdkVersion/512000 Oauth2Client/0.9 (Linux 4_14_186-perf-gddfs8vbb238b) (JAVA 0)
accept: application/json;charset=UTF-8
x-device-id:   <32位hex device_id>
x-client-id:   Xp6vsxz_7IYVw2BB
x-client-version: 8.31.0.9726
Authorization:  Bearer <access_token>
X-Captcha-Token: <captcha_token>
```

`client_id` / `client_secret` 是从安卓 App 逆向出来的固定值（见 E 节）。

### B.1 设备签名与验证码签名（两个不同算法）

**(1) `captcha_sign`（每次请求前刷新 captcha_token 时用）**——`Common.GetCaptchaSign()`：

```
timestamp = 当前毫秒
str = ClientID + ClientVersion + PackageName + DeviceID + timestamp
for algo in Algorithms[]:               # 10 个固定盐，逐轮 md5
    str = md5(str + algo)
captcha_sign = "1." + str
```

`Algorithms[]` 是 10 个硬编码盐（来自 App 逆向），见 driver.go 第 48-59 行。**这是会随 App 大版本更新而变的脆弱点**。

**(2) `device_sign`（仅 core login 与 review 验证时用）**——`generateDeviceSign()`：

```
base = DeviceID + PackageName + APPID + APPKey
        # APPID="40", APPKey="34a062aaa22f906fca4fefe9fb3a3021"
sha1_hex = sha1(base)           # hex
md5_hex  = md5(sha1_hex)        # hex
device_sign = "div101." + DeviceID + md5_hex
```

### B.2 登录链路（4 步，拿到 access_token + refresh_token）

#### Step 1 — core login（拿 sessionID）
```bash
POST https://xluser-ssl.xunlei.com/xluser.core.login/v3/login
Header: User-Agent: android-ok-http-client/xl-acc-sdk/version-5.0.12.512000
Body:
{
  "protocolVersion":"301","sequenceNo":"1000012","platformVersion":"10",
  "isCompressed":"0","appid":"40","clientVersion":"8.31.0.9726",
  "peerID":"00000000000000000000000000000000",
  "appName":"ANDROID-com.xunlei.downloadprovider","sdkVersion":"512000",
  "devicesign":"div101.<deviceID><md5hex>",   # 见 B.1(2)
  "netWorkType":"WIFI","providerName":"NONE",
  "deviceModel":"M2004J7AC","deviceName":"Xiaomi_M2004j7ac","OSVersion":"12",
  "creditkey":"",          # 信任密钥，首次为空
  "hl":"zh-CN",
  "userName":"<手机号/邮箱>","passWord":"<明文密码>",
  "verifyKey":"","verifyCode":"","isMd5Pwd":"0"
}
# 返回 CoreLoginResp，关键字段: sessionID, userID, creditkey, isNewNo...
```

#### Step 2 — 刷新 captcha_token（登录态前）
```bash
POST https://xluser-ssl.xunlei.com/v1/shield/captcha/init
Body:
{
  "action":"POST:/v1/auth/signin/token",   # GetAction(method, urlpath)
  "captcha_token":"",                       # 首次空
  "client_id":"Xp6vsxz_7IYVw2BB",
  "device_id":"<deviceID>",
  "meta": { "phone_number":"13800138000" }, # 登录前用 RefreshCaptchaTokenInLogin
  "redirect_uri":"xlaccsdk01://xunlei.com/callback?state=harbor"
}
# 返回 { "captcha_token":"...", "expires_in":3600, "url":"" }
# 若 url 非空 → 触发验证(滑块/短信)，需要人工过
```

> 登录后刷新用 `RefreshCaptchaTokenAtLogin`，meta 里是 `client_version`/`package_name`/`user_id` + `timestamp`/`captcha_sign`。

#### Step 3 — signin token（换 access_token）
```bash
POST https://xluser-ssl.xunlei.com/v1/auth/signin/token
Path-Param: client_id=Xp6vsxz_7IYVw2BB
Header: X-Captcha-Token: <step2 的 token>
Body:
{
  "client_id":"Xp6vsxz_7IYVw2BB",
  "client_secret":"<redacted>",
  "provider":"access_end_point_token",     # SignProvider 常量
  "signin_token":"<step1 的 sessionID>"
}
# 返回 TokenResp:
# { token_type:"Bearer", access_token:"...", refresh_token:"...",
#   expires_in:7200, sub:"...", user_id:"..." }
```

#### Step 4 — refresh token（后续续期，不必再走密码）
```bash
POST https://xluser-ssl.xunlei.com/v1/auth/token
Body:
{ "grant_type":"refresh_token", "refresh_token":"<rt>",
  "client_id":"Xp6vsxz_7IYVw2BB", "client_secret":"<redacted>" }
# 返回新的 TokenResp（服务端可能不轮换 refresh_token，此时保留旧 rt）
```

#### 登录态自检
```bash
GET https://xluser-ssl.xunlei.com/v1/user/me
# 200 + 无 error_code 即登录有效
```

### B.3 submit() —— 提交离线下载任务（核心）

```bash
POST https://api-pan.xunlei.com/drive/v1/files
Header: Authorization / X-Captcha-Token / x-device-id ...（见 B.0）
Body:
{
  "kind":"drive#file",
  "name":"<期望文件名，可空或由服务端定>",
  "parent_id":"<云盘目标目录ID，根目录用空字符串>",
  "upload_type":"UPLOAD_TYPE_URL",
  "url": { "url": "<magnet:?xt=... 或 ed2k:// 或 thunder:// 或 http(s)://>" }
}
# 返回 OfflineDownloadResp:
# { "file": "<fileID 或 null>", "task": OfflineTask, "upload_type":"...", "url":{...} }
```

**关键事实**：`url.url` 字段同时接受 magnet / ed2k / thunder:// / http(s) — 这是"云离线"入口。thunder:// 会被服务端再解一层。提交后服务端创建一个 `OfflineTask`（`type=user#download-url`）并在后台用迅雷 P2SP 网络抓取，落盘到你的云盘。

### B.4 status() —— 轮询任务状态

```bash
GET https://api-pan.xunlei.com/drive/v1/tasks?type=offline&limit=10000&page_token=
# 返回 OfflineListResp: { expires_in, next_page_token, tasks:[OfflineTask,...] }
```

`OfflineTask.phase` 枚举（来自 types.go 注释）：

| phase | 含义 |
|-------|------|
| `PHASE_TYPE_PENDING` | 排队中 |
| `PHASE_TYPE_RUNNING` | 下载中（`progress` 0-100） |
| `PHASE_TYPE_ERROR` | 失败（看 `message` / `statuses[]`） |
| `PHASE_TYPE_COMPLETE` | 完成，可取直链 |
| `PHASE_TYPE_PAUSED` | 暂停 |

任务完成度还可看 `file_size` / `status_size`。`file_id` 字段指向云盘里的文件对象。

### B.5 resolve() —— 取直链

```bash
GET https://api-pan.xunlei.com/drive/v1/files/{fileID}
# 返回 Files 对象，关键字段:
#   web_content_link  : 通用下载直链（CDN，带 token 与 expire）
#   medias[].link.url : 转码视频直链（UseVideoUrl 时优先用这个）
#   hash              : GCID（迅雷自有内容指纹）
```

alist `Link()` 逻辑：优先 `web_content_link`；若 `UseVideoUrl=true` 则取第一个非空 `medias[].link.url`。返回时强制带 header `User-Agent: Dalvik/2.1.0 (Linux; U; Android 12; M2004J7AC Build/SP1A.210812.016)`（即 `DownloadUserAgent`）。

直链特征（实测/社区共识）：
- 形如 `https://xxx-pan-ssl.xunlei.com/...?...&e=<unix秒>`，`e` 是过期时间戳，**约 24h 有效**；
- 是 CDN，**支持 Range / 多线程**（HttpEngine 可放心并发分片）；
- **必须带 `DownloadUserAgent`（安卓 UA）下载**，否则可能 403；建议同时带 `Referer: https://pan.xunlei.com/` 更稳。

### B.6 remove() —— 删除任务/文件

```bash
# 删离线任务（可同时删云盘文件）
DELETE https://api-pan.xunlei.com/drive/v1/tasks?task_ids=<id1,id2>&delete_files=true

# 删云盘文件（移到回收站）
PATCH https://api-pan.xunlei.com/drive/v1/files/{fileID}/trash
Body: {}
```

### B.7 （补充）.torrent 文件提交

云盘离线**也支持种子文件**，但走的不是 `url` 字段，而是**先上传种子文件到云盘再用文件创建任务**，或更常见的：本地把 `.torrent` 转成 magnet（`magnet:?xt=urn:btih:<infohash>`）后走 B.3。alist 没实现种子直传，`pyxunlei` 的 `download_torrent` 也是先 `_torrent2magnet` 再提交。

### B.8 NAS / 远程下载路径（pan-xunlei-com CGI，仅作对比）

这是 cnk3x/xunlei（群晖套件提取）暴露的本地 HTTP API，`pyxunlei` 调用它：

```
BASE: http://<nas-host>:<port>/webman/3rdparty/pan-xunlei-com/index.cgi/drive/v1
Header: pan-auth: <从 web 页 JS uiauth() 提取的本地 JWT>   # 本地鉴权，非迅雷账号
        device-space: ""
```

- 提交磁力：`POST /resource/list` body `{"urls":"<magnet>"}` → 解析出文件列表；再 `POST /task` body `{type:"user#download-url", params:{target:device_id, url:magnet, ...}}`；
- **文件下载到 NAS 本地磁盘**，不返回云直链。

这条路 **也需要 NAS 上绑定迅雷账号**（未绑定时 `/tasks?type=user#runner` 返回 500）。它**不适合**作为"云兜底返回直链"的 Provider，只适合"把迅雷引擎当本地下载器"的场景。本报告后续以 **B.1–B.6 的 api-pan.xunlei.com 路径**为设计基准。

---

## C. 风控 / 限流 / 失败模式

### C.1 限流（免费非 VIP 账号实测，2026 年口径）

| 维度 | 限制 |
|------|------|
| 云添加（离线下载提交） | **每日 3 次**（zhihu 迅雷17尝鲜版实测） |
| 高速下载流量 | 每日约 **2GB**，超出后限速 ~150KB/s |
| 初始云盘高速流量 | 2026-06-23 后新注册用户**不再赠送**（IT之家） |
| 云盘空间 | 免费约 2TB（足够离线中转） |
| 同设备并发任务 | 无硬性并发数文档，但提交过快会被 captcha 拦 |

> 重要：每日 3 次云添加是免费账号的硬约束。对"云兜底"场景意味着**一天最多兜底 3 个死种**，需在 Provider 层做配额管理与优先级排队。

### C.2 错误码（来自 alist `XunLeiCommon.Request` 的 switch）

| error_code | 含义 | alist 处理 | 建议重试 |
|-----------|------|-----------|---------|
| `0` | 成功 | 直接返回 | — |
| `9` | **captcha_token 过期** | 调 `RefreshCaptchaTokenAtLogin` 重取后再重试一次 | 可重试（自动） |
| `10` / `16` | token 类错误 | 调 `refreshTokenFunc` 刷新 access_token 后重试 | 可重试（自动） |
| `4121` / `4122` | **access_token 过期/失效** | 同上，刷 refresh_token | 可重试（自动） |
| `4001` | `invalid_grant`（refresh_token 也失效） | 回退到账号密码重新 Login | 可重试（需重新登录） |
| `4002` | `captcha_invalid`（meta 格式错，如 username 期望手机号却传了别的东西） | 报错 | **不可重试**，需修参数 |
| `review_panel`（ErrorMsg） | **触发风控验证**（短信/滑块） | 返回带 `creditkey` + `reviewurl` 的结构化错误，需人工 | **不可自动重试**，需人工介入或换号 |
| HTTP 403 | pan-auth / 设备未授权 | 报错 | 不可重试，检查 device_id / 账号绑定 |

### C.3 触发"需要登录/验证"的场景

1. **异地登录**：device_id 与上次登录 IP 地区差异大 → `review_panel`；
2. **频繁换 device_id**：同一账号短时间换多个 device_id → 触发验证；
3. **高频提交**：短时间连续云添加 → captcha 拦截甚至临时封 device；
4. **密码错误多次** → 锁账号需短信解锁。

### C.4 资源大小行为差异

- **100MB 资源**：若 GCID/infohash 命中"秒存"（已被任意用户下过），几乎瞬时 `PHASE_TYPE_COMPLETE`；否则走 P2SP，速度取决于热度。
- **4GB 资源**：同上命中即秒存；未命中则受"高速下载流量 2GB/日"限制——**离线到云盘不消耗你的高速流量**（那是下载阶段才计），但取回本地时会卡在 2GB 后限速。
- 失败资源（完全死种）：任务会卡在 `PHASE_TYPE_RUNNING` 进度 0 或转 `PHASE_TYPE_ERROR`，`message` 含原因（如"资源不存在"/"版权限制"）。

---

## D. 已知坑 / 兼容性

### D.1 `thunder://` URL Scheme（FileCentipede / 通用）

`thunder://` **不是协议，是包装**：

```
thunder://<Base64("AA" + 真实URL + "ZZ")>
```

解码伪代码：
```
b64 = url.remove_prefix("thunder://")
raw = base64_decode(b64)              # = "AA" + real + "ZZ"
real_url = raw[2:-2]                  # 去掉首 "AA" 末 "ZZ"
# real_url 可能是 magnet:? / ed2k:// / http(s):// / ftp:// / ftps://
```

**纯客户端逻辑**，HttpEngine 入口前预处理即可。FileCentipede 的 `tasks_add_task` 也是先做 scheme 归一化再分派。

### D.2 infohash v1 vs v2

- **v1**（`xt=urn:btih:<40 hex>`）：云离线**完整支持**。
- **v2 / hybrid**（`xt=urn:btmh:<base32>`）：迅雷云盘的 BT 网络仍以 v1 为主，**v2 磁力提交后大概率无法秒存或长时间 0 速**。建议：libtorrent 侧若是 v2/hybrid 种子，优先用种子文件转 v1 infohash 提交，或直接走本地 P2P，不要进云兜底。
-迅雷自有 GCID（SHA1 分块哈希，见 `getGcid()`）与 BT infohash 是两套体系，互不通用。

### D.3 磁力链接里的 tracker / x.dn / x.tr

- 云离线**完全忽略** `&tr=`、`&dn=`、`&xl=` 等参数——它只用 `xt=urn:btih:<hash>` 在迅雷自己的 P2SP 网络里找源。
- 这意味着：**不要期望靠加 tracker 提升云离线成功率**，迅雷用的是自有 DHT + 镜像库。

### D.4 .torrent 文件提交

- `upload_type=UPLOAD_TYPE_URL` 的 `url` 字段**不能直接塞种子文件内容**；
- 要么本地转 magnet（`magnet:?xt=urn:btih:<infohash>`，可附 `&dn=<name>`）后走 B.3；
- 要么先把 `.torrent` 当普通文件上传到云盘（`upload_type=UPLOAD_TYPE_RESUMABLE`，需算 GCID），再基于该文件创建任务——alist 没实现这条，复杂度高，**不推荐**。
- **结论**：Provider 的 submit 入口统一接受"磁力/ed2k/thunder/http(s)"四种字符串，种子文件在 Provider 适配层先转 magnet。

### D.5 直链 CDN 与 Range

- `web_content_link` 是 `*-pan-ssl.xunlei.com` 的 CDN，**支持 Range 请求**，HttpEngine 可多线程分片下载；
- **必须带 `User-Agent: Dalvik/2.1.0 (Linux; U; Android 12; ...)`**（安卓下载 UA），否则可能 403；
- 建议同时带 `Referer: https://pan.xunlei.com/`；
- 直链**约 24h 过期**（`e=` 时间戳），过期需重新 `resolve()` 取新链。

### D.6 地域限制

- `api-pan.xunlei.com` 与 `xluser-ssl.xunlei.com` **主要服务中国大陆 IP**；海外 IP 可能登录被拒或功能受限（部分功能有"国内 IP only"）；
- 若部署在海外节点，需走国内代理出口；建议 Provider 配置项里留 `proxy` 字段。

---

## E. 代码结构建议（Rust）

### E.1 `ThunderProvider` 状态

```rust
pub struct ThunderProvider {
    // —— 静态身份（构造时固定，来自配置）——
    client_id: &'static str,        // "Xp6vsxz_7IYVw2BB"
    client_secret: &'static str,     // "Xp6vsy4tN9toTVdMSpomVdXpRmES"
    client_version: &'static str,   // "8.31.0.9726"
    package_name: &'static str,     // "com.xunlei.downloadprovider"
    app_id: &'static str,            // "40"
    app_key: &'static str,           // "34a062aaa22f906fca4fefe9fb3a3021"
    algorithms: &'static [&'static str],  // 10 个签名盐（见 driver.go）
    user_agent: &'static str,        // 安卓 UA
    download_user_agent: &'static str,    // 下载直链专用 UA

    // —— 账号凭证（来自配置，敏感）——
    username: String,
    password: String,

    // —— 运行期可变状态（需持久化到磁盘/DB）——
    device_id: String,           // 32 hex，本地生成后固定不变
    access_token: Mutex<Option<String>>,
    refresh_token: Mutex<Option<String>>,
    access_expires_at: Mutex<DateTime<Utc>>,
    captcha_token: Mutex<Option<String>>,
    captcha_expires_at: Mutex<DateTime<Utc>>,

    // —— 云盘中转目录 ——
    parent_id: String,           // 离线文件落盘的云盘目录 ID（根目录传 ""）

    // —— 限流 ——
    daily_add_quota: AtomicU32,  // 每日剩余云添加次数（上限 3）
    quota_reset_at: Mutex<DateTime<Utc>>,

    http: reqwest::Client,       // 复用连接池
}
```

> 持久化项：`device_id`、`refresh_token`、`captcha_token`、`parent_id`、`daily_add_quota` 落盘（JSON 或 SQLite）。`access_token` 可只缓存在内存。重启时优先用 `refresh_token` 走 B.2 Step4 恢复登录态，避免每次重启触发风控（alist 的核心改进就是这个）。

### E.2 四个方法映射

```rust
impl RemoteProvider for ThunderProvider {
    /// submit: B.3，提交 magnet/ed2k/thunder/http
    /// 入参先 normalize_thunder() 解掉 thunder:// 包装
    /// 成功返回 task_id（OfflineTask.id）与 file_id
    async fn submit(&self, source: &Source) -> Result<TaskHandle, ProviderError>;

    /// status: B.4，GET /tasks?type=offline，按 task_id 过滤 phase/progress
    async fn status(&self, task_id: &str) -> Result<TaskStatus, ProviderError>;

    /// resolve: B.5，GET /files/{fileID} → web_content_link
    /// 仅当 status == Complete 时可调；返回带 expire 与必备 header 的直链
    async fn resolve(&self, task_id: &str) -> Result<ResolvedLink, ProviderError>;

    /// remove: B.6，DELETE /tasks?task_ids=..&delete_files=true
    async fn remove(&self, task_id: &str) -> Result<(), ProviderError>;
}
```

补充内部方法：
- `login()` → B.2 Step1-3（core login → captcha init → signin token）
- `refresh_access_token()` → B.2 Step4
- `refresh_captcha_token(action, metas)` → B.2 Step2
- `ensure_token()` → 请求前检查 access_token / captcha_token 过期则自动刷新

### E.3 鉴权凭证持久化与刷新策略

| 凭证 | 持久化 | 刷新触发 | 刷新方式 |
|------|--------|---------|---------|
| `device_id` | 持久化，**生成后永不变** | 不刷新 | — |
| `refresh_token` | 持久化 | access_token 过期(4121/4122/10/16) 时用之换新 | POST /v1/auth/token |
| `access_token` | 内存即可（expires_in≈7200s） | 距过期 <5min 主动刷 / 收到 4122 被动刷 | 同上 |
| `captcha_token` | 持久化（expires_in≈3600s） | 距过期 <1min 主动刷 / 收到 error_code=9 被动刷 | POST /v1/shield/captcha/init |
| `credit_key`（信任密钥） | 持久化 | review_panel 验证成功后获得；登录成功后清空 | 人工过验证后回填 |

**关键策略**（直接抄 alist 改进）：
1. 进程启动先尝试 `refresh_token` 恢复，**不要每次重启都走密码登录**（易触发 review_panel）；
2. `refresh_token` 失效(error 4001) 才回退密码登录；
3. 密码登录成功后立即清空 `credit_key`（一次性消费）。

### E.4 哪些调用需要签名？签名算法

| 调用 | 是否签名 | 算法 |
|------|---------|------|
| 所有业务请求（/drive/v1/*） | 不直接签名，但需 `X-Captcha-Token` | captcha_token 由 `/shield/captcha/init` 换取，**该换取过程**才用 `captcha_sign` |
| `/shield/captcha/init` | 需要 `captcha_sign`（或预置 timestamp+sign） | B.1(1) 多轮 md5(Algorithms) |
| `/xluser.core.login/v3/login` | 需要 `device_sign` | B.1(2) sha1→md5→`div101.` 拼接 |
| 下载直链 | 无签名，靠 UA + token in URL | — |

即：**只有两条签名链**——captcha_sign（高频，每次刷 token）和 device_sign（低频，仅登录/验证）。两者盐值/密钥都来自 App 逆向（见 E.1 的 app_key / algorithms）。

### E.5 重试策略

```rust
// 可重试（自动，指数退避，最多 2 次）
- error_code 9        → 刷 captcha_token 后重试
- error_code 4121/4122/10/16 → 刷 access_token 后重试
- error_code 4001     → 用密码重新登录后重试（但 1 次为限，避免锁号）
- 网络超时 / 5xx      → 指数退避重试（1s,2s,5s）

// 不可重试（直接返回错误）
- error_code 4002 captcha_invalid → 参数错误，需改入参
- review_panel        → 需人工验证，返回 NeedVerification 错误
- HTTP 403            → 设备/账号问题，返回 AuthInvalid
- 资源不存在/版权限制  → 业务错误，返回 ResourceUnavailable
```

### E.6 错误类型映射建议

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("access token expired, auto-refreshing")]  // 内部可重试
    TokenExpired,
    #[error("captcha token expired")]
    CaptchaExpired,
    #[error("refresh token invalid, need re-login")]
    SessionInvalid,
    #[error("need manual verification (review_panel): {creditkey}")]
    NeedVerification { creditkey: String, review_url: String },
    #[error("daily cloud-add quota exhausted ({used}/{limit})")]
    QuotaExhausted { used: u32, limit: u32 },
    #[error("resource unavailable: {reason}")]
    ResourceUnavailable { reason: String },
    #[error("auth invalid (device/account rejected)")]
    AuthInvalid,
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("unexpected api error code={code} msg={msg}")]
    Api { code: i64, msg: String, desc: String },
}
```

上层调度器据此决定：`TokenExpired/CaptchaExpired/SessionInvalid` → 自动续期重试；`NeedVerification/QuotaExhausted` → 切换到下一个 Provider（降级）；`ResourceUnavailable` → 标记死种。

---

## F. 风险标记

### F.1 接口性质

- `api-pan.xunlei.com` / `xluser-ssl.xunlei.com` **全部是逆向接口**，非官方公开 API。迅雷官方开放平台 `open.xunlei.com` 只提供"迅雷下载 SDK"（P2SP 引擎集成），**不提供云离线/直链服务**。
- `client_id/client_secret/app_key/algorithms` 全部来自安卓 App `com.xunlei.downloadprovider` 反编译，属于"借壳调用"。

### F.2 风控与封禁风险

- **device_id 滥用**：频繁提交磁力（尤其同一 infohash 反复提交）可能被风控判定异常，轻则 captcha 拦截，重则临时封 device 或要求短信验证；
- **账号封禁**：明显自动化（如固定 UA + 高频请求）有被限速/封号风险。建议：
  - 严格遵守每日 3 次云添加配额，本地排队；
  - 请求间隔加随机抖动（500ms-2s）；
  - 多账号轮换（Provider 支持配置多个账号，配额用尽自动切换）；
  - 不要把单个账号当高频 API 用。
- **流量侧**：提交是上行小请求（磁力串），风控概率低；取回直链是下行 CDN 流量，迅雷主要按"高速流量配额"计，不封 IP，但超额限速。

### F.3 "明天就失效"的脆弱点

1. **`Algorithms[]` 签名盐数组**：随 App 大版本（8.31→8.32…）更新会变。一旦迅雷发版且旧盐失效，captcha_sign 全部失败 → 整个 Provider 不可用。**这是头号风险**。
   - 缓解：把 Algorithms 做成**可热更新的配置**（不硬编码），并在 Provider 启动时做一次 `user/me` 探活，失败则标记 Provider 降级。
2. **`client_id/client_secret`**：随 App 大版本可能轮换，但历史上变化频率低于 Algorithms。
3. **`captcha_sign` 与 `timestamp` 的 Expert 模式**：alist ThunderExpert 支持直接填抓包得到的 `captcha_sign` + `timestamp` 绕过 Algorithms 计算——可作为"盐失效时的应急通道"，但需要人工抓包。
4. **API 路径**：`/drive/v1/files` 的 `upload_type=UPLOAD_TYPE_URL` 是离线入口，历史上迅雷改过几次接口形态，需关注 alist 上游 issue（如 #7550、#8288）。
5. **直链 UA**：下载 UA 偶尔会被收紧（某些版本要求更完整的安卓 UA），需保持与最新 App 一致。

### F.4 合规与降级建议

- 把 ThunderProvider 设计为 RemoteProvider 的**可选/降级**实现，而非唯一兜底；
- 启动时探活失败（连续 3 次 user/me 异常）→ 自动禁用并告警，不阻塞主下载链路；
- 配额（3 次/日）用尽 → 上层调度器应切换到其他云兜底（如自建离线、其他网盘 Provider）或回退本地 P2P 长时挂机；
- review_panel 触发 → 该账号进入"需人工"冷却，切备用账号。

---

## 附录：最小可用 curl 序列（用于联调）

```bash
DEV=0123456789abcdef0123456789abcdef   # 本地生成 32 hex
# 1) core login
curl -sX POST https://xluser-ssl.xunlei.com/xluser.core.login/v3/login \
  -H 'User-Agent: android-ok-http-client/xl-acc-sdk/version-5.0.12.512000' \
  -H 'Content-Type: application/json' \
  -d '{"protocolVersion":"301","sequenceNo":"1000012","platformVersion":"10","isCompressed":"0","appid":"40","clientVersion":"8.31.0.9726","peerID":"00000000000000000000000000000000","appName":"ANDROID-com.xunlei.downloadprovider","sdkVersion":"512000","devicesign":"div101.DEV.<md5hex>","netWorkType":"WIFI","providerName":"NONE","deviceModel":"M2004J7AC","deviceName":"Xiaomi_M2004j7ac","OSVersion":"12","creditkey":"","hl":"zh-CN","userName":"13800138000","passWord":"PWD","verifyKey":"","verifyCode":"","isMd5Pwd":"0"}'
# → sessionID

# 2) captcha init（meta.phone_number=用户名）
curl -sX POST https://xluser-ssl.xunlei.com/v1/shield/captcha/init \
  -H 'Content-Type: application/json' \
  -d '{"action":"POST:/v1/auth/signin/token","captcha_token":"","client_id":"Xp6vsxz_7IYVw2BB","device_id":"DEV","meta":{"phone_number":"13800138000"},"redirect_uri":"xlaccsdk01://xunlei.com/callback?state=harbor"}'
# → captcha_token

# 3) signin token
curl -sX POST 'https://xluser-ssl.xunlei.com/v1/auth/signin/token?client_id=Xp6vsxz_7IYVw2BB' \
  -H 'Content-Type: application/json' -H 'X-Captcha-Token: <ct>' \
  -d '{"client_id":"Xp6vsxz_7IYVw2BB","client_secret":"<redacted>","provider":"access_end_point_token","signin_token":"<sessionID>"}'
# → access_token / refresh_token

# 4) submit 离线（磁力）
curl -sX POST https://api-pan.xunlei.com/drive/v1/files \
  -H 'Authorization: Bearer <at>' -H 'X-Captcha-Token: <ct>' \
  -H 'x-device-id: DEV' -H 'x-client-id: Xp6vsxz_7IYVw2BB' -H 'x-client-version: 8.31.0.9726' \
  -H 'Content-Type: application/json' \
  -d '{"kind":"drive#file","name":"","parent_id":"","upload_type":"UPLOAD_TYPE_URL","url":{"url":"magnet:?xt=urn:btih:xxxx"}}'

# 5) status 轮询
curl -s 'https://api-pan.xunlei.com/drive/v1/tasks?type=offline&limit=100' \
  -H 'Authorization: Bearer <at>' -H 'X-Captcha-Token: <ct>' -H 'x-device-id: DEV' -H 'x-client-id: Xp6vsxz_7IYVw2BB'

# 6) resolve 直链（fileID 来自 task.file_id）
curl -s 'https://api-pan.xunlei.com/drive/v1/files/<fileID>' \
  -H 'Authorization: Bearer <at>' -H 'X-Captcha-Token: <ct>' -H 'x-device-id: DEV' -H 'x-client-id: Xp6vsxz_7IYVw2BB'
# → web_content_link

# 7) 下载（注意 UA）
curl -A 'Dalvik/2.1.0 (Linux; U; Android 12; M2004J7AC Build/SP1A.210812.016)' \
  -e 'https://pan.xunlei.com/' \
  '<web_content_link>' -o file.bin
```

---

## 落地清单（动笔前确认）

- [ ] 配置结构：`username/password/device_id/refresh_token/captcha_token/parent_id/proxy` 全部可配 + 可持久化
- [ ] `device_id` 首次随机生成后写盘，**永不更换**
- [ ] 启动优先 `refresh_token` 恢复，失败回退密码登录
- [ ] `Algorithms[]` / `client_id` / `client_secret` / `app_key` 放配置文件，支持热更新（应对 App 发版）
- [ ] submit 入口统一 normalize：`thunder://` → base64 解码去 AA/ZZ；`.torrent` → 转 magnet
- [ ] 每日 3 次配额计数器 + 跨天重置
- [ ] error_code 9/4121/4122/10/16/4001 自动续期重试；review_panel/4002/403 不重试
- [ ] 直链下载强制 `DownloadUserAgent` + `Referer`，支持 Range
- [ ] Provider 探活失败自动降级，不阻塞主链路
