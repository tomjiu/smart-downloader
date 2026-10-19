## CI 已全绿 👍 但请先修两个 P0 再合并

`mergeState: CLEAN`、4/4 job 通过 —— fmt 和 rustls advisory 都修好了，谢谢。

但**我在 batch10 里报的两个 P0 一个都没修**，代码与 `169ff0c` 完全一致。
按用户要求，这两个修完再合并。

---

## P0-1：`cid_store` 二分边界用错（`<=` vs `<`）

### 位置

`crates/xunlei-convert/src/cid_store.rs:224`（`extract_binary_entries` 内）

### 现状

```rust
// ① 二分：首个 off > i 的路径（paths 已按 off 升序）
let after_idx = paths.partition_point(|&(off, _)| off <= i);   // ← 用 <=
let after = paths
    .get(after_idx)
    .filter(|&&(off, _)| off <= i + 64 + hlen);
// ① 二分：最后一个 off < i 的路径（同一次 partition_point
//    的前驱，分摊一次查找）
let before = if after_idx > 0 {
    paths[..after_idx].last().filter(|&&(off, _)| i <= off + 64)   // ← 用了 after_idx
} else {
    None
};
```

**注释写的是「最后一个 `off < i`」，但 `after_idx` 是按 `off <= i` 算的。**

当 `paths` 里存在 `off == i` 的条目时，`paths[after_idx - 1]` 就是**那条 `off == i` 的元素本身**，
被当成了「前驱」。而原实现（main）要求**严格** `off < i`。

### 最小复现（一行就能证明）

```rust
paths = [(51, "p0")], i = 51

原实现 (main)  : before = None        ← 51 < 51 为假，不算前驱
batch10       : before = Some(51)    ← 把【自身】当前驱   ❌
```

### 全量等价性验证

我复刻了原实现与 batch10 实现逐点对比（`repro/p0repro/src/round12.rs`）：

```
seed=0xdeadbeef: 检查 62217 点  batch10 差异=1500  修正版差异=0
seed=0x12345678: 检查 62940 点  batch10 差异=1500  修正版差异=0
seed=0xcafebabe: 检查 62316 点  batch10 差异=1500  修正版差异=0
```

另一测法（`round10.rs`）：8889 个检查点 → **243 处不一致（2.7%）**。

**性质是「错配」不是「漏检」**：输出的 `CidStoreEntry.hash ↔ path` 配对错乱，
path 指向自身而非真正的相邻路径。

### 建议修复（保持 O(log n)）

```rust
// after 用 <= ：取首个 off > i
let gt_i = paths.partition_point(|&(off, _)| off <= i);
let after = paths.get(gt_i).filter(|&&(off, _)| off <= i + 64 + hlen);

// before 用 <  ：取最后一个严格 off < i 的
let lt_i = paths.partition_point(|&(off, _)| off < i);
let before = if lt_i > 0 {
    paths[lt_i - 1].clone().into()
} else {
    None
}.filter(|&&(off, _)| i <= off + 64);
```

`gt_i` 与 `lt_i` 只在 `paths` 中存在 `off == i` 时不同 —— **而那一刻正是缺陷触发点**。

### 建议补的回归测试

```rust
#[test]
fn before_neighbor_requires_strictly_less_offset() {
    // 单条路径 off == 窗口起点 i → before 必须是 None（不能把自己当前驱）
    let paths = vec![(51usize, "p0".to_string())];
    // 断言 find_before(&paths, 51) == None
}
```

---

## P0-2：`tasks` / `slot_reservations` 锁序相反 → ABBA 死锁

### 位置

- `crates/daemon/src/state/ops.rs:507`（`attach_reserved_handle`）
- `crates/daemon/src/state/ops.rs:552`（`rollback_slot_reservation`）
- 对照：`ops.rs:441-444`（`gate_or_enqueue`）与 `ops.rs:682`（递补筛选）

### 锁序冲突

两把锁都是 `parking_lot::Mutex`（不可重入、**无超时**）：

```rust
tasks: Mutex<HashMap<TaskId, TaskRecord>>,
slot_reservations: Mutex<HashSet<TaskId>>,
```

**路径 A —— `tasks → slot_reservations`：**

```rust
// ops.rs:441-444  gate_or_enqueue（batch10 新写）
let mut tasks = self.tasks.lock();                                     // ① tasks
Self::active_slot_counts_locked(&tasks, &self.slot_reservations.lock())  // ② slot_res

// ops.rs:682  递补候选筛选
)> = tasks
    .iter()
    .filter(|(_, rec)| {
        ...
        || self.slot_reservations.lock().contains(&rec.task.id)         // ② slot_res
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

**线程 1 持 `tasks` 等 `slot_res`，线程 2 持 `slot_res` 等 `tasks` → 永久阻塞。**

### 实证

最小同步模型复刻两条路径（`repro/p0repro/src/round18.rs`）：

```
2 秒后完成数: 0/2
❌ 死锁：两个线程互相等待对方持有的锁
   parking_lot::Mutex 无超时 → 永久阻塞
```

### 历史与 batch10 的影响

- **ABBA 在 main 就存在**（`active_slot_counts` 早就是 `tasks → slot_res`）
- batch10 把「计数 + 插入 + 登记」合并进同一 `tasks` 临界区（**修超卖 TOCTOU，方向正确**）
- 但**持锁时间变长 → 死锁窗口增大**

### 触发条件

```
线程 1: add 任务 → gate_or_enqueue 持 tasks 锁
线程 2: 另一任务 engine.add 完成 → attach_reserved_handle 持 slot_reservations 锁
```

批量添加任务 + 引擎接入完成重叠时触发。**后果：daemon 永久卡死，无超时无恢复。**

### 建议修复：统一锁序为 `tasks → slot_reservations`

```rust
// attach_reserved_handle
pub(super) async fn attach_reserved_handle(&self, ...) {
    let attached = {
        let mut tasks = self.tasks.lock();                  // ← 先 tasks
        self.slot_reservations.lock().remove(task_id);      // ← 再 slot_res
        match tasks.get_mut(task_id) {
            Some(rec) if rec.engine_tid.is_none() && rec.task.state == *expected_state => {
                // ... 原有逻辑不变
                true
            }
            ...
        }
    };   // ← 两把锁一起释放
    if attached {
        self.autosave();
        self.hub.publish(SchedulerEvent::StateChanged { ... });
    } else {
        // ... 原有回滚逻辑
    }
}

// rollback_slot_reservation 同理：先 tasks，再 slot_reservations
```

**注意**：`autosave()` 和 `hub.publish()` 必须留在**锁外**（batch10 已注意到
「autosave 重入 tasks 锁自死锁」这个坑，别破坏它）。

### 建议同时做的一件事

在 `docs/LOCK_MODEL.md` 里**明确规定全局锁序**。
该文档目前只讲「任何时刻至多持一把锁」，但 `tasks` 与 `slot_reservations`
实际经常同时持有 —— **文档已过时**。

---

## 为什么 CI 全绿却没抓到这两个

这不是 CI 的问题，是**测试覆盖的边界**：

| P0 | 为什么 1000+ 测试没抓到 |
|---|---|
| `cid_store` 二分 | 单测只断言「能返回结果」，未断言「结果与原实现逐点一致」 |
| ABBA 死锁 | 需要并发时序巧合；随机顺序测试命中率极低 |

**建议补的测试**：
- `cid_store`：加一个「新旧实现逐点等价」的属性测试（我 `round10.rs` 的写法可直接搬）
- ABBA：加一个并发压测（多线程同时 `gate_or_enqueue` + `attach_reserved_handle`），
  断言在超时内全部完成

---

## 合并建议

**修完这两个 P0 我就确认可以合并。**

这两处改动都很小：
- `cid_store`：约 6 行（多个 `partition_point` + 用 `lt_i`）
- `ops.rs`：2 处函数内锁序调整（逻辑不变）

相对地，如果现在合并，main 会带着「数据错配」+「死锁」两个风险，
而这两个恰恰是**最难在事后定位**的类型。
