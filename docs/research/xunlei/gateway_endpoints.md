# api-gateway-pan 端点清单 + cloud_upload.dll 分诊

> 取证时间：桌面迅雷 Chromium disk cache 与安装目录 DLL 静态扫描（全部离线，`cargo` 一律 `--offline`）。
> 关联任务脚本：`tools/research/gateway_cache_scan.py`、`tools/research/gateway_auth_context.py`、`tools/research/cloud_upload_pe.py`。
> 机读产物：`docs/research/xunlei/_gateway_scan.json`、`docs/research/xunlei/_cloud_upload_scan.json`。

---

## 0. 重要取证修正（方法学）

1. **cache 只落响应体与请求 URL，绝不缓存请求头。**
   对整份 `data_1`（1,056,768 字节）做全局字符串 grep：
   `Authorization`=0、`Bearer `=0、`x-captcha-token`=0、`x-client-id`=0、`x-device-id`=0。
   → 任何「邻近字节找 Authorization」的鉴权推断在本数据源上**天然取证失败**，下文鉴权列一律标注【B级推断】。
2. **`/xlppc.resinfo.api/...` 并不属于该网关。** 它在 cache 中实际挂在
   `https://api-shoulei-ssl.xunlei.com/` 之下（dump 上下文确认），仅因 `/xlppc` 前缀被正向匹配误中。
   → 该网关真实 `/xlppc` 端点只有两个 `xlppc.searcher.api` 搜索接口。
3. **DLL 实际路径比任务描述深一级**：`C:\Program Files\Thunder Network\Thunder\program\upload\cloud_upload.dll`
   （任务写的 `...\program\cloud_upload.dll` 不存在）。
4. **gateway 命中次数**：本机 cache 当前为 **123** 次（任务描述的 107 次是另一时间点的快照，缓存动态变化属正常；另一代理 api-pan 154 次对应更老的快照）。

---

## 1. 任务 A：api-gateway-pan 端点清单（6 类完整 URL，去重计数）

数据源：`%APPDATA%\thunder\Cache\Cache_Data\data_1`（1,056,768 B）。
提取规则：所有 `https://api-gateway-pan.xunlei.com/...` 完整 URL，按「scheme+host+path（去 query）」分组计数。
去重后共 **6 类** URL，合计 **123** 次原始命中。

| # | 路径 | 用途推断 | 频次 | 鉴权推断 |
|---|------|----------|------|----------|
| 1 | `/speed-center/v2/rule` | 提速规则（v2，客户端提速/会员加速策略下发，最常用） | **68** | 【B级推断】三要素头同 api-pan（Bearer+x-captcha-token+x-device-id）；非搜索类，可能仅需匿名/OAuth |
| 2 | `/speed-center/v1/trial` | 提速试用（v1 试用资格/时长查询） | **25** | 【B级推断】同上，可能匿名 |
| 3 | `/speed-center/v1/rule` | 提速规则（v1 旧版，仍在用） | **23** | 【B级推断】同上 |
| 4 | `/xlppc.searcher.api/drive_common_search` | **云盘通用搜索**（关键字/磁力 `magnet:?xt=urn:btih:...`/链接；cache 实证 keyword 直接传磁力串） | **4** | 【B级推断】三要素头同 api-pan（见 §1.1）；请求带 `user_id` |
| 5 | `/xlppc.searcher.api/drive_file_search` | 云盘文件搜索（按 `space=*` 限定全盘文件） | **2** | 【B级推断】同上 |
| 6 | `/report/v1/config` | 上报/埋点配置拉取（客户端遥测开关） | **1** | 【B级推断】匿名/三要素其一 |

### 1.1 鉴权方式判断（任务 A 第 3 点）

- **直接取证**：cache 中**无任何** `Authorization`/`Bearer `/`x-captcha-token` 字符串（见 §0-1），无法从本数据源直接确认请求头。
- **间接推断（【B级】）**：
  - 两个 `drive_common_search`/`drive_file_search` 请求 URL **携带 `user_id=860599297`**，且属于迅雷云盘体系（与 `api-pan` 的 `drive/*` 同源业务）；
  - 同仓库 `crates/provider/src/xunlei/client.rs::auth_headers` 已确立 api-pan 云盘接口使用**三要素头**
    （`Authorization: Bearer <access_token>` + `x-captcha-token` + `x-device-id` + `x-client-id`）；
  - 故**暂推断 gateway 的搜索接口与 api-pan 同构**，复用三要素头。
  - 注意：`speed-center/*` 与 `report/*` 是否为匿名、是否需 OAuth，证据更弱，仅标注为「可能匿名/三要素其一」。
- **结论**：鉴权**形状推断与 api-pan 相同**（三要素头），但**未经真实抓包/实测验证** → 全部标【B级待验】。真正的请求头需对桌面 App 抓包或实测才能定级（A级）。

### 1.2 高价值端点实现：云盘搜索客户端

`drive_common_search` 形状清晰、命中可观且用途高价值（云盘检索，可直接对接离线下载/取链链路），已在
`crates/provider/src/xunlei/cloud_search.rs` 落地【B级待验】搜索客户端：

- `CloudSearch` 结构体 + `common_search()` / `file_search()` 两个 async 方法（复用 `client::Client::auth_headers` 三要素头）；
- 纯函数 `build_common_search_url()` / `build_file_search_url()`：URL 组装形状严格复现 cache 取证
  （`user_id`/`keyword`/`limit`/`order_by_fields` 或 `space=*`；百分号编码为大写 `%3A/%3F/%3D`，与 desktop App 一致）；
- 5 个单测覆盖 URL 组装纯函数（含磁力关键字编码、大写 hex、`space=*` 原样透传）；
- `mod.rs` 已 `pub mod cloud_search;` 注册；**未改动 `client.rs`/`provider.rs`/`serve.rs`**。
- 网络方法未实测 → 标注【B级待验】，仅 offline 单测验证纯函数 URL 形状。

---

## 2. 任务 B：cloud_upload.dll 分诊

数据源：`C:\Program Files\Thunder Network\Thunder\program\upload\cloud_upload.dll`（3,132,576 B，x86-64 PE32+）。
方法：手写最小 PE 导出表解析（参考 `E:/Code/tools/xunlei-re/local/pe_iat_probe.py` 思路；因是**磁盘文件**而非内存 dump，额外实现 section 头 RVA→文件偏移映射，修复原脚本「直接把 RVA 当偏移」的偏移错误）+ 双编码（UTF-8 / UTF-16LE）字符串扫描。

### 2.1 导出函数（PE 导出表）

导出表共 **1 个**命名导出：

| 导出名 | 函数 RVA |
|--------|----------|
| `InitCloudUpload` | `0x1d670` |

其余为按序号导出（ordinal-only），本分诊只看命名导出即可定性。

### 2.2 关键词字符串命中（UTF-16LE 全 0，以下为 UTF-8 计数）

| 关键词 | UTF-8 命中 | 代表性上下文（节选） |
|--------|-----------|----------------------|
| `upload` | 84 | `uploading` / `uploadId` / `partSize` / `doneParts` / `client_id` / `errCode.uploadingCount` / `uploadedCount` |
| `task` | 39 | `taskId` / `nextPageFlag` / `tasks` / `deleteCloudFile.taskIds.fileIds` / `temp_batch_update_task_ids(taskId INTEGER PRIMARY KEY)`（本地 SQLite 表） |
| `token` | 22 | `provider_auth_token_expired` / `security_token` / `session_token` / `space_token_required` / `drive#file` |
| `url` | 16 | `dataInfo` / `form` / `url.resumable` / `original_url` / `CurlHttpClient` / `curl_slist_append` |
| `signature` | 4 | `signaturedoesnotmatch` / `CanonicalString` / `SignerV4` / `x-oss-signature-version` / `x-oss-credential`（阿里云 OSS V4 签名） |
| `gcid` | 0 | — |
| `btih` | 0 | — |

### 2.3 一句话结论 + 证据

**结论（值得深挖，但属「云上传/对象存储客户端」而非「P2P 核心」）：**
`cloud_upload.dll` 是桌面迅雷的**云端上传/对象存储（COS/OSS）上传客户端**，对外唯一命名入口 `InitCloudUpload`，
内部用 `libcurl`（`CurlHttpClient`/`curl_slist_append`）做分片上传（`uploadId`/`partSize`/`doneParts`），
集成阿里云 OSS V4 签名（`SignerV4`/`x-oss-signature`/`CanonicalString`）与腾讯云 COS（`security_token`/`session_token`/`cos_path`），
并用本地 SQLite（`temp_batch_update_task_ids(taskId ...)`）维护上传任务状态。

**是否值得深挖**：
- ✅ 值得——它承载「云盘文件上传 / 离线任务回写」能力，与本项目 `client.rs` 已有的 `offline_submit`/`torrent_upload` 链路互补，可补齐「上传」半环；
- ⚠️ 但与 P2P 引擎解耦明显：扫描中 **`gcid`/`btih` 均为 0 命中**，说明它**不负责哈希计算/磁力/种子逻辑**，纯做「把字节传上云」的 HTTP 客户端，深挖收益在于对象存储签名与断点续传，而非协议逆向。

**证据**：导出表仅 `InitCloudUpload` 一项；UTF-8 字符串密集出现 `upload`/`uploadId`/`partSize`/`doneParts`（分片上传）、`CurlHttpClient`（自研 curl 封装）、`SignerV4`+`x-oss-signature`（OSS 签名）、`security_token`+`cos_path`（COS）、`temp_batch_update_task_ids`（本地任务库）；`gcid`/`btih` 零命中佐证其与 P2P 哈希体系无关。

---

## 3. 验证结果

- `cargo test --offline -p smart-dl-provider --lib`：**77 passed**（原 72 + 新增 5 个 cloud_search URL 纯函数单测），0 failed。
- `cargo check --offline --all-features`：**无错**（仅有 `xunlei-ffi` 的既有命名/未用导入 warning，与本次改动无关）。
- 改动范围：`crates/provider/src/xunlei/cloud_search.rs`（新增）、`crates/provider/src/xunlei/mod.rs`（仅加一行 `pub mod cloud_search;`）。**未改 `client.rs`/`provider.rs`/`serve.rs`**，无 git 操作。
