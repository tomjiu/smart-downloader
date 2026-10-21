## 合并总结：15/18 已合，3 个需 rebase

主分支 `main` 现状：`b609522`（本轮已合 15 个 PR）。

### ✅ 已合并（15）

| PR | 内容 |
|---|---|
| #142 | webpki-roots 安全升级（解 CI 全局阻塞） |
| #126 | refs/ 损坏 gitlink 清理 |
| #134 | 死代码清理 1080 行 + flashget:// 抢救落位 |
| #135 | NAS 引擎 403 重试双缺陷（P1-20/P1-21） |
| #136 | 第三轮 P2 健壮性清扫 |
| #139 | 操作日志环形上限 + sftp 移除竞态 |
| #125 | 并发死锁 + 生命周期 + 安全三维实机测试 + RSS 丢失更新修复 |
| #128 | fsync 批量落盘策略 + 乱序账本被拒全量重下修复 |
| #141 | /rss/feeds 回读 interval_override_secs |
| #131 | 引擎不再对 BT 语义方法撒谎 + TD-2 提案 |
| #132 | RSS 纯逻辑抽出为 smart-dl-rss |
| #130 | bencode 统一到 core::bencode |
| #116 | UI Phase 1 骨架 + **Issue #127 六项实测修复全部落地** |
| #133 | UI P2 虚拟滚动 |
| #140 | 契约审计 A1/A2/A3 |

### ❌ 待 rebase（3）

| PR | 冲突原因 | 建议做法 |
|---|---|---|
| **#138** TD-3 I/O 下沉 | 与 #132（RSS 抽 crate）同改 `crates/core/src/session/` 与 daemon 侧 | `git rebase origin/main`，`session/rss` 相关已迁出，本地改动应只剩 output/storage |
| **#137** BtEngine → btcore | 与 #134（死代码清理）同改 `crates/core/src/` 与 daemon | rebase 后注意 `sniffer`/`xltd` 已删，`bt.rs` 引用需相应调整 |
| **#129** Playwright e2e | 与 #133（虚拟滚动）同改 UI 表格 DOM | rebase 后 e2e 选择器需适配虚拟滚动（行可能不在 DOM 中，需 `scrollIntoView` 或按索引定位） |

**#129 特别注意**：虚拟滚动后，不可见行不在 DOM 里。
e2e 断言「第 N 行存在」会失败，需要改为：
- 先滚动到该行（`page.locator('[data-row-index="N"]').scrollIntoViewIfNeeded()`）
- 或断言「可视窗口内的行数」而非「全量行数」

---

## 下一步开发建议（基于当前 main）

按 **投入产出比** 排序。前置条件：`main = b609522` 已含全部 15 个 PR。

### 第一梯队：补完 #121 剩余的实机验证（1 周）

`REALTEST-PLAN.md` 的四维度中，A/B/C/D 已由 #125 + #129 覆盖大部分，但仍有缺口：

| 缺口 | 说明 |
|---|---|
| **契约矩阵只做了 15/23** | `contract_matrix.rs` 现有 15 条，缺 #2/#3（torrent_b64 / metalink_b64）、#11（sequential）、#13（任务级 proxy）、#14（piece-priority）、#15（super-seeding）、#18（batch）、#20（trackers）、#23（PUT /settings 生效验证） |
| **ts-rs 类型生成未做** | REALTEST-PLAN §A-1 方案 1。这是**根治字段名不一致**的手段（编译期报错而非运行时静默） |
| **CI 未接入契约矩阵** | 现在契约矩阵只在本地跑，需加 job 防止回退 |

**建议**：先做 ts-rs（1 天）→ 再补 8 条契约（1 天）→ CI 接入（0.5 天）。

### 第二梯队：UI 完整度（1-2 周）

`UI-STATUS-ASSESSMENT.md` 的 31 条未接路由，本轮只补了部分。仍缺：

| 功能 | 后端路由 | 用户价值 |
|---|---|---|
| **种源管理**（tracker 增删查 / peer / webseed / super-seeding / recheck） | 9 条现成 | BT 用户 80% 排障动作 |
| **文件选择优先级** | `/tasks/:id/files/priority` | 大种子只下需要的文件 |
| **批量操作栏**（多选 + 批量暂停/恢复/删除） | `/tasks/batch` | 任务多时刚需 |
| **队列优先级** | `/tasks/:id/priority` | 同上 |
| **IP 封禁页** | `/security/bans` 4 条 | PT 用户 |
| **新建任务表单扩展**（torrent_b64 / metalink_b64 / headers / auth / sha256 / 定时） | `/tasks` 全字段 | **现在只能贴裸 URL** |

**建议优先级**：种源管理 > 新建表单扩展 > 批量 > 其余。

### 第三梯队：架构重构收尾（2-3 周）

`ARCHITECTURE-REVIEW.md` §6 的 10 项建议，本轮完成 6 项（TD-1/3/4 + RSS 抽 crate + 引擎契约 + 死代码）。

**剩余**：

| # | 建议 | 状态 |
|---|---|---|
| **5** | 拆分 `DownloadEngine` trait（40 方法 → EngineCore 13 + BtEngineOps + HttpEngineOps） | ⚠️ #131 只做了提案（TD-2），未实施 |
| **7** | `core/src/strategy.rs` 迁回 btcore | ❌ 未做 |
| **9** | `rss.rs` 独立成 crate | ✅ #132 完成 |
| **10** | `state/ops.rs` 按域拆分（4,534 行 → 5 个模块） | ❌ 未做 |

**建议**：#10 优先（纯移动，零风险，但收益大）→ 然后 #7 → #5 最后（涉及 trait 接口变更，影响面最大）。

---

## 文档一致性检查结果（重要）

合并后我核对了文档与实际代码，发现**若干不一致**，建议限期修正。

### 🔴 严重：`ARCHITECTURE-REVIEW.md` 的旧汇总表仍在误导

**§0 或 §4 的汇总表写着：**

> 死代码 ~1,800 行 | `core::sniffer`(735，0 消费者)、`core::xltd`(216)、`core::session::single_instance`(34)

**而 §6.5 已经推翻了这个判断**（明确写「大部分不能删」）。

**实际后果**：PR #134 就是引用了这张旧表才动手删的。
虽然它验证后处理得不错（`sniffer` 的 `flashget` 抢救出来了），
但**这是运气好，不是流程对**。

**建议**：
1. **删掉或改写 §0/§4 的那一行**，改为指向 §6.5
2. 或在旧表每行加删除线 + 指向 §6.5 的链接

### 🟡 `REALTEST-PLAN.md` 完成度需更新

该文档写的是计划，现在 #125/#129 已实现大部分。建议在文档顶部加：
「状态：维度 B/C/D 已由 #125 完成；维度 A 完成 15/23 条（#124）；Playwright e2e 由 #129」

### 🟡 `UNWIRED-CAPABILITIES.md` 需更新状态

5 项未接线现在状态：

| # | 能力 | 状态 |
|---|---|---|
| 1 | `should_retry` | ✅ #122 |
| 2 | `Progress`/`Speed` 事件 | ✅ #117 |
| 3 | `QuarkProvider` | ✅ #119 |
| 4 | `sniffer` | ✅ #134（不接线，但 flashget 抢救） |
| 5 | `web_seeds` | ✅ #118 |

**建议整篇标记为「已全部处置」**，并加一行指向各 PR。

### 🟡 `UI-STATUS-ASSESSMENT.md` 已被 #116 大幅改写

该文档描述的是重构前状态。**#116 已合并**，现状完全不同。

**建议**：要么删除，要么在顶部加「⚠️ 本文档描述的是 #116 合并前的状态，最新状态见 issue #127 / PR #116」

### 🟢 `PR114-AUDIT.md` §4 的承诺已验证

> 引用的是损坏的 gitlink

✅ #126 已清理，且验证了 `git submodule status` 零输出。

---

## 建议的文档整理 PR

建议开一个 docs-only PR 做以下事（零代码风险）：

1. `ARCHITECTURE-REVIEW.md`：**修正 §0/§4 的旧死代码表**（最重要）
2. `UNWIRED-CAPABILITIES.md`：标记全部已处置 + 指向 PR
3. `REALTEST-PLAN.md`：加状态标注
4. `UI-STATUS-ASSESSMENT.md`：加「已被 #116 取代」警示
5. 新增 `docs/STATUS.md`：**单一事实来源**，列出当前版本、已合并能力、待办、已知限制

**第 5 项最有价值** —— 现在有 20+ 份 review/design 文档，没人知道哪份是最新的。
`STATUS.md` 应该成为唯一入口。
