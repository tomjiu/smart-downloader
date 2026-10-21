# 评审报告索引

> 本目录三份报告来自 2026-10-16 的全面审查（代码 + 架构 + 文档）。
> **这些发现尚未修复** —— 报告是问题清单，不是修复记录。

---

## 报告清单

| 报告 | 范围 | 核心产出 |
|------|------|----------|
| [BACKEND-REVIEW-2026-10-16.md](BACKEND-REVIEW-2026-10-16.md) | daemon 核心 + httpdl | 第一轮：5 个 P0 |
| [BACKEND-REVIEW-ROUND2.md](BACKEND-REVIEW-ROUND2.md) | provider / core / daemon 未覆盖 | **12 个 P0** |
| [BACKEND-REVIEW-ROUND3.md](BACKEND-REVIEW-ROUND3.md) | btcore 业务层 / provider 剩余 | P0-13 + 并发闸门失效等 |
| [ARCHITECTURE-REVIEW.md](ARCHITECTURE-REVIEW.md) | 结构设计（7 crate + 文档体系） | 7 类结构问题 + 3 处依赖错误 |
| [_AUDIT_CONTRADICTIONS.md](../research/xunlei/_AUDIT_CONTRADICTIONS.md) | `docs/research/xunlei/` 状态类文档 | 24 项文档矛盾（两层分裂） |

**累计覆盖：约 49,000 / 52,000 行**（httpdl / daemon / provider / core 100%，btcore 业务层完成）

**复现工程**：[`repro/p0repro/`](../../repro/p0repro)（独立 crate，`cargo test -- --nocapture`）

**UI 相关**：[`docs/design/`](../design/)（设计规范 + 12 主题预览）、[`scripts/check-ui-spec.ps1`](../../scripts/check-ui-spec.ps1)（规范检查脚本）

---

## 最需要立刻知道的 5 件事

### 1. 🔴 多文件 `.torrent` 导入即 panic（已复现）

`daemon/src/state/bt_alerts.rs:117` + `:215` —— bencode 字典序下 `files` 在 `piece length` 之前，
扫到 `files` 时 `piece_length` 仍为 0 → 除零。

**1180 个测试全绿**，因为 fixture 用了**非法键序**（`torrent_tests.rs:90-112`）。

### 2. 🔴 `total_size` 溢出绕过磁盘预检（已复现）

`core/src/torrent_meta.rs:316` —— `[u64::MAX, 1]` 回绕为 **0** → 声称 0 字节 → 预检放行 → 写爆磁盘。

### 3. 🔴 `is_loopback_addr` 前缀绕过 → 未鉴权绑公网（已复现）

`daemon/src/config.rs:443` —— `starts_with("127.")` 让 `127.evil.com:8787` 通过
`serve.rs:47` 的 fail-closed 闸门。**实测 4 项绕过。**

### 4. 🔴 登录页零防护

`provider/src/xunlei/login_page.rs:78-103` —— 生产代码 299 行中
`from_fn`/`middleware`/`layer`/`Origin`/`HOST` **全部 0 次命中**，而 `POST /api/login/pwd` 收明文密码。

对比：daemon 的 `http.rs:2622` 有完整 Host/Origin 守卫。

### 5. 🟡 架构层面：`DownloadEngine` 是胖接口

**40 个方法，仅 13 个必需**，且必需集是 BT 中心的 → HTTP/FTP/SFTP 被迫实现 peer/piece 概念，
`HttpEngine::peers()` 返回 `Ok(vec![])`（**撒谎而非报错**）。

后果：生产代码 **33 处运行时 `EngineKind` 判断**，`ops.rs` 独占 14 处。

### 6. 🔴 迅雷引擎 add 失败遗留孤儿任务

`btcore/src/xunlei_engine.rs:213-220` —— `insert_task` 先于 `start_task`，
后者失败时 daemon 拿到 `Err` 丢弃 `engine_tid`，但**引擎侧 `tasks` 表已插入**。

`rollback_slot_reservation` **只清理 daemon 侧**，不碰引擎 → 孤儿任务
**永不 stop、永不 delete、重启也不清理**，持续占带宽/磁盘。

### 7. 🟡 provider 并发闸门恒真

`provider/src/coordinator.rs:70` 的 `rt.busy < rt.concurrency_limit` ——
**`busy` 在生产代码中恒为 0**（三个构造点全硬编码），全仓无递增点
（只有 `mock.rs` 的 `set_busy()` 供测试）。

「每 provider 并发上限 2」**从未生效** → 100 任务可同时打进同一 provider。

---

## 三个系统性模式

### 模式 A：在一处修了，没同步到结构同构的另一处

| 修了 | 漏了 | 性质 |
|------|------|------|
| `xunlei/auth.rs` 先 0600 建文件 | `quark/types.rs` 先写后 chmod（**注释自认"此前漏同步"**） | 凭据可读窗口 |
| `ledger.rs` 纳秒唯一 tmp 名 | `hls.rs`/`dash.rs` 固定 `.tmp` | 并发互踩 |
| `http.rs` Host/Origin 守卫 | `login_page.rs` 零中间件 | **DNS rebinding** |
| `MockProvider::next_id` 原子自增 | `quark/share.rs`、`xunlei/provider.rs` 用常量/秒级戳 | 句柄覆盖 |
| `Config::save_to` 有 `sync_all` | `persistence.rs` 无 fsync | 掉电丢任务 |
| `config.rs` 有校验 | `serve.rs:461` 热重载裸解析 | 校验绕过 |

### 模式 B：测试固件与真实数据形态不一致

- `torrent_tests.rs` 用**非法 bencode 键序** → P0 恒绿
- `config.rs:634` 的 `assert!(bt.enabled)` 仅在**整表缺失**时成立
- `config.rs:597` 的 loopback 测试覆盖 `0.0.0.0`，**恰好漏掉 `127.` 前缀非 IP 主机名**

### 模式 C：注释与实现脱节

| 位置 | 注释声称 | 实际 |
|------|----------|------|
| `share.rs:439` | "与 captcha/init 一致" | 设备号每次不同 |
| `ops.rs:36-39` | "锁内停留亚毫秒" | exit 分支 sleep 250ms |
| `LOCK_MODEL.md` §2 | "任何时刻至多持一把锁" | `autosave` 同持两把 |
| `core/lib.rs:2` | "仅契约类型与纯逻辑" | 含 ~590 行 `std::fs` I/O |

---

## 结构层面的 3 条错误依赖

| 问题 | 证据 |
|------|------|
| **幽灵依赖** `xunlei-convert → xunlei-ffi` | 全仓 0 处 `xunlei_ffi` 引用；而真正需要的 `smart-dl-core` 未声明 |
| **死代码 ~1,800 行** | `core::sniffer`(695，0 消费者)、`core::xltd`(216)、`core::session::single_instance`(34) |
| **校准版不可达** | `btcore::nas_remote`(551，A2-A5 已校准)被 feature 关死；`daemon::nas_remote`(136，自述"假设")在跑 |

---

## 文档层面的关键问题

| 问题 | 证据 |
|------|------|
| **D28 同号两义** | `superpowers/plans/...design.md:71`=FFI 绑定工具 vs `BACKLOG.md:70`=P2P 排除（被 31 处二手引用） |
| **版本号全线漂移** | CHANGELOG `0.2.2` vs workspace `0.2.1` vs desktop `0.2.0` |
| **无 LICENSE 正文** | `Cargo.toml` 声明 `MIT OR Apache-2.0` 且 `deny.toml` 门禁，但三个 LICENSE 文件均不存在 |
| **引用不存在的文件** | `IMPLEMENTED.md:520` 引用 `config.toml.example` |
| **p2p 三份合集 96-99% 重复** | 239-255KB × 3，仅保留 1 份即可 |
| **README 文档地图仅覆盖 5/77** | `LOCK_MODEL.md`、`SECURITY_AUDIT.md`、本目录全部不可发现 |

---

## 修复优先级

| 顺序 | 项 | 成本 |
|------|-----|------|
| 1 | **P0-1 除零 panic**（`bt_alerts.rs:117/215`） | 低 |
| 2 | **P0-11 total_size 溢出**（`torrent_meta.rs:316`） | 低 |
| 3 | **P1 loopback 绕过**（`config.rs:443`，删前缀兜底） | 极低 |
| 4 | **P0-7 登录页防护**（复用 daemon 的 `request_from_local_origin`） | 低 |
| 5 | **P0-2/P0-3 BT 并发**（`bt.rs:236`/`:395`） | 低 |
| 6 | **统一 bencode 到 `core::bencode`** | 中 |
| 7 | **修 `LOCK_MODEL.md` + `autosave`** | 低 |
| 8 | **拆 `DownloadEngine` trait** | 中 |

---

## 核实说明

本索引中所有结论均经**实测验证**：

- **程序化提取**：trait 方法数（40）、运行时判断数（33）、bencode 函数分布（5 文件 17 函数）
- **可运行复现**：`repro/p0repro` 跑出 `10 passed / 3 failed`，3 个 failure 即 3 个缺陷
- **交叉核对**：版本号、LICENSE、空目录、README 链接均用文件系统检查确认

**已推翻的早期判断**（记录以免误导）：

| 早期判断 | 实际 |
|----------|------|
| `provider → httpdl` 是生产耦合 | ❌ 是 `dev-dependencies`，仅 1 个测试用 |
| `core/xltd.rs` 与 `xunlei-convert/xltd.rs` 是重复实现 | ❌ 算法不同（块扫描 vs SHA1 校验） |
| README 有断链 | ❌ 6 个链接全部有效 |
| `build_bitfield_from_indices` 越界 panic | ❌ 有 `if i < num_pieces` 检查 |
