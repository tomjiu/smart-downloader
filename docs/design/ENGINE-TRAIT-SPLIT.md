# TD-2 设计提案：`DownloadEngine` trait 拆分（最小契约 + 扩展 trait）

> 状态：**提案——待 tomjiu 决策后实施**
> 来源：ARCHITECTURE-REVIEW §6 建议 5 / BACKEND A0 TD-2 / 审查 README 优先级 #8
> 本文档由 TD-2 前置修复（引擎「撒谎」消除，见下）随批产出

---

## 1. 现状（2026-10-20 实测数据）

### 1.1 trait 面

| 指标 | 值 |
|---|---|
| 总方法数 | **40** |
| 必须实现（无默认） | **13** |
| 带默认实现 | 27（绝大多数默认 `Err(Unsupported)`） |

必须实现的 13 个中有 **5 个是 BT 概念**：`peers` / `add_url_seed` / `ban_peer` /
`read_piece` / `update_sources`（最后一个双语义：HTTP 换源 + BT tracker 追加）。
HTTP/FTP/SFTP 引擎被迫为它们写桩实现——这正是「撒谎」问题的根源（本批已修：
`HttpEngine::peers/add_url_seed/ban_peer` 与 trait 默认 `add_peer` 原返回
`Ok(空)`，现已改为 `Err(Unsupported)`）。

### 1.2 运行时判断面

| 位置 | `EngineKind` 判断数（2026-10-20 实测） |
|---|---|
| `state/ops.rs` | **52**（审查时 14，代码增长翻三倍） |
| daemon 全域（非测试） | **73** |

每处判断都是「能力询问」的手工展开：调用方先问 kind 再决定调不调某个方法。
新增引擎能力时需要同步 N 处判断，漏一处就是运行时错配。

### 1.3 已有缓解（默认 Unsupported 模式）

27 个默认方法事实上已经构成「可选能力」的雏形——`set_file_priorities` /
`force_recheck` / `magnet_uri` 等 BT 方法已经不强制 HTTP 引擎实现。
本提案是把这条已有的路**走完**，而非推翻重来。

---

## 2. 目标形态

### 2.1 三层 trait

```rust
/// 最小契约：一切引擎必须实现（生命周期 + 状态 + 换源）。
#[async_trait]
pub trait DownloadEngine: Send + Sync {
    fn id(&self) -> &str;
    fn kind(&self) -> EngineKind;
    fn capabilities(&self) -> Vec<Capability>;

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId, EngineError>;
    async fn pause(&self, id: &EngineTaskId) -> Result<(), EngineError>;
    async fn resume(&self, id: &EngineTaskId) -> Result<(), EngineError>;
    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus, EngineError>;
    async fn remove(&self, id: &EngineTaskId, delete_data: bool) -> Result<(), EngineError>;

    /// 换源/追加源（HTTP 镜像 + BT tracker 双语义）。
    async fn update_sources(&self, id: &EngineTaskId, urls: Vec<String>)
        -> Result<(), EngineError>;

    /// 可选通用能力的查询入口（取代调用方按 kind 猜测）。
    fn bt(&self) -> Option<&dyn BtEngineExt> { None }
    fn http(&self) -> Option<&dyn HttpEngineExt> { None }
}

/// BT 扩展：peer/piece/tracker/做种/封禁 等 22 个方法的归宿。
#[async_trait]
pub trait BtEngineExt: DownloadEngine {
    async fn peers(&self, id: &EngineTaskId) -> Result<Vec<PeerInfo>, EngineError>;
    async fn add_url_seed(&self, id: &EngineTaskId, url: &str) -> Result<(), EngineError>;
    async fn add_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<(), EngineError>;
    async fn ban_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<(), EngineError>;
    async fn read_piece(&self, id: &EngineTaskId, idx: u32) -> Result<Vec<u8>, EngineError>;
    async fn set_file_priorities(/* … */) -> Result<(), EngineError>;
    // …（force_reannounce / magnet_uri / ban_ip / seeding_ratio_limit 等全部 BT 面）
}

/// HTTP 扩展：任务级代理热改等。
#[async_trait]
pub trait HttpEngineExt: DownloadEngine {
    async fn set_task_proxy(&self, id: &EngineTaskId, url: Option<&str>) -> Result<(), EngineError>;
}
```

`BtEngine` 实现 `fn bt(&self) -> Option<&dyn BtEngineExt> { Some(self) }`；
`HttpEngine` 同理实现 `http()`。调用方：

```rust
// 旧：52 处手工判断之一
if rec.engine_kind != EngineKind::Bt {
    return Err(DaemonError::UnsupportedOp(/* … */));
}
let engine = self.engine_for(EngineKind::Bt)?;

// 新：能力询问一次到位，编译器保证方法只在对应扩展上存在
let engine = self.engine_for(rec.engine_kind)?;
let bt = engine.bt().ok_or_else(|| DaemonError::UnsupportedOp(/* … */))?;
bt.force_recheck(&tid).await?;
```

### 2.2 为什么用「扩展 trait 访问器」而非备选方案

| 方案 | 评价 |
|---|---|
| **A. 访问器 downcast**（本提案） | 零新依赖；`dyn` 兼容；渐进迁移（旧方法可先留默认转发）；Rust 2021 即可 |
| B. `Any` + 具体类型 downcast | 要求调用方知道具体类型，注册表失去 `dyn` 意义 |
| C. enum dispatch（`EngineEnum` 包一层） | 每加一个引擎要改 enum + 全部 match；与现有 `Arc<dyn>` 存储冲突 |
| D. capabilities 位标志查询 | 仍需在 trait 上留全量方法，只是把撒谎换成查询，没解决面的问题 |

### 2.3 迁移路径（三步，每步独立可合）

1. **Step 1（已完成，本批）**：消灭「撒谎」——`HttpEngine` 与 trait 默认的
   BT 语义方法全部返回 `Unsupported`。行为零变化（daemon 全部调用点已按
   kind 守卫，回归全绿）。
2. **Step 2**：core 增加 `BtEngineExt`/`HttpEngineExt` + 访问器默认 `None`；
   `BtEngine`/`HttpEngine` 实现访问器。**trait 面暂不动**（40 方法保持），
   daemon 侧新代码开始走扩展。零破坏。
3. **Step 3**：把 5 个 BT 概念必需方法移入 `BtEngineExt`，trait 收缩到
   最小契约；ops.rs 的 52 处判断逐域改写（peers/trackers/webseed/priorities
   /recheck 五个域先行）。全量测试回归 + 锁模型复查（不改锁，只改调用形状）。

### 2.4 明确不做

- 不改 `EngineKind` 枚举与 `engine_for` 注册表形状
- 不改锁模型（LOCK_MODEL.md 不变量不受影响——扩展 trait 是只读借用）
- 不动 `FakeEngine`（测试替身语义自洽）
- 不与 TD-4（ops.rs 按域拆分文件）混同批——先拆 trait 再拆文件，
  避免同一 PR 双重移动

---

## 3. 风险与对策

| 风险 | 对策 |
|---|---|
| 大范围签名移动引发冲突（#128 等开放 PR） | Step 2 零破坏先行；Step 3 等开放 PR 合并窗口再动 |
| 异步 trait 对象安全（`dyn BtEngineExt`） | `#[async_trait]` 全链已验证（现有 40 方法同机制） |
| 迁移期两套入口并存 | Step 3 完成后加 `#![allow]` 清理 + clippy 门禁防回潮 |

## 4. 收益预估

- 40 方法 → 9（最小契约）+ 22（BtExt）+ 1（HttpExt）+ 8（通用可选默认）
- ops.rs 52 处 kind 判断 → 按域递减（能力询问内聚到 `bt()`/`http()` 一次）
- 「新增引擎能力漏改 N 处判断」类缺陷结构性消失
