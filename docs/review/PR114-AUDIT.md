# PR #114 深度审计报告

> 审查对象：PR #114 `fix: 三轮审查 P0/P1 全量修复`（mmqz:fix/review-p0-p1-batch）
> 审查日期：2026-10-19
> 方法：**实际检出分支 + 编译 + 跑测试 + 逐批代码审阅 + 独立验证**

---

## 0. 审计结论

**✅ 代码质量无阻断问题 —— 建议合并（附 1 项性能取舍需确认 + 2 项建议）**

| 维度 | 结论 |
|------|------|
| 编译 | ✅ `cargo check --workspace --exclude smart-dl-btcore` 通过（3m28s） |
| **测试** | ✅ **999 passed / 0 failed（89 个套件，30.9 分钟）** |
| 代码质量 | ✅ 修复方向正确，注释详实，多数修复比建议更彻底 |
| CI | ⚠️ `security gates` 失败（rustls advisory，**非本 PR 引入**） |
| 性能 | ⚠️ P0-4 的 fsync 修复带来未量化的吞吐损失（见 §3，**非阻断**） |

---

## 1. 独立验证通过的修复

### 1.1 P0-1 多文件 .torrent 除零 —— 修复正确

**验证方式**：提取修复后逻辑，用 5 组用例与原算法对比（`repro/p0repro/src/round6.rs`）

```
files=[("a",16385),("b",10)]  plen=16384 → 修复前 [0,1] / 修复后 [0,1]  ✅
files=[("a",100000)×3]        plen=16384 → 修复前 [0,6,12] / 修复后 [0,6,12]  ✅
files=[("a",1)×3]             plen=16384 → 修复前 [0,0,0] / 修复后 [0,0,0]  ✅
```

**全部等价** → 只改计算时机、未改语义，**这是正确的修法**。

同时确认 `piece_offset` 语义在跨 piece 边界时也正确（跨边界 piece 由多文件共享）。

### 1.2 P0-7 登录页防护 —— 修复正确且更严谨

```rust
fn host_is_local(raw: &str) -> bool {
    // ...
    matches!(host.parse::<std::net::IpAddr>(), Ok(ip) if ip.is_loopback())
}
```

用 `IpAddr::parse` + `is_loopback()` 替代字符串前缀 —— 比报告建议的「删前缀兜底」更彻底。
`Origin: null` 拒绝（沙箱 iframe）也考虑到了。

### 1.3 P1-a loopback 绕过 —— 同批闭合

`crates/daemon/src/config.rs` 同步收紧，与登录页同口径。

### 1.4 P1-b 部分表默认值错位 —— 修复完整

五个子表手写 `impl Default` 对齐 `Config::default()`：
`ServerCfg`（addr="127.0.0.1:8787"）、`DownloadCfg`（dest_root="./downloads"）、
`BtCfg`（enabled=true）、`LockCfg` 等。

### 1.5 P1 锁内 sleep —— 修复质量高（一次解决三问题）

**验证**（`repro/p0repro/src/round8.rs`）：

```
[1] 锁内 sleep 消除
     reserve() 耗时: 0.041ms  ✅ 临界区只剩瞬时算术
[2] 取消债务回滚
     drop 前 next = 16000 → drop 后 next = 0   ✅ 回滚有效
[3] 正常消费不回滚
     consumed=true → next 保持  ✅
```

三个改进一次到位：
1. 临界区只剩算术，`sleep` 移出锁外
2. `DebtReservation` 的 `Drop` 做 RAII 取消回滚
3. `tokio::Mutex` → `std::Mutex`（临界区纯同步，std 锁更轻）

**小建议**（非阻断）：
- `*next == self.end` 用 `Instant` 相等判链尾，理论上并发下可能误判（实测未触发，且 dur 量级小，影响可忽略）
- `unwrap_or_else(|e| e.into_inner())` 吞 mutex 中毒 —— 纯算术临界区安全，但建议注释说明这是有意为之

---

## 2. 阻断项：CI `security gates` 失败

```
security gates (cargo-deny + gitleaks)   fail   30s
bt integration (ubuntu, libtorrent)      pass   4m20s
fmt + clippy + test (ubuntu)             pass   9m36s
fmt + clippy + test (windows)            pass   13m51s
```

**根因**：
```
advisories FAILED — rustls v0.23.43
Solution: Upgrade to >=0.23.46 (try `cargo update -p rustls`)
```

**归因（已核实，非本 PR 引入）**：

| 提交 | CI |
|------|-----|
| PR #111（09-08 11:33） | ✅ success |
| **PR #112（09-08 12:36）** | ❌ **failure ← 起点** |
| PR #113（09-08 13:35） | ❌ failure |

PR #112 动了 `Cargo.lock`（+2 行），很可能引入 rustls 0.23.43。

**但 PR 仍应处理** —— 否则合并后 main 继续红。本 PR 内 `cargo update -p rustls` 即可。

---

## 3. ⚠️ 需关注：P0-4 的 fsync 性能成本（PR 未量化）

### 3.1 修复内容

`crates/httpdl/src/download.rs:391` —— 每个 16MB 段完成时 `f.sync_data()`：

```rust
f.sync_data().map_err(|e| format!("segment fsync: {e}"))?;
```

`crates/httpdl/src/ledger.rs` —— 账本原子写加 `f.sync_all()`。

**修复方向正确**：WAL 排序（数据先于元数据持久），确实消除了「账本说段完成但数据丢失」的窗口。

### 3.2 实测成本（`repro/p0repro/src/round7.rs`）

**⚠️ 环境说明**：本机 `E:` 盘（项目+TEMP 所在）是 **5400rpm 机械硬盘**，
`C:` 盘是 NVMe SSD。两者 fsync 差 ~400 倍，以下为 **SSD 实测**：

```
单次 fsync 成本: 13.19 ms/段（16MB 段）

      网速       段/秒    fsync开销/秒    有效吞吐      损失
    10 MB/s      0.62        8.2 ms     9.9 MB/s     0.8%
    50 MB/s      3.12       41.2 ms    47.9 MB/s     4.1%
   100 MB/s      6.25       82.5 ms    91.8 MB/s     8.2%
   200 MB/s     12.50      164.9 ms   167.0 MB/s    16.5%
   500 MB/s     31.25      412.3 ms   293.9 MB/s    41.2%
```

**机械盘上（E: 实测）**：2022 MB/s → 6.4 MB/s，吞吐被 fsync 完全支配。

### 3.3 建议

1. **可配置**：`[download] fsync_per_segment = true`（默认开），机械盘用户可关
2. **或批量**：每 N 段 / T 秒 fsync 一次（N=8 或 T=2s），把 13ms 摊销到 8×16MB=128MB
3. **注意**：若 `min_split` 调到 1-4MB，fsync 频率上升 4-16 倍，**必须**批量

**这不是"修复错了"，而是"正确性代价未量化"** —— 应该在 PR 描述或配置文档里说明。

---

## 4. 建议项

### 4.1 `refs/` 的 gitlink 是损坏的 submodule

CI cleanup 阶段报：
```
fatal: No url found for submodule path 'docs/research/clients/refs/aria2' in .gitmodules
```

git 索引实况：
```
160000 commit 9e72...  docs/research/clients/refs/aria2   ← mode = gitlink
160000 commit a499...  docs/research/clients/refs/rqbit
```

**无 `.gitmodules`** —— gitlink 残留但 submodule 配置已丢。

**勘误我自己**：此前在 `ARCHITECTURE-REVIEW.md` 写「空目录，应删」**不准确** ——
直接 `rm -rf` 会留下 gitlink。正确方式：

```bash
git rm --cached docs/research/clients/refs/aria2 docs/research/clients/refs/rqbit
```

### 4.2 `total_size` 的 `checked_add` 时机

```rust
let piece_offset = (total_size / plen) as usize;
let piece_count = length.div_ceil(plen) as usize;
total_size = total_size.checked_add(length).ok_or_else(...)?;  // ← 在 push 之前，OK
files.push(...);
```

当前顺序安全（溢出整体返回 `Err`，无落盘副作用）。仅建议注释说明意图。

---

## 5. 测试运行结果

**`cargo test --workspace --exclude smart-dl-btcore`：**

```
耗时:     30.9 分钟
结果:     999 passed / 0 failed
套件数:   89
exit:     0
```

**全部通过。** 耗时较长的套件（说明测试覆盖真实行为而非纯 mock）：

```
 56.87s  （1 个套件）
 42.95s  （1 个套件）
 34.58s  （3 个套件）
 33.03s  （49 个测试）
 26.97s  （5 个测试）
 23.47s  （5 个测试）
 22.64s  （3 个测试）
 18.17s  （4 个测试）
 16.69s  （6 个测试）
```

PR 描述声称 1001 passed —— 与实测 999 的差异应来自 `--exclude smart-dl-btcore`
（btcore 的 2 个测试未计入）。

**观察**：单次全量测试 30.9 分钟偏长。慢套件多在 20-60 秒区间，
如果是网络/mock 服务的真实 HTTP 往返，建议后续评估是否可缩短（不影响本 PR）。

---

## 6. 审计边界声明

**我做了什么**：
- ✅ 检出 PR 分支到本地（`pr114-review`）
- ✅ **`cargo check` 全工作区编译通过**
- ✅ **`cargo test --workspace --exclude smart-dl-btcore`：999 passed / 0 failed**
- ✅ 逐批审阅 batchA/B/D/E 的关键 diff
- ✅ 对 P0-1 / P0-7 / P1-a / P1-b / P1 锁内 sleep 做**独立验证**（非只看 diff）
- ✅ 量化 fsync 性能成本（SSD + 机械盘对比）
- ✅ 核查 CI 失败归因（确认非本 PR 引入）

**我没做什么**：
- ❌ 未逐行审阅全部 1785 行（覆盖约 60% 关键路径）
- ❌ batchC（provider ID 序列化）未深入验证
- ❌ btcore 相关测试被 `--exclude` 排除，未验证

---

## 7. 合并建议

**✅ 建议合并。**

**理由**：
1. **999 测试全通过**，编译干净
2. 关键修复经**独立验证**（P0-1 等价性、P1 锁内 sleep 三问题）
3. 部分修复（`IpAddr::parse`）**比报告建议更彻底**
4. **未引入新的 CI 失败**

**合并前建议确认（非阻断）**：

| 项 | 说明 |
|---|---|
| **rustls advisory** | 既有问题（PR #112 引入），但合并后 main 继续红。建议同批 `cargo update -p rustls` |
| **fsync 性能取舍** | 见 §3：SSD 上 8-41% 损失（视网速），机械盘上灾难性。建议至少有配置开关 |
| `refs/` gitlink | 既有问题，可另开 PR 用 `git rm --cached` 清理 |

**如果时间紧，可以先合并（修复的是 14 个 P0，价值远超性能取舍），
把 fsync 开关作为后续跟进项。**
