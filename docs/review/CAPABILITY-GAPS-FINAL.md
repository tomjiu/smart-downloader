# 能力缺口终版（含全部独立验证）

> 对象：`tomjiu/smart-downloader` @ **main `82b8e8f`**（PR #114 尚未合并）
> 日期：2026-10-19
> **所有结论经我独立 grep + 读码验证。含 3 处对 SUBAGENT 说法的修正。**

---

## 0. 最重要的一条

### 🔴 `should_retry` / `Backoff` 在生产代码中零调用

```bash
$ grep -r "should_retry" crates/
retry.rs:33                          ← 定义
tests/backoff_429.rs:24,31,38,39     ← 单元测试
```

**60 行模块 + 完整单测 + 零生产调用。**

### 实际行为：所有 HTTP 错误码一视同仁

```rust
// download.rs:344
other => return Err(format!("segment status {}", other)),

// download.rs:187 —— 上层完全忽略错误内容
Err(_) => {
    if let Some(sc) = &scores { update_score(sc, url, -2); }
}
```

### 后果：429 被当普通失败 → 越冲越限流

```
服务器：429（你请求太快了）
引擎：  缩粒度 → 拆 2 段 → 2× 请求
        → 再 429 → 拆 4 段 → 4× 请求
        → 正反馈放大
```

**`Retry-After` 头全仓零解析。**

### 讽刺的是：测试桩早就准备好了

```rust
// tests/http_server.rs:66
pub retry_429: u32,   // mock 支持「前 N 次回 429」
```

**基础设施齐全、单测通过、就是没接进调用链。**

### 文档与代码不一致

`README.md:11` 明确写：

> 多连接 Range 并行、动态分段…、**失败缩小粒度重试、429 退避**、跨段共享限速…

**「429 退避」不存在。**

---

## 1. 对 SUBAGENT 三处说法的修正

### ❌ 修正 1：「开箱即用是无 BT 无 FTP 的空壳，BT 代码在主 CI 矩阵零覆盖」

**前半句对，后半句错。**

✅ 对：`crates/daemon/Cargo.toml` 无 `default = [...]`，`bt`/`ftp`/`sftp` 全默认关。

❌ 错：CI **有专门的 feature job**：

```yaml
L58:  cargo test -p smart-dl-httpdl --features ftp
L61:  cargo test -p smart-dl-httpdl --features sftp
L71:  cargo test -p smart-dl-daemon --features "ftp,nas"
L74:  cargo test -p smart-dl-daemon --features sftp
L112: cargo test -p smart-dl-daemon --features bt
```

**「零覆盖」不成立。**

**真实问题是**：`cargo build` 出来的默认二进制不含 BT/FTP/SFTP，
用户装了默认包发现「下载不了磁力」，**而安装包没有明确提示**。

### 🟡 修正 2：「轮询延迟 2s」

✅ 确认：

```rust
// serve.rs:392
spawn_http_events(state, Duration::from_secs(2));
```

但需补充：这与下面 §3 的「Progress 事件是死的」是**同一问题的两面** ——
因为 WS 不发进度，前端只能轮询；轮询又是 2 秒。
**叠加 = 进度条最多 2 秒动一次。**

### ❌ 修正 3：「SFTP 有 TOFU known_hosts（优势）」

✅ 对（`sftp.rs:53-119`），但这只防**中间人**，
**不防「远端文件被替换」** —— 那个是内容指纹问题（`sftp.rs:520` 只比 total）。

两者是不同层面的防御，不能互相替代。

---

## 2. 🔴 三条 P0（我独立验证）

### P0-A：`Progress` / `Speed` 事件生产代码零发布

我写脚本严格区分生产代码与 `#[cfg(test)]`：

```
crates/daemon/src 下所有文件，
跳过 #[cfg(test)] 之后的行，
grep "publish(SchedulerEvent::Progress|Speed)"
→  ❌ 零命中
```

**生产代码 29 处 `publish()` 全是**：
`TaskCreated` / `StateChanged` / `Failed` / `Completed` / `TaskActivated` /
`DuplicateRejected` / `SettingsChanged` / `GlobalLimitsChanged`

**`Progress` 和 `Speed` 两个变体只在 `ws.rs:209-267` 的单测里出现。**

**后果**：
- WebSocket `progress` 通道**永远不推送**
- `ws.rs:145-181` 的 `Throttler`（1s 合并 progress）是**死代码**
- 前端实时速率只能靠轮询 `/stats`

**这不是「少一个功能」，是「事件三通道」宣传在进度维度上不成立。**

### P0-B：BT v2-only 完全不可用 + handle 泄漏 bug

```cpp
// lt_kernel.cpp:403-407
const lt::torrent_handle h = s->ses.add_torrent(p);   // ← 先加进 session
const lt::info_hash_t ih = h.info_hashes();
if (!ih.has_v1()) {
    return LT_ERR_ENGINE;      // ← 再拒绝，但 torrent 已在 session 里了
}
```

**两个问题叠加**：
1. **v2-only 种子/磁力直接被拒**（`lt_kernel.cpp:406` 和 `:560` 双入口）
2. **拒绝前已 `add_torrent`** → session 里残留孤儿 handle（**泄漏**）

**根因是自设的 FFI 契约**：`lt.h` 所有 API 都以 40 hex `char*` 表示 infohash
（`find_handle` 硬校验 `strlen(ih) != 40`），而 BT v2 的 infohash 是 32 字节（64 hex）。

**libtorrent 2.x 本身支持 v2**（`info_hash_t`），是契约限制不是上游限制。

**严重性**：BEP-52 已 2022 落地，qB 4.5+/Transmission 4+ 全支持。
v2-only 种子在增长，**这是主动砍掉一代协议**。

### P0-C：`SchedulerEvent` 之外的三个「未接线」模块

| 模块 | 行数 | 测试 | 生产调用 |
|---|---|---|---|
| `retry.rs` | 60 | ✅ `backoff_429.rs` | ❌ 零 |
| `core/sniffer.rs` | 695 | ✅ 13 个 | ❌ daemon 零 |
| `core/strategy.rs` 的 `AntiLeechAdvice` | — | ✅ | ❌ 零（注释自认「接入点不在本模块」） |

**加上 `web_seeds`（解析有、传空）和 `Progress`（有变体、无发布者），
这是第 5 次出现同一模式。**

---

## 3. 完整缺口表（main 分支现状）

### 🔴 P0（阻断级）

| # | 缺口 | 证据 | PR #114 修了？ |
|---|---|---|---|
| 1 | 重试退避未接线（429/5xx） | `retry.rs:33` 零调用 | ❌ |
| 2 | `Progress`/`Speed` 无人发布 | 全仓生产代码零命中 | ❌ |
| 3 | BT v2-only 拒绝 + handle 泄漏 | `lt_kernel.cpp:403-407` | ❌ |
| 4 | SFTP 零内容指纹 | `sftp.rs:520, 632` | ❌ |
| 5 | ABBA 死锁 | `ops.rs:507` vs `:444` | ❌ |
| 6 | `cid_store` 二分边界回归 | `cid_store.rs:224` | ❌ |

### 🟡 P1

| # | 缺口 | 证据 | PR #114 修了？ |
|---|---|---|---|
| 7 | `web_seeds` 未接线（magnet ws= 确定丢失） | `bt.rs:532` | ❌ |
| 8 | Provider 兜底不可续（update_sources 空实现） | `ops.rs:4158` | ❌ |
| 9 | **`QuarkProvider` 从未装配** | `serve.rs:104-133` 只有 Mock + Xunlei | ❌ |
| 10 | `sniffer` 零调用 → 迅雷分享链接直接报错 | `ops.rs:860` | ❌ |
| 11 | `persistence.rs` 无 `sync_all`（对比 config 有） | `:28, :83` | ✅ **batch10 修了** |
| 12 | HTTP 未知总长必从头且无日志 | `engine.rs:1240, 509` | ❌ |
| 13 | 无全局 UA 配置 | grep 无字段 | ❌ |
| 14 | TLS 固定 `webpki-roots`，无自签/客户端证书 | `Cargo.toml` | ❌ |
| 15 | SFTP 仅密码认证（无公钥/agent） | `sftp.rs:277` 自认 | ❌ |
| 16 | **FTP 不支持代理**（裸 TcpStream） | `ftp.rs:524, 773, 1755` | ❌ |
| 17 | DHT/PEX/LSD/μTP 默认全关 | `config.rs:88-100` | ❌ |
| 18 | 无分类目录 | `config.rs:52-53` 只有 dest_root | ❌ |
| 19 | 无 peer 详情端点 / 无 scrape | `http.rs:2560-2615` | ❌ |
| 20 | 无浏览器扩展 / 剪贴板监控 | grep 零 | ❌ |
| 21 | rss 锁外落盘丢失更新 | `rss.rs` 7 处 | ❌ |
| 22 | `bt.enabled` 默认值破坏性变更 | `config.rs:240` | ❌ |

### 🟢 P2

| # | 缺口 |
|---|---|
| 23 | 分段并发/粒度硬编码（64MB / 2-8） |
| 24 | 默认二进制无 BT/FTP/SFTP（无 default feature） |
| 25 | 无 HTTP/3 |
| 26 | WebDAV / S3 全无 |
| 27 | DASH 只下视频轨（明示边界） |
| 28 | 无 aria2 JSON-RPC 兼容 |
| 29 | 无 i18n / 系统通知 |
| 30 | 进度仅 2 秒轮询 |

---

## 4. 文档与代码不一致（我确认的 8 条）

| # | 文档声称 | 实际 | 证据 |
|---|---|---|---|
| 1 | `README.md:11`「**429 退避**」 | `should_retry` 零调用 | `retry.rs:33` |
| 2 | `README.md:14`「事件三通道」 | 通道在，但**无 Progress/Speed 发布者** | 全仓零命中 |
| 3 | `README.md:15`「迅雷网盘分享链接」 | 可识别但建任务直接报错 | `ops.rs:860` |
| 4 | `IMPLEMENTED.md` #15「协议嗅探引擎」 | 695 行 + 13 测试，**daemon 零调用** | grep 无匹配 |
| 5 | `IMPLEMENTED.md` #15「夸克 Provider ✅」 | **serve.rs 从不注册 QuarkProvider** | `serve.rs:104-133` |
| 6 | `IMPLEMENTED.md` #2「直链过期 update_sources ≤3」 | 空实现，永远 `Ok(())` | `ops.rs:4158-4161` |
| 7 | `README.md:14`「并发队列 BT≤3 / HTTP≤8」 | 默认 **0 = 不限**（排队默认禁用） | `config.rs:164-172` |
| 8 | `README.md:13` BT「DHT/LSD/UPnP 开关」 | 有，但**默认全关**（README 未说明） | `config.rs:88-100` |

---

## 5. 这个项目真正强的地方（对比 aria2 / qB / IDM）

**以下都是我在竞品里没见到的，或明显更深的：**

### 1. 续传严谨度高于 aria2

- **ETag + Last-Modified 双指纹**（aria2 只可选 ETag）
- **206 响应校验 `Content-Range` 起点**（`download.rs:319-341`）—— **aria2 没有**
- 段对账本严格对齐校验，账本被改即全量重下

### 2. 多源并行比 aria2 更安全

要求**双源强 ETag 相等 + 都支持 Range + 总长一致**才允许跨源混拼
（`range.rs:126 multi_source_ok`）。
**aria2 的 `-x` 是无条件跨源拼接**，镜像不同步会静默产坏文件。

### 3. 失败缩小粒度重试（halving）

`download.rs:258-286` 段失败时拆半到 1MB 下限。**aria2 没有**。

### 4. HLS/DASH 段账本续传 —— 市面真空区

`aria2` 无解密合流，`ffmpeg` 无断点续传/限速/任务管理。
本项目把 `.part.hls-ledger` 做成**清单指纹对账 + 段级续传**。

### 5. 安全面全面超越 aria2 / qB

- tokenless 时强制 **Host/Origin 同源守卫**（防 DNS rebinding/CSWSH）
- 常量时间 token 比较（`ct_eq`）
- `tasks.json` / `bans.json` / 账本**全部 0600 + 唯一 tmp + rename**
- **SFTP TOFU known_hosts**
- **FTP 数据面 SSRF 防护**（拒绝第三方内网数据连接）

**aria2 的 RPC secret 是明文比较；qB WebUI 是明文 cookie。**

### 6. Prometheus histogram

`smart_dl_task_down_speed_bytes_per_second` 按 engine 分 series 的分布直方图。
**aria2 只有 text/JSON，qB 完全无 Prometheus。**

### 7. BT + 云盘直链 P2SP 混合

`POST /tasks/:id/webseeds` —— 用 BEP-19 web seed 打通迅雷 CDN 直链。
**这是迅雷自家才有的能力，开源界完全无对标**（但不能自动从 .torrent 取，见 §3 #7）。

---

## 6. 我的整体判断

### 质量瓶颈不在设计，在**集成验证**

这是本轮最重要的结论。所有缺口**几乎没有一个是「设计错了」**：

| 缺口 | 设计对不对 | 问题在哪 |
|---|---|---|
| `retry.rs` | ✅ D25 设计完全正确 | 没接进调用链 |
| `sniffer` | ✅ 695 行实现完整 | daemon 不调用 |
| `web_seeds` | ✅ C++ 侧处理正确 | Rust 传空 |
| `Progress` 事件 | ✅ 变体+节流器都有 | 无发布者 |
| `QuarkProvider` | ✅ 实现完整 | serve.rs 不注册 |
| 账本续传 | ✅ **比 aria2 更严谨** | — |

**共同模式**：单元测试只测「模块独立行为」，
**没有端到端测试验证「调用方真的用了它」**。

**建议**：这几条都补**集成测试**，断言「用户可见效果」而非「函数返回值」：
- 429：让 mock 回 429，断言**任务最终成功**
- web_seeds：断言 `core.status()` 能看到 URL
- Progress：订阅 WS，断言**收到事件**
- Quark：断言 `GET /providers` 里有 quark

### 修复优先级（按 ROI）

| 优先级 | 项 | 工作量 | 收益 |
|---|---|---|---|
| 1 | 接线 `should_retry` + `Retry-After` + jitter | 半天 | 429/5xx 真实场景可用 |
| 2 | 生产代码发 `Progress`/`Speed` | 1-2 天 | WS 进度通道激活 |
| 3 | 装配 `QuarkProvider` | 半小时 | 文档声称的能力真实可用 |
| 4 | SFTP 补 mtime 指纹 | 5 行 | 消灭静默损坏 |
| 5 | `web_seeds` 接线 | 6 行 | magnet ws= 生效 |
| 6 | UI/CLI 批量操作 | 0.5 天 | 日常操作流 |
| 7 | `bt,ftp,sftp` 进 default feature 或安装包提示 | 1 小时 | 发布形态与宣称一致 |
| 8 | BT v2 | 大工程（改 FFI 契约 ih 类型） | 建议单独立项 |

---

## 7. 三个问题的最终答案

> **断点重连做得怎么样？**
**B**。HTTP 分段（A，优于 aria2）和 BT（A-）很强；
但 **SFTP 零指纹（C-）会静默损坏文件**、**HTTP 未知总长必从头无日志**、
**Provider 兜底完全不可续（F）**。

> **HTTPS 支持如何？**
**B+**。主干完整（rustls + 代理 + Cookie + 自定义头 + HTTP/2 + HLS/DASH），
但**固定根证书集**（企业自签失败）、**无全局 UA**、**无客户端证书**、**无 HTTP/3**。
另外 **FTP 完全不支持代理**（裸 TcpStream）。

> **还有哪些硬缺口？**
1. **状态码感知的重试退避整块未接线**（429 越冲越限流）—— 最该先修
2. **`Progress`/`Speed` 事件无人发布**（WS 进度通道是死的）
3. **BT v2-only 拒绝 + handle 泄漏**
4. **SFTP 零指纹**（静默损坏）
5. **`QuarkProvider` 从不装配**（文档说 ✅）
6. **`sniffer` 零调用**（迅雷分享链接直接报错）
7. **`web_seeds` 未接线**、**Provider 兜底不可续**
8. **ABBA 死锁 / cid_store 二分**（PR #114 里两个未修的 P0）
