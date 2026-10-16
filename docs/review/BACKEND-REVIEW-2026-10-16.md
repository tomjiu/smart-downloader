# 后端代码审查报告（重构前基线）

> 审查对象：`tomjiu/smart-downloader` @ `0520e25`（main，PR #113 之后）
> 审查日期：2026-10-16
> 方法：三路独立深度审查（并发正确性 / 下载引擎 / 数据解析）+ 关键项实证复现
> 代码规模：约 52,000 行 Rust，1180 个测试

---

## 0. 执行摘要

**已有修复确实有效** —— 三路审查均逐条验证了 V1–V13 / batch1–9 / E1–E33 的修复项，**未发现复发**。锁纪律尤其干净：锁序反转、`select!` 取消安全、无界 channel、文件句柄泄漏均**未发现**问题。

**但存在 3 个 P0，其中 1 个已实证复现崩溃。**

| 编号 | 问题 | 严重度 | 状态 |
|------|------|--------|------|
| **P0-1** | 多文件 `.torrent` 解析除零 **panic** | 🔴 P0 | **已实证复现** |
| **P0-2** | `pause_intents` guard 跨 FFI 调用 | 🔴 P0 | 代码确认 |
| **P0-3** | `poll_ring` 互吞 alert → 任务永久卡住 | 🔴 P0 | 代码确认 |
| **P0-4** | 无摘要 + 预分配 = 静默交付空洞文件 | 🔴 P0 | 代码确认 |
| **P0-5** | 双源无 ETag 时跨源混拼 | 🔴 P0 | 代码确认 |

---

## 1. P0：必须修

### P0-1 多文件 `.torrent` 解析除零 panic ★已复现

**位置**：`crates/daemon/src/state/bt_alerts.rs:117` + `:215`

**复现**（`repro/p0repro`，见 [P0-EVIDENCE.md](P0-EVIDENCE.md)）：

```
=== 对照组: 非法键序（现有测试口径）===  → ok
=== 复现 P0: 真实键序多文件 torrent ===
thread 'p0_divide_by_zero' panicked at 'attempt to divide by zero'
test result: FAILED. 1 passed; 1 failed
```

**根因**：

```rust
// L117  扫到 "files" 键时立即解析，此刻 piece_length 仍为初值 0
files = parse_file_list(&b[i + 1..list_end], piece_length)?;
// L168  plen = 0
let plen = piece_length as u64;
// L215  除零
let piece_offset = (total_size / plen) as usize;
```

`piece_length` 只在扫到 `piece length` 键时赋值，而有效性检查（L128）在循环**之后**。

**为什么测试没抓到**：bencode 规范要求按键名字典序排列（`files` < `name` < `piece length` < `pieces`），所有主流生成器（mktorrent / libtorrent / Transmission）都遵守。但 `state_tests/torrent_tests.rs:90-112` 的 fixture **手工把 `piece length` 写在 `files` 之前** —— 非法键序，因此 `plen` 被正确赋值，**测试恒绿**。

**影响**：
1. `add` 路径同步可达 → 导入真实多文件 .torrent **直接 panic**
2. 即使不 panic，`piece_offset`/`piece_count` 全 0 → `ops.rs:1205` 取空切片 → **完成位图归零（静默损坏）**

**修复**：把 `parse_file_list` 调用与 piece 偏移计算**延后到扫描结束**；补「按字典序写键」的 fixture（关键，否则修了还会再退化）。

---

### P0-2 `pause_intents` guard 跨越 FFI 调用

**位置**：`crates/daemon/src/bt.rs:236-243`

```rust
pub fn set_pause_intent(&self, id: &str, intended: bool) {
    let mut m = self.pause_intents.lock();      // ← guard 获取
    if intended {
        let done = self.core.status(id)          // ← FFI 调用（阻塞）
            .map(|s| s.downloaded.max(0) as u64)
            .unwrap_or(0);
        m.insert(id.to_string(), done);
```

**问题**：guard 跨 `core.status()`（libtorrent FFI，同步阻塞）。而 `enforce_pauses()`（`bt.rs:258`）在 **500ms alert 心跳线程**抢同一把锁。心跳线程是**唯一 alert 消费者**，被占住则整轮停摆 → 其它任务 fastresume **集体 3s 超时**。

**加重因素**：此路径从 HTTP handler 同步调用（`bt.rs:567`、`613`），不在 `spawn_blocking` 内，**占的是 tokio worker**。

**修复**：先取 status 再拿锁（两行改动）。

---

### P0-3 `poll_ring` 兜底路径互吞 alert

**位置**：`crates/daemon/src/bt.rs:395-408`

```rust
let poll_ring = !loop_active.load(Ordering::SeqCst);   // ← 循环外一次性采样
while Instant::now() < deadline {
    if poll_ring {
        if let Ok(alerts) = core.pop_alerts(256) {     // ← 消费整个 ring
            for a in alerts {
                if a.kind == AlertKind::Resume && a.is_resume_ready() { ... }
                // ↑ 其它 alert（State/Error/Finished）被直接丢弃
```

**问题**：`poll_ring` 在入口采样。若此刻 `alert_loop_active` 仍为 false，blocking 线程 `pop_alerts(256)` 消费整个 ring，**但循环体只处理 `Resume`** → 其它任务的终态 alert 丢失 → **永久停在 `Downloading`**。

**根因（结构性）**：`bt_events.rs:52-54` 把 `mark_alert_loop_active()` 包在 `if let Some(g) = &guard` 里 —— `guard = None` 时常驻循环照样跑 `core.pop_alerts(128)`，但标志**永不置位** → 所有 fastresume 恒走 `poll_ring` 自消费 → **持续态双消费者**。

**修复**：`mark_alert_loop_active()` 移出 guard 守卫；`poll_ring` 每轮重新采样（或删除该分支）。

---

### P0-4 无摘要 + 预分配 = 静默交付空洞文件

**位置**：`crates/httpdl/src/download.rs:111` + `crates/httpdl/src/engine.rs:739` + `core/src/session/output.rs:123`

```rust
// download.rs:111  立即把 .part 撑到 total（稀疏全零）
f.set_len(total)?;

// engine.rs:739   无摘要 = 恒通过
(None, None, None) => Ok(true),

// output.rs:123   只比长度 —— 而预分配已让长度天然相等
if pl != expected_size { ... }
```

**问题**：`set_len` 让长度检查失效，而**无摘要时没有任何内容防线**。

**触发路径**（真实可达）：
1. 段内 `write_all` 成功但页缓存未落盘时进程崩溃 → 账本已 `ledger::save`（段完成即写）→ 重启 resume 后该段不再重下 → **交付空洞文件且状态 `Completed`**
2. 磁盘满场景：`set_len` 是稀疏操作，磁盘满时仍能成功 → 配合无摘要 → 交付全零

**FTP/SFTP 更彻底**：`ftp.rs` / `sftp.rs` **全文无任何内容校验调用**，只有长度比对（`ftp.rs:1344`）。

**修复**：无摘要时至少做「非全零」抽样校验；或去掉 `set_len` 改用 extent 记录。这是**设计层缺口**。

---

### P0-5 双源均无 ETag 时跨源混拼

**位置**：`crates/httpdl/src/engine.rs:1632`

```rust
let etag_changed = probe.etag.is_some() && t.etag.is_some() && probe.etag != t.etag;
if etag_changed {
    remove_part(...);  t.done = 0;  t.gen += 1;   // ← 只有这里才作废旧账本
}
t.mirrors = urls.clone();   // ← 无条件替换源表
```

**问题**：`etag_changed` 要求**双方都 `Some`**。而**大量 CDN 不发 ETag** → 双 `None` → `false` → **不 gen+1、不删账本、不重置 done** → 直接用**旧源的段账本续传新源**。

这正是 G2（跨源混拼）要根治的场景，在无 ETag 源上**完全敞开**。

**附带**：`delete_task_files`（`engine.rs:982`）只枚举 `gen 0..=3`，**gen ≥ 4 的 `.part` + `.progress` 永久泄漏**。

**修复**：`etag_changed` 缺省改为「无法证明相同即作废」。

---

## 2. P1：应该修

| 编号 | 问题 | 位置 |
|------|------|------|
| P1-1 | 限速器**持锁 sleep** → 跨段串行排队，实际速率低于设定值 | `rate.rs:103-108` |
| P1-2 | `RateSample` 短窗口丢字节 → 速率系统性低估 | `rate.rs:130-148` |
| P1-3 | 限速热调不清 `next` → 旧速率债务继续生效 | `rate.rs:72-74` |
| P1-4 | FTP PASV 六元组无范围校验 → **SSRF 防护失效** | `ftp.rs:701-704` |
| P1-5 | FTP `is_terminal` 的 `starts_with("5")` 包含 `550` → 停用二分降粒重试 | `ftp.rs:1334-1336` |
| P1-6 | SFTP 靠**英文文案**判永久错误 → 换语言即退化为长退避空耗 | `sftp.rs:173-176` |
| P1-7 | HLS/DASH 段账本**固定 `.tmp` 名**（`ledger.rs` 已修，这两处漏改） | `hls.rs:583`、`dash.rs:1070` |
| P1-8 | `attempt_completion_action` **持 tasks 锁 sleep 250ms** 后退出 | `ops.rs:57-72` |
| P1-9 | RSS 添加**持锁做整表 JSON 落盘** | `rss.rs:664-687` |
| P1-10 | RSS **未处理条目无上限**，单调增长撑爆内存 | `rss.rs:895-909` |
| P1-11 | `alert_loop_active` 单向置位 → abort 后保存链路必死 | `bt.rs:284-287` |
| P1-12 | `core.pause` 失败**不回滚暂停意图** → 僵尸条目永久 FFI 重压 | `bt.rs:566-583` |
| P1-13 | `let _ =` 吞掉 fastresume 写盘失败与 panic | `bt.rs:578-581`、`609-612` |
| P1-14 | fastresume 3s deadline **不覆盖磁盘写** → 慢盘可无限阻塞 | `bt.rs:450-458` |
| P1-15 | `Disconnected` 分支**不可达**的固定 3s 忙等 | `bt.rs:421-428` |

---

## 3. P2：可择机修

| 编号 | 问题 | 位置 |
|------|------|------|
| P2-1 | **`DELETE /security/bans` 带 body** → HTTP 语义违规，AI/多数客户端不支持 | `http.rs:2584-2588` |
| P2-2 | **API 动词不一致**：26 个 POST，改名/打标签/改限速都该用 PATCH | `http.rs:2560-2615` |
| P2-3 | tokenless 模式 `Host` 头缺失时 `None => true` 直接放行 | `http.rs:2688` |
| P2-4 | token 经 **query 传递**（`/events/stream`）→ 会进日志/历史/Referer | `http.rs:2651-2661` |
| P2-5 | `ops.rs` **4534 行单 impl 块**，69 个方法 | `state/ops.rs` |
| P2-6 | **4 个 `add_*_task_opts` 高度重复**（http/link/ftp/sftp） | `ops.rs:794/1367/1587/1754` |
| P2-7 | `DaemonState` **30+ 把独立锁**，锁序靠人工保证 | `state.rs:577-696` |
| P2-8 | loop 任务全部「**先 sleep 后工作**」→ 启动后有盲区 | `bt_events.rs:57`、`serve.rs:411` |
| P2-9 | 长驻循环**无取消路径**，`JoinHandle` 被丢弃 | `serve.rs:409-513` |
| P2-10 | 日志 typo：`"ttpdl]"` 应为 `"[httpdl]"` | `engine.rs:840` |
| P2-11 | `active_slot_counts` 双锁嵌套（顺序一致无死锁，但 N 次加解锁） | `ops.rs:387-388` |
| P2-12 | `rss.rs:687` 的 `st.feeds.last().unwrap()` 不变式靠调用图巧合 | `rss.rs:687` |
| P2-13 | `.corrupt` 固定名覆盖 → 二次损坏静默覆盖上次证据 | `rss.rs:133-134` |
| P2-14 | RSS 去重键仅 URL 精确串 → `?utm=1` 视为两个 feed | `rss.rs:666` |
| P2-15 | DASH 段数**无上限**（HLS 有） | `dash.rs:835` |
| P2-16 | HLS 段用 `resp.bytes()` **全量驻内存**（DASH 已改流式） | `hls.rs:484` |
| P2-17 | `finalize_part` 先删 dest，若 rename 失败 → **数据丢失** | `engine.rs:943-949` |
| P2-18 | 206 缺 `Content-Range` 时**宽容放行**（与 unknown-len 路径策略相反） | `download.rs:319-345` |
| P2-19 | 降级接受把**哈希不符**的文件标 `Completed` | `engine.rs:844-865` |

---

## 4. 明确「未发现」的类别

审查价值的一部分是**排除**。以下类别经逐条核查确认无问题：

| 类别 | 结论 |
|------|------|
| 锁序反转死锁 | **未发现**。方向恒为 `persist_lock→tasks`、`tasks→slot_reservations`，无环 |
| 锁内调用重入同锁 | **未发现**。历史 Bug B 已在 `bt_alerts.rs:367-370`、`ops.rs:3919-3925` 修复 |
| `select!` 取消安全 | **未发现**（全域仅 `http.rs:1529` 一处，两分支均安全） |
| 无界 channel | **未发现**（唯一 `unbounded` 在 `bt.rs:377`，元素级有界且有清理） |
| 段边界 off-by-one | **未发现**。三处口径完全一致 |
| 并发写同一文件 | **未发现**。gen/epoch 双闸门有效 |
| >4GB u64 溢出 | **未发现**。逐处核对全部 `as` 窄化 |
| FTP 命令注入 | **未发现**。CR/LF/NUL 三层拦截 |
| `std::sync::Mutex` 混用 | **未发现** |
| 文件句柄泄漏 | **未发现** |

---

## 5. 值得注意的模式

### 5.1 「审计修复」本身成为负债

`ops.rs:34-39` 注释论证「execute 锁内停留亚毫秒、无死锁风险」，但 batch5-P2 **自己给 `exit` 分支加了 250ms 睡眠**（`lifecycle.rs:647`）—— 后一批修复推翻了前一批注释的前提，**注释未更新**。

**建议**：审计修复应同时更新受影响的历史注释。

### 5.2 测试固件与真实数据形态不一致

P0-1 是最典型的实例：**测试恒绿，线上必崩**，差异仅在 bencode 键序。

**建议**：涉及外部格式（bencode / TOML / MPD）的测试，fixture 应**从真实样本生成**而非手工拼接。

### 5.3 批修复的「漏改」

`ledger.rs` 在 batch8 已修「固定 tmp 名 + 0600」，注释写明理由，但 **HLS/DASH 两处同款代码漏改**。

**建议**：同类模式修复时应全仓 grep 确认无遗漏。

---

## 6. 修复优先级建议

| 顺序 | 项 | 理由 |
|------|-----|------|
| 1 | **P0-1 除零 panic** | 导入真实种子即崩；修复量小；测试盲区明确 |
| 2 | **P0-2 guard 跨 FFI** | 改两行；影响全任务 fastresume |
| 3 | **P0-3 poll_ring 互吞** | 任务永久卡住；一次提交可闭合（含根因） |
| 4 | **P0-5 无 ETag 跨源混拼** | 静默数据损坏，主流 CDN 场景 |
| 5 | **P0-4 空洞文件** | 设计层缺口，需先定策略 |
| 6 | P1-4 FTP PASV 校验 | 安全（SSRF 防护失效） |
| 7 | P1-1 限速器持锁 sleep | 性能 + 取消语义 |
| 8 | P1-9/P1-10 RSS 持锁落盘 + 无上限 | 唯一持续劣化的缺陷 |
| 9 | P2-1/P2-2 API 语义 | 影响"AI 可调用"目标 |

---

## 附：复现工程

```
repro/
├── P0-EVIDENCE.md          复现证据
└── p0repro/                独立 crate（已 exclude 出 workspace）
    ├── Cargo.toml
    └── src/lib.rs          代码提取自 bt_alerts.rs L5-161/163-254/449-538
```

运行：
```powershell
cd repro\p0repro
cargo test -- --nocapture
```
