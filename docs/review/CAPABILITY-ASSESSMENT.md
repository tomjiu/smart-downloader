# 下载能力评估：断点重连 / HTTP 支持 / 硬缺口

> 评估对象：`tomjiu/smart-downloader` @ main（`340277c`）
> 评估日期：2026-10-19
> 方法：**读代码 + 跑复现**，非凭 README 描述

---

## 一、断点重连：做得相当好，比多数开源下载器严格

### 1.1 HTTP 分段下载（主路径）—— ⭐ 优秀

**架构**：显式段账本 + 双指纹校验，**不用「文件长度」当进度**（这是关键设计）。

`crates/httpdl/src/ledger.rs` 的 `Ledger`：

```rust
pub struct Ledger {
    pub version: u32,
    pub total: u64,
    pub min_split: u64,              // 段粒度必须一致，否则整个账本作废
    pub etag: Option<String>,        // 内容指纹 1
    pub last_modified: Option<String>, // 内容指纹 2（E26 备援）
    pub done: Vec<(u64, u64)>,       // 已完成段（闭区间）
}
```

**为什么这个设计好**：动态分段会 `set_len(total)` 预分配 → 中断后文件长度**恒等于 total**。
用文件长度当进度会产出「静默空洞文件」。账本用**显式段区间**取代，这个坑被彻底填掉。

**7 步决策矩阵**（`ledger.rs:198-223 decide`）：

| # | 条件 | 动作 |
|---|---|---|
| 1 | part 超过总长（源变小） | 重下 |
| 2 | 无账本 | **重下**（预分配长度不可信） |
| 3 | 账本 total ≠ 探测 total | 重下 |
| 4 | 指纹核对失败（ETag/Last-Modified 任一） | 重下 |
| 5 | 服务器不支持 Range | 重下 |
| 6 | 段对齐校验失败（防篡改） | 重下 |
| 7 | 其余 | **续传** |

**指纹判定特别严谨**（`fingerprint_ok`）：

```
(Some, Some) 相等 → 放行
(Some, Some) 不等 → 拒绝
(Some, None)      → 拒绝（指纹消失，宁枉勿纵）
(None, _)         → 放行
```

`(Some, None) → 拒绝`这条尤其好——**服务器突然不返回 ETag 时不冒险续传**，
避免「旧前缀 + 新尾部」的静默损坏。业界很多下载器这里是放行的。

**段校验防篡改**（`validate_segments`）：

```rust
if s % self.min_split != 0 { return false; }                    // 必须对齐
if e != (s + self.min_split).min(self.total) - 1 { return false; } // 必须正好一段
if self.done.len() > 100_000 { return false; }                  // 防炸
```

**不信任任何来路不明的「已完成」声明** —— 账本被改坏就全量重下。

**配合 batch10 的段级 fsync**（`download.rs:391`）+ 账本 fsync 先于 rename，
形成了完整的崩溃一致性链。

### 1.2 跨 daemon 重启续传 —— ✅ 完整

`persistence.rs:146 restore_from` 流程：

```
读 tasks.json → 对每个非终态任务 → engine.add()
                                    ↓
                    httpdl 探测 .part + 读账本
                                    ↓
                          ledger::decide() → 续传 or 重下
```

**实测确认的关键点**：终态任务（Completed/Stopped/Failed）**不会**重新下载：

```rust
// persistence.rs:167-183
if matches!(t.state, Completed | Stopped | Failed) {
    // 仅重建记录（有记录无句柄），状态原样保留
}
```

这是 batch 修复过的缺陷（原实现一律 `state=Queued` 并 `engine.add` →
HTTP 完成任务整文件重下覆盖）。

**暂停意图跨重启保留**（P4 G5）：`was_paused` → `engine.pause()` 重放。

### 1.3 BT 续传 —— ⭐ 完整

`bt.rs:8-10` 的设计：

```
remove/pause 前 request_save_resume
  → 轮询 RESUME·ready alert
  → take_resume_data
  → 原子写 <save_path>/<ih>.fastresume

重启后 add 同一 magnet/torrent 时按 infohash 查 .fastresume
  → add_torrent_resume（恢复 piece 位图 + metadata，免全盘 checking）
```

**三重保障**：
1. pause/remove 时保存
2. **周期保存**（`serve.rs:403`，crash/断电最多丢一个周期）
3. **优雅退出前保存**（`serve.rs:561`）

做种状态也跨重启保留（`persistence.rs:206, 317`）。

### 1.4 ⚠️ 断点续传的两个边界

#### 边界 1：服务器不支持 Range → 无法续传

**这是 HTTP 协议限制，不是实现缺陷。** aria2/IDM 同样如此。

`engine.rs:425-427` 注释与实际代码一致：

```rust
/// 断点续传以「文件当前大小」为凭据（Range: bytes={size}-；服务器忽略
/// Range 回 200 → truncate 重下）

let start = fs::metadata(&part).len();
req.header(RANGE, format!("bytes={start}-"));
match resp.status() {
    PARTIAL_CONTENT if cr_ok => 续传(append),
    PARTIAL_CONTENT (cr 不符) => truncate 重下,
    OK => truncate 重下,       // ← 不支持 Range 走这里
}
```

**行为正确**：先尝试 Range，服务器忽略就全量重下。不会产出错误文件。

#### 边界 2：`206` 但拿不到总长 → 退化到单流

探测用 `Range: bytes=0-0`，只有拿到 `Content-Range: bytes 0-0/TOTAL` 才能走分段路径。

| 服务器应答 | 判定 | 后果 |
|---|---|---|
| `206` + `bytes 0-0/1000000` | range=true, total=Some | ✅ 分段+账本 |
| `206` + `bytes 0-0/*` | range=true, total=**None** | ⚠️ 未知总长单流 |
| `206` 无 Content-Range | range=true, total=**None** | ⚠️ 未知总长单流 |
| `200`（忽略 Range） | range=false | ❌ 无续传 |
| `416` + `bytes */500000` | range=false, total=Some | ❌ 无续传（但知总长）|

**单流路径的续传**：靠文件大小 + `Range: bytes={size}-`，服务器回 206 且首字节匹配才 append。
**仍然依赖 Range**。

#### 边界 3：无任何指纹时只剩两道防线

若服务器**从不返回 ETag 也从不返回 Last-Modified**：

```rust
fingerprint_ok(None, None) = true   // 放行
```

此时只剩「总长核对 + 段对齐校验」。

**残余风险**：服务器内容变了但总长恰好相同（版本迭代同大小）→ 「旧前缀+新尾部」混合文件。

**概率低**，且业界同款（aria2 也只靠这两项）。用户配 sha256 可兜底。

---

## 二、HTTP 支持：主干扎实，几处配置缺口

### 2.1 ✅ 做得好的

| 能力 | 实现 | 位置 |
|---|---|---|
| **多连接分段** | 动态分段 + FIFO 段调度 | `segment_manager.rs` |
| **Range 探测** | `bytes=0-0` 探测 206/200/416 | `range.rs:70-104` |
| **Content-Range 校验** | 206 必校验首字节 == 段起点（防错位应答） | `download.rs:321-340` |
| **自定义请求头** | 探测/段/HLS/DASH/备用源全链透传 | `range.rs:36`, `download.rs:301` |
| **Cookie** | 全局 client + 任务级 client 各自 jar | `serve.rs:81` |
| **代理** | http/https/socks5/socks4 + `user:pass@` 凭据 | `engine.rs:142`, `btcore/ffi.rs:97` |
| **重定向** | 可配 1..=100 跳 | `serve.rs:74` |
| **超时** | connect 10s / read 30s（不设总超时，护长下载） | `serve.rs:69-70` |
| **限速** | 全局 + 任务级，串联两级 deadline 链 | `rate.rs` |
| **多源并行** | 强 ETag 相等 + 均支持 Range + 总长一致才混拼 | `range.rs:126 multi_source_ok` |
| **HTTP/2** | 已启用 feature | workspace `Cargo.toml` |
| **HLS / DASH** | 完整实现（含加密/DRM 拒绝） | `hls.rs` / `dash.rs` |

**`multi_source_ok` 的门控特别严谨**——要求**强 ETag**（排除 `W/` 弱 ETag），
避免跨源内容不同导致混拼损坏。这个比 aria2 更保守。

### 2.2 ⚠️ 配置缺口

| 缺口 | 影响 | 严重性 |
|---|---|---|
| **HTTP/3 未启用** | 无 QUIC；高丢包网络下比 h2 慢 | P2 |
| **User-Agent 无默认值可配** | 用 reqwest 默认 UA，部分站点会拒（反爬） | **P1** |
| **TLS 用 `webpki-roots` 固定根集** | 不支持系统证书库 → 企业自签 CA / 中间人审查环境失败 | **P1** |
| **`build_proxied_client` 不设 redirect** | 任务级代理走 reqwest 默认 10 跳，与全局可配不一致 | P3 |

**UA 缺口最实际**：很多网盘/论坛的直链会检查 UA，
reqwest 默认的 `reqwest/0.12` 容易被拦。用户只能手填 `headers`，但**没有全局默认 UA 配置项**。

**TLS 固定根集**：`Cargo.toml` 里 `rustls-tls` 默认用 `webpki-roots`（编译进二进制的 Mozilla 根集）。
企业内网自签证书、或某些地区的中转代理，会直接 TLS 握手失败。

---

## 三、硬缺口

### 🔴 P0-1：Web Seed 从「.torrent / magnet」自动注入的链路是断的

> **定级说明**：本节曾被我初判为「完全死链路」，但深入核实后发现
> **在 libtorrent 2.0.x 上 `.torrent` 场景可能被内核自动兜住**。
> 下面是核实后的准确表述 —— 保留修正过程因为差异很重要。

#### 事实：Rust 侧确实传空

| 环节 | 状态 |
|---|---|
| `.torrent` 的 `url-list` 解析 | ✅ `torrent_meta.rs:213 collect_web_seeds` |
| magnet 的 `ws=` 参数解析 | ✅ `source_parse/magnet.rs:163` |
| 暴露到 API | ✅ `http.rs:1245` 返回 `"web_seeds"` |
| **传给引擎** | ❌ **`daemon/bt.rs:532` 恒空** |

```rust
// daemon/src/bt.rs:532
let web_seeds: Vec<String> = vec![];   // ← 永远是空

DownloadSource::Magnet(m) => match &fastresume {
    Some(data) => self.core.add_torrent_resume(data, &web_seeds),
    None => self.core.add_magnet(m, &web_seeds),      // ← 传空
},
DownloadSource::TorrentFile(bytes) => match &fastresume {
    Some(data) => self.core.add_torrent_resume(data, &web_seeds),
    None => self.core.add_torrent_file(bytes, &web_seeds),  // ← 传空
},
```

`DownloadSource::TorrentFile(Vec<u8>)` 存的是**原始字节**（`types.rs:11`），
`ops.rs` 添加任务时**完全不解析** torrent（grep 无 `parse_torrent` 命中）。
所以引擎拿到的就是原始 bytes + 空 web_seeds。

#### 但 `.torrent` 场景可能被内核自动兜住（版本相关）

`ffi/src/lt_kernel.cpp:597-611`：

```cpp
#if LIBTORRENT_VERSION_NUM >= 20100
    // ≥2.1：只传 info section —— **不解析 url-list**
    auto ti = std::make_shared<lt::torrent_info>(
        info, ec, lt::load_torrent_limits{}, lt::from_info_section);
#else
    // 2.0.x：传完整 node —— libtorrent 可能自行解析 url-list
    auto ti = std::make_shared<lt::torrent_info>(node, ec);
#endif
```

CI 与本地基准是 **libtorrent 2.0.11**（`.github/workflows/ci.yml:77` 注「本地 Debian 13 (2.0.11)」），
走 `else` 分支。

**所以状态是**：

| 场景 | 2.0.x（当前） | ≥2.1（未来） |
|---|---|---|
| `.torrent` 自带 `url-list` | ⚠️ **可能被内核自动读取**（取决于 libtorrent 内部实现，**需实测确认**） | ❌ **必然失效**（只传 info section） |
| magnet 带 `&ws=` | ❌ **必然失效**（magnet 字符串的 ws 参数由 Rust 侧解析后传入，内核不解析 magnet 的 ws） | ❌ 必然失效 |
| 手动 `POST /tasks/:id/webseeds` | ✅ 有效（`ops.rs:3490`） | ✅ 有效 |

**重要旁证**：libtorrent 有已知 issue
[#3946 "Incorrect loading of url_seeds"](https://github.com/arvidn/libtorrent/issues/3946)，
说明 `url_seeds` 的自动加载路径本身并不可靠。

#### 测试盲区（这是确定的）

所有 web_seeds 测试都**只测解析层**：

```rust
assert_eq!(s.web_seeds, vec!["https://ws/1", "https://ws/2"]);   // torrent_meta.rs:458
```

**没有任何测试验证引擎真的收到了 web seed。**
`status_controls.rs:118` 那个 `add_url_seed` 测的是**手动 API**，不是 `.torrent` 自动注入。

#### 结论与建议

**确定的缺陷**：magnet 的 `ws=` 参数**必然丢失**（Rust 解析了却不传）。
**潜在的缺陷**：`.torrent` 的 `url-list` 在 libtorrent ≥2.1 后**必然失效**（当前 2.0.x 可能靠内核兜住）。

**修复**（无论哪个版本都该修）：

```rust
// daemon/src/bt.rs:532 改为从 source 提取
let web_seeds = match &task.source {
    DownloadSource::TorrentFile(bytes) =>
        smart_dl_core::torrent_meta::parse_torrent(bytes)
            .map(|s| s.web_seeds).unwrap_or_default(),
    DownloadSource::Magnet(m) =>
        smart_dl_core::source_parse::parse_magnet(m)
            .map(|p| p.web_seeds).unwrap_or_default(),
    _ => vec![],
};
```

**并补一个 e2e 测试**：构造带 `url-list` 的 .torrent → add → 断言
`core.status()` 或 `url_seeds()` 里能看到该 URL。**这是当前测试体系缺的那一环。**

### 🔴 P0-2：`xunlei` feature 下的 `bt.enabled` 默认值破坏性变更

（已在 `PR114-BATCH10-REMAINING.md` §1.1 详述）

batch10 把 `BtCfg::default().enabled` 从 `false` 改成 `true`，
与 `serve.rs:242` 的互斥检查冲突：

```rust
if cfg.bt.enabled {
    return Err(ServeError::Engine(
        "配置 [bt].enabled 与 [xunlei].enabled 互斥：请仅启用其一"
    ));
}
```

**只写 `[xunlei] enabled = true`、不写 `[bt]` 段的用户 → 升级后 daemon 起不来。**

### 🟡 P1：断点续传的其它边界

1. **服务器不支持 Range → 完全无续传**（协议限制，与 aria2 同）
2. **无 ETag/Last-Modified → 只剩总长+段校验两道防线**（低概率残余风险）
3. **`min_split` 变更 → 整个账本作废重下**（`validate_segments` 严格要求段对齐）

第 3 点值得注意：如果未来把 `min_split` 做成可配置且允许运行时改，
**已下载的进度会全部作废**。当前 `DEFAULT_MIN_SPLIT = 16MB` 是常量，暂无影响。

### 🟡 P1：HTTP 配置缺口

- 无全局默认 User-Agent 配置
- TLS 固定 `webpki-roots`，不支持系统证书库（企业/自签 CA 场景失败）
- 无 HTTP/3

### 🟢 P2：`verify_nonempty_blocks` 在 Windows 为 no-op

（已在 `PR114-BATCH10-REMAINING.md` §3 详述）

---

## 四、总结

### 断点重连评分：**A-**

**强于多数开源下载器**：
- 用显式段账本而非文件长度（避开了预分配空洞文件这个大坑）
- 双指纹校验 + `(Some, None) → 拒绝` 的保守策略
- 段对齐严格校验（防篡改）
- BT 三重 fastresume 保存（pause/remove + 周期 + 退出）
- 跨重启的暂停意图、做种状态、定时任务全部保留

**扣分项**：
- 服务器不支持 Range 时无续传（协议限制，但可以给用户更明确提示）
- 无 ETag 时只剩两道防线

### HTTP 评分：**B+**

主干（分段/Range/Cookie/代理/多源/HLS/DASH）很扎实，
`multi_source_ok` 的强 ETag 门控甚至比 aria2 更保守。

**扣分**：
- 无全局 UA 配置（实际最容易踩）
- TLS 固定根集（企业场景）
- 无 HTTP/3

### 最该修的三件事

1. **`bt.rs:532` 的 web_seeds 未接线** —— magnet 的 `ws=` 确定丢失、`.torrent` 的 `url-list`
   在 libtorrent ≥2.1 后必然失效；修复约 6 行，并需补 e2e 测试
2. **全局 User-Agent 配置** —— 很多站点靠 UA 拦下载器
3. **TLS 支持系统证书库**（可选 `native-tls` 或 `rustls-native-certs`）—— 企业场景

### 三个问题的直接回答

> **断点重连做得怎么样？**
**比多数开源下载器更严格**（显式账本 + 双指纹 + 段对齐校验），A- 级。
特别是 `(Some, None) → 拒绝` 这条保守策略和段对齐防篡改校验，业界罕见。

> **HTTPS 支持如何？**
主干完整（rustls + 代理 + Cookie + 自定义头 + HTTP/2），但**固定根证书集**
（`webpki-roots`）会在企业自签 CA 环境失败，且**无全局 UA 配置**。

> **还有哪些硬缺口？**
1. **Web Seed 未接线**（magnet 的 `ws=` 确定丢失；`.torrent` 的 `url-list` 依赖内核兜底，
   ≥2.1 后失效）+ **测试体系缺「引擎实际收到」这一环**
2. `bt.enabled` 默认值破坏性变更 —— 影响 `--features xunlei` 用户启动
3. 全局 UA / 系统证书库 —— 影响真实站点兼容性

---

## 五、我的核查过程（含一次自我修正）

本轮我先判定「web_seeds 是完全死链路」，**后在核查 libtorrent 版本分支时发现结论需要收窄**：

```
ffi/src/lt_kernel.cpp:597
#if LIBTORRENT_VERSION_NUM >= 20100
    torrent_info(info, ..., from_info_section)   // 只解析 info → 不读 url-list
#else
    torrent_info(node, ec)                        // 完整 node → 可能读 url-list
#endif
```

当前用 2.0.11（走 `else`），所以 `.torrent` 场景**可能被内核自动兜住**。
**magnet 的 `ws=` 仍然是确定丢失的**（那个由 Rust 侧解析，内核不解析 magnet 的 ws 参数）。

**这个修正很重要**：如果我按初判说「Web Seed 完全失效」，云端可能去追一个
在 2.0.x 上实际不存在的现象。**准确表述应该是「未接线 + 测试盲区 + 版本风险」。**
