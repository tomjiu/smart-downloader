# batch10 补审报告（剩余文件）

> 审查对象：PR #114 batch10-a ~ g 相对前一轮审计快照（`pr114-review` → `pr114-latest`）的增量
> 审查日期：2026-10-19
> **性质：只做审计，未修改任何代码**

---

## 0. 结论

**batch10 质量整体高于前一轮。** 大部分修复比我原报告的建议更彻底，还修了几个我没发现的缺陷。

| 等级 | 数量 | 明细 |
|---|---|---|
| 🔴 必须修 | 1 | `cid_store` 二分边界回归（**已在上一份报告提交**） |
| 🟡 建议修 | 3 | `bt.enabled` 默认值破坏性变更、`set_rate_kb_s` 热调竞态、`piece_length` 注释错误 |
| 🟢 平台缺口 | 1 | `verify_nonempty_blocks` 在 Windows 为 no-op |
| ✅ 修得好 | 8 | 见 §2 |

---

## 1. 🟡 建议修

### 1.1 `BtCfg::default().enabled` 从 false 改为 true —— 破坏性变更

**位置**：`crates/daemon/src/config.rs:240`

```rust
impl Default for BtCfg {
    fn default() -> Self {
        BtCfg {
            enabled: true,   // ← 原 false
            ...
```

**修复意图正确**：用户只写 `[bt] max_upload_kb_s = 100` 时，`enabled` 不应退化为 false。
这个缺陷确实存在（我原报告 P1-b）。

**但把「部分表」和「完全无 `[bt]` 段」混为一谈了**：

| 场景 | 期望 | 现状 |
|---|---|---|
| 写了 `[bt] max_upload_kb_s` 但没写 enabled | 应继承 true ✅ | true |
| 完全没写 `[bt]` 段 | 应保持 false ❌ | **true** |

**实际影响（已修正我的初判）**：`xunlei` 是可选 feature（`daemon/Cargo.toml` 无 `default = [...]`），
所以「与 `xunlei.enabled` 互斥导致启动失败」只影响显式 `--features xunlei` 的用户：

```rust
// serve.rs:242
if cfg.bt.enabled {
    return Err(ServeError::Engine(
        "配置 [bt].enabled 与 [xunlei].enabled 互斥（两引擎共用 BT 槽位）：请仅启用其一"
    ));
}
```

只写 `[xunlei] enabled = true`、不写 `[bt]` 段的用户 → **升级后启动失败**。

**定性**：P2（非默认构建路径的破坏性变更）。
**建议**：在 CHANGELOG 标注 BREAKING，或在 `serve.rs` 的互斥检查里区分
「显式配置的 bt.enabled」与「Default 提供的 bt.enabled」。

---

### 1.2 `set_rate_kb_s` 热调在并发窗口下失效

**位置**：`crates/httpdl/src/rate.rs`（P1-3 修复）

```rust
pub fn set_rate_kb_s(&self, kb_s: u32) {
    let new_rate = kb_s as u64 * 1024;
    let old = self.inner.rate.swap(new_rate, Ordering::Relaxed);
    if old != new_rate {
        *self.inner.next.lock() = Instant::now();   // 清空旧债务
    }
}
```

**但 `wait_inner` 的 `rate` 读取与 `next` 推进不在同一临界区**：

```rust
let rate = self.inner.rate.load(...);      // ← 锁外读
let dur = Duration::from_secs_f64(n / rate);  // ← 锁外算（用旧 rate）
let mut next = self.inner.next.lock();     // ← 锁内
*next = target + dur;                      // ← 用旧 dur 推进
```

**时序**（`repro/p0repro/src/round15.rs` 已实证）：

```
T1 wait_inner: 读 rate=1MB/s → dur=4s     （锁外）
T2 set_rate(100MB/s): next = now           （清债务 ✓）
T1 wait_inner: 拿锁 → next = now + 4s      ← ❌ 旧债务复活
```

实测：热调到 100 MB/s 后，第一块仍被旧债务压制 **4.0 秒**。

**定性**：P3 体验级 —— 不是数据错误，只是一次性延迟，且热调非高频操作。
**建议**：把 `rate.load()` 移入临界区（代价：一次 atomic load，纳秒级）。

---

### 1.3 `piece_length` 上限注释理由错误

**位置**：`crates/core/src/torrent_meta.rs:65,291`

```rust
/// 非法值会传染给下游除法/偏移计算。上限与 libtorrent 一致（1<<24）。
const MAX_PIECE_LEN: i64 = 1 << 24;   // 16 MiB
```

**核实结果：「与 libtorrent 一致」不成立。**

libtorrent `load_torrent_limits`（官方文档）：
```cpp
struct load_torrent_limits {
   int max_buffer_size  = 10000000;
   int max_pieces       = 0x200000;
   int max_decode_depth = 100;
   int max_decode_tokens= 3000000;
   int max_duplicate_filenames = 500;
   int max_directory_depth = 100;
};
```
**没有任何 piece_length 上限字段。**

外部证据：
- [libtorrent issue #8139](https://github.com/arvidn/libtorrent/issues/8139) "no support for piece length of 2147483648"（2^31 ≈ 2 GiB）
- [PR #7736](https://github.com/arvidn/libtorrent/pull/7736) "raise the max piece size to **512 MiB**"

**实际影响**（`repro/p0repro/src/round14.rs`）：

```
16 KiB ~ 16 MiB  → ✅ 通过（覆盖主流）
32 MiB           → ❌ 拒绝
64 MiB           → ❌ 拒绝
512 MiB          → ❌ 拒绝
```

16 MiB 是**合理的工程选择**（主流 GUI 自动上限就是 16 MiB），但：
- 32/64 MiB 的合法种子（部分私有站、大文件手工创建）被当作「非法输入」硬失败
- 注释误导后续维护者以为这是 libtorrent 约束

**定性**：P3 文档/边界。
**建议**：改注释说明「本项目工程选择」，或把上限提到 64 MiB / 512 MiB 对齐 libtorrent。

---

## 2. ✅ 修得好的

| # | 项 | 位置 | 评价 |
|---|---|---|---|
| 1 | `lockfile.rs` unlink 竞态 | `daemon/src/lockfile.rs` | **标准解法** —— Drop 只 `unlock` 不删文件，且**同步更新了测试语义**（`lock_file_removed_on_drop` → `lock_file_survives_drop_but_unlocks`） |
| 2 | `frame.rs` 越界回绕 | `btcore/src/xudt/frame.rs` | `checked_add` + 复用 `end` 变量，正确 |
| 3 | `ftp.rs` PASV 值域 | `httpdl/src/protocol/ftp.rs` | 截断**前**校验 `>255` + 端口溢出，正是 SSRF 防护的正确位置 |
| 4 | `torrent_meta` P0-11/12 | `core/src/torrent_meta.rs` | 三处修复 + **三个回归测试**（`invalid_piece_length_rejected` 覆盖 5 个非法值） |
| 5 | `rate.rs` P1-1/P1-2 | `httpdl/src/rate.rs` | 锁内 sleep 消除 + **短窗口基线不推进**（修了我没发现的「速率系统性低估」） |
| 6 | `login_page.rs` P0-7 | `provider/src/xunlei/login_page.rs` | **比我审计的版本更严格**：精确字面量 `127.0.0.1` 而非 `is_loopback()` 全段；Host 缺失从「放行」改 **fail-closed**；带真 HTTP e2e 测试 |
| 7 | `engine.rs` `finalize_part` | `httpdl/src/engine.rs` | **修了预删 dest 的数据丢失窗口**（P2-17）；rename 失败走 copy 覆盖 + 校验 + 保留源 |
| 8 | `engine.rs` `0..=3` → `0..=255` | `httpdl/src/engine.rs` | **修了 gen≥4 的 `.part` 永久泄漏**（我漏掉的） |

**特别说明 §2.7**：原实现 `remove_file(dest)` 后再 `finalize_to`，若 rename 跨卷失败且 copy 也失败，
旧 dest 已被删除 → 数据丢失。batch10 改为直接 `rename`（Unix/Windows 均原子覆盖），
失败才 copy 兜底且保留源文件。**这个修法比我的建议更完整。**

---

## 3. 🟢 平台能力缺口

### `verify_nonempty_blocks` 在 Windows 上恒返回 `Ok(true)`

**位置**：`crates/httpdl/src/verify.rs`

```rust
pub fn verify_nonempty_blocks(path: &Path) -> io::Result<bool> {
    let md = std::fs::metadata(path)?;
    if md.len() == 0 { return Ok(true); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if md.blocks() == 0 { return Ok(false); }   // 完全稀疏
    }
    Ok(true)   // ← Windows 走到这里
}
```

`md.blocks()` 是 `std::os::unix::fs::MetadataExt` 专属 → **Windows 上这段不编译**。

**而项目桌面主战场是 Windows**（Tauri 2 / `desktop/src-tauri`）。

**实测（`repro/p0repro/src/round16.rs`）**：

```
[空洞文件] set_len(16MB) 不写 → 长度 16777216（长度检查通过）
  Unix:    blocks = 0     → 能识别 ✅
  Windows: blocks = None  → 无法识别 ❌
[全零内容] set_len + 写 4MB 零字节
  Unix:    blocks > 0     → 不误报 ✅（真实写入总会分配块）
```

**但这不是 P0** —— 主防线（batch10 新增的段级 fsync + 账本顺序）覆盖了主要场景：

```
1. download.rs:391  每段完成 → f.sync_data()（数据落盘）
2. ledger.rs        账本 save → f.sync_all() + rename
→ 数据先于元数据持久（WAL 语义）→ 「账本说段完成但数据丢失」窗口已关闭
```

剩余风险（全部有其它防线）：
- 账本丢失/损坏 → resume 从零重下，覆盖而非留空洞
- 全部段失败 → 走 Failed 而非 Completed
- 磁盘满 → 段失败重试/换源，最终 Failed

**注释已诚实说明**（「块信息不可得的平台（非 Unix）保守放行」）。

**建议**：doc 里明确写「Windows 为 no-op」，或用 `windows-sys` 的
`GetFileInformationByHandleEx(FileStandardInfo)` 拿 `AllocationSize` 补齐。
（项目已有 `windows-sys 0.59`，但那是 `xunlei-ffi` 的依赖，`httpdl` 需自己加。）

---

## 4. 审计覆盖说明

**本轮审了**：

| 文件 | 改动 | 结论 |
|---|---|---|
| `daemon/src/config.rs` | +153/-54 | 单源重构正确，`bt.enabled` 默认值有破坏性（§1.1） |
| `core/src/torrent_meta.rs` | +117/-1 | ✅ 三处修复 + 测试齐全；注释理由错（§1.3） |
| `httpdl/src/rate.rs` | +93/-23 | ✅ 三处修复；热调残余竞态（§1.2） |
| `provider/src/xunlei/login_page.rs` | +169/-2 | ✅ **比前一轮更严格** |
| `httpdl/src/engine.rs` | +97/-28 | ✅ 修了预删窗口与 gen 泄漏 |
| `httpdl/src/verify.rs` | +26 | 🟢 Windows no-op（§3） |
| `daemon/src/lockfile.rs` | +24 | ✅ 标准解法 |
| `httpdl/src/protocol/ftp.rs` | +21/-2 | ✅ PASV；`is_terminal` 只删死代码未改语义 |
| `httpdl/src/protocol/sftp.rs` | +13 | ✅ 文案匹配扩面（仍是文案匹配，非枚举） |
| `btcore/src/xudt/frame.rs` | +8/-4 | ✅ |

**未审**（本轮时间/优先级原因）：
- `daemon/src/rss.rs`（+118/-72）
- `daemon/src/state/ops.rs`（+63/-34）
- `daemon/src/bt.rs`（+73/-17）
- `core/src/xltd.rs`（+63/-4）
- `provider/src/xunlei/provider.rs`（+50/-15）
- `xunlei-convert/src/cid_store.rs` 的其余部分（性能部分已审，边界已单独报告）

---

## 5. 新增复现文件

| 文件 | 内容 |
|---|---|
| `round9.rs` | FTP `is_terminal` 三版对比（修复前后语义相同） |
| `round10.rs` | `cid_store` 二分 vs 线性等价性（暴露 243/8889 不一致） |
| `round11.rs` | 根因诊断（排除「漏检」假设） |
| `round12.rs` | 最小复现 + 修正实现零差异验证 |
| `round13.rs` | `bt.enabled` 默认值与 xunlei 互斥的冲突分析 |
| `round14.rs` | `piece_length` 上限 vs libtorrent 实际上限 |
| `round15.rs` | `set_rate_kb_s` 热调竞态时序复现 |
| `round16.rs` | 空洞检测的平台能力对比（Unix vs Windows） |
