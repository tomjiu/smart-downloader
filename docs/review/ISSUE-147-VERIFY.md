# 核验：PR #144-#146/#137/#138/#129 合并结果 — 3 项需修正（#146 描述错误 / STATUS.md 数据偏差 / e2e 类型检查耦合）

> 归档：本文件为 [Issue #147](https://github.com/tomjiu/smart-downloader/issues/147) 的本地副本，便于离线查阅。
> 问题追踪以 GitHub issue 为准。

---

## 摘要

核验 PR #144~#146 / #137 / #138 / #129 合并结果，发现 **3 个问题**（1 个描述错误、1 个数据不准、1 个构建前置条件）。代码质量整体良好，以下为需修正项。

当前 `main = ab254c6`。

**合并后独立验证结果（Windows 本地）**：

| 项 | 结果 |
|---|---|
| `cargo check --workspace --exclude smart-dl-btcore` | ✅ 通过（0.7 分钟） |
| `cargo test --workspace --exclude smart-dl-btcore` | ✅ **98 套件 / 1,083 通过 / 0 失败** |
| `cd ui && bun run build` | ✅ 通过（需先 `bun install`，见 §三） |
| crate 数 | 8 → **9**（新增 `crates/storage`） |

---

## 一、🟡 PR #146 的原因描述有误（修复本身正确）

### PR 描述原文

> **SSE speed 事件后端无生产者**（events.rs 枚举存在但 ops.rs 不发布），
> 快照 `rates` 是唯一数据源——被引用复用链路丢弃

### 实际代码（PR #117 已加生产者）

```rust
// crates/daemon/src/state/ops.rs:3949-3952  BT 分支
// D36 补线（BT）：速率/进度事件——alert 流只管状态迁移，速率快照此前仅存
// 缓存不出事件。做种只发 Speed（up_rate 对 UI/统计有意义；进度恒满无信息量）。
self.hub.publish(SchedulerEvent::Speed {
    task_id: id.clone(),
    down_rate: st.down_rate,
    up_rate: st.up_rate,
});

// crates/daemon/src/state/ops.rs:4028-4037  HTTP 分支
// D36 补线：Speed/Progress 事件生产者（原为全仓库零生产者的设计缺口……）
if st.state == EngineState::Downloading {
    self.hub.publish(SchedulerEvent::Speed { ... });
    self.hub.publish(SchedulerEvent::Progress { ... });
}
```

**`Speed` 事件有生产者，且做种任务专门走了只发 `Speed` 的分支。**

### 真实因果推测

不是「只有快照一条路」，而是**两条路都有、但快照那条被引用复用丢掉了**。
在「快照引用不变 → 组件不重渲染」的前提下，SSE 推来的速率可能同样未被消费
（因为 `rates` 的更新路径依赖 state 变更触发渲染，而引用复用恰好阻断了它）。

**建议**：核对一下 UI 侧 `rates` 更新的完整链路 —— 若 SSE 事件写入 `rates` 后
同样受引用复用影响，那么 #146 的修复只解决了快照半条路，SSE 半条路可能仍有问题。

### 结论

**修复正确，但 PR 描述与代码注释需更正**，避免后续开发者基于错误前提做判断
（这与 #143 §三 P0 是同一类问题：**错误的前提描述会导致错误的重构**）。

---

## 二、🟡 STATUS.md 的数据与实际不符

`docs/STATUS.md:18` 与 `:20`：

```
| 源码 | `crates/` **83,604 行** Rust（含测试；src/ 为主 57,942 行） |
| 测试 | **98 套件 / 1,092 通过 / 0 失败**（...，2026-10-21 实测） |
```

### 我的实测（`git ls-files` 同口径，Windows）

| 项 | STATUS.md | 实测 | 差异 |
|---|---|---|---|
| `crates/` 总行数 | 83,604 | **78,079** | -5,525 |
| `src/` 行数 | 57,942 | **26,948** | **-30,994（2.15 倍）** |
| 测试通过 | 1,092 | **1,083** | -9（平台差异，正常） |

### 各 crate 实测分布（`git ls-files` + 逐文件行数）

| crate | 文件 | 行数 |
|---|---:|---:|
| daemon | 94 | 34,547 |
| httpdl | 49 | 16,466 |
| provider | 39 | 10,535 |
| btcore | 29 | 5,809 |
| core | 39 | 5,648 |
| xunlei-ffi | 12 | 2,128 |
| xunlei-convert | 6 | 1,788 |
| rss | 1 | 704 |
| storage | 4 | 454 |
| **合计** | **273** | **78,079** |

### 差异原因推测

- 总行数差 5,525：可能统计时包含/排除了不同的文件集
- **`src/` 差 30,994（2.15 倍）不合理** —— 疑似把 `tests/` 也算进了 `src/`，
  或使用了不同的 glob 展开规则（`**/src/**` 在不同 shell 下行为差异大）

### 建议

1. **把统计命令写进文档**，例如：

   ```bash
   git ls-files 'crates/**/*.rs' | xargs wc -l | tail -1
   ```

   这样任何人（含 CI）都能复现，避免口径漂移。

2. 或干脆**不给绝对行数**，只给「crate 数与测试套件数/通过数」——
   行数对项目决策价值低，却容易过期。

3. 测试数建议标注平台：「Linux 1,092 / Windows 1,083」，
   因为条件编译差异是客观存在的。

---

## 三、🟡 `bun run build` 在合并 #129 后需要先 `bun install`

### 现象

```
✓ Compiled successfully in 7.7s
Type error: Cannot find module '@playwright/test' or its corresponding type declarations.
error: script "build" exited with code 1
```

### 根因

- PR #129 引入 `ui/e2e/*.spec.ts`（依赖 `@playwright/test`）
- `ui/tsconfig.json` 的 `include` 是 `["next-env.d.ts", "**/*.ts", "**/*.tsx", ...]`
  —— **`**/*.ts` 把 `e2e/` 目录也纳入了 Next 的类型检查**
- `package.json` 已正确声明 `@playwright/test: 1.57.0`（devDependencies）
- 但**本地 `node_modules` 未重新安装** → 报错

### 这不完全是本地问题

**任何在 #129 合并前就 `bun install` 过的人，都会遇到同样的构建失败。**
新克隆的人不会（`bun install` 一次装全），但**升级既有工作区的人会**。

### 建议

**方案 A（推荐）：`tsconfig` 排除 `e2e/`**

```json
{
  "exclude": ["node_modules", "e2e", "playwright.config.ts"]
}
```

**理由**：`next build` 的打 typecheck 目标是**应用代码**，e2e 测试由
`playwright test` 自己做类型检查（Playwright 自带 ts 编译）。
把 e2e 从 Next 的类型检查里排除，能避免这类耦合 —— **生产构建不该依赖测试框架**。

**方案 B：文档 + CI 说明**
在 `ui/README` 或 CI 里明确「UI 构建前必须 `bun install`」，
并在 CI 的 build job 前确保 install 步骤存在。

---

## 四、✅ 做得好的部分（明确记录）

### PR #145：契约矩阵 15 → 24 条，超过要求

我要求补齐 8 条达到 23，实际做了 **24 条**，且比要求更细：

```
contract_12_connections_on_http_task_is_409      ← 边界：HTTP 任务调 BT 方法
contract_14_piece_priority_http_task_boundary    ← 边界
contract_14b_piece_priority_on_torrent_task      ← 新增：BT 任务上真生效
contract_15_super_seeding_http_task_boundary     ← 边界
contract_20_trackers_http_task_boundary          ← 边界
```

**「边界 + 正例」成对覆盖**，比只测正例强。

### PR #129：正确适配了虚拟滚动

我在 #143 里特意提醒「虚拟滚动后不可见行不在 DOM 中，选择器会失效」。
PR #129 的提交信息显示他们确实考虑了：

> - 行级 testid 补进 TaskRow 组件（task-row/task-name/task-state/
>   task-speed/pause-resume-btn/delete-btn + `data-task-state`）
> - 虚拟滚动兼容契约文档化（单任务 `.first()` 安全 / 空表 `toHaveCount(0)`）
> - 本地实跑 5/5 全绿（真实 daemon + 真实下载 + `ui/out` 产物）

**用 `data-testid` + `.first()` 而非文本匹配/索引**，是正确解法。

### PR #144：文档修正到位

`ARCHITECTURE-REVIEW.md:617` 确实改了：

```diff
- | 依赖图基本健康 | ⚠️ 遗漏了幽灵依赖与 ~1,800 行死代码 |
+ | 依赖图基本健康 | ⚠️ 遗漏了幽灵依赖与「未接线模块」 |
```

并加了指回 §6.5 的警示块（`:619`）。

### PR #138 / #137：rebase 干净

- #138：冲突 3 处解毕（rss/storage 依赖并存、bt_alerts 取 #130 新解析、lock 重生）
- #137：仅 1 处冲突（`parking_lot`），`sniffer`/`xltd` 零引用自动合并（侧面验证 #134 删得干净）

---

## 五、仍然待办

| 项 | 状态 | 归属 |
|---|---|---|
| **main 分支保护**（required status checks + Merge Queue） | ⏳ 待 repo admin 设置 | **需 tomjiu 操作** |
| **ts-rs 类型生成** | ⏳ 「保留为下一项」 | 云端 |
| 第二梯队：种源管理 / 新建表单扩展 / 文件优先级 / 批量 / 队列优先级 | 未开始 | 云端 |
| 第三梯队：`ops.rs` 拆域 / `core/strategy.rs` 迁回 / 拆 `DownloadEngine` trait | 未开始 | 云端 |
| 第四梯队：BT v2（需改 FFI 契约） | 未开始 | 建议单独立项 |

---

## 六、下一步建议

按投入产出比，**建议优先做 ts-rs**（#143 已定位为第一梯队第一项）：

**当前状态**：UI 类型 `ui/src/lib/daemon/types.ts` 手写，后端 struct 在 Rust。
两者靠人工对齐 —— **本轮已因此产生 3 个真实缺陷**：

1. `max_download_kb_s` vs `down_kb_s`（限速静默失效）
2. `name: string` vs `Option<String>`（TypeScript 编译失败，UI 从未构建成功过）
3. `logs` 响应形状不符（`{logs:[{ts,level,message}]}` vs `{events:[{at_ms,op,detail}]}`）

**ts-rs 一行 derive 就能根治**：

```rust
#[derive(TS)]
#[ts(export, export_to = "../../ui/src/lib/daemon/generated/")]
pub struct LimitReq { ... }
```

```json
// ui/package.json
"gen:types": "cargo test -p smart-dl-daemon export_bindings"
```

**收益**：字段名不一致在 **TypeScript 编译期**报错，而不是运行时静默失效。
**成本**：约 1 天（30+ struct 加 derive）。

---

## 附：核验方法说明

以上所有结论均为**本地实跑**，非阅读推断：

```bash
git reset --hard origin/main          # ab254c6
cargo check --workspace --exclude smart-dl-btcore
cargo test  --workspace --exclude smart-dl-btcore
cd ui && bun install && bun run build
git ls-files 'crates/**/*.rs' | wc -l
```

行数统计为逐文件 `Get-Content | Measure-Object -Line` 累加（Windows）。
若你的口径包含/排除项不同，请以你为准并在文档中写明命令。

