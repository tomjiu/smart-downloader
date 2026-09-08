# Daemon 锁模型与并发不变量（2026-09 锁模型审计报告）

本文是 daemon 并发面的长期工程资产：记录全部锁的清单、审计后确立的
不变量、审计方法与结论，以及未来演进时必须遵守的加锁规约。任何触碰
`DaemonState` 锁面的 PR 都应对照本文复查。

## 1. 锁清单

| 文件 | 锁 | 类型 | 保护对象 |
|------|-----|------|---------|
| state.rs | `tasks` | std Mutex | 任务记录主表 `HashMap<TaskId, TaskRecord>`（主锁，临界区最多） |
| state.rs | `default_dest_root` / `allowed_roots` | std Mutex | 默认下载目录 / dest 白名单 |
| state.rs | `config_snapshot` / `pending_file_prio` / `global_limits` | std Mutex | 配置快照 / 文件优先级待重放集 / 全局限速 |
| state.rs | `webhook_url` / `post_move_to` / `post_hook` / `cleanup` | std Mutex | 完成通知与后处理配置 |
| ws.rs | `queue` | parking_lot Mutex | 事件历史缓冲队列（4096） |
| bt.rs | `pause_intents` / `network` | parking_lot Mutex | BT 暂停执法意图 / 网络配置缓存 |

HTTP 层（http.rs/serve.rs/ws.rs 路由）与 main.rs **不直接持有任何锁**，
全部经 `DaemonState` 方法进入——锁面完全集中在 state.rs。

## 2. 核心不变量（审计后确立）

1. **任何时刻至多持一把锁**。全部临界区都是单锁作用域；跨锁操作一律
   「取 A → 克隆所需数据 → 释放 → 再取 B」（参照 `dest_roots` 与
   `resume` 的块作用域模式）。
2. **`autosave()` 只允许在锁外调用**。其内部经 `persisted_tasks()` 取
   `tasks` 锁（Bug B 修复模式，见 `apply_bt_alert` 注释）。违反 =
   同线程自死锁 → alert 循环挂死 → 全端点 hang。
3. **引擎 FFI/IO 只在锁外**。bt.rs 的 `apply_network`、`pause`、
   `status` 等调用全部发生在自身锁的 guard 释放之后；state.rs 持
   `tasks` 锁期间不调用任何引擎方法（`activate_one` 等调用点均在
   guard 块外）。
4. **锁依赖图必须是 DAG**。bt.rs（引擎层）不反向引用 DaemonState
   （单向依赖）；因此 `tasks → pause_intents/network` 即使在调用图上
   相邻也不构成环。
5. **持锁不跨 `.await`**。由 `clippy::await_holding_lock`（clippy
   五门禁 `-D warnings` 的一部分）持续机检，编译即拦截。

## 3. 2026-09 审计方法与结论

静态分析四层（脚本化，可复跑）：

- **[A] 自重入**：对每个函数构建调用图与传递闭包，判定「持锁 L 的
  guard 存活区间（NLL 近似：绑定行 → 变量最后一次真实使用行）内」
  是否调用会再取 L 的兄弟函数或 `autosave`/`persisted_tasks`。
  guard 存活区间剔除两类假阳性：`.field` 字段访问与同名 shadowing
  重新绑定（区间在该处截断）。
- **[B] 同函数多锁**：命名 guard 存活区间两两重叠判定 + 按获得顺序
  记录锁序边，反查反向共存对。
- **[C] 内联临时锁盲区**：命名 guard 区间内出现 `self.X.lock()` 内联
  临时（如 `lock().clone()` 链）——[B] 看不到的边，单独扫描。
- **[D] 持锁跨 await**：交由 clippy 门禁（不变量 5）。

结论（main = 5a0d07e 时点）：

- 自重入：**0 处**。Bug B 修复模式（autosave 锁外化）已全域生效，
  含 `apply_bt_alert`、19 个 add/set/pause/resume 入口。
- `autosave` 持锁调用：**0 处**（19 个调用点全部锁外）。
- 同时持锁：**唯一一条边** `dest_roots`（`allowed_roots` →
  `default_dest_root`），无反向共存（无死锁风险）；已在同批 PR 中
  消除（改为顺序获取），不变量 1 由「事实上如此」升级为「结构性保证」。
- 持锁跨 await：clippy 门禁通过。

## 4. 加锁/改锁规约（面向未来演进）

1. 新增锁前先问能否复用既有锁；两个数据总是原子地一起读写才值得
   合并进同一把锁，否则各自独立锁 + 单锁临界区。
2. 持锁临界区内**禁止**：调用 `autosave`/`persisted_tasks`、调用任何
   会 `self.<其它锁>.lock()` 的方法（包括看似无害的 getter）、执行
   引擎 FFI、网络/文件 IO。需要这些效果时：锁内提取所需数据
   （clone 出来），锁外执行副作用。
3. guard 一律块作用域（`{ let g = ...; ... }`），避免函数级长持有；
   顺序获取两把锁时第一把必须在取第二把前释放（参照 `dest_roots`）。
4. 同函数内多个同名 guard（不同块）会干扰静态审计工具，命名建议
   带后缀区分（`tasks_a`/`tasks_b`）。
5. 触碰锁面的 PR 须在描述中声明：改了哪些临界区、是否引入多锁同持、
   是否调用 autosave/FFI，并复跑审计脚本比对。

## 5. 残余风险声明（记录在案，非锁债）

- `enforce_pauses` 每 500ms 补偿压制的 pause 复活问题属**调度层与
  引擎层状态不同步**债（metadata alert 尊重记录态方为根治），非锁
  模型问题。
- 静态审计对跨文件间接调用（经 trait 对象回调进 state 的路径）覆盖
  有限；当前唯一 trait 回调面（BtAlertEffect 提取）已人工核过。
- e2e 时序脆弱性（固定 sleep/轮询窗口）独立成项，见测试基建改进线。
