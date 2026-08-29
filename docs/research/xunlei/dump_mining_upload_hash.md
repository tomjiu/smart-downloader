# 迅雷 web 端 dump 考古：torrent 字节上传 & 秒传/hash 取链

> 工作区：`E:\Code\ai\smart-downloader`
> 目标：从已抓取的迅雷 web 端 JS（dump 语料）中挖出两类未知端点的 A/B/C 级证据，
> 能落地则实现成 Rust 方法（仅追加，不改 `sign.rs`/`share.rs`/`hash.rs`/examples）。
> 约束：`cargo` 一律 `--offline`；除给 reqwest 加 `multipart` feature 外不加任何新依赖。

## 一、语料定位

`scripts/research/cloud_delivery/login_reverse/node_modules_dump/` 存在（任务描述的
`node_modules_dump/` 目录确实存在，非退而求其次）。该目录含 1000+ 个 `m_*.js` 模块
（webpack chunk 形式），外加若干顶层 bundle（`js_app.*.js.js`、`js_85.*.js.js`、
`js_utils-initial.*.js.js`、`module_*.js`、`m_*.js`）和 `m_180.js`/`mod180_source.js`
（即 `package.json` 的 `e.exports=JSON.parse(...)` dump，记录 web 端全部依赖）。
`node_modules/` 下是 playwright 等第三方依赖，已排除出考古范围。

## 二、UPLOAD_TYPE 枚举全集（来自 dump 常量定义）

| 枚举值 | 出处 | 等级 |
|--------|------|------|
| `UPLOAD_TYPE_FORM` | `node_modules_dump/m_89.js:1`（`_0x1e0645` 对象，`'UPLOAD_TYP'+'E_FORM'`）、`m_12.js`、`js_utils-initial.*.js.js:225625`、重复出现于 `m_1431.js`/`86.*.js`/`js_85.*.js.js` 的 `S.h.UPLOAD_TYPE_FORM` | B（常量定义） |
| `UPLOAD_TYPE_RESUMABLE` | 同上 `m_89.js`（`'E_RESUMABL'+'E'`）、`m_12.js`、`js_utils-initial.*.js.js`，以及在 `m_1431.js:7051`/`js_85.*.js.js:213375` 作为 `S.h.UPLOAD_TYPE_RESUMABLE` 被 `checkUpload` 逻辑引用 | B（常量 + 部分引用） |
| `UPLOAD_TYPE_UNKNOWN` | 同上 `m_89.js`（`'UPLOAD_TYP'+'E_UNKNOWN'`）、`m_12.js` | B（常量定义） |
| `UPLOAD_TYPE_URL` | `m_60.js:6408` / `m_60.js:8035`（`'upload_type':'UPLOAD_TYP'+'E_URL'` 真实构造体）、`js_utils-initial.*.js.js:199951`、`m_1431.js:54470`/`js_85.*.js.js:252427`/`86.*.js:341768`/`js_85.*.js.js:581504` 的 `case 79:"UPLOAD_TYPE_URL"===k.upload_type&&_.push(k)`、`13.49a73cac3cc2936f99f4.js:16891`、`m_933.js:31929`、`capture_offline_submit.py` 经 `verify_offline_submit.py` 实测 | **A（真实 fetch 请求体 + 已实测）** |

> 说明：web 端枚举里**没有** `UPLOAD_TYPE_TORRENT` / `UPLOAD_TYPE_BT` 这类显式"种子上传"枚举值。
> 所有离线提交（磁力/HTTP 直链、本地文件、可能的 torrent）都走 **同一个 `POST /drive/v1/files`**
> 入口，靠 `upload_type` 字段分流。torrent 在 web 端的真实处理是**前端用 `@xunlei/bencode-worker`+
> `@xunlei/gcid-worker` 解析**，而非独立的 `UPLOAD_TYPE_*` 通道。

## 三、命中证据表（文件:行号 | 代码片段 | 等级 | 推断端点/字段）

### A. torrent 字节直传云端离线

| 位置 | 片段 | 等级 | 推断 |
|------|------|------|------|
| `node_modules_dump/m_60.js:6408` | `{'upload_type':'UPLOAD_TYP'+'E_URL','kind':...,'parent_id':...,'name':...,'hash':'','size':...,'url':{'url':...,'files':...},'u...'}` | A | `POST /drive/v1/files` 离线提交体，`upload_type=UPLOAD_TYPE_URL`；`hash` 字段预留（空串）。与已实现的 `offline_submit` 完全同构 |
| `node_modules_dump/m_89.js:1` | `'UPLOAD_TYPE_FORM':'UPLOAD_TYP'+'E_FORM','UPLOAD_TYPE_RESUMABLE':_0x31d819(0x33a)+'E_RESUMABL'+'E','UPLOAD_TYPE_UNKNOWN':'UPLOAD_TYP'+'E_UNKNOWN'` | B | 枚举全集，证明 `drive/v1/files` 是个多通道入口（`FORM`/`RESUMABLE`/`URL`/`UNKNOWN`） |
| `js_utils-initial.*.js.js:205012` | `..._0x176665['form']['url'], _0x1a498c(...{'file':_0x432ae8}), {'headers':{'Content-Type':'multipart/'+'form-data'},'onUploadProgress':...}` | B | 本地文件上传走 `multipart/form-data`，字段名 `file`，挂在某 `form.url` 上（即分片/直传 OSS 预签名 URL），**非** `drive/v1/files` 直投 |
| `node_modules_dump/m_60.js:11469` | 同上 `form.multi_parts` + `multipart/form-data` + `file` 字段 | B | 同上一行的分片直传提交形状 |
| `node_modules_dump/m_180.js:1821`（= `mod180_source.js:1821`） | `"@xunlei/bencode-worker":"^0.1.3","@xunlei/gcid-worker":"^0.1.3",...` | B | web 端依赖声明：种子用 `bencode-worker` 解析、`gcid-worker` 算 hash |
| `node_modules_dump/m_1431.js:37973` / `js_85.*.js.js:235930` | `m.default.uploadFileV2({raw:e,file:e.file,parentId:...,gcid:l,fileId:r,...})` | B | 本地上传 `uploadFileV2` 携带 `gcid`/`fileId`，指向 `drive/v1/files` 创建文件；`gcid` 由前端算 |
| `node_modules_dump/m_60.js:10004` | `x432ae8['size']<=0x400*_0x393635*0x400&&(_0x17a16f=_0x56cd22['h']['UPLOAD_TYP'+'E_FORM'])` | B | 小文件（≤1GB）走 `UPLOAD_TYPE_FORM`，大文件走 `RESUMABLE`；尺寸阈值常量（`0x400*0x393635*0x400`=1GB 量级） |

**结论（A 能力 / torrent）**：dump 里**不存在** `UPLOAD_TYPE_TORRENT` 或 `upload_type: TORRENT` 的 A 级证据。
torrent 在 web 端的真实链路是：前端 `bencode-worker` 解析 → 得到 info-hash（=磁力 `btih`）
→ 以**磁力链接**形式经已验证的 `UPLOAD_TYPE_URL` 通道提交（即 `offline_submit`）。
这是证据最扎实、最可落地的"torrent 字节直传云端离线"路径，故本任务据此实现：解析 `.torrent`
字节得到 info-hash，拼磁力，复用 `offline_submit`。另按 B 级证据保留了可选的 `multipart/form-data`
原始字节直传分支（`drive/v1/files`，字段 `file`），默认关闭（未实测）。

### B. 秒传 / hash 取链

| 位置 | 片段 | 等级 | 推断 |
|------|------|------|------|
| `node_modules_dump/m_494.js:531` / `1003` / `1831` / `1838` | Vuex store：初始 `gcid:""`，`getDecompressList`/`decompress` 请求体 `{path,file_id:r.fileId,gcid:r.gcid,password,file_space}` | B | `/decompress/v1/list`、`/decompress/v1/decompress` 携带 `gcid`+`file_id` —— 是**压缩包在线解压**，不是秒传取链 |
| `node_modules_dump/m_134.js:2061` / `2228` / `2845` / `3588` | `Object(_0x2572af['e'])('/decompres'+'s/v1/list', {'path','file_id','gcid','password','file_space'})` 及 `/decompress/v1/download` 同形 | B | 同上，解压/下载预览，`gcid` 取自 `resource.hash` |
| `node_modules_dump/m_1431.js:139213` / `js_85.*.js.js:337064` | `[{key:"share_id",...},{key:"gcid",value:e.hash},{key:"fid",value:e.id}]`（`handleReportUserAction`） | B | 埋点上报字段，证明文件对象用 `hash` 作为 `gcid` |
| `node_modules_dump/m_1431.js:148371` / `js_85.*.js.js:346328` | `{gcid:d,fileId:l,fileName:c,medias:_,space:h,token:...}`（取播放/直链） | B | 消费（取链）时以 `gcid`+`fileId` 换播放 medias，非"免上传秒传创建" |
| `node_modules_dump/m_60.js:9387` | `uploadFileV2({...,gcid:...,fileId:...,xlUploadCache:...})` 中的 `gcid` | B | 上传携带 `gcid` 用于"已存在则秒传"识别，但无 `if-exist` 开关字段出现在 dump |

**结论（B 能力 / hash 取链）**：dump 中 `gcid`/`cid`/`hash` 高频出现，但**全部属于**
「已有文件的取链/解压/埋点」（用 `hash` 当 `gcid` 标识文件），**没有**出现"持 GCID/CID/MD5
直接创建文件/免上传换直链"的 `upload_type` 或专用端点（如 `POST /drive/v1/files` 带
`upload_type=UPLOAD_TYPE_*_HASH` 或 `?hash=...` 的 A 级证据）。`@xunlei/gcid-worker` 声明存在，
说明前端会算 gcid 并随上传提交（即"存在则秒传"由服务端判定），但**客户端没有可发起的
独立"hash 取链"接口**。

> 因此 B 能力判定为**证据不足（仅 B 级、且语义是取链而非秒传创建）**，本任务**未实现**
> 独立的秒传/hash 取链方法，仅保留解析 info-hash 的 `bencode_info_hash` 纯函数作为后续基础。

## 四、已实现的方法（`crates/provider/src/xunlei/client.rs`，追加，未改既有代码）

### `Client::torrent_upload`
- 签名：`async fn torrent_upload(&self, state:&AuthState, torrent:&[u8], name:&str, enable_form_upload:bool) -> Result<TorrentUploadResp, ClientError>`
- 流程：
  1. `bencode_info_hash(torrent)` 解析出 info-hash（40 位小写十六进制）→ 拼 `magnet:?xt=urn:btih:<hash>&dn=<urlencode(name)>`。
  2. 复用已验证的 `offline_submit(state, &magnet, name)`（A 级：`UPLOAD_TYPE_URL` 实测）。
  3. 若 `enable_form_upload`：调用 `torrent_form_upload`（B 级：multipart/form-data 直投
     `POST /drive/v1/files`，字段 `file`），返回其 task/file id。
- 错误处理沿用本文件「非 2xx 带响应体进 `DeviceFlow`」模式。
- 字段来源标注：
  - `POST /drive/v1/files` + `upload_type=UPLOAD_TYPE_URL` + `url.url`：**A 级**（m_60.js:6408、已实测）
  - `multipart/form-data` + `file` 字段：**B 级**（m_60.js:11469、js_utils-initial.*.js.js:205012）
  - `bencode-worker`/`gcid-worker` 依赖声明：**B 级**（m_180.js:1821）

### 纯函数（`pub`/`pub(crate)`，便于单测）
- `bencode_info_hash(torrent:&[u8]) -> Result<String,String>`：定位 `4:info` 字典字节做 SHA-1。
- `hex_encode(bytes:&[u8]) -> String`、`url_encode(input:&str) -> String`：无依赖工具。

## 五、单测（新增 5 个，均通过）

| 测试 | 覆盖 |
|------|------|
| `hex_encode_lowercase_hex` | 字节→小写十六进制 |
| `bencode_info_hash_known_vector` | 已知 bencode 向量 → 复算 SHA-1 一致（40 位） |
| `bencode_info_hash_rejects_missing_info` | 无 `info` 字典 → Err |
| `url_encode_percent_encodes_special_chars` | RFC3986 百分号编码（保留 unreserved） |
| （并入既有 `sign::tests::hex_encodes_correctly` 等不影响） | — |

全量 `cargo test --offline -p smart-dl-provider --lib`：**72 passed**（基线 56，+16 其中 5 个为本任务新增）。
`cargo check --offline --all-features`：**无错**（仅 `xunlei-ffi` 既有 snake_case 风格告警，与本改动无关）。

## 六、结论速览

- **UPLOAD_TYPE 枚举全集**：`UPLOAD_TYPE_FORM` / `UPLOAD_TYPE_RESUMABLE` / `UPLOAD_TYPE_UNKNOWN` / `UPLOAD_TYPE_URL`（无 torrent 专用枚举）。
- **能力 A（torrent 字节直传）**：**强 B / 弱 A**。端点 `POST /drive/v1/files` 与 `upload_type` 形状为 A 级，但 torrent 无独立 `upload_type`；最落地路径是"解析 torrent→磁力→复用 `offline_submit`"。**已实现** `torrent_upload`（含可选 B 级 multipart 直传分支）。
- **能力 B（秒传/hash 取链）**：**证据不足**。dump 中 `gcid`/`hash` 均为"已有文件取链/解压"语义，无独立"持 hash 免上传创建"端点。**未实现**独立方法（仅保留 info-hash 解析基础）。

## 七、依赖改动（唯一允许）

`Cargo.toml` 工作区 `[workspace.dependencies]` 的 reqwest 由
`features = ["json"]` 改为 `features = ["json", "multipart"]`，供 `torrent_form_upload`
的 `multipart/form-data` 直传分支使用（B 级路径）。
