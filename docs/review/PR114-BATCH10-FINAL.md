# batch10 剩余 5 文件补审报告

> 审查对象：`daemon/bt.rs`、`daemon/state/ops.rs`、`daemon/rss.rs`、`core/xltd.rs`、`provider/xunlei/provider.rs`
> 审查日期：2026-10-19
> **性质：只做审计，未修改任何代码**

---

## 0. 结论

**发现 2 个严重问题（1 个已实证死锁、1 个已实证数据丢失）+ 1 个残留缺陷。**

批注：**这两个新问题都不是 batch10 引入的**（死锁在 main 就存在，丢失更新是 batch10 的性能优化引入）。

| 等级 | 项 | 状态 |
|---|---|---|
| 🔴 P0 | `state/ops.rs` ABBA 死锁 | **已实证复现**，main 即存在，batch10 扩大窗口 |
| 🔴 P1 | `rss.rs` 锁外落盘丢失更新 | **已实证**，**batch10 引入** |
| 🟡 P2 | `bt.rs` poll_ring 残留吞 alert | batch10 缓解但未根除 |
| 🟢 P3 | `provider.rs` 磁盘落后一代 | 影响可忽略 |
| ✅ | `core/xltd.rs` 修复 | 修得好，含我报的截断 bug |

---

## 1. 🔴 P0：`state/ops.rs` 的 ABBA 死锁

### 锁序冲突

两把锁（`parking_lot::Mutex`，不可重入、无超时）：

```rust
tasks: Mutex<HashMap<TaskId, TaskRecord>>,
slot_reservations: Mutex<HashSet<TaskId>>,
```

**路径 A —— `tasks → slot_reservations`：**

```rust
// ops.rs:444  gate_or_enqueue（batch10 新写）
let mut tasks = self.tasks.lock();                                    // ① tasks
Self::active_slot_counts_locked(&tasks, &self.slot_reservations.lock())  // ② slot_res
 
// ops.rs:682  递补候选筛选（在 tasks 锁内取 slot_reservations）
)> = tasks
    .iter()
    .filter(|(_, rec)| {
        ...
        || self.slot_reservations.lock().contains(&rec.task.id)       // ② slot_res
```

**路径 B —— `slot_reservations → tasks`：**

```rust
// ops.rs:507  attach_reserved_handle
self.slot_reservations.lock().remove(task_id);   // ① slot_res
let attached = {
    let mut tasks = self.tasks.lock();           // ② tasks
    ...

// ops.rs:552  rollback_slot_reservation
self.slot_reservations.lock().remove(task_id);
{
    let mut tasks = self.tasks.lock();           // ② tasks
```

**ABBA 死锁：线程 1 持 `tasks` 等 `slot_reservations`，线程 2 持 `slot_reservations` 等 `tasks`。**

### 实证

`repro/p0repro/src/round18.rs` —— 用最小同步模型复刻两路径，2 秒超时检测：

```
2 秒后完成数: 0/2
❌ 死锁：两个线程互相等待对方持有的锁
   parking_lot::Mutex 无超时 → 永久阻塞
```

### 历史与 batch10 的影响

| 版本 | 状态 |
|---|---|
| main（batch10 之前） | `active_slot_counts` 已是 `tasks → slot_reservations`，**ABBA 已存在** |
| batch10 | 把「计数检查 + insert + 登记」合并进同一 `tasks` 临界区（修超卖 TOCTOU）→ 正确性提升 ✅，但**持锁时间变长 → 死锁窗口增大** ⚠️ |

### 触发条件与后果

```
线程 1: add 任务 → gate_or_enqueue 持 tasks 锁
线程 2: 另一任务 engine.add 完成 → attach_reserved_handle 持 slot_reservations 锁
```

批量添加任务 + 引擎接入完成重叠时触发。**后果：daemon 永久卡死，无超时无恢复。**

### 修法

统一锁序为 `tasks → slot_reservations`（两处都改）：

```rust
// attach_reserved_handle
let attached = {
    let mut tasks = self.tasks.lock();                 // ← 先 tasks
    let mut reservations = self.slot_reservations.lock();  // ← 再 slot_res
    reservations.remove(task_id);
    // ... 更新 rec
    ...
};
// 锁外 autosave/publish
```

并在 `docs/LOCK_MODEL.md` 里**明确规定全局锁序**——该文档目前只讲
「任何时刻至多持一把锁」，但这两把锁实际经常同时持有，**文档已过时**
（这也是我在 `ARCHITECTURE-REVIEW.md` §5.1 报过的问题，batch10 加了「盲区」说明但锁序仍未定义）。

---

## 2. 🔴 P1：`rss.rs` 锁外落盘导致丢失更新（batch10 引入）

### 问题

batch10 `P1-9` 把落盘从锁内移到锁外（性能优化，方向对）：

```rust
let snapshot = {
    let mut st = self.rss_state().lock();
    st.feeds.push(...);          // 改内存
    st.clone()                   // ← 快照（时刻 T1）
};                               // ← 释放锁
self.rss_save_snapshot(&snapshot);   // ← 写盘（时刻 T2）
```

**7 处调用点全部如此**（`rss.rs:699/716/730/806/820/997` 等），且 `rss_save` 内部**没有任何串行化保护**：

```rust
fn rss_save(&self, st: &RssState) {
    if let Some(p) = self.rss_persist_path() {
        st.save(&p);     // 直接整表覆盖写
    }
}
```

### 实证

`repro/p0repro/src/round19.rs`：

```
[时序] A 先锁内加 feed-a，睡眠 20ms 后才写盘
       B 随后锁内加 feed-b，立即写盘

  内存中的 feeds : ["feed-a", "feed-b"]
  磁盘上的 feeds : ["feed-a"]

  ❌ 丢失更新：磁盘与内存不一致
     B 写入的 feed-b 被 A 的旧快照覆盖掉了。
```

**断言通过**：内存 2 条、磁盘 1 条 —— 丢失更新已发生。

### 影响

| 项 | 说明 |
|---|---|
| 触发 | 两个并发 RSS 操作（add/update/refresh/规则匹配） |
| 后果 | 磁盘状态丢失部分变更：重启后新订阅消失、已读标记回退（重复推送） |
| 概率 | 中等（RSS 定时刷新 + 用户手动 add 并发常见） |
| 定性 | **P2 数据丢失**（非崩溃，但用户可见的状态丢失） |

### 修法

```rust
// 方案 1（推荐）：独立落盘串行锁 + 从当前状态取快照
rss_persist_lock: Mutex<()>,

fn rss_save_now(&self) {
    let _g = self.rss_persist_lock.lock();          // 串行化写
    let snapshot = self.rss_state().lock().clone(); // 持锁取最新
    snapshot.save(...);
}
```

代价是要短暂持 `rss_state` 锁——但只持 clone 的时间（远短于原实现的序列化+写盘）。

**方案 2**：版本号 + 乐观写入（`RssState { version: u64 }`，写前比对，变了就重取）。

---

## 3. 🟡 P2：`bt.rs` poll_ring 残留吞 alert

### 现状

batch10 修复把 `poll_ring` 从「循环外采样一次」改为「每轮重采样」+ `dispatch_resume_alerts`：

```rust
while Instant::now() < deadline {
    let poll_ring = !loop_active.load(...);       // ← 每轮重采样 ✅
    if poll_ring {
        if let Ok(alerts) = core.pop_alerts(256) {
            dispatch_resume_alerts(waiters, &alerts);   // ← 分发
            for a in alerts {
                if a.kind == Resume && a.is_resume_ready() { ... }   // 只处理本任务 Resume
            }
```

**但 `dispatch_resume_alerts` 只处理 Resume 类：**

```rust
for a in alerts {
    if a.kind != AlertKind::Resume || a.ih.is_empty() {
        continue;      // ← State/Finished、State/Error 被跳过
    }
```

而 `bt_events.rs` 的常驻循环处理的正是：

```rust
match (&a.kind, a.state_subkind()) {
    (AlertKind::State, StateSubKind::Finished) => ...   // 完成
    (AlertKind::State, StateSubKind::Error) => ...      // 失败
}
```

### 实证

`repro/p0repro/src/round17.rs`：

```
ring: [本任务 Resume, other-1 Finished, other-2 Error, other-3 Resume]
pop_alerts 消费了: 4 条
dispatch_resume_alerts 分发: 2 条（仅 Resume）
❌ 被丢弃的 State 类: 2 条
→ other-1 永不标 Completed
→ other-2 永不标 Failed
```

### batch10 的实际效果

| 版本 | 窗口 |
|---|---|
| 修复前 | 循环外采样一次 → **整个 3s 窗口**内所有非 Resume alert 全被吞 |
| 修复后 | 每轮重采样 → 常驻循环一置位就立刻让位 → 窗口缩短为**启动延迟**（毫秒级） |

**✅ 显著缩小，但根因未除**：`poll_ring` 模式下仍只分发 Resume。

### 触发与影响

- 常驻 alert 循环尚未启动（daemon 刚启动 / 引擎刚创建）
- 此刻恰好有 pause/remove 调用 `save_fastresume_impl`
- 且 ring 里有其它任务的终态 alert

**概率低，但后果是任务永久卡在 `Downloading`。**

**定性：P2 残留缺陷**（非 batch10 引入，是 P0-3 修复未覆盖全部 alert 类型）。

### 修法

`poll_ring` 模式下，pop 到的非 Resume alert 不应丢弃：
- (a) 回填到等待常驻循环的队列（最干净）
- (b) 让 `BtCore` 提供按 ih 过滤的 pop
- (c) 把 `State/Finished` 与 `State/Error` 也投递到统一的 task-state 通道

---

## 4. 🟢 P3：`provider.rs` refresh_auth 磁盘落后一代

### 现状

batch10 的 `refresh_auth` 三段式修复（我报的 P1-2）：

```rust
let snapshot = self.auth.lock().await.clone();      // ① 锁内快照
// ② 锁外刷新（candidate）
let _ = save_auth(&self.token_path, &candidate);    // ← 无条件落盘
{
    let mut guard = self.auth.lock().await;
    let already_advanced = guard.as_ref().is_some_and(|g| {
        g.access_token != old_access || g.captcha_token != old_captcha
    });
    if !already_advanced {
        *guard = Some(candidate);                   // ← 条件回写
    }
}
```

**「落盘无条件 / 内存有条件」→ 两者可能分叉。**

### 实证

`repro/p0repro/src/round20.rs`：

```
内存 access_token = NEW-T1
磁盘 access_token = NEW-T2
⚠️ 内存与磁盘不一致（磁盘落后一代）
```

### 但影响可忽略

注释的推理成立：**两份 token 都是服务端合法签发的**，任取其一都能用。只是磁盘比内存旧一代。

- 内存 token 始终最新（CAS 保证不回退）✅
- 重启后可能多一次刷新，但旧 token 在有效期内仍可用
- **不会导致鉴权失败**

**定性：P3 轻微冗余，不是缺陷。**

---

## 5. ✅ `core/xltd.rs` —— 修得好

按我 `ARCHITECTURE-REVIEW.md` §6.5.1 的结论做了三件事：

| 改动 | 评价 |
|---|---|
| 补模块级文档标注「早期研究雏形，当前未接线」 | ✅ 正是建议的做法 |
| `recover()` 不再 `set_len(last_data_end)` 截断 | ✅ **修了我报的「会损坏以零结尾的文件」缺陷** |
| 加 `MAX_BLOCK_SIZE = 16 MiB` 防 `vec![0u8; block_size]` OOM | ✅ 额外发现的好修复 |
| 加回归测试 `recover_keeps_trailing_legal_zeros` | ✅ |

**这个模块的处理完全符合我 §6.5 的建议（「保留 + 加注释说明」而非删除）。**

---

## 6. 汇总：本轮 5 文件结论

| 文件 | 改动 | 结论 |
|---|---|---|
| `daemon/state/ops.rs` | +63/-34 | 🔴 **ABBA 死锁**（main 即存在，batch10 扩大窗口）；超卖 TOCTOU 修复本身正确 |
| `daemon/rss.rs` | +118/-72 | 🔴 **锁外落盘丢失更新**（batch10 引入）；性能优化方向对但牺牲了正确性 |
| `daemon/bt.rs` | +73/-17 | 🟡 P0-2/P0-3 主修复正确（P0-2 让出锁、P0-3 每轮重采样）；poll_ring 残留 |
| `provider/xunlei/provider.rs` | +50/-15 | 🟢 P1-2 三段式正确（CAS 不回退），磁盘落后一代可忽略 |
| `core/xltd.rs` | +63/-4 | ✅ 完全按 §6.5.1 建议处理，修了我报的截断 bug |

---

## 7. 累计审计结论（batch10 全部）

| 来源 | P0 | P1 | P2 | P3 | ✅好 |
|---|---|---|---|---|---|
| 前一轮（`PR114-BATCH10-AUDIT.md`） | 1（cid_store 二分） | 2 | — | — | 4 |
| 剩余（本报告） | 1（ABBA 死锁） | 1（RSS 丢失更新） | 1（poll_ring） | 1（磁盘落后） | 1 |
| 中间一轮（`PR114-BATCH10-REMAINING.md`） | — | 1（bt.enabled） | 2（热调/注释） | 1（Windows） | 8 |
| **合计** | **2** | **4** | **3** | **2** | **13** |

### 合并前必须修

1. 🔴 `cid_store` 二分边界（`<=` vs `<`）—— **数据正确性回归，batch10 引入**
2. 🔴 `state/ops.rs` ABBA 死锁 —— **daemon 永久卡死**
3. 🔴 `cargo fmt --all --check` 失败（18 文件）—— CI 红

### 建议修（可合并后跟进）

- `rss.rs` 锁外落盘丢失更新（batch10 引入，建议至少加落盘串行锁）
- `bt.rs` poll_ring 残留吞 State alert
- `bt.enabled` 默认值破坏性变更（CHANGELOG 标注）
- `rate.rs` 热调竞态（P3）
- `piece_length` 注释错误（P3）

---

## 8. 新增复现

| 文件 | 内容 |
|---|---|
| `round17.rs` | poll_ring 残留吞 State alert（4 条中 2 条被丢） |
| `round18.rs` | **ABBA 死锁实证**（2 秒 0/2 完成） |
| `round19.rs` | **RSS 锁外落盘丢失更新实证**（内存 2 条 / 磁盘 1 条） |
| `round20.rs` | refresh_auth 磁盘落后一代（可忽略） |
