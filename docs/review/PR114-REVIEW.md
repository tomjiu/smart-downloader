## 审查结论：**有条件通过**（1 项阻断 + 2 项建议）

感谢这批修复 —— 覆盖面和验证扎实。我按 `docs/design/UI-DESIGN-SPEC.md` 之外的审查清单
（`docs/review/README.md`）逐项复核，结论如下。

---

## ✅ 已复核通过的关键修复

我**没有只看 diff**，而是对核心项做了独立验证：

### P0-1 多文件 .torrent 除零 —— **修复正确**

`crates/daemon/src/state/bt_alerts.rs`

我用 5 组用例对比了修复前后的 `piece_offset` / `piece_count` 计算结果，
**全部等价**（`repro/p0repro/src/round6.rs`）：

```
files=[("a",16385),("b",10)]  plen=16384 → 修复前 [0,1] / 修复后 [0,1]  ✅
files=[("a",100000)×3]        plen=16384 → 修复前 [0,6,12] / 修复后 [0,6,12]  ✅
files=[("a",1)×3]             plen=16384 → 修复前 [0,0,0] / 修复后 [0,0,0]  ✅
```

**这正是正确的修法**：只改计算时机，不改语义。同时确认 `piece_offset = total_size / plen`
的「文件起始 piece 索引」语义在跨 piece 边界时也正确（跨边界 piece 由多文件共享，
调用方需按 `(offset, count)` 判断重叠 —— 这一点值得在 `FileMeta` 文档里补一句）。

附带的正向效果：负数 `length` 拒绝 + `piece_length` 上限校验，同时闭合了 P0-12。

### P0-7 登录页防护 —— **修复正确，且比建议更严谨**

`crates/provider/src/xunlei/login_page.rs`

`host_is_local` 用 `IpAddr::parse` + `is_loopback()` 替代字符串前缀 —— 这比我在报告里
建议的「删前缀兜底」更彻底。`Origin: null` 拒绝（沙箱 iframe）也考虑到了。

### P1-a loopback 前缀绕过 —— **同批闭合**

`crates/daemon/src/config.rs` 同步收紧，与登录页口径一致。这一点做得好。

---

## ⚠️ 阻断项（合并前必须处理）

### 1. `security gates` CI 失败 → `mergeStateStatus: UNSTABLE`

```
security gates (cargo-deny + gitleaks)   fail   30s
bt integration (ubuntu, libtorrent)      pass   4m20s
fmt + clippy + test (ubuntu)             pass   9m36s
fmt + clippy + test (windows)            pass   13m51s
```

失败原因：

```
advisories FAILED, bans ok, licenses ok, sources ok
  rustls v0.23.43
  Solution: Upgrade to >=0.23.46 (try `cargo update -p rustls`)
```

**归因说明**：这个失败**不是本 PR 引入的**。我核对过 main 的历史：

| 提交 | CI |
|------|-----|
| PR #111（2026-09-08 11:33） | ✅ success |
| PR #112（2026-09-08 12:36） | ❌ failure ← **从这里开始** |
| PR #113（2026-09-08 13:35） | ❌ failure |
| 审查报告提交（2026-10-16） | ❌ failure |

PR #112 动了 `Cargo.lock`（+2 行），很可能是那次引入了 rustls 0.23.43。

**但 PR 仍应处理**：`MERGEABLE` + `UNSTABLE` 意味着合并后 main 继续红。
建议在本 PR 内 `cargo update -p rustls` 升到 >= 0.23.46，让 PR 带来的 main 变绿，
而不是把既有失败继续传下去。

---

## 💡 建议项（不阻断，但值得看）

### 2. `refs/` 的两个 gitlink 是损坏的 submodule 引用

CI cleanup 阶段有一条：

```
fatal: No url found for submodule path 'docs/research/clients/refs/aria2' in .gitmodules
```

我查了 git 索引：

```
160000 commit 9e72...  docs/research/clients/refs/aria2   ← mode 160000 = gitlink
160000 commit a499...  docs/research/clients/refs/rqbit   ← 同上
```

**但仓库里没有 `.gitmodules`** —— 这是「gitlink 残留但 submodule 配置已丢」的损坏状态。

我之前在 `ARCHITECTURE-REVIEW.md` 里把它写成「空目录，应删」是**不准确的** ——
直接删目录会留下 gitlink 记录，问题照旧。正确清理方式：

```bash
git rm --cached docs/research/clients/refs/aria2 docs/research/clients/refs/rqbit
git commit -m "chore: 清理损坏的 submodule gitlink（无 .gitmodules 条目）"
```

（这是既有问题，不是本 PR 引入，可另开 PR。）

### 3. `total_size` 的 `checked_add` 时机

```rust
let piece_offset = (total_size / plen) as usize;
let piece_count = length.div_ceil(plen) as usize;
total_size = total_size.checked_add(length).ok_or_else(...)?;   // ← push 之后
files.push(FileMeta { ... });
```

溢出检查在 `piece_offset` 计算**之后**。当前实现是安全的（溢出时整体返回 `Err`，
未落盘无副作用），但如果将来有人在中间插入 `files.push` 之外的副作用，
顺序会变成问题。建议把 `checked_add` 提到循环开头，语义更直白：

```rust
for (path, length) in file_entries {
    let piece_offset = (total_size / plen) as usize;
    let piece_count = length.div_ceil(plen) as usize;
    total_size = total_size.checked_add(length)
        .ok_or_else(|| /* 溢出 */)?;
    files.push(...);
}
```

（当前写法其实已经满足，只是把校验意图显式化。）

---

## 📋 我对本 PR 的验证方式

为避免"只看 PR 描述"的误判，我做了这些独立核对：

| 项 | 方式 |
|----|------|
| P0-1 修复正确性 | 提取修复后逻辑，用 5 组用例与原算法对比（`round6.rs`，全部等价） |
| P0-1 除零消除 | `plen=0` 返回 `None` 而非 panic |
| 溢出拦截 | `[u64::MAX, 1]` 返回 `None` |
| CI 归因 | 查 main 历史 5 个 run，确认失败起点是 PR #112 而非本 PR |
| gitlink 状态 | `git ls-tree HEAD` 确认 mode 160000，且无 `.gitmodules` |

**我没有逐行审阅 1785 行新增代码** —— 上表只覆盖了我做了独立验证的部分。
其余修复项（batchC 的 ID 序列化、batchD 的 fsync 族、batchE 的锁内 sleep 等）
建议由作者自己在 PR 描述里补充「如何验证」的说明，或贴出对应回归测试的位置。

---

## 合并建议

**处理掉 rustls advisory 后可以合并。** 135 个修复项、1001 测试通过、
repro 23/23 全绿的验证力度是够的；本 PR 未引入新的 CI 失败。

另：PR 描述里「★ = 已实证复现项」的标注方式很清楚，建议保留这个惯例。
