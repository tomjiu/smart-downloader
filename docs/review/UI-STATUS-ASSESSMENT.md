# UI 现状评估（对照 UI-DESIGN-SPEC）

> ## ⚠️ 本文档已过期（2026-10-21 标注）
>
> **描述的是 PR #116 合并之前的状态。** 该 PR 已于 2026-10-21 合并，
> 12 主题 / 密集表格 / 虚拟滚动 / 预设方案 / Issue #127 六项实测修复**全部落地**，
> 且 #133（虚拟滚动）、#140（契约审计）也已合入。
>
> **当前状态请看 [`docs/STATUS.md`](../../STATUS.md)。**
>
> 本文档保留价值：作为「重构前基线」的历史记录，以及 53 条路由覆盖矩阵的参照。

---

> 对象：`tomjiu/smart-downloader` @ main `f5c3a7a`
> 日期：2026-10-19
> 方法：读码 + 运行规范检查脚本 + 逐条验证关键断言

---

## 0. 先纠正三个常见误解

### 误解 1：「UI 有 12 主题」—— 实际是 8 个，且配色完全不同

```typescript
// ui/src/lib/daemon/types.ts
export const THEME_KEYS = [
  "forest-light", "forest-dark",
  "bee-light",    "bee-dark",
  "mint-light",   "mint-dark",
  "light-parchment", "parchment-dark",
] as const;   // ← 8 个
```

**规范要求的是**：`nord` / `carbon` / `gruvbox` / `catppuccin` / `solarized` / `mono` × 明暗 = **12 个**。

**实测验证**：grep `nord|carbon|gruvbox|catppuccin|solarized` 在 `ui/` 内命中 3 处，
**全部是误命中**（`list-unordered` 图标名里的 "ord" 匹配到 "nord"）。

**12 主题色板只存在于 `docs/design/UI-DESIGN-SPEC.md` 和 `theme-preview.html` 里，
UI 代码一行都没有。**

默认主题是 `forest-dark`（`layout.tsx:16`），不是规范要求的 `nord-dark`。

### 误解 2：「qoder-ui.min.css 是 325KB」—— 实测 170.7 KB

| 文件 | 大小 |
|---|---|
| `qoder-ui.min.css` | **170.7 KB**（2 行压缩） |
| `fonts/codicon.ttf` | 122.9 KB |
| `fonts/qoder-seti.woff` | 31.8 KB |
| **目录合计** | **≈325 KB** |

CSS 本体是 170.7 KB。规范要求移除它。

### 误解 3：「检查脚本能跑」—— 用 `powershell` 跑会解析失败

```powershell
# ❌ Windows PowerShell 5.1：中文乱码 → ParserError，一条检查都没执行
powershell -File scripts/check-ui-spec.ps1

# ✅ PowerShell 7
pwsh -NoProfile -File scripts/check-ui-spec.ps1
```

---

## 1. 规范检查结果：5 通过 / 5 警告 / 8 失败

```
§9.1 阶段门禁 (P0)
  [FAIL] G0-1  qoder-ui 目录仍存在（325 KB）
  [FAIL] G0-2  仍有 164 处 qoder- 引用
  [FAIL] G0-3  主题定义仅 0 个（需 12）
  [PASS] G0-4  全部主题 token 完整    ← ⚠️ 假 PASS，见下
  [FAIL] G0-5  transition: all 2 处（globals.css:96, :336）
  [FAIL] G0-6  backdrop-filter 4 处（≤1）
  [FAIL] G0-8  错峰入场动画 1 处（globals.css:162）
  [PASS] G0-9  crates/ 无改动

§9.3 细节规范
  [WARN] D4  未找到行选中态样式
  [FAIL] D6  禁用态缺 cursor: not-allowed
  [WARN] D8  7 处间距非 4 倍数
  [PASS] D9  数字列 tabular-nums
  [PASS] D10 prefers-reduced-motion
  [WARN] D5  focus-visible 仅 0 处   ← 0 处是「完全没做」，不是「做得少」
  [WARN] D7  2 处过渡 >200ms

§9.5 性能
  [FAIL] P1  2 处 JSON.stringify 全量比较（DlApp.tsx:60, :63）
  [PASS] F6  进度计算有 total>0 防护
```

### ⚠️ G0-4 是假 PASS（脚本 bug）

G0-3 查到「主题定义 0 个」，而 G0-4「全部主题 token 完整」在**空集合**上判定 →
`all()` 对空集合恒真 → 假 PASS。

**脚本这处逻辑要修**（0 个主题时应直接 fail 或 skip）。

### qoder- 引用的精确分布

```
164 处命中行，244 次出现：

  46  SettingsView.tsx
  29  globals.css
  26  RssView.tsx
  20  TaskDetail.tsx
  14  Common.tsx
  14  TasksView.tsx
   6  StatsView.tsx
   5  DlApp.tsx
   3  LogsView.tsx
   1  layout.tsx
```

**全部 10 个源文件都有引用** —— 不是「局部残留」，是「整个 UI 站在 qoder-ui 上」。

### 规范明令禁止、当前全部违反的视觉项

| 项 | 出现位置 |
|---|---|
| 渐变 | `globals.css:12,16,42,64,188`（按钮/logo/背景网点） |
| 辉光 | `.qoder-btn--primary` box-shadow + `SpeedChart` 的 `feGaussianBlur` |
| 毛玻璃 | header `backdrop-filter: blur(14px)`（4 处） |
| 卡片 hover 位移 | `translateY(-2px)` |
| 错峰入场动画 | `globals.css:162` 的 `dl-enter`（规范点名要删） |

---

## 2. 🔴 三个真实 bug（比样式问题严重得多）

### P0-1：任务限速从未生效

**UI 发的字段名与后端不匹配：**

```typescript
// ui/src/lib/daemon/store.ts:94-99
taskLimit(id: string, down?: number, up?: number) {
  return req(`/tasks/${id}/limit`, {
    method: "POST",
    body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up }),
  });                              // ^^^^^^^^^^^^^^^^^^  ^^^^^^^^^^^^^^^^
}
```

```rust
// crates/daemon/src/http.rs:248-253
pub struct LimitReq {
    #[serde(default)] pub down_kb_s: Option<u32>,
    #[serde(default)] pub up_kb_s: Option<u32>,
}                    // ^^^^^^^^^       ^^^^^^
```

**为什么比「报错」更糟**：两个字段都是 `#[serde(default)] Option<u32>`，
serde 会**静默忽略未知字段** → 两边都是 `None` → **接口返回 200 + 快照，但限速一次也没生效**。

**用户看到「限速已应用」的提示，实际下载速度毫无变化。**

**修法**（一行）：

```typescript
body: JSON.stringify({ down_kb_s: down, up_kb_s: up }),
```

**同类对比**：`AddTaskReq` 里 `down_kb_s` 的用法是对的，**只有 `store.ts` 的 `taskLimit` 写错**。

### P0-2：任务名为 null 时崩掉整个任务列表

```typescript
// ui/src/lib/daemon/types.ts:31
name: string;        // ← 声明为必填 string

// crates/daemon/src/state.rs:142
pub name: Option<String>,   // ← 实际是 Option，且 skip_serializing_if 会省略
```

**两个使用点，都会出问题：**

```typescript
// TasksView.tsx:167 —— 渲染
{t.name}                        // ← 字面显示 "null"

// TasksView.tsx:60 —— 搜索
(t.name.toLowerCase().includes(q.toLowerCase()))
//  ↑ None 时 TypeError → 整个任务列表白屏
```

**修法要两处一起改**（只修渲染的话，搜索框一输入就白屏）：

```typescript
// types.ts
name?: string | null;

// TasksView.tsx:60
(t.name ?? "").toLowerCase().includes(...)

// TasksView.tsx:167
{t.name || t.source?.slice(0, 40) || t.id}
```

### P1：`GET /tasks/:id/logs` 响应形状对不上

```typescript
// store.ts:91 —— UI 期望
taskLogs(id): Promise<{ logs: { ts: string; level: string; message: string }[] }>
```

```rust
// ops.rs:3753-3763 —— 后端实际返回
{
  "task_id": ..., "state": ..., "source": ..., "error": ...,
  "events": [ { at_ms, op, detail } ]     // ← 字段名和结构都不同
}
```

**当前无调用方**（`taskLogs` 是死代码），**但一旦有人接上就立刻错**。

---

## 3. UI 侧的死代码（7 个方法有封装无调用）

```
taskLogs()      config()      globalLimits()
setGlobalLimits()  version()   providers()   listEvents()
```

另外：

```
format.ts 的 fmtEta / fmtTime / fmtKb    →  从未被引用
Common.tsx 的 SizeText                    →  从未被引用
```

**后果：UI 里根本没有「预计剩余时间」列和「文件大小」列** —— 函数写了但没接。

---

## 4. 后端能力 UI 完全没接（53 条路由里）

| 分类 | 未接的路由 |
|---|---|
| **任务操作** | `/tasks/batch`、`/tasks/:id/priority`、`/tasks/:id/name`、`/tasks/:id/tags`、`/tasks/:id/fallback` |
| **BT 管理** | `/tasks/:id/trackers`(GET/POST/DELETE)、`/tasks/:id/peers`、`/tasks/:id/webseeds`、`/tasks/:id/super-seeding`、`/tasks/:id/announce`、`/tasks/:id/recheck`、`/tasks/:id/files/priority`、`/bt/metadata` |
| **安全** | `/security/bans`、`/security/bans/import` |
| **探测** | `/probe` |
| **导出** | `/tasks/:id/export`、`/tasks/:id/magnet` |
| **系统** | `/metrics`、`/health`、`/providers`、`/version` |
| **NAS** | `/nas/*`（5 条） |
| **导入** | `/tasks/xunlei-import` |

**其中是 downloader 刚需的**：
tracker 管理、IP 封禁、**限速（有 UI 但失效）**、批量操作、recheck、队列优先级。

### 设置页覆盖度（这块是亮点）

9 个域接了 8 个 + meta。**唯一漏的是 `scheduler.completion_action`**
（none/exit/shutdown/sleep/hibernate，`settings.rs:120`）。

### AddTaskReq 的字段 UI 基本没发

```
torrent_b64   metalink_b64   headers   auth
sha256        backup_url     dest      proxy
```

**UI 只能建裸 URL 任务** —— 不能加种子、不能带 Cookie/Referer、不能设校验和、不能指定目录。

---

## 5. 端到端真能用的功能（就这 5 组）

| # | 功能 | 状态 |
|---|---|---|
| 1 | 新建任务（**仅裸 url**） | ✅ |
| 2 | 暂停 / 恢复 / 删除 | ✅ |
| 3 | 顺序下载 / 连接数 / 任务代理 / 首尾块优先 | ✅ |
| 4 | RSS 全套（feed + rule + item + refresh） | ✅ **接得最完整** |
| 5 | 设置 8 域 + 主题切换（8 个假主题） | ✅ |

---

## 6. 完成度评分

| 口径 | 评分 | 依据 |
|---|---|---|
| 对照 `UI-DESIGN-SPEC` | **5-8%** | P0 门禁 9 条里 7 条 fail；风格仍是规范要替换的旧状态；12 主题 0 实现 |
| 作为"能用的下载器 UI" | **约 30%** | 增删改查通，但限速失效、无 ETA 列、无批量、无种源管理 |

---

## 7. TOP 5 UI 缺口（按用户价值）

### 1. 🔴 限速失效（P0 bug）

一行修好，但当前用户**以为限速生效了**。

### 2. 🔴 任务列表 null 崩溃（P0 bug）

任务名未回填时搜索框一输入就白屏。

### 3. 🟠 没有 ETA / 文件大小列

`fmtEta` / `SizeText` 写了没接。
下载器没有「还剩多久」和「多大」是致命的 —— **用户不知道自己还要等多久**。

### 4. 🟠 AddTaskReq 字段全没发

不能加种子/磁力、不能带 Cookie/Referer、不能设校验和 ——
**实际上只能用 UI 下公开的裸 URL**。

### 5. 🟠 批量操作 + 种源管理全缺

- 批量暂停/恢复（后端 `POST /tasks/batch` 已有）
- tracker 管理
- peer 详情
- IP 封禁

**qB/IDM 用户的日常操作在 UI 里一个都做不了。**

---

## 8. 一个给后续开发的坑

```typescript
// TaskDetail.tsx:157
task.engine === "bt"    // ← 这是对的，别改！
```

后端 `engine` 走 `eng.id()` 返回小写 `"bt"`（`bt.rs:501`）。
**如果误判为大小写 bug 改成 `"Bt"`，BT 专属控件会全部消失。**

---

## 9. 结论与建议

### 云端没有执行 UI 规范

`docs/design/UI-DESIGN-SPEC.md` 和 `theme-preview.html` 我已经写好并提交，
但云端 UI 是**重构前的老状态**（qoder-ui 版），与规范**完全未对齐**。

`ui/` 最后一次提交是 `0520e25`（PR #113 batch9 安全审计），
**之后没有任何 UI 工作。**

### 两条路可选

**路线 A：按规范重做 UI**（我的设计）
- 移除 qoder-ui（164 处引用 + 325KB）
- 自建 12 主题 CSS 变量
- 工作量：**估计 3-5 天**（2340 行代码基本重写）

**路线 B：保留 qoder-ui，只修 bug**
- 修 3 个 P0/P1 bug（限速字段、null 崩溃、logs 形状）
- 补 ETA/大小列、批量操作、AddTaskReq 字段
- 主题就用 qoder-ui 的 8 个
- 工作量：**1-2 天**

**我的判断**：路线 B 的**用户价值更高**。

理由：
1. 当前 UI 的**功能缺口**（限速失效、无 ETA、不能加种子）比**样式问题**严重得多
2. 12 主题是「锦上添花」，而「限速能真正生效」是刚需
3. qoder-ui 的 8 主题虽然不是我设计的配色，但**能用**，且不违反可用性

**建议顺序**：
1. 先修 3 个 bug（半天）
2. 补 ETA/大小列 + AddTaskReq 字段（1 天）
3. 补批量操作 + tracker 管理（1-2 天）
4. **再**考虑是否按规范重做主题

**这个判断需要你确认** —— 因为「12 主题」是你明确要求过的，
但现在看来它和「功能可用性」在抢资源。
