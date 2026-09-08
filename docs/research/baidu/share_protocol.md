# 百度网盘分享协议实测记录（B3-a）

> 2026-09-05，真实分享链接全链路实测（curl 手工 + Rust 实现双验证）。
> 证据等级：**A 级**（请求/响应原文级，非逆向推断）。
> 实现落点：`crates/provider/src/baidu/`（协议形状与本文件一一对应）。

## 样本

- 分享链接：`https://pan.baidu.com/s/13fTBd5tvk-6a7TdxsTaS_w?pwd=nsdp`（提取码 nsdp）
- 分享内容：目录 `/SDP新客户端`（6 个文件：Win/Mac 客户端安装包 2.1.0/2.1.1 + 2 个 PDF 指引）
- shareid = `16364495271`，uk = `1227964813`

## 结论速览

| 环节 | 可行性 | 关键点 |
|---|---|---|
| 分享链接解析 | ✅ 免登录 | `/s/1<code>` 短码固定带 `1` 前缀；`/share/init?surl=<code>` 已去前缀 |
| 提取码校验 verify | ✅ 免登录 | **必须 POST**（GET 同参数实测 errno -12 风控拦截） |
| 文件列表 share/list | ✅ 免登录 | 需 BDCLND cookie + shareid/uk（分享页 HTML 内嵌） |
| dlink 直链（112 转换） | ❌ 需登录态 | 免登录 `/api/download` 实测 errno -6；`/share/download` 实测 errno 2 |

## 协议细节

### 1. 分享页导航（种 BAIDUID/csrfToken/PANPSC）

```
GET https://pan.baidu.com/s/13fTBd5tvk-6a7TdxsTaS_w?pwd=nsdp
→ 302 → https://pan.baidu.com/share/init?surl=3fTBd5tvk-6a7TdxsTaS_w&pwd=nsdp
Set-Cookie: BAIDUID=...; BIDUPSID=...; PANPSC=...; csrfToken=...
```

- 短码 `13fTBd5tvk-6a7TdxsTaS_w` 去 `1` 前缀 = `surl` 参数 `3fTBd5tvk-6a7TdxsTaS_w`。
- 首次访问必须成功种上 BAIDUID 后再 verify，否则风控。

### 2. 提取码校验 verify（POST）

```
POST /share/verify?surl=<surl>&t=<毫秒时间戳>&channel=chunlei&web=1&app_id=250528&clienttype=0
Referer: https://pan.baidu.com/share/init?surl=<surl>
Origin:  https://pan.baidu.com
Content-Type: application/x-www-form-urlencoded
Cookie: BAIDUID=...（首访已种）

body: pwd=nsdp

← 200 {"errno":0,"err_msg":"","request_id":9206942887972188257,
        "randsk":"YSIAmmkz1F%2FgW0OI2xmqqzSZQS%2BOdvYE9MPwrKCZg%2FE%3D"}
```

- `randsk` 即 **BDCLND cookie 值**（保持 URL-encoded 原样）。
- **GET 形态同参数实测 `errno:-12`**（提取码错误与风控共用该码）；POST 稳定通过。
- 实测口径：浏览器 UA 必须；沙盒数据中心 IP 通过（无住宅 IP 要求）。
- **频率风控**：短时间内连续多次 verify 后分享页会退化为无 shareid 的
  风控页（HTTP 200，无 errno），数分钟后自动恢复——实现侧以
  `MetaParse` 错误归类（"分享可能已失效或风控"）。

### 3. 分享页元信息（shareid/uk）

带 BDCLND 再 GET `/s/1<code>` → 200 HTML，内嵌两种形状（实测并存）：

```
JS 形状：  var d={shareid:"16364495271",uk:"1227964813",...};
JSON 形状："shareid":"16364495271"
噪声：     "uk":0（数字值形状，不得命中——以 uk:" 引号值形状匹配规避）
```

### 4. 文件列表 share/list

```
GET /share/list?shareid=<sid>&uk=<uk>&root=1&clienttype=0&web=1&app_id=250528   （根目录）
GET /share/list?shareid=<sid>&uk=<uk>&dir=/SDP新客户端&clienttype=0&web=1&app_id=250528   （子目录）
Referer: https://pan.baidu.com/s/1<code>
Cookie:  ...; BDCLND=<randsk>

← {"errno":0,"list":[{...}], "cur_total":1, ...}
```

条目字段实测（**数值全为字符串**）：

```json
{"category":"6","fs_id":"892549727248113","isdir":"0",
 "path":"/SDP新客户端/2.0客户端问题排查指引0702.pdf",
 "server_filename":"2.0客户端问题排查指引0702.pdf",
 "size":"1082476","md5":"4f013bf9c...","server_mtime":"1781773902",...}
```

- 分页字段存在（`cur_total`），v1 单页取全（实测 6 条全量返回）；大目录分页留后续。
- `errno:9019` = need verify（无 BDCLND 调 list）。

### 5. dlink 直链（未闭环，B3-b）

- 免登录 `POST /api/download?fidlist=[<fs_id>]&origin=dlna`（带 BDCLND）→ `errno:-6`（需登录）。
- 免登录 `POST /share/download?web=1`（带 BDCLND）→ `errno:2`（需签名/登录态）。
- 分享页 `sign1/sign3/timestamp` 仅登录后由接口下发（免登录页只有字段名骨架）。
- 结论：**dlink 转换必须登录态（BDUSS，可选 STOKEN）**；路径有二：
  1. 分享直下（`/api/download` + 登录 cookie，非 SVIP 限速）；
  2. 转存到自己网盘后走 xpan `/filemetas?dlink=1`（对齐 quark provider 的转存语义）。
  待用户提供 BDUSS 后真机校准（同 G1/G2 人工桶）。

## 「112 链接」术语备注

BACKLOG E 段用户术语「百度网盘（112 链接）」的格式定义：2026-08-30 与
2026-09-05 两轮 web 检索均无公开资料，且逆向证据目录
（`docs/research/clients/multi_downloader/`）无百度样本。本文件确立的
输入面为**标准分享链接**；若后续拿到「112 链接」真实样本，在
`provider/src/baidu/share.rs` 单点增补解析规则。
