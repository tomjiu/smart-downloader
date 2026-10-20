# 未接线清单 —— 给云端的可执行任务

> 对象：`tomjiu/smart-downloader` @ main `49d208c` + PR #114（`401127e`）
> 日期：2026-10-19
> 性质：**每一条都经过代码验证，含精确位置、根因、可执行修法**

---

## 0. 一句话总结

项目里有一批「**模块写完 + 单元测试写完 + 忘记接进调用链**」的能力。

**共同特征**：单元测试只测「模块独立行为」，没有端到端测试验证「调用方真的用了它」。

这 5 处加起来 **约 1200 行已实现但失效的代码**。

| # | 模块 | 规模 | 用户影响 | 修复量 |
|---|---|---|---|---|
| 1 | `retry.rs` 重试退避 | 60 行 | **429 越冲越限流** | 半天 |
| 2 | `SchedulerEvent::Progress` | — | **WS 进度通道是死的** | 1-2 天 |
| 3 | `QuarkProvider` | 完整实现 | 文档说 ✅ 但从不装配 | **半小时** |
| 4 | `core/sniffer.rs` | 695 行 | 迅雷分享链接直接报错 | 1 天 |
| 5 | `web_seeds` | 解析完整 | magnet `ws=` 丢失 | **1 小时** |

---

## 1. 🔴 `retry.rs` —— 重试退避整块未接线

> **✅ 已修复（2026-10-20，PR #122）**：`SegmentError` 枚举（状态码 +
> Retry-After 不再被 String 抹平）+ 根段退避阶梯（Retry-After 优先钳 60s，
> 否则 1/2/4s + jitter）+ **429 永不拆半**（正反馈放大源头）。总时延有界
> 设计：退避只在根段、拆半子段单发（防退避×拆半树 2^depth 等待放大——
> task_proxy e2e 暴露后修正）。测试 ×10（单测 8 + e2e 2，含请求计数精确
> 有界断言；旧代码判别已验证）。

### 现状

```bash
$ grep -rn "should_retry\|Backoff" crates/ --include=*.rs
crates/httpdl/src/retry.rs:12    pub struct Backoff { ... }       ← 定义
crates/httpdl/src/retry.rs:33    pub fn should_retry(...)         ← 定义
crates/httpdl/tests/backoff_429.rs:24,31,38,39                   ← 单元测试
```

**60 行模块，生产代码零调用。**

### 实际行为

```rust
// crates/httpdl/src/download.rs:344
other => return Err(format!("segment status {}", other)),
```

**403 / 404 / 429 / 500 / 503 全部走这一条，无差异化。**

```rust
// crates/httpdl/src/download.rs:187 —— 上层完全忽略错误内容
Err(_) => {
    if let Some(sc) = &scores { update_score(sc, url, -2); }
}
```

### 后果

```
服务器：429 Too Many Requests
引擎：  段失败 → 拆成 2 个子段（download.rs:270-280）
        → 2× 请求量
        → 再 429 → 拆成 4 个
        → 正反馈放大
```

**`Retry-After` 头全仓零解析。**

### 根因：错误类型太窄

```rust
// crates/httpdl/src/download.rs:292
async fn download_segment_streaming(
    ...
) -> Result<(), String> {        // ← 只有 String，状态码信息已丢失
```

**要接线必须先改错误类型。**

### 修法（分两步）

**Step 1：引入错误枚举**

```rust
// download.rs 新增
#[derive(Debug, Clone)]
enum SegmentError {
    /// HTTP 状态码（含可重试/终态语义）
    Status(u16, Option<u64>),   // (status, retry_after_secs)
    /// 传输/IO 错误
    Transport(String),
    /// Content-Range 不匹配
    RangeMismatch(String),
}

impl SegmentError {
    fn is_retryable(&self, attempt: u32) -> bool {
        match self {
            Self::Status(code, _) => should_retry(*code, attempt, MAX_ATTEMPTS),
            Self::Transport(_) => true,
            Self::RangeMismatch(_) => false,   // 错位不重试，直接失败
        }
    }
}
```

**Step 2：在段循环里真正退避**

```rust
// download.rs:270 附近（download_segment_with_retry 内）
let mut attempt: u32 = 0;
loop {
    attempt += 1;
    match download_segment_streaming(...).await {
        Ok(()) => return Ok(()),
        Err(e) if e.is_retryable(attempt) => {
            // 优先用服务器给的 Retry-After
            let delay = match &e {
                SegmentError::Status(_, Some(secs)) => Duration::from_secs(*secs),
                _ => Backoff::default().next_delay(attempt),
            };
            // 关键：加 jitter 避免惊群
            let jitter = Duration::from_millis(rand::random::<u64>() % 500);
            tracing::warn!("段 [{},{}] 第 {attempt} 次重试，{delay:?} 后", cur.start, cur.end);
            tokio::time::sleep(delay + jitter).await;
            continue;
        }
        Err(_) if cur.len() / 2 >= MIN_RETRY_GRANULARITY => {
            // 现有的 halving 逻辑保留（对 5xx 仍有用）
            ...拆半...
        }
        Err(e) => return Err(...),
    }
}
```

**Step 3：解析 `Retry-After`**

```rust
// download.rs:344 附近
other => {
    let retry_after = resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    return Err(SegmentError::Status(other.as_u16(), retry_after));
}
```

### 建议的端到端测试

```rust
#[tokio::test]
async fn http_429_is_retried_and_succeeds() {
    // 用 tests/http_server.rs 现成的 retry_429 字段
    let server = spawn(ServerConfig { retry_429: 2, ..default() });
    let result = download(...).await;
    assert!(result.is_ok(), "429 应在退避后重试成功");
}
```

**注意**：`tests/http_server.rs:66` 已经有 `retry_429` 字段 —— 基础设施齐全。

### 文档要同步

`README.md:11` 写的「429 退避」现在是假的。修完才算数。

---

## 2. 🔴 `SchedulerEvent::Progress` / `Speed` —— 生产代码零发布

### 现状

我写脚本严格排除 `#[cfg(test)]` 后验证：

```
crates/daemon/src/**/*.rs
  排除 #[cfg(test)] 之后的代码
  grep "publish(SchedulerEvent::Progress\|Speed)"
→  ❌ 零命中
```

**生产代码 29 处 `publish()` 全是**：
`TaskCreated` / `StateChanged` / `Failed` / `Completed` / `TaskActivated` /
`DuplicateRejected` / `SettingsChanged` / `GlobalLimitsChanged`

**`Progress` 与 `Speed` 两个变体只在 `ws.rs:209-267` 的单测里出现。**

### 后果

- **WebSocket 的 `progress` 通道永远不推送**
- `ws.rs:145-181` 的 `Throttler`（1s 合并 progress）是**死代码**
- 前端实时速率只能轮询 `/stats`
- 而轮询间隔是 **2 秒**（`serve.rs:392`）

**「事件三通道」宣传在进度维度上不成立。**

### 修法

**Step 1：在轮询器里发 Progress**

```rust
// crates/daemon/src/http_events.rs（2 秒轮询）
// 或 poll_engine_states 里

for (tid, rec) in tasks.iter() {
    if let Some(status) = &rec.engine_status {
        state.hub().publish(SchedulerEvent::Progress {
            task_id: tid.clone(),
            done: status.downloaded,
            total: status.total,
        });
        state.hub().publish(SchedulerEvent::Speed {
            task_id: tid.clone(),
            down_rate: status.download_rate,
            up_rate: status.upload_rate,
        });
    }
}
```

**Step 2：BT 侧也要发**

BT 的 alert 循环（`bt_events.rs`）比轮询快（500ms），
应该在收到 `State` alert 或定时采样时也发 `Progress`/`Speed`。

**Step 3：降轮询间隔**

```rust
// serve.rs:392
spawn_http_events(state, Duration::from_secs(2));
//                              ↓
spawn_http_events(state, Duration::from_millis(500));
```

**有了 WS 推送后，轮询主要是兜底，可以降到 500ms 而不增加太多负载**
（因为前端不再依赖轮询刷新 UI）。

### 建议的端到端测试

```rust
#[tokio::test]
async fn ws_receives_progress_events() {
    let (tx, mut rx) = connect_ws(&addr, token).await;
    state.add_task(http_url).await.unwrap();
    // 等 3 秒
    let events: Vec<_> = timeout(Duration::from_secs(3), async {
        collect_until(&mut rx, |e| matches!(e, SchedulerEvent::Progress { .. })).await
    }).await.expect("应收到 progress 事件");
    assert!(!events.is_empty());
}
```

---

## 3. 🔴 `QuarkProvider` —— 实现了但从不装配

> **✅ 已修复（2026-10-20，PR #119）**：`[provider_quark]` 配置段 +
> `assemble_providers()` 装配（顺手把 mock/xunlei/quark 三内联块抽出可单测）
> + `/providers` 透出 + 测试 ×7（config/serve/e2e 三层）。cookie 持久化
> 安全：provider 侧 save_auth 已是 0600 原子写（P1-6 在位，原文担忧已消）。
> 范围边界：HTTP cookie 登录端点未含（新 API 面需专门设计，另议）。

### 现状

```rust
// crates/daemon/src/serve.rs:102-133
let mut providers: Vec<Arc<dyn RemoteProvider>> = Vec::new();
if cfg.provider.enabled && cfg.provider.mock {
    providers.push(Arc::new(MockProvider::new("mock")));      // L104
}
// 3b+. 迅雷云盘 Provider 装配
let xp = XunleiProvider::with_tier(...);                       // L127
providers.push(Arc::new(xp));                                  // L133

// ← 没有 QuarkProvider ！
```

而 `crates/provider/src/quark/share.rs` 有**完整实现**（215+ 行，
含 `submit` / `resolve` / `refresh_links` / `runtime`）。

### 后果

`docs/IMPLEMENTED.md` 声称「夸克网盘 Provider ✅」，
但**用户永远用不上** —— `GET /providers` 里不会有 quark。

### 修法（半小时）

```rust
// serve.rs，紧接 XunleiProvider 之后
#[cfg(feature = "quark")]   // 或直接用现有配置判断
if cfg.provider_quark.enabled {
    match smart_dl_provider::quark::QuarkProvider::new(
        cfg.provider_quark.cookie.clone(),   // 需要新增配置项
    ) {
        Ok(qp) => providers.push(Arc::new(qp)),
        Err(e) => tracing::warn!("夸克 Provider 初始化失败: {e}"),
    }
}
```

需要补：
1. `Config` 加 `[provider_quark]` 段（`enabled` + `cookie`）
2. `serve.rs` 装配
3. 一个测试断言 `GET /providers` 含 quark

### 注意

夸克的认证是 Cookie（`quark/share.rs` 里 `is_valid()` 检查），
配置项和迅雷的 token 管理不同 —— **要确认 cookie 的持久化安全**（参考 `xunlei/auth.rs:163-176` 的 `0600` 写法）。

---

## 4. 🟡 `core/sniffer.rs` —— 695 行，daemon 零调用

### 现状

```bash
$ grep -rn "sniffer" crates/daemon/
→  无匹配
```

而 `core/src/sniffer.rs` 有 **695 行 + 13 个单元测试**，
文档（`IMPLEMENTED.md` #15）标记为「✅ 已落地」。

### 实际表现

```rust
// crates/daemon/src/state/ops.rs:860
NormalizedSource::XunleiShare(u) => Err(DaemonError::InvalidSource(format!(
    "迅雷网盘分享暂不支持直接导入: {u}"
))),
```

**用户粘贴迅雷分享链接 → 直接报错。**

### 修法

```rust
// ops.rs:860 附近
NormalizedSource::XunleiShare(u) => {
    // 用 sniffer 的规则解析
    match smart_dl_core::sniffer::sniff(&u) {
        Some(sniffed) => {
            // 转成 Xunlei 云盘任务（走 provider）
            self.add_xunlei_share_task(sniffed, opts, ...).await
        }
        None => Err(DaemonError::InvalidSource(format!("无法解析迅雷分享: {u}"))),
    }
}
```

**但要先确认**：
- `sniffer` 的输出格式与 `add_xunlei_share_task` 期望的输入是否匹配
- 迅雷分享走 provider 兜底链路时，`[provider_xunlei]` 必须已配置

### 与 `web_seeds` 的关系

`sniffer` 和 `source_parse/normalize.rs` 有功能重叠 —— 我在
`docs/review/ARCHITECTURE-REVIEW.md` §6.5 提过。**接线时要先定哪个是权威入口。**

---

## 5. 🟡 `web_seeds` —— 解析完整，传空

> **✅ 已修复（2026-10-20，PR #118）**：bt.rs add 路径按源提取（.torrent
> url-list / magnet ws=）并注入三个内核入口。**作用面修正**：magnet ws= 与
> 2.0.x 的 url-list 实际由 libtorrent 原生兜底（parse_magnet_uri 读 ws、
> 全文件 ctor 读 url-list）；本修复的关键价值在 **2.1**——info-section ctor
> 丢弃顶层 url-list，daemon 侧提取是唯一路径。e2e：零 tracker 零 peer、
> HTTP 服务器唯一数据源，完成 + 请求计数 > 0 + 内容逐字节一致。

### 现状

```rust
// crates/daemon/src/bt.rs:532
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

而解析层**已经实现**：
- `.torrent` 的 `url-list` → `core/src/torrent_meta.rs:213 collect_web_seeds`
- magnet 的 `ws=` → `core/src/source_parse/magnet.rs:163`

### 修法（1 小时）

```rust
// crates/daemon/src/bt.rs:532
let web_seeds: Vec<String> = match &task.source {
    DownloadSource::TorrentFile(bytes) => {
        smart_dl_core::torrent_meta::parse_torrent(bytes)
            .map(|s| s.web_seeds)
            .unwrap_or_default()
    }
    DownloadSource::Magnet(m) => {
        smart_dl_core::source_parse::magnet::parse_magnet(m)
            .map(|p| p.web_seeds)
            .unwrap_or_default()
    }
    _ => vec![],
};
```

### 重要：版本风险

`ffi/src/lt_kernel.cpp:597-611`：

```cpp
#if LIBTORRENT_VERSION_NUM >= 20100
    // ≥2.1：只传 info section → **不解析 url-list**
    auto ti = std::make_shared<lt::torrent_info>(
        info, ec, lt::load_torrent_limits{}, lt::from_info_section);
#else
    // 2.0.x：传完整 node → libtorrent 可能自行解析 url-list
    auto ti = std::make_shared<lt::torrent_info>(node, ec);
#endif
```

**当前 2.0.11 走 `else`，`.torrent` 场景可能被内核自动兜住。**

所以：
- **magnet 的 `ws=` 是确定丢失的**（内核不解析 magnet 的 ws 参数）
- `.torrent` 的 `url-list` 在 **libtorrent ≥2.1 后必然失效**

**无论哪个版本都该修**，但要明确「现在修的是哪部分」。

### 建议的端到端测试

```rust
#[tokio::test]
async fn torrent_url_list_reaches_engine() {
    // 构造带 url-list 的 .torrent
    let torrent = make_torrent_with_url_list(&["http://seed/1"]);
    state.add_torrent(torrent).await.unwrap();
    // 断言引擎侧能看到
    let status = core.status(&ih).unwrap();
    assert!(status.url_seeds.contains("http://seed/1"));
}
```

**当前的测试盲区就在这里** —— `torrent_meta.rs:458` 只断言了解析结果：
```rust
assert_eq!(s.web_seeds, vec!["https://ws/1", "https://ws/2"]);
```
**没有一条测试验证引擎真的收到了。**

---

## 6. 这 5 个的共同根因与建议

### 根因

**单元测试的边界画错了。**

```
test_parse_magnet_ws_parameter()          ← 测「解析对不对」✅
                                          ← 缺「引擎收到没」❌
test_should_retry_returns_true_for_429()  ← 测「函数返回对不对」✅
                                          ← 缺「429 时下载能成功」❌
```

**每个模块都单测充分，但没有一层断言「集成效果」。**

### 建议：加一层「能力存在性」集成测试

```rust
// crates/daemon/tests/capability_wiring.rs
//! 这组测试的目的不是验证逻辑，是验证「接线」。
//! 它们全部是端到端断言，只要调用方忘了接线就会红。

#[tokio::test]
async fn capability_retry_backoff() {
    // 429 → 应该最终成功，而不是立即失败
    let srv = spawn(ServerConfig { retry_429: 2, ..default() });
    assert!(download(srv.url()).await.is_ok());
}

#[tokio::test]
async fn capability_progress_events() {
    // WS 应该收到 progress 事件
    let mut rx = ws_connect().await;
    add_task(...).await;
    assert!(recv_progress(&mut rx, 3.secs()).await.is_some());
}

#[tokio::test]
async fn capability_quark_provider_registered() {
    // providers 列表里应该有 quark
    let cfg = Config { provider_quark: enabled(), ..default() };
    assert!(list_providers(&cfg).await.contains("quark"));
}

#[tokio::test]
async fn capability_sniffer_accepts_xunlei_share() {
    // 迅雷分享链接应该能建任务，而不是 InvalidSource
    assert!(add_task("thunder://...").await.is_ok());
}

#[tokio::test]
async fn capability_webseed_reaches_engine() {
    // .torrent 的 url-list 应传到引擎
    assert!(engine_url_seeds(&ih).await.contains("http://seed/1"));
}
```

**5 个测试，覆盖 5 处缺口，成本极低。**

---

## 7. 下一步开发建议（按 ROI 排序）

### 🔥 第一梯队：修复已有能力（1-2 周）

| # | 任务 | 为什么值得做 |
|---|---|---|
| 1 | **接线 `retry.rs` + `Retry-After` + jitter** | CDN 限流是最常见的真实失败模式，现在会越冲越限流 |
| 2 | **生产代码发 `Progress`/`Speed`** | 让「事件三通道」名副其实，UI 实时速率才有数据源 |
| 3 | **装配 `QuarkProvider`** | 半小时工作量，直接多一个云盘源 |
| 4 | **`web_seeds` 接线** | 6 行，magnet ws= 立即可用 |
| 5 | **`sniffer` 接入 add 路径** | 695 行代码从「死的」变「活的」 |
| 6 | **补 `capability_wiring.rs` 集成测试** | 防止这 5 处再退化 |

### 🟠 第二梯队：补核心缺口（2-4 周）

| # | 任务 | 说明 |
|---|---|---|
| 7 | **SFTP 补内容指纹** | 5 行，消灭最严重的静默损坏路径 |
| 8 | **HTTP 未知总长加可观测性** | 现在重启白下无任何提示 |
| 9 | **全局 User-Agent 配置** | 很多站点靠 UA 拦下载器 |
| 10 | **TLS 支持系统证书库** | 企业自签 CA 场景 |
| 11 | **Provider 兜底 `update_sources` 实现** | 夸克已能重签直链，接上即可 |
| 12 | **分类目录（category → dest 规则）** | 日常使用流 |

### 🟡 第三梯队：体验完善（1-2 月）

| # | 任务 |
|---|---|
| 13 | UI 批量操作（后端已有 `POST /tasks/batch`） |
| 14 | peer 详情端点 + 前端 peer 面板 |
| 15 | Tracker 状态详情（不只 URL 列表） |
| 16 | 自定义分段并发/粒度 |
| 17 | HLS live 录制 |
| 18 | DASH 音视频合流 |

### 🔵 第四梯队：长期工程

| # | 任务 | 说明 |
|---|---|---|
| 19 | **BT v2 支持** | 需改 FFI 契约（infohash 从 40 hex `char*` 改为二进制 + 长度） |
| 20 | 浏览器扩展 + 剪贴板监控 | 3000 行级工作量 |
| 21 | WebDAV / S3 | 新协议 |

### ⚠️ 优先级判断依据

**第一梯队全部是「已有代码只是没接线」** —— 投入产出比最高：
- 5 项合计源码改动 < 500 行
- 每项都能立即产生用户可见的效果
- 风险低（不引入新架构）

**第二梯队是「真实缺陷」** —— 影响可用性但需要新代码。

**BT v2 建议单独立项** —— 它需要改整个 FFI 契约
（`lt.h` 所有 API 都以 40 hex `char*` 表示 infohash，v2 是 32 字节 64 hex），
波及 btcore / daemon / task_id / 查重键，是最重的一项。

---

## 8. 附：验证方法（云端可自行复核）

```bash
# 1. retry 未接线
grep -rn "should_retry" crates/ --include=*.rs | grep -v "tests/"

# 2. Progress 未发布
grep -rn "publish(SchedulerEvent::Progress" crates/daemon/src/ --include=*.rs

# 3. QuarkProvider 未装配
grep -n "QuarkProvider" crates/daemon/src/serve.rs

# 4. sniffer 未调用
grep -rn "sniffer" crates/daemon/src/ --include=*.rs

# 5. web_seeds 传空
grep -n "web_seeds: Vec<String> = vec!\[\]" crates/daemon/src/bt.rs
```

**五条命令，全部零命中（或命中即证据）。**

复现脚本在我的 `repro/p0repro/` 里（`round18.rs` 是 ABBA 死锁的实证）。
