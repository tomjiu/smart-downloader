# 断点续传深度评估（六协议矩阵）

> 评估对象：`tomjiu/smart-downloader` @ main（`cec67cb`）
> 方法：逐文件读码 + 抽样验证关键断言（非纯推测）
> **本文取代 `CAPABILITY-ASSESSMENT.md` §1 的早期版本** —— 早期版本对
> 「HTTP 未知总长可续传」「SFTP 有防线」两处判断**过于乐观**，此处修正。

---

## 0. 结论先行

**矩阵化评估后，续传能力评级从 A- 下调为 B。**

主干（HTTP 分段 / BT fastresume / FTP）确实强，
但**六个协议里有三个存在真实的续传缺陷**，其中 SFTP 会让用户拿到
**静默损坏的文件**。

| 协议 | 同进程恢复 | 跨重启恢复 | 持久化机制 | 缺陷 |
|---|---|---|---|---|
| **HTTP 已知总长 + Range** | ✅ | ✅ 段级 | `.part` + `.part.progress` | 无 |
| **HTTP 未知总长/非 Range** | ⚠️ 仅文件大小 | ❌ **必从头** | 仅 `.part` 长度 | 🔴 无日志 |
| **HLS** | ✅ | ✅ 段级 | `.part.hls-ledger` | 🟡 清单 token 变即从头 |
| **DASH** | ✅ | ✅ 段级 | `.part.dash-ledger` | 🟡 同上 + 只下视频轨 |
| **FTP** | ✅ | ✅ 段级 | `.part.progress` + MDTM | 🟡 MDTM 缺失即无指纹 |
| **SFTP** | ✅ | ✅ 段级 | `.part.progress` | 🔴 **零指纹** |
| **BT** | ✅ | ✅ piece 级 | `<ih>.fastresume` | 🟡 5min 窗口 + 计时重置 |
| **Provider 兜底** | ❌ | ❌ | 无 | 🔴 完全不可续 |

---

## 1. HTTP 已知总长 + 支持 Range：⭐ 做得最好

### 设计正确

`ledger.rs` 用**显式段账本**替代「文件长度」当进度源 —— 这是关键。
因为分段下载会 `set_len(total)` 预分配，中断后文件长度**恒等于 total**，
用长度当进度会产出静默空洞文件（G1 缺陷，已修）。

**7 步决策矩阵**（`ledger.rs:198-223`）：

| # | 条件 | 动作 |
|---|---|---|
| 1 | part 超过总长 | 重下 |
| 2 | 无账本 | 重下 |
| 3 | total 不一致 | 重下 |
| 4 | 指纹核对失败 | 重下 |
| 5 | 不支持 Range | 重下 |
| 6 | 段对齐校验失败 | 重下 |
| 7 | 其余 | 续传 |

**双指纹**（ETag + Last-Modified 各自独立核对），其中：

```rust
(Some, None) → 拒绝   // 指纹消失：宁枉勿纵
```

**这条尤其好** —— 服务器突然不返回 ETag 时不冒险续传。
比 aria2 保守（aria2 只靠 Last-Modified）。

**段对齐防篡改**（`validate_segments`）：`start % min_split == 0`、
`end = min(start+min_split, total) - 1`、`done.len() <= 100_000`。
账本被改坏就全量重下，**不信任任何来路不明的「已完成」声明**。

### 但有三个缺口

**① 不发 `If-Range` / `If-Match`**

段请求只有裸 `Range: bytes=start-end`（`download.rs:300-314`）。
若服务器内容在探测后、段请求前变了，**没有协议层防护**，只能靠账本指纹在 add 时核对。

**② 不校验 `Content-Range` 的 `/TOTAL`**

```rust
// download.rs:319-345 —— 只校验起始字节
parse_content_range_start(...)  // 只取 start
```

服务器返回 `206 bytes 1000-2000/9999`（总长已变），只要起始字节对就照写。
**这是「旧前缀 + 新尾部」的潜在通路**。

**③ 校验失败的「降级接受」**

```rust
// engine.rs:844-859
// 主源两次 + 备用源/隔离池耗尽 → 降级接受
finish(&inner, &tid, EngineState::Completed, Some(warn));  // ← 标 Completed！
```

**哈希不符的文件仍被标为 Completed**，只在 `error` 字段留 warning。
**前端如果只看 state 不看 error，用户拿到损坏文件却显示「已完成」。**

> 注：batch10 已把它改成 `EngineState::Error`（我报的 P2-19），
> 但**这个修复在 PR #114 里还没合并**。

---

## 2. 🔴 HTTP 未知总长 / 非 Range：必从头，且完全静默

### 两条独立的「删 .part」路径

**路径 A：add 时因 `total == 0` 直接删**（`engine.rs:1240-1243`）

```rust
if total == 0 {
    // 未知长度（探测无 Content-Length/Content-Range）：无法分段，
    // 旧凭据不可用（同 legacy 语义：无 total 不续传）
    remove_part(&part0);        // ← 先删
}
```

注释诚实承认「无 total 不续传」。

**路径 B：单流循环里 200 响应 → truncate**（`engine.rs:506-511`）

```rust
let mut f = std::fs::OpenOptions::new()
    .create(true)
    .write(true)
    .truncate(mode == 2)     // ← mode==2 即服务器忽略 Range
    .open(&part)
```

### 实测：整个函数零日志

```
download_loop_unknown 函数范围：engine.rs:428-618（190 行）
grep println|tracing|warn|info|log  →  **空**

remove_part 实现（engine.rs:1000 附近）：
    let _ = std::fs::remove_file(part);
    remove_credentials(part);
    →  无日志
```

**用户的 1GB 下载重启后白下，界面没有任何提示。**

对比：分段路径作废重下**有** `println!("[httpdl] 断点续传 ...")` 和
`println!("[httpdl] 账本作废 ...")`。单流路径什么都没有。

### 触发场景（都不罕见）

- CDN 动态生成内容（无 `Content-Length`）
- 服务器配置忽略 `Range`（部分老服务器 / 防盗链中间层）
- 探测返回 `416` 且不带 `Content-Range`
- 探测返回 `206 bytes 0-0/*`（总长未知）

### 建议

**最小改动**：在 `mode == 2` 且 `.part` 已存在时打一条 warn：

```rust
if mode == 2 && start > 0 {
    tracing::warn!(
        "任务 {tid}: 服务器忽略 Range，已完成 {} 字节作废，从头重下",
        start
    );
}
```

**更好的做法**：意识到「无 total 不续传」是**当前实现限制**而非协议限制 ——
单流路径已经有 `bytes={size}-` 的能力，`total == 0` 时不该删 `.part`。

---

## 3. 🔴 SFTP：零内容指纹（最严重）

### 对比 FTP 的实现

**FTP**（`ftp.rs:968-970`）：
```rust
l.total == total
  && validate_segments()
  && fingerprint_ok(&l.last_modified, &mdtm)    // ← 有 MDTM 指纹
```

**SFTP**（`sftp.rs:520`）：
```rust
let loaded = ledger::load(&ledger_path)
    .filter(|l| l.total == total && l.validate_segments());   // ← 只有总长
```

**账本快照里硬编码 None**（`sftp.rs:629-636`）：
```rust
let snapshot = ledger::Ledger {
    version: ledger::LEDGER_VERSION,
    total,
    min_split: eff_min_split,
    etag: None,
    last_modified: None,       // ← 永远 None，连探测都没做
    done: m.done_ranges().to_vec(),
};
```

### 后果

**远端文件被替换但大小相同 → 直接续写旧前缀，产出「旧前缀 + 新内容」的混合文件，无任何检测。**

SFTP 协议本身能拿到 `attrs.mtime` / `attrs.size`（`stat` 已经在 `add` 里调用了，
`sftp.rs:910-933`），**拿到 mtime 只是几行代码的事**。

### 严重性

这是**当前最严重的续传缺陷**：唯一会让用户**静默拿到损坏文件**且毫无提示的路径。

---

## 4. 🟡 HLS / DASH：能用，但指纹脆弱

### 机制

- HLS：`.part.hls-ledger`（清单 **sha256 指纹** + 段数 + 字节数）
- DASH：`.part.dash-ledger`（同构）

恢复条件严格（`hls.rs:421-435`）：version + 清单指纹 + 段数 +
**`.part` 长度 == 账本 bytes_done**，任一不符 → 删 `.part` 从头。

**顺序 append + 前缀式段号** → 最坏丢 1 段，跨重启有效。

### 两个问题

**① 清单 URL 带时效 token 时，指纹每次都变**

```rust
// hls.rs:339-346 —— 指纹 = base_url + 全清单文本
```

很多 CDN 的 m3u8 带 `?token=xxx&expires=yyy`，**每次重启指纹都不同 →
静默从头下载**（无日志）。

**② 全程无内容哈希**：`hls.rs` / `dash.rs` 全文无 verify 调用。
段内容变了只能靠清单指纹间接发现，清单不变但 CDN 内容变 → 无从检测。

**③ DASH 只下视频轨（设计明示的边界，非缺陷）**

`dash.rs:9` 注释明确写：

> （音视频分轨清单不混流，产出无音轨视频文件——明示边界）

选流逻辑（`dash.rs:489-490`）只挑 `contentType`/`mimeType` 为 `video` 的
`AdaptationSet`。**文档诚实标注了这个限制**，但从用户角度仍是功能缺口
（下载 HLS 有声音、下载 DASH 没声音）。

---

## 5. ✅ BT：设计最好，但有窗口

### 三重保存

```
pause/remove 时保存 → 周期保存（5min）→ 优雅退出前保存
```

实现（`bt.rs:366-460`）：`request_save_resume` → 等 RESUME alert(≤3s) →
`take_resume_data` → **唯一 tmp + 0600 + rename**。

### 恢复

`persistence.rs:146-357 restore_from` → `engine.add()` → BT `add()` 按
infohash 找 `.fastresume` → 有则 `add_torrent_resume`（恢复 piece 位图，免全盘 checking）。

### 三个空洞

| 空洞 | 影响 |
|---|---|
| `kill -9` / 断电只靠 5min 周期快照 | 最多丢 5 分钟 piece（libtorrent 重开仍会 re-check 磁盘） |
| `save_fastresume_impl` 3s 超时即放弃且仅 warn（`bt.rs:434-437`） | 退出时若 alert 链路已断 → 静默丢凭据 |
| magnet 的 resume 数据**不含 info dict** | 重启后 metadata 需重新抓取 |

**做种计时重置**（`persistence.rs:317-320`）：`seeding_since = Instant::now()`，
**`max_seeding_time` 被反复重置**。分享率（all-time 计数）随 fastresume 保留。

**`xunlei` / NAS 引擎完全不支持 resume**：
- `xunlei_engine.rs:326-328` 明确不支持 fastresume
- `nas_remote.rs:18` 明确不支持 pause/resume

---

## 6. 🔴 Provider 兜底：完全不可续

### 三层断裂

**① `update_sources` 是空实现**

```rust
// ops.rs:4158-4161
async fn update_sources(&self, _task_id: &str, _urls: Vec<String>) -> Result<(), SinkError> {
    // v1：直链不续期
    Ok(())
}
```

而 `coordinator.rs:206-222` 的 `refresh_links` 三次重试拿到的 URL **根本装不进去**。

**② 只能 resubmit = 重新离线**

`coordinator.rs:224-232` 重试路径 = `provider.submit()` 重新提交云盘离线任务。
夸克其实**每次都能重签直链**（`quark/share.rs:354-361`），
迅雷的 `refresh_links` 返回 `Ok(None)`（`xunlei/provider.rs:371-377`）
—— **能力存在但链路上没接**。

**③ 重启后中间态全丢**

- `poll_engine_states` 跳过 provider（`ops.rs:3840-3841`）
- `ops.rs` 全文**未找到** `FallbackProvider` 写入点
- `cloud_task_id` / `fids` 全在内存（`xunlei/provider.rs:254-260`、`quark/share.rs:313-319`）

### 还有个 bug

`ops.rs:4148` 的错误文案写「直链传输超时(**60s**)」，
**实际 deadline 是 600s**（`ops.rs:4120`）。**误导排查。**

---

## 7. 三类风险清单（用户视角）

### A. 静默从头重下（用户不知情）

| # | 场景 | 位置 | 有无提示 |
|---|---|---|---|
| 1 | HTTP 未知总长 / 忽略 Range | `engine.rs:1240` / `:509` | ❌ **无** |
| 2 | 探测 416 无 Content-Range | `engine.rs:1240-1243` | ❌ 无 |
| 3 | 账本任一不合规 | `engine.rs:1258-1261` | ⚠️ 一行 println |
| 4 | HLS/DASH 清单指纹变 | `hls.rs:431` / `dash.rs:964` | ❌ **无** |
| 5 | FTP/SFTP 账本不符 | `ftp.rs:971` / `sftp.rs:521` | ❌ 无 |
| 6 | 校验失败「重下 1 次」 | `engine.rs:762-766` | ⚠️ 有 |

### B. 会产出损坏文件

| # | 场景 | 严重性 |
|---|---|---|
| 1 | **SFTP 无指纹 + 同大小替换** | 🔴 最严重 |
| 2 | HTTP 无 ETag/LM + 用户未给哈希 | 🟡 |
| 3 | FTP 无 MDTM + 用户未给哈希 | 🟡 |
| 4 | 哈希失败「降级接受」标 Completed | 🔴（batch10 已修但未合并） |
| 5 | HLS/DASH 无内容哈希 | 🟡 |

### C. 进度丢失

| # | 场景 |
|---|---|
| 1 | HTTP 单流重启后 `done` 归零（同时数据也重下） |
| 2 | 引擎 task 表全内存，`done` 不落 tasks.json |
| 3 | BT kill -9 最多丢 5min piece |
| 4 | BT 做种时长重启重置（`max_seeding_time` 反复触发） |
| 5 | `pause → resume → autosave 前崩溃` → 重启仍 Paused |
| 6 | Provider 兜底中间态重启即丢 |

---

## 8. TOP 5 缺口（按修复价值排序）

### 1. 🔴 SFTP 补远端指纹（工作量最小，收益最大）

`add` 里已经 `stat` 了（`sftp.rs:910-933`），只需：

```rust
// 拿到 attrs.mtime 后写入账本
let snapshot = ledger::Ledger {
    ...
    last_modified: Some(mtime.to_string()),   // ← 改这里
    ...
};
// 恢复条件加一行
.filter(|l| l.total == total && l.validate_segments()
         && fingerprint_ok(&l.last_modified, &current_mtime))
```

**约 5 行代码，消灭最严重的损坏路径。**

### 2. 🔴 HTTP 未知总长的「静默从头」加可观测性

最小改动是在 `mode == 2 && start > 0` 时打 warn。
更进一步：`total == 0` 时**不要删 `.part`** —— 单流路径本身支持
`Range: bytes={size}-`，删掉纯属浪费。

### 3. 🟡 「无强指纹也放行 + 降级接受」的组合

- 前者（`None, _ → true`）是有意取舍，但可以让它**仅在用户提供哈希时放行**
- 后者 batch10 已修（改成 Error），但**需要合并**

### 4. 🔴 Provider 兜底接线

- `update_sources` 实现（夸克已能重签直链，接上就有）
- 加 provider 任务轮询路径
- 修 600s 超时的文案错误（写成 60s）

### 5. 🟡 BT 两处小空洞

- `save_fastresume_impl` 3s 超时只 warn → 应重试或至少发事件
- 做种计时重置 → 应持久化 `seeding_since` 的墙钟值

---

## 9. 最终评级

| 维度 | 评级 | 理由 |
|---|---|---|
| HTTP 分段续传 | **A** | 显式账本 + 双指纹 + 段对齐防篡改，业界领先 |
| HTTP 单流续传 | **D** | 必从头且无任何提示 |
| FTP 续传 | **B+** | 段级 + MDTM，但 MDTM 缺失即裸奔 |
| SFTP 续传 | **C-** | 段级机制完整，但**零指纹**会静默损坏 |
| BT 续传 | **A-** | 三重保存 + 做种态保留，两个小窗口 |
| HLS/DASH | **B** | 段级账本好，清单指纹脆弱 |
| Provider 兜底 | **F** | 完全不可续 |
| **综合** | **B** | 主干优秀，但三个协议有真实缺陷 |

**一句话**：这个下载器的 HTTP 分段续传设计比 aria2 更严谨，
但 SFTP 的零指纹是**必须修**的严重缺陷 —— 它会静默损坏文件，
而用户以为是续传成功的。
