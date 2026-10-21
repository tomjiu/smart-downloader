# 架构结构评审报告

> 审查对象：`tomjiu/smart-downloader` @ `0520e25`
> 审查日期：2026-10-16
> 视角：**结构设计合理性**（非代码缺陷 —— 代码正确 ≠ 架构合理）

---

## 0. 先给结论

**整体架构的意图是健康的**，分层清晰、依赖单向、无环。以下判断有实证支撑：

| 维度 | 评价 |
|------|------|
| 分层意图 | ✅ **清晰** —— core 为契约层，引擎层依赖它，daemon 为顶层 |
| 依赖方向 | ✅ **单向无环**（但存在错误边，见问题 2） |
| `DownloadEngine` trait 位置 | ✅ **正确** —— 在 core 是合法的依赖倒置 |
| **FFI 隔离** | ✅ **优秀** —— `unsafe` 计数：core 0 / httpdl 0 / provider 0 / daemon 0，仅 btcore 116、xunlei-ffi 69 |

**但存在 7 类结构问题**，其中若干会持续产生新缺陷。

### ⚠️ 本报告对早期判断的三处自我更正

审查过程中有三条初始结论被推翻，记录在此以免误导：

| 我最初的判断 | 实际 | 证据 |
|-------------|------|------|
| `provider → httpdl` 是生产耦合 | ❌ **不存在** —— 它是 `[dev-dependencies]`，仅 1 个测试文件用 | `provider/Cargo.toml:29`、`grep smart_dl_httpdl` 仅命中 `tests/mock_lifecycle.rs:10` |
| `core/xltd.rs` 与 `xunlei-convert/xltd.rs` 是"同一格式两套实现" | ❌ **不是重复** —— 前者是零/非零块扫描器，后者是 SHA1 piece 校验器 | 两者 API 与算法均不同 |
| 依赖图基本健康 | ⚠️ **遗漏了幽灵依赖与死代码** | 见问题 2 与问题 6 |

---

## 1. 依赖图（已核实）

```
                    core (5,311 行, 无内部依赖)
                     ↑    ↑    ↑
        ┌────────────┘    │    └────────────┐
        │                 │                 │
   httpdl (9,059)    btcore (2,700)    provider (8,291)
        │                 │                 │
        └────────┬────────┴─────────────────┘
                 │
            daemon (23,195 行)  ← 顶层，被 0 个 crate 依赖

   xunlei-ffi (2,002, 无依赖)
        ↑
   xunlei-convert (1,685)
        ↑
      daemon
```

**核对要点：**

- ✅ `provider` **不依赖** `httpdl`（`httpdl` 只是它的 `dev-dependency`，仅测试用）
- ✅ 无循环依赖
- ✅ `core` 被 4 个 crate 依赖，作为契约层合理
- ✅ `xunlei-ffi` 完全隔离（无依赖），unsafe 边界被限制在该 crate 内

**这个分层是对的。** 特别值得肯定的是 **`xunlei-ffi` 的隔离设计** ——
把「动态加载第三方 DLL + 184 处 unsafe」全部关在一个无内部依赖的 crate 里，
上层（`xunlei-convert` → `daemon`）通过安全 API 使用。

---

## 2. 结构问题

### 🔴 问题 1：`DownloadEngine` trait 是「能力并集」而非「最小契约」

**位置**：`core/src/types.rs:303-532`（**230 行**）

**trait 有 40 个方法**，其中至少 20 个是 BT 专属：

```
通用：    id, kind, capabilities, add, pause, resume, status, remove
BT 专属： peers, add_url_seed, add_peer, ban_peer, read_piece,
          set_file_priorities, file_priorities, set_super_seeding,
          force_reannounce, force_dht_announce, force_recheck,
          export_torrent, magnet_uri, ban_ip, unban_ip, ban_ip_range,
          set_piece_first_last, set_session_storage_allocate,
          seeding_ratio_limit, seeding_time_limit,
          add_trackers, remove_tracker, list_trackers,
          add_xunlei_resume, apply_bt_session
HTTP 专属：update_sources, set_global_proxy, set_task_proxy
```

**后果一：每个引擎必须实现大量桩**

| 引擎 | `Unsupported` 返回 | 空 `Ok(())` 桩 |
|------|-------------------|---------------|
| `HttpEngine` | 2 | 2 |
| `FtpEngine` | 1 | 3 |
| `SftpEngine` | 1 | 3 |
| `BtEngine` | 0 | 0 |

**后果二（更严重）：调用侧被迫做运行时类型判断**

**生产代码中 33 处 `EngineKind` 判断**，其中：

```
state/ops.rs          14 处
state/persistence.rs   3 处
```

典型模式（`ops.rs` 反复出现）：

```rust
if rec.engine_kind != EngineKind::Bt {
    return Err(/* ... */);
}
// L2505, 2562, 2729, 3235, 3501, 3530, 3568, 3597, 3665
if kind != EngineKind::Bt { ... }   // L2679, 3201
```

**这是「用 `if kind != Bt` 手写模拟能力检查」** —— 本该由类型系统保证的东西，
退化成了运行时的字符串式判断。**加一个新引擎要改 14 处判断散落在 ops.rs 里。**

**建议**（不需要大重构）：

```rust
// 拆分：保留最小契约 + 可选扩展 trait
pub trait DownloadEngine: Send + Sync {          // 所有引擎必须实现
    fn id(&self) -> &str;
    fn kind(&self) -> EngineKind;
    async fn add/pause/resume/status/remove(...);
}

pub trait BtExt: DownloadEngine {                 // BT 专属能力
    async fn peers(...); async fn set_file_priorities(...); ...
}

pub trait HttpExt: DownloadEngine {               // HTTP 专属能力
    async fn update_sources(...); ...
}
```

`daemon` 侧用 `engines.get(&EngineKind::Bt).and_then(|e| e.as_bt())` 一次拿到具体类型，
**把 14 处散落的判断收敛到 1 处**。

---

### 🔴 问题 2：`core` 的职责超出其文档声明

**`core/src/lib.rs` 开篇写：**

> 本 crate 不 import 任何引擎，**仅定义契约类型与纯逻辑**。

**但 `core` 实际包含：**

| 模块 | 行数 | 是否符合「契约/纯逻辑」 |
|------|------|----------------------|
| `sniffer.rs` | 695 | ❌ 协议嗅探（有解析但不纯） |
| `torrent_meta.rs` | 530 | ⚠️ bencode 解析 + 摘要 |
| `bencode.rs` | 354 | ⚠️ 格式编解码 |
| **`session/output.rs`** | **252** | ❌ **文件落盘 + symlink 逃逸检查** |
| **`session/manager.rs`** | **130** | ❌ **会话持久化（磁盘 I/O）** |
| **`session/single_instance.rs`** | **34** | ❌ **单实例锁（进程级）** |
| **`xltd.rs`** | **216** | ❌ **迅雷私有格式解析** |

**问题在于：**

1. **`core` 有磁盘 I/O**（`session/output.rs` 写文件、`session/manager.rs` 持久化）
   —— 而 `lib.rs` 声称是"纯逻辑"
2. **`core/src/xltd.rs` 与 `xunlei-convert/src/xltd.rs` 是同一个迅雷格式的两套解析器**

| 文件 | 行数 | 职责 |
|------|------|------|
| `core/src/xltd.rs` | 216 | `BlockStatus` / `XltdProgress` / `analyze` / `recover` |
| `xunlei-convert/src/xltd.rs` | 185 | `XltdError` / `XltdAnalysis` / `verify_piece_hashes` |

**同一格式、两份实现、在两个 crate 里** —— 而 `xunlei-convert` 本该是
「迅雷格式转换」的唯一归属地。`core` 不该知道 `.xltd` 是什么。

**建议**：把 `core/src/xltd.rs` 移入 `xunlei-convert`；
`session/output.rs` 的落盘职责考虑独立成 `core::output` 或下沉到 `httpdl`。

---

### 🟡 问题 3：`daemon` 承担了 8 个领域职责

**23,195 行**，是第二大 crate 的 2.5 倍。

| 职责 | 模块 | 行数 |
|------|------|------|
| 任务状态机 | `state.rs` + `state/*` | ~7,900 |
| HTTP API | `http.rs` + `ws.rs` + `events.rs` + `http_events.rs` | ~3,200 |
| RSS 订阅 | `rss.rs` | 1,105 |
| **BT 引擎实现** | `bt.rs` + `bt_events.rs` | ~1,000 |
| 配置 | `config.rs` + `lockfile.rs` | 1,024 |
| CLI | `cli.rs` + `client.rs` + `main.rs` + `baidu_resolve.rs` | 810 |
| NAS 托管 | `nas.rs` + `nas_remote.rs` | 796 |
| 其他 | `metalink.rs` 446 / `serve.rs` 697 / `xunlei_login.rs` 242 / `health.rs` 44 | ~1,400 |

**三个具体的结构不一致：**

**① `bt.rs`（919 行）是引擎实现，却住在 daemon 里**

对比：
```
httpdl/     → HTTP/FTP/SFTP 引擎（独立 crate）
btcore/     → libtorrent FFI 绑定（独立 crate）
daemon/bt.rs → BtEngine 实现（住在 daemon 里）    ← 不对称
```

`btcore` 只提供「FFI 薄核」，真正的 `BtEngine`（实现 `DownloadEngine` trait）
在 `daemon/bt.rs`。**为什么 BT 引擎的实现层在 daemon，而 HTTP 引擎在独立 crate？**

**② `rss.rs`（1,105 行）与「守护进程」无关**

RSS 订阅是**独立的产品功能**（feed 解析 + 规则匹配 + 自动建任务 + ticker）。
把它放在 daemon 里，意味着 daemon 必须依赖 `regex`、`chrono`、XML 解析等。

**③ `daemon` 同时是库和二进制**

`lib.rs` 暴露 **20 个 pub 模块**，`main.rs` 又是 CLI 入口。
这导致测试必须链接整个 daemon（含所有 feature），也是
`state_tests/` 目录膨胀到 20+ 文件的原因。

**建议**：
```
新增 crates/rss/       — RSS 订阅（独立能力，纯逻辑 + 自己的持久化）
新增 crates/btengine/  — BtEngine 实现（与 httpdl 对称）
或：把 bt.rs 移入 btcore（让 btcore 从「FFI 绑定」升级为「BT 引擎」）
daemon 保留：HTTP API + 状态机 + 配置 + CLI
```

---

### 🟡 问题 4：bencode 有三套独立实现

| 位置 | 实现 | 行数 |
|------|------|------|
| `core/src/bencode.rs` | `Value` / `decode_bytes` | 354 |
| `core/src/torrent_meta.rs` | `into_bencode` | 部分 |
| **`xunlei-convert/src/fastresume.rs:303-480`** | `bencode_fastresume` / `bdecode` / `encode_bytes` / `dict_get` / `as_list` | **~180** |
| **`daemon/src/client.rs:100`** | `bencode_info_hash` | 独立 |
| **`daemon/src/state/bt_alerts.rs`** | `be_str` / `be_int` / `dict_skip` / `list_skip` / `value_skip` | ~80 |

**5 个文件、17 个 bencode 相关函数:**

| 文件 | 函数数 | 内容 |
|------|--------|------|
| `core/src/bencode.rs` | 2 | `decode_bytes` + `Value`（**唯一有防护的**：`MAX_DEPTH=64` + `checked_add`） |
| `xunlei-convert/src/fastresume.rs` | **10** | `bencode_fastresume` / `bdecode` / `bdecode_bytes` / `encode_bytes` / `dict_get` / `as_list` … |
| `daemon/src/client.rs` | 3 | `bencode_info_hash` 及测试 |
| `core/src/torrent_meta.rs` | 1 | `into_bencode` |
| `daemon/src/state/bt_alerts.rs` | ~5 | `be_str` / `be_int` / `dict_skip` / `list_skip` / `value_skip`（grep 未计，因命名不含 bencode） |

**这不是"重复代码"这么简单 —— 它直接导致了已知缺陷：**

- `bt_alerts.rs` 的 `TorrentMeta::parse` 有**除零 P0**（单遍扫描 + 键序假设）
- 而 `core/src/bencode.rs` 有 `MAX_DEPTH=64`、`checked_add` 等**完善防护**（经审查确认无缺陷）

**如果 `bt_alerts.rs` 复用 `core::bencode`，那个 P0 根本不会存在。**

**建议**：统一到 `core::bencode`，其余各处改为调用它。

---

### 🟡 问题 5：文档与实际状态不一致

#### 5.1 `LOCK_MODEL.md` 的不变量已被破坏

**文档 §2 声称：**

> 1. **任何时刻至多持一把锁**。全部临界区都是单锁作用域
> 2. `autosave()` 只允许在锁外调用

**实际代码（`state/persistence.rs:130-142`）：**

```rust
pub(super) fn autosave(&self) {
    let Some(path) = self.persist_path.clone() else { return };
    let _g = self.persist_lock.lock();        // ① 持 persist_lock
    let data = self.persisted_tasks();        // ② 内部取 tasks 锁 → 同持两把
    if let Err(e) = write_tasks_atomic(&path, &data) { ... }  // ③ 持锁做磁盘 IO
}
```

**同时违反不变量 1（持两把锁）和规约 §4.2（临界区内禁止文件 IO）。**

#### 5.2 为什么审计漏了这条边

**文档 §3 的审计方法：**

> **[A] 自重入**：…判定「持锁 L 的 guard 存活区间（**NLL 近似：绑定行 → 变量最后一次真实使用行**）内」…

**而代码用的是 `_g`：**

```rust
let _g = self.persist_lock.lock();   // ← 波浪线前缀，从不被"使用"
```

**`_g` 从未被引用 → 按"最后一次真实使用行"启发式，guard 区间为空 → 审计工具认为它当场释放 → 漏报。**

**于是 §3 得出了：**

> 同时持锁：**唯一一条边** `dest_roots`…**已在同批 PR 中消除**…不变量 1 由「事实上如此」升级为「**结构性保证**」

**这个"结构性保证"的结论是错的** —— 实际存在 `persist_lock → tasks` 这条边。

**教训**：静态审计的启发式（"最后一次使用"）与实际语义（Rust 的 guard 活到作用域结束）
存在系统性偏差，且用 `_` 前缀命名会稳定触发这个偏差。

#### 5.3 `lifecycle.rs` 的注释同样矛盾

`state/lifecycle.rs:319-324` 注释宣称：

> 全域锁模型维持『任何时刻至多持一把锁』强不变量…`dest_roots` 是唯一的多锁同持边，**现已消除**

与 `autosave` 的实际行为矛盾。

---

## 3. 明确「结构上是对的」的部分

评审的价值包括**确认哪些设计是正确的**，避免过度重构。

| 设计 | 为什么是对的 |
|------|-------------|
| **`core` 作为契约层被 4 个 crate 依赖** | 引擎 trait、类型、状态机确实是共享语言。放在 core 合理 |
| **`DownloadEngine` trait 放在 core** | 抽象与实现分离的正确做法 —— trait 在稳定层，实现在易变层 |
| **`xunlei-ffi` 完全隔离** | 184 处 unsafe + 动态 DLL 加载被关在无依赖 crate 里，边界清晰 |
| **`provider` 不依赖 `httpdl`** | 网盘 API 层不需要下载引擎，依赖收窄是对的 |
| **feature 门控设计** | `bt`/`ftp`/`sftp`/`nas`/`xunlei-import` 门控到位，CI 基线不链接 libtorrent |
| **`state_tests/` 拆分** | 20+ 个测试文件按主题拆分，比单文件 5000 行好 |
| **`httpdl` 把 FTP/SFTP 并入** | 三者共享 segment/ledger/rate/resume 基础设施，合并有合理性（尽管 1983 行的 `ftp.rs` 偏大） |
| **`LockModel.md` 文档本身** | 把并发不变量写成长期资产、要求 PR 对照复查 —— 这是好实践（只是实现没跟上） |

**特别注意**：`LOCK_MODEL.md` 的存在说明团队**有意识地在管理复杂度**。
问题不是"没做"，而是"做了但工具的方法论有偏差"。

---

## 4. 结构问题与代码缺陷的关联

**这一节说明：结构问题不只是"不优雅"，它直接产生缺陷。**

| 结构问题 | 导致的缺陷 |
|----------|-----------|
| **bencode 三套实现** | `bt_alerts.rs` 除零 P0（core 版的完善防护没被复用） |
| **`DownloadEngine` 胖接口** | 33 处运行时类型判断；新引擎要改 14 处 ops.rs |
| **`xltd` 两套解析** | 同一个格式两套校验口径（`core` 版按 block，`xunlei-convert` 版按 piece） |
| **`LOCK_MODEL.md` 与实现脱节** | `autosave` 同持两锁 + 持锁 IO 未被发现 |
| **`core` 含 `session/output.rs`** | `sanitize_rel` 的职责不清（它既是路径安全校验，又是落盘前置） |
| **`daemon` 职责过宽** | `state/ops.rs` 4,534 行单 impl 块（8 个领域的方法混在一起） |

**最典型的因果链：**

```
bencode 重复实现（结构问题）
    ↓
bt_alerts.rs 自己写了一套单遍扫描解析
    ↓
单遍扫描需要按顺序遇到 piece_length 才能算偏移
    ↓
真实 bencode 键序里 files 在 piece length 之前
    ↓
除零 panic（P0）
```

---

## 5. 文档体系的问题

> 详见 [README.md](README.md) 索引；本节记录**文档与代码的一致性缺陷**。

### 5.1 `LOCK_MODEL.md` 的不变量已被破坏

**文档 §2 声称：**

> 1. **任何时刻至多持一把锁**。全部临界区都是单锁作用域
> 2. `autosave()` 只允许在锁外调用

**实际代码（`state/persistence.rs:130-142`）：**

```rust
pub(super) fn autosave(&self) {
    let Some(path) = self.persist_path.clone() else { return };
    let _g = self.persist_lock.lock();        // ① 持 persist_lock
    let data = self.persisted_tasks();        // ② 内部取 tasks 锁 → 同持两把
    if let Err(e) = write_tasks_atomic(&path, &data) { ... }  // ③ 持锁做磁盘 IO
}
```

**同时违反不变量 1（持两把锁）和规约 §4.2（临界区内禁止文件 IO）。**

### 5.2 为什么审计漏了这条边

**文档 §3 的审计方法：**

> **[A] 自重入**：…判定「持锁 L 的 guard 存活区间（**NLL 近似：绑定行 → 变量最后一次真实使用行**）内」…

**而代码用的是 `_g`：**

```rust
let _g = self.persist_lock.lock();   // ← 下划线前缀，从不被"使用"
```

**`_g` 从未被引用 → 按"最后一次真实使用行"启发式，guard 区间为空 → 审计工具认为它当场释放 → 漏报。**

**于是 §3 得出了：**

> 同时持锁：**唯一一条边** `dest_roots`…**已在同批 PR 中消除**…不变量 1 由「事实上如此」升级为「**结构性保证**」

**这个"结构性保证"的结论是错的** —— 实际存在 `persist_lock → tasks` 这条边。

**教训**：静态审计的启发式（"最后一次使用"）与实际语义（Rust 的 guard 活到作用域结束）
存在系统性偏差，且用 `_` 前缀命名会稳定触发这个偏差。

### 5.3 `lifecycle.rs` 的注释同样矛盾

`state/lifecycle.rs:319-324` 注释宣称：

> 全域锁模型维持『任何时刻至多持一把锁』强不变量…`dest_roots` 是唯一的多锁同持边，**现已消除**

与 `autosave` 的实际行为矛盾。

### 5.4 决策编号 `D28` 同号两义

| 出处 | 定义 |
|------|------|
| `superpowers/plans/2026-08-16-smart-downloader-design.md:71` | **D28** = FFI 绑定工具 M0 spike（**正典**） |
| `BACKLOG.md:70` | **D28** = 迅雷私有 P2P 加速引擎排除（**冲突**） |

后者被 **31 处文档**二手引用（`CROSS_PLATFORM_UNIVERSAL_SOLUTION.md:198` 甚至标注
"D28 决策原文（BACKLOG.md D 段）"）。

**代码注释的 D 编号全部指向 design 文档**（`engine.rs:116` 的 D29、`retry.rs:1` 的 D25、
`session/output.rs:2` 的 D36），是正确的；但 `docs/research/xunlei/DECISIONS.md`
**不记录任何 `D<n>` 式编号**（只有 `D-YYYY-MM-DD-NN` 日期式），名不副实。

### 5.5 版本号全线漂移

```
CHANGELOG.md 最新条目    : 0.2.2 - 2026-09-08
workspace Cargo.toml     : 0.2.1        ← 代码里不存在 0.2.2
desktop/src-tauri/Cargo  : 0.2.0
desktop tauri.conf.json  : 0.2.1        ← desktop 自己打架
ui/package.json          : 0.1.0
```

另：`xunlei-ffi` / `xunlei-convert` **硬编码** `version = "0.2.1"` 而非 `version.workspace = true`；
`xunlei-ffi` 的 `repository` 指向 `github.com/example/smart-downloader`（与 workspace 的 `tomjiu/` 冲突）。

### 5.6 引用了不存在的文件

`IMPLEMENTED.md:520` 引用 `config.toml.example` —— **仓库中不存在该文件**。

### 5.7 无 LICENSE 正文却声明双许可

```
Cargo.toml            : license = "MIT OR Apache-2.0"
deny.toml             : 执行 license 门禁
LICENSE / LICENSE-MIT / LICENSE-APACHE : 均不存在
```

对双许可项目是**合规硬缺口**。

### 5.8 最新审查报告不可发现

`docs/review/`（三份报告）与 `repro/`（P0 复现工程）**均未入 git**，
且不被 README 文档地图、`BACKLOG.md`、`PROJECT_STATUS.md` 任何一处索引。

**全仓最新、最有价值的缺陷记录处于不可发现状态。**

---

### 5.9 文档层面的结构性建议

| 优先级 | 动作 |
|--------|------|
| **1** | 把 `docs/review/` + `repro/` 纳入 git，并在 `BACKLOG.md` 置顶 P0 条目 |
| **2** | 建 `docs/adr/`，把 design 文档的 D1–D36 抽为唯一正典，`BACKLOG.md` 的 D28 改名 |
| **3** | 统一版本号（`Cargo.toml` → 0.2.2 或 CHANGELOG 回退 0.2.1）；子 crate 改 `version.workspace = true` |
| **4** | 补 `LICENSE-MIT` + `LICENSE-APACHE`（已被 `deny.toml` 门禁） |
| **5** | 给 6 个未修正的迅雷状态文档加**勘误横幅**（模式照抄 `NAS_REMOTE_ENGINE.md`，那是全仓做得最好的范式） |
| **6** | 新建 `docs/ARCHITECTURE.md`（52k 行代码目前无本项目架构总览） |
| **7** | 修 README 文档地图（当前仅覆盖 5/77） |
| **8** | p2p 三份合集（96-99% 重复）保留 1 份，其余移入 `_ARCHIVE/` |

---

## 6. 重构建议（按性价比排序）

| 优先级 | 建议 | 收益 | 成本 |
|--------|------|------|------|
| **1** | **统一 bencode 到 `core::bencode`** | 消除一类缺陷根源；`bt_alerts.rs` 的 P0 自然消失 | 中 |
| **2** | **删除 3 条错误依赖**：<br>① `xunlei-convert → xunlei-ffi`（幽灵，0 引用）<br>② `xunlei-convert` 补 `→ core`（用它的 bencode）<br>③ `daemon` 的 `nas = []` 真正转发 `smart-dl-btcore/nas` | 依赖方向归正；校准版 nas_remote 可达 | 低 |
| **3** | **修正 `LOCK_MODEL.md` + 修 `autosave`** | 让文档与实现一致；消除持锁 IO | 低 |
| **4** | **未接线模块的处置（⚠️ 不是"删死代码"）** —— 详见 §6.5 | 决策清晰；避免文档与代码冲突 | 低 |
| **5** | **拆分 `DownloadEngine` trait** → `EngineCore`(13 必需) + `BtEngineOps` + `HttpEngineOps` | 33 处类型判断收敛；消除 `peers()` 撒谎 | 中 |
| **6** | **`core` 的 I/O 下沉**：`session/{output,manager,single_instance}` 抽为独立 `storage` crate；core 只留纯函数 | core 回归"契约+纯逻辑"，doc 自述变真 | 中 |
| **7** | **`core/src/strategy.rs` 迁回 `btcore`** | 消除唯一"抄近路"违规（该文件自认本应放 btcore） | 低 |
| **8** | **`daemon/bt.rs` 移入 `btcore`** | 与 `httpdl` 对称（先例：`btcore/src/xunlei_engine.rs:153` 已实现 trait） | 中 |
| **9** | **`rss.rs` 独立成 crate** | daemon 减 1,105 行；RSS 可单独测试 | 中 |
| **10** | **`state/ops.rs` 按域拆分** | 4,534 行 → 5 个 ~900 行模块 | 低（纯移动） |

**建议先做 1–4、7、10** —— 都是低成本高收益，且不涉及接口变更。

---

## 6.5 ⚠️ 「未接线模块」的处置（不是"删死代码"）

> **本节是对早期判断的修正。** 初版报告曾建议"删除死代码 ~1,800 行"，
> 经逐项复核后发现**大部分不能删** —— 记录于此以免误导。

### 逐项核实结果

| 模块 | 初版判断 | 实际 | 处置 |
|------|----------|------|------|
| `core::sniffer`(695) | 死代码 | ❌ **判错** —— `CAPABILITY_ABSORBED.md:54` 标 ✅ 已落地，`IMPLEMENTED.md:246` 记录行为契约，含 13 测试 | **不删**，需决策"接线"或"标注未接线" |
| `core::session::single_instance`(34) | 死代码 | ❌ **判错** —— `session/mod.rs:2` 明示对应 **设计文档 §12 单实例锁 D24**（正式决策编号） | **不删**，应与 `daemon/lockfile.rs` **合并** |
| `core::xltd`(216) | 死代码 | ✅ 确认 0 消费者 | ⚠️ **被替代**（非重复）—— 见下方专项分析 |
| `btcore::xudt`(270) | 死代码 | ✅ 确认 0 消费者 | **不删** —— 是迅雷 uDT 传输层的**逆向研究雏形**（文档称"自研变体，完全未公开"） |
| `provider::xunlei::hash`(161) | 死代码 | ❌ **判错** —— `xunlei/provider.rs:383,471` 在用 | **不动** |
| `btcore::nas_remote`(551) | 死代码 | ⚠️ feature 永不启用 | 需修正 `daemon` 的 `nas = []` 空转发，让**校准版可达** |

### 为什么初版判断会错

1. **只查了直接引用**，没做传递闭包（`pub` API 可能被测试/示例间接使用）
2. **没查文档** —— 这些模块在 `IMPLEMENTED.md`/`CAPABILITY_ABSORBED.md` 里**有正式交付记录**
3. **没区分「未接线」与「无用」** —— 前者是实现完成但调用方没接，后者才是死代码

### 三类处置方式

| 类别 | 模块 | 正确做法 |
|------|------|----------|
| **已交付未接线** | `sniffer` | 决策：daemon 接线使用，或标注"未接线"并说明与 `source_parse/normalize.rs` 的分工 |
| **有决策编号** | `single_instance` | 与 daemon 的 `lockfile.rs` 合并（两份实现语义不同，需选一个） |
| **研究资产** | `xudt`、`core::xltd` | 保留，但加注释说明"研究雏形/已替代，未接线" |

**关键**：`sniffer` 与 `source_parse/normalize.rs` 功能重叠 —— 这**不是"删一个"的问题**，
而是「哪个是权威入口」的架构决策。

### 6.5.1 `core::xltd` 专项分析（唯一确认可处置的）

逐行读完后确认：**它是被 `xunlei-convert::xltd` 取代的早期实现。**

| 维度 | `core::xltd`（216 行） | `xunlei-convert::xltd`（185 行） |
|------|----------------------|--------------------------------|
| 粒度 | **block 级**（零/非零启发式） | **piece 级**（SHA1 密码学校验） |
| 依据 | "非零即已下载" | 与 torrent piece hash 比对 |
| `recover()` | 有 | 无 |
| 文档记录 | ❌ **不在** `IMPLEMENTED.md` / `CAPABILITY_ABSORBED.md` | ✅ `README.md` 有 V1-V8 样本验证 |
| 测试 | 5 个 | 含真实样本 e2e |
| 消费者 | **0** | `daemon/ops.rs:1197` 实际在用 |

**与 `sniffer` / `single_instance` 的关键区别**：那两个有正式交付记录，
而 `core::xltd` **在文档中完全没有出现** —— 它是纯遗留。

#### 附带发现：`recover()` 会损坏以零结尾的文件

`core/src/xltd.rs:145-148`：

```rust
// 截断到数据末尾，去掉尾部纯零区/记录表之外的空洞
if last_data_end < file_size {
    out.set_len(last_data_end)?;
}
```

**如果目标文件本身以零字节结尾（合法数据），这个函数会把它截断** ——
是逻辑缺陷，不是特性。且该函数无任何调用方，缺陷未被暴露。

**处置建议**：确认无未来用途后删除（它是唯一满足"被替代 + 无文档 + 有缺陷 + 0 消费者"四项的模块）。
若要保留，至少应修 `recover()` 的截断逻辑并加注释说明"未接线"。

---

## 7. 一个必须记录的方法论教训

**`LOCK_MODEL.md` §3 声称「不变量 1 由『事实上如此』升级为『结构性保证』」，但该保证是错的。**

原因不是"没审计"，而是：

1. 审计用「变量最后一次真实使用行」近似 guard 存活区间
2. 代码用 `_g` 命名，从不被使用 → 区间被判定为空
3. 工具报告"0 处多锁同持"，文档据此写下"结构性保证"

**启示**：
- 静态审计的启发式与 Rust 语义（guard 活到作用域结束）有系统性偏差
- `_` 前缀命名会稳定触发这个偏差 —— 应避免用于需要被审计的 guard
- **「工具说没问题」不等于「没问题」** —— 审计工具的盲区需要被记录

**建议**：在 `LOCK_MODEL.md` 里补一节「审计方法的已知盲区」，
并把 `_g` 这类命名列入规约（用 `_persist_guard` 这样可被追踪的名字）。

---

## 附：核实过程

| 结论 | 核实方式 |
|------|----------|
| 依赖图 | 逐 crate 读 `Cargo.toml`（**修正误报**：`provider → httpdl` 实为 dev-dependency） |
| 幽灵依赖 | 全仓 grep `xunlei_ffi` = **0 处命中**（除 btcore 与 xunlei-ffi 自身） |
| 死代码 | grep 消费者：`sniffer` / `core::xltd` 除 `lib.rs` 声明外 **0 引用** |
| trait 方法数 | 程序化提取 `core/src/types.rs:303-532` 的方法签名 = **40** |
| 运行时类型判断 | 全仓 grep 生产代码（排除 `state_tests`）= **33 处**（`ops.rs` 14 + `persistence.rs` 3 + 其余） |
| bencode 重复 | grep `fn.*bencode\|fn decode_bytes\|fn bdecode` = **17 函数 / 5 文件** |
| `LOCK_MODEL.md` 矛盾 | 文档 §2/§3 与 `persistence.rs:130-142` 逐行对照；`_g` 命名与 §3 审计方法对照 |
| **D28 冲突** | `design.md:71` vs `BACKLOG.md:70` 直接比对 |
| **版本漂移** | 程序化读取 CHANGELOG / workspace Cargo / desktop Cargo / tauri.conf |
| **LICENSE 缺失** | `Test-Path` 三处 + `Cargo.toml` 的 `license` 声明比对 |
| **`config.toml.example`** | `Test-Path` 确认不存在，而 `IMPLEMENTED.md:520` 引用了它 |
| README 链接 | **全部有效**（修正了我最初"断链"的误判） |

### 本报告明确推翻的自身判断

| 早期判断 | 实际 |
|----------|------|
| `provider → httpdl` 是生产耦合 | ❌ 是 `[dev-dependencies]` |
| `core/xltd.rs` 与 `xunlei-convert/xltd.rs` 是重复实现 | ❌ 算法不同（块扫描 vs SHA1 piece 校验） |
| README 有断链 | ❌ 链接全部有效 |
| `build_bitfield_from_indices` 越界 panic | ❌ 有 `if i < num_pieces` 检查 |
| 依赖图基本健康 | ⚠️ 遗漏了幽灵依赖与「未接线模块」 |

> **⚠️ 上表最后一行的措辞已修正（2026-10-21）**：原文写作「~1,800 行死代码」，
> 这个数字**不准确且具误导性**。经 §6.5 逐项复核，其中只有 `core::xltd`(216)
> 一项真正可删；`sniffer`/`single_instance`/`provider::hash` 均为**误判**，
> `btcore::xudt` 是研究资产不应删。**任何以「死代码」为名的批量删除都必须先过 §6.5。**
>
> 实际处置结果（PR #134）：删除 `sniffer`(735) + `xltd`(306) + `single_instance`(39)
> = 1,080 行，且从 `sniffer` **抢救出 `flashget://` 解码能力**接入活链
> （`source_parse/normalize.rs:79`）。详见 §6.5 与 PR #134。
