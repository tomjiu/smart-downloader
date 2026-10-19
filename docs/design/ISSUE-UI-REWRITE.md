## 任务

按 `docs/design/UI-REWRITE-PLAN.md` 执行 **Phase 1（骨架重构）**。

**决策前提（用户已确认）**：
- 先重构骨架，功能分批 —— 骨架稳定后每批只填槽位，不动布局，零返工
- **纯 CSS + CSS 变量**，不引入 Tailwind
- 总耗时优先

---

## 现状（务必先读）

**`ui/` 最后一次提交是 `0520e25`（PR #113 batch9，2026-09-08）**，
至今 1000+ 提交**一行未动** —— `docs/design/UI-DESIGN-SPEC.md` 从未被执行。

审计结论：

| 项 | 现状 | 规范要求 |
|---|---|---|
| 主题 | 8 个（forest/bee/mint/parchment） | **12 个**（nord/carbon/gruvbox/catppuccin/solarized/mono） |
| `globals.css` 的 `[data-theme]` 块 | **0 个**（全靠 qoder-ui 库） | 12 个自有色板 |
| `qoder-ui.min.css` | 170.7 KB，164 处引用，10 个源文件全中 | 应删除 |
| 任务列表 | 单列卡片（单屏 3-4 条） | 密集表格（单屏 15+ 条） |
| 侧栏 | 208px 常驻 | 顶部工具栏 + 底部状态栏 |
| 路由覆盖 | 15 / 53 | 骨架期先做到形状正确，功能分批补 |

规范检查（`pwsh -NoProfile -File scripts/check-ui-spec.ps1`）：
**5 通过 / 5 警告 / 8 失败**。

---

## 🔴 三个必须内建修复的 bug

**这三个不是「可选优化」，是当前代码在撒谎。重写时必须一并修好。**

### P0-1：任务限速从未生效

```typescript
// ui/src/lib/daemon/store.ts:94-99
body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up })
//                     ^^^^^^^^^^^^^^^^^^      ^^^^^^^^^^^^^^^^
```

```rust
// crates/daemon/src/http.rs:248-253
pub struct LimitReq {
    #[serde(default)] pub down_kb_s: Option<u32>,
    #[serde(default)] pub up_kb_s: Option<u32>,
}
```

**两个字段都是 `#[serde(default)] Option` → serde 静默丢弃未知字段 → 双 `None` → 空操作却返回 200。**

用户看到「限速已应用」toast（`TaskDetail.tsx:137`），**实际限速一次都没生效。**

**修法**：
```typescript
body: JSON.stringify({ down_kb_s: down, up_kb_s: up })
```

（`AddTaskReq` 里 `down_kb_s` 是对的，说明只是 `taskLimit` 写错。）

### P0-2：任务名为 null 时崩掉整个列表

```rust
// crates/daemon/src/state.rs:142
pub name: Option<String>,   // + skip_serializing_if 省略
```

```typescript
// ui/src/lib/daemon/types.ts:31
name: string;               // ← 错

// ui/src/components/dl/TasksView.tsx:60
t.name.toLowerCase()        // ← None 时 TypeError，整个任务列表白屏

// ui/src/components/dl/TasksView.tsx:167
{t.name}                    // ← 字面显示 "null"
```

**两处必须同时改**。只修渲染的话，用户在搜索框一输入就白屏。

### P1：`GET /tasks/:id/logs` 响应形状不符

```
UI 期望：{ logs: [{ ts, level, message }] }
后端实际：{ task_id, state, source, error, events: [{ at_ms, op, detail }] }
```

当前 `taskLogs` 无调用方（死代码），**但接上即错**。

---

## Phase 1 交付物

### 新建

```
ui/src/app/tokens.css                12 主题 × 15 token
ui/src/components/dl/TasksTable.tsx  密集表格 7 列
ui/src/components/dl/Toolbar.tsx     顶部工具栏 44px
ui/src/components/dl/StatusBar.tsx   底部状态栏 28px
```

### 修改

```
ui/src/app/globals.css      重写（不用任何 qoder- 变量）
ui/src/app/layout.tsx       引入 tokens.css，默认 nord-dark
ui/src/lib/daemon/types.ts  12 THEME_KEYS + name 改可选
ui/src/lib/daemon/store.ts  修 taskLimit / taskLogs，补 batchTasks
ui/src/components/dl/DlApp.tsx  去侧栏，改工具栏+状态栏布局
ui/src/components/dl/SettingsView.tsx  主题卡改 2×6，默认 nord-dark
ui/src/components/dl/TaskDetail.tsx    修 name 渲染
```

### 删除

```
ui/public/qoder-ui/                 整个目录（325 KB）
ui/src/components/dl/TasksView.tsx  被 TasksTable 替代
```

---

## 关键实现要点

### 1. tokens.css 直接抄，不要重新设计

`docs/design/theme-preview.html` 里已经有**完整且已验证的 12 主题色板**，
直接抄成 CSS 变量即可。规范 §4.1-4.12 有每个主题的完整色值表。

每个主题需要 15 个 token（规范 §3.1）：
`--bg --bg-2 --bg-3 --hover --line --line-2 --fg --fg-2 --fg-3 --accent --accent-fg --ok --warn --err --sel`

### 2. TasksTable 的硬性要求（规范 §5.1）

- 行高 `--row-h: 32px`
- 7 列：名称 / 状态 / 进度 / 速度 / 大小 / 剩余 / 操作
- **无竖线**，`thead th` 只有 `border-bottom`
- 数字列 `font-variant-numeric: tabular-nums`
- 行选中：`box-shadow: inset 3px 0 0 var(--accent)`
- 悬停：`background: var(--hover)`，**禁止 `translateY` 位移**

### 3. 已知缺口：ETA 和大小列

`format.ts` 里的 `fmtEta` / `fmtTime` / `fmtKb` 和 `Common.tsx` 的 `SizeText`
**写了但从未被引用** → 现在 UI 里根本没有「剩余时间」和「文件大小」列。

**Phase 1 接上它们** —— 下载器没有 ETA 是致命的。

### 4. 后续批次的槽位先预留

骨架里给这些位置**先留空或 disabled**，后续批次只填实现：

| 批次 | 位置 | 后端路由 |
|---|---|---|
| A | 工具栏「新建」下拉 | `POST /tasks` 全字段 |
| A | 工具栏「批量」 | `POST /tasks/batch` |
| B | 详情「种源」页 | `/trackers`(3) `/peers` `/webseeds` `/super-seeding` `/announce` `/recheck` |
| B | 详情「文件」页 | `/files/priority` `/piece-priority` |
| C | 设置「完成动作」 | `scheduler.completion_action` |
| C | 新页「安全」 | `/security/bans`(4) |
| C | 新页「Provider」 | `/providers` `/tasks/:id/fallback` |
| D | 状态栏 | `/health` `/version` `/metrics` |

---

## ⚠️ 两个必须避免的坑

### 坑 1：`task.engine === "bt"` 是对的，别「修」成 `"Bt"`

```typescript
// ui/src/components/dl/TaskDetail.tsx:157 —— 这是正确的
task.engine === "bt"
```

后端 `engine` 走 `eng.id()` 返回**小写** `"bt"`（`crates/daemon/src/bt.rs:501`）。
`EngineKind` 枚举的 serde 默认是 `"Bt"` —— **两个不同口径**。
改成 `"Bt"` 会让 BT 专属控件（首尾块优先 / 连接数）**全部消失**。

### 坑 2：检查脚本必须用 pwsh 7

```powershell
pwsh -NoProfile -File scripts/check-ui-spec.ps1   # ✅
powershell -File scripts/check-ui-spec.ps1        # ❌ 中文乱码 ParserError，一条检查都不执行
```

**顺带修脚本自身两个 bug**：
- `G0-4` 在 0 个主题时**假 PASS**（空集合 `all()` 恒真）→ 应为 FAIL
- `D5` `focus-visible` 为 0 处时只报 WARN → 规范要求 FAIL

---

## 验收标准

### 自动化

```powershell
# 1. 规范检查 —— P0 门禁须 0 FAIL
pwsh -NoProfile -File scripts/check-ui-spec.ps1

# 2. 构建
cd ui && bun run build
```

### 手工（10 项）

| # | 项 | 标准 |
|---|---|---|
| 1 | 12 主题全部可切换 | 无白屏、无颜色错乱 |
| 2 | 默认主题 | `nord-dark` |
| 3 | 主题持久化 | 刷新后保持 |
| 4 | 跟随系统 | 无 localStorage 时按 `prefers-color-scheme` |
| 5 | 表格密度 | 1920×1080 单屏 ≥ 15 行 |
| 6 | 无侧栏 | 导航在顶部工具栏 |
| 7 | **限速真生效** | 设 100 KB/s → 实测速度 ≤ 100 KB/s |
| 8 | **无任务名不崩** | 新建任务后立即搜索，不白屏 |
| 9 | 行选中 | 左侧 3px accent 竖条 |
| 10 | 键盘可达 | Tab 遍历所有按钮，有 focus 环 |

**第 7、8 条是本次重构的核心验收项** —— 它们现在是坏的。

---

## 工作量

**约 6 个工作日**（详见 `UI-REWRITE-PLAN.md` §8）。

---

## 参考文档

- `docs/design/UI-REWRITE-PLAN.md` —— 本任务完整实施规格
- `docs/design/UI-DESIGN-SPEC.md` —— 设计规范（657 行，12 主题色板）
- `docs/design/theme-preview.html` —— 12 主题可视预览（色板可直接抄）
- `docs/review/UI-STATUS-ASSESSMENT.md` —— 现状审计（含 53 路由覆盖矩阵）
- `scripts/check-ui-spec.ps1` —— 自动化检查
