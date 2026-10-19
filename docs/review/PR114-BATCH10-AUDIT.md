## 🔴 新发现：batch10 的 `cid_store` 二分优化引入了**数据正确性回归**

上一轮退回时我列了「batch10 新增部分未逐行审」，现在补审了，
**在 `crates/xunlei-convert/src/cid_store.rs` 发现一个必须修的缺陷。**

---

## 缺陷：`partition_point` 边界用错（`<=` vs `<`）

### 现状代码（batch10 `P0-14` 修复里）

```rust
// ① 二分：首个 off > i 的路径
let after_idx = paths.partition_point(|&(off, _)| off <= i);
let after = paths.get(after_idx).filter(|&&(off, _)| off <= i + 64 + hlen);
// ① 二分：最后一个 off < i 的路径（同一次 partition_point 的前驱，分摊一次查找）
let before = if after_idx > 0 {
    paths[..after_idx].last().filter(|&&(off, _)| i <= off + 64)
} else { None };
```

注释写的是「最后一个 **off < i** 的路径」，但 `after_idx` 是按 **`off <= i`** 算出来的。
当 `paths` 中存在 `off == i` 的条目时，`paths[after_idx - 1]` 就是**那条 `off == i` 的元素本身**，
而原实现要求的是**严格** `off < i`。

### 最小复现（不用构造复杂输入）

```rust
paths = [(51, "p0")], i = 51

原实现   (main)  : before = None          ← 51 < 51 为假，不算前驱
batch10          : before = Some(51)      ← 把【自身】当成了前驱   ❌
修正实现         : before = None          ← 恢复严格小于语义
```

### 全量等价性验证（`repro/p0repro/src/round12.rs`）

```
seed=0xdeadbeef: 检查 62217 点  batch10 差异=1500  修正版差异=0
seed=0x12345678: 检查 62940 点  batch10 差异=1500  修正版差异=0
seed=0xcafebabe: 检查 62316 点  batch10 差异=1500  修正版差异=0
```

另一组测法（round10）：8889 个检查点 → **243 处不一致（2.7%）**。

### 后果：是「错配」不是「漏检」

触发条件：hash 窗口起点 `i` 恰好等于某条路径的 `offset`。

此时输出

```rust
CidStoreEntry {
    hash_hex: <位置 i 的 hash 块>,
    path:     <同一个位置的路径>,   // ← 指向自己，而非真正的相邻路径
}
```

候选条目的 `hash ↔ path` 配对错乱。**这是数据正确性回归，不是性能问题。**

`cid_store` 的输出本身是「候选 ≠ 事实」的 best-effort 结果，
所以不会崩溃、也不会让下载出错 —— 但**性能优化不应该改变输出语义**，
而且这个改动是**新引入**的（main 上没这个问题）。

---

## 修复（保持 O(log n) 等价语义）

```rust
// after 用 <= ：取首个 off > i
let gt_i = paths.partition_point(|&(off, _)| off <= i);
let after = paths.get(gt_i).filter(|&&(off, _)| off <= i + 64 + hlen);

// before 用 <  ：取最后一个严格 off < i 的
let lt_i = paths.partition_point(|&(off, _)| off < i);
let before = if lt_i > 0 {
    paths[lt_i - 1].clone()          // 或 as_ref()
        .into()
} else {
    None
}.filter(|&&(off, _)| i <= off + 64);
```

`gt_i` 与 `lt_i` 只在 `paths` 中存在 `off == i` 时不同 —— 而那一刻正是缺陷触发点。

**建议补一个回归测试**（对应 `round12.rs` 的最小复现）：

```rust
#[test]
fn before_neighbor_requires_strictly_less_offset() {
    // 单条路径 off == 窗口起点 i → before 必须是 None（不能把自己当前驱）
    // 用 analyze_cid_store 构造出这种布局，断言 candidate 的 path 不指向自身
}
```

---

## 另外两项（batch10 补审结论）

### ✅ 修得好的

| 项 | 评价 |
|---|---|
| `lockfile.rs` unlink 竞态 | **正确** —— Drop 只 `unlock` 不删文件，且同步更新了测试语义（`lock_file_survives_drop_but_unlocks`），这是标准解法 |
| `frame.rs` 越界回绕 | **正确** —— `checked_add` + 复用 `end` 变量 |
| `ftp.rs` PASV 值域校验 | **正确** —— 截断前校验 `>255` 和端口溢出 |
| `cid_store` 性能优化本身 | 方向对（二分 + CAP + BTreeMap），实测 512KB 从 6s 降到 **10ms**，但边界写错（见上） |

### ⚠️ 未达成我原报告的诉求

**`ftp.rs` 的 `is_terminal`（P1-5）：**

```rust
// 修复前：e.starts_with("550") || e.starts_with("5")   ← 前者被后者完全包含（死代码）
// 修复后：e.starts_with('5')                            ← 只删了死代码，语义完全相同
```

`500 Command not understood` / `502 Command not implemented` / `504` 仍然被判为**终态**。

而 `is_terminal` 用在 4 个地方，其中 **`probe_size`** 最关键：

```rust
match size {
    Some(t) => t,
    None => return Err(EngineError::Other(format!("ftp probe failed: {last}"))),  // ← 整个任务失败
}
```

`SIZE` 是 **RFC 3659 的可选扩展**，不少服务器回 `500`。当前实现 → 整个任务直接失败。

**实测对比（`repro/p0repro/src/round9.rs`）：**

```
500 Command not understood   修复前=true  修复后=true  建议版=false
502 Command not implemented  修复前=true  修复后=true  建议版=false
504 ... for that parameter   修复前=true  修复后=true  建议版=false
```

**这条不是新引入的**（两版行为一致），是既存 P1 未修 —— 可以合并后跟进，
但建议至少让 `probe_size` 拿到 5xx 能力缺失时**降级为单流下载**而不是任务失败。

---

## 合并结论更新

| 项 | 状态 |
|---|---|
| `cargo fmt --check` | ❌ 18 文件（上一轮已报） |
| **`cid_store` 二分边界** | ❌ **新发现，必须修** |
| `security gates`（rustls） | ❌ 既有 |

**这两条（fmt + cid_store）修好之前不建议合并。** cid_store 那条改起来只有几行，
但如果不修，等于用一个数据正确性回归换取性能提升。
