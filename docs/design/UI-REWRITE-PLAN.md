# UI 重构实施规格（Phase 1：骨架）

> 决策：**先重构骨架，功能分批。纯 CSS + CSS 变量，不引入 Tailwind。**
> 目标：**总耗时最短** —— 骨架一次做对，后续功能往固定槽位里填，不返工。

---

## 0. 为什么这个顺序最快

现有 UI 的问题不是「代码质量差」，而是**架构形状不对**：

| 现状 | 需要 |
|---|---|
| 卡片列表（单屏 3-4 条） | 密集表格（单屏 15+ 条） |
| 208px 常驻侧栏 | 顶部工具栏 + 状态栏 |
| 样式全在 qoder-ui（164 处引用） | 自有 12 主题 tokens |
| 15/53 路由接入 | 分批补到 40+ |

**在错误形状上修 bug = 白工**（会随重写一起扔掉）。
所以 Phase 1 只做「形状」，**但把已知 bug 的修复直接内建进去**。

---

## 1. Phase 1 交付物清单

### 必须新建的文件

```
ui/src/app/tokens.css              ← 12 主题色板（从规范 §4 抄）
ui/src/app/globals.css             ← 重写（覆盖，不用 qoder 变量）
ui/src/app/layout.tsx              ← 改：引入 tokens.css，默认 nord-dark
ui/src/components/dl/TasksTable.tsx ← 新建（密集表格）
ui/src/components/dl/Toolbar.tsx    ← 新建（顶部工具栏 44px）
ui/src/components/dl/StatusBar.tsx  ← 新建（底部状态栏 28px）
ui/src/lib/daemon/types.ts         ← 改：12 主题 + 修 name 类型
ui/src/lib/daemon/store.ts         ← 改：修 taskLimit 字段名
```

### 必须删除的

```
ui/public/qoder-ui/                ← 整个目录（325KB）
ui/src/components/dl/TasksView.tsx ← 被 TasksTable 替代
```

### 必须修改的

```
ui/src/components/dl/DlApp.tsx     ← 去掉侧栏，改工具栏 + 状态栏布局
ui/src/components/dl/SettingsView.tsx ← 主题卡改 2×6，默认 nord-dark
ui/src/components/dl/TaskDetail.tsx   ← 修 name 渲染
```

---

## 2. Step 1：tokens.css（12 主题）

**直接从 `docs/design/theme-preview.html` 抄** —— 那份是完整且已验证的。

### 文件结构

```css
/* ui/src/app/tokens.css */

/* ── 尺寸 Token（跨主题统一，规范 §3.2）── */
:root {
  --radius-sm: 2px;
  --radius:    3px;
  --radius-lg: 4px;

  --row-h:     32px;
  --toolbar-h: 44px;
  --status-h:  28px;

  --space-1: 4px;
  --space-2: 8px;
  --space-3: 12px;
  --space-4: 16px;
  --space-5: 24px;

  --font-sans: -apple-system, BlinkMacSystemFont, "Segoe UI",
               "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif;
  --font-mono: ui-monospace, SFMono-Regular, "SF Mono", Menlo, monospace;
  --fs-xs: 11px;
  --fs-sm: 12px;
  --fs:    13px;
  --fs-lg: 15px;

  --dur-fast:   120ms;
  --dur:        160ms;
  --dur-slow:   200ms;
  --ease:       cubic-bezier(0.4, 0, 0.2, 1);
}

/* ── 12 主题 ── */
[data-theme="nord-dark"]    { --bg:#2e3440; --bg-2:#3b4252; ... }
[data-theme="nord-light"]   { ... }
[data-theme="carbon-dark"]  { ... }
/* ... 共 12 个 */
```

### 每个主题需要的 token（规范 §3.1）

```css
--bg        页面背景
--bg-2      卡片/面板
--bg-3      输入框/悬浮层
--hover     悬停态
--line      边框
--line-2    强调边框
--fg        主文字
--fg-2      次要文字
--fg-3      弱化文字
--accent    强调色
--accent-fg 强调色上的文字
--ok        成功
--warn      警告
--err       错误
--sel       选中态背景
```

**共 15 个 token × 12 主题 = 180 行**，直接从 preview html 抄。

### ⚠️ 关键：验证 token 完整性

写完后跑：

```powershell
pwsh -NoProfile -File scripts/check-ui-spec.ps1
# 期望：G0-3 主题定义 12 个 PASS
#       G0-4 token 完整 PASS（这次是真的）
```

---

## 3. Step 2：types.ts 修改

### 3.1 修 `name` 类型（内建 P0-2 修复）

```typescript
// ❌ 现状
name: string;

// ✅ 改为
name?: string | null;
```

### 3.2 换成 12 主题

```typescript
export const THEME_KEYS = [
  "nord-dark", "nord-light",
  "carbon-dark", "carbon-light",
  "gruvbox-dark", "gruvbox-light",
  "catppuccin-mocha", "catppuccin-latte",
  "solarized-dark", "solarized-light",
  "mono-dark", "mono-light",
] as const;

export type ThemeKey = (typeof THEME_KEYS)[number];

export const THEME_META: Record<ThemeKey, { label: string; accent: string; dark: boolean }> = {
  "nord-dark":       { label: "Nord 暗",        accent: "#88c0d0", dark: true  },
  "nord-light":      { label: "Nord 亮",        accent: "#5e81ac", dark: false },
  "carbon-dark":     { label: "Carbon 暗",      accent: "#4589ff", dark: true  },
  "carbon-light":    { label: "Carbon 亮",      accent: "#0f62fe", dark: false },
  "gruvbox-dark":    { label: "Gruvbox 暗",     accent: "#d79921", dark: true  },
  "gruvbox-light":   { label: "Gruvbox 亮",     accent: "#af3a03", dark: false },
  "catppuccin-mocha":{ label: "Catppuccin 暗",  accent: "#cba6f7", dark: true  },
  "catppuccin-latte":{ label: "Catppuccin 亮",  accent: "#8839ef", dark: false },
  "solarized-dark":  { label: "Solarized 暗",   accent: "#268bd2", dark: true  },
  "solarized-light": { label: "Solarized 亮",   accent: "#268bd2", dark: false },
  "mono-dark":       { label: "极简灰 暗",      accent: "#d4d4d4", dark: true  },
  "mono-light":      { label: "极简灰 亮",      accent: "#171717", dark: false },
};
```

---

## 4. Step 3：store.ts 修改

### 4.1 修 taskLimit 字段名（内建 P0-1 修复）

```typescript
// ❌ 现状（store.ts:94-99）
taskLimit(id: string, down?: number, up?: number) {
  return req(`/tasks/${id}/limit`, {
    method: "POST",
    body: JSON.stringify({ max_download_kb_s: down, max_upload_kb_s: up }),
  });
}

// ✅ 改为（对齐 http.rs:248-253 的 LimitReq）
taskLimit(id: string, down?: number, up?: number) {
  return req(`/tasks/${id}/limit`, {
    method: "POST",
    body: JSON.stringify({ down_kb_s: down, up_kb_s: up }),
  });
}
```

### 4.2 修 taskLogs 返回类型（或删除）

```typescript
// ❌ 现状（store.ts:91）
taskLogs(id): Promise<{ logs: { ts: string; level: string; message: string }[] }>

// ✅ 对齐 ops.rs:3753-3763 的实际返回
taskLogs(id: string): Promise<{
  task_id: string;
  state: string;
  source: unknown;
  error: string | null;
  events: { at_ms: number; op: string; detail: string | null }[];
}>
```

### 4.3 补齐批量操作（现在就能接）

```typescript
// POST /tasks/batch —— 后端已有，UI 零调用
batchTasks(ids: string[], op: "pause" | "resume" | "remove"): Promise<unknown> {
  return req(`/tasks/batch`, {
    method: "POST",
    body: JSON.stringify({ ids, op }),
  });
}
```

> ⚠️ 需先确认 `batch_tasks` 的请求体字段名。
> 见 `crates/daemon/src/http.rs` 的 `BatchReq`。

---

## 5. Step 4：TasksTable.tsx（核心）

### 布局

```
┌─────────────────────────────────────────────────────────────────────┐
│ ☐  名称              状态    进度        速度      大小     剩余  操作 │  ← 32px
├─────────────────────────────────────────────────────────────────────┤
│ ☐  ubuntu-24.04.iso  ↓下载   68% ████░░  12.4M/s  1.8G/2.7G 3:42 ⋮  │  ← 32px
│ ☐  fedora-41.iso     暂停    31% ██░░░░  —         820M/2.6G —    ⋮  │
└─────────────────────────────────────────────────────────────────────┘
```

### 规范要求（§5.1）

- **行高 32px**（`--row-h`）
- **7 列**：名称 / 状态 / 进度 / 速度 / 大小 / 剩余 / 操作
- **无竖线**，`thead th` 只有 `border-bottom`
- **数字列 `tabular-nums`**（对齐）
- **行选中态**：`box-shadow: inset 3px 0 0 var(--accent)`
- **悬停**：`background: var(--hover)`，**不做 translateY 位移**

### 关键实现

```tsx
// ui/src/components/dl/TasksTable.tsx
export function TasksTable({ tasks, selected, onSelect, onAction }: Props) {
  return (
    <table className="dl-table">
      <thead>
        <tr>
          <th className="dl-th-check"><input type="checkbox" ... /></th>
          <th>名称</th>
          <th>状态</th>
          <th className="dl-th-progress">进度</th>
          <th className="dl-th-num">速度</th>
          <th className="dl-th-num">大小</th>
          <th className="dl-th-num">剩余</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {tasks.map(t => (
          <tr key={t.id}
              className={selected.has(t.id) ? "is-selected" : ""}
              onClick={() => onSelect(t.id)}>
            <td onClick={e => e.stopPropagation()}>
              <input type="checkbox" checked={selected.has(t.id)}
                     onChange={() => toggle(t.id)} />
            </td>
            {/* ⚠️ P0-2 修复内建：name 可空 */}
            <td className="dl-td-name" title={displayName(t)}>
              {displayName(t)}
            </td>
            <td><StateBadge state={t.state} /></td>
            <td><ProgressBar done={t.progress?.done ?? 0}
                             total={t.progress?.total ?? 0} /></td>
            <td className="dl-td-num">{fmtSpeed(t.speed?.down)}</td>
            <td className="dl-td-num">{fmtBytes(t.progress?.total)}</td>
            <td className="dl-td-num">{fmtEta(t)}</td>
            <td><TaskMenu task={t} onAction={onAction} /></td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

// 关键：displayName 兜底，绝不返回 null
function displayName(t: TaskSnapshot): string {
  return t.name?.trim()
      || deriveNameFromSource(t.source)
      || t.id;
}
```

### 搜索框也要同步修（P0-2 的另一处）

```tsx
// ❌ 现状（TasksView.tsx:60）
t.name.toLowerCase().includes(q.toLowerCase())

// ✅ 改为
displayName(t).toLowerCase().includes(q.toLowerCase())
```

---

## 6. Step 5：后续批次的槽位（不做，先预留）

骨架做好后，后面每一批都是**往固定位置填**：

| 批次 | 填在哪 | 依赖的后端路由 |
|---|---|---|
| **Batch A** | 工具栏「新建」下拉 | `POST /tasks`（含 torrent_b64/metalink_b64/headers/auth/sha256） |
| **Batch A** | 工具栏「批量」 | `POST /tasks/batch` |
| **Batch B** | 详情抽屉「种源」标签页 | `/trackers`(3) `/peers` `/webseeds` `/super-seeding` `/announce` `/recheck` |
| **Batch B** | 详情抽屉「文件」标签页 | `/files/priority` `/piece-priority` |
| **Batch C** | 设置页「完成动作」 | `scheduler.completion_action` |
| **Batch C** | 新页面「安全」 | `/security/bans`(4) |
| **Batch C** | 新页面「Provider」 | `/providers` `/tasks/:id/fallback` |
| **Batch D** | 状态栏 | `/health` `/version` `/metrics` |
| **Batch D** | 任务右键菜单 | `/name` `/tags` `/priority` `/export` `/magnet` |

**关键**：骨架里这些位置**先放按钮但 disabled 或直接留空**，
后续批次只填实现，不动布局 → **零返工**。

---

## 7. 验收标准（Phase 1）

### 自动化

```powershell
# 1. 规范检查
pwsh -NoProfile -File scripts/check-ui-spec.ps1
# 期望：P0 门禁 0 FAIL
#   G0-1 qoder-ui 已删除  ✅
#   G0-2 qoder- 引用 0 处 ✅
#   G0-3 主题 12 个      ✅
#   G0-4 token 完整      ✅
#   G0-5 transition:all 0 ✅
#   G0-6 backdrop-filter ≤1 ✅
#   G0-8 错峰动画 0      ✅

# 2. 构建
cd ui && bun run build
```

### 手工验收

| # | 项 | 通过标准 |
|---|---|---|
| 1 | 12 主题全部可切换 | 逐一点击，无白屏/无颜色错乱 |
| 2 | 默认主题 | 首次启动 = `nord-dark` |
| 3 | 主题持久化 | 切换后刷新页面保持 |
| 4 | 跟随系统 | 无 localStorage 时按 `prefers-color-scheme` |
| 5 | 表格密度 | 1920×1080 单屏 ≥ 15 行 |
| 6 | 无侧栏 | 导航在顶部工具栏 |
| 7 | **限速真生效** | 设 100 KB/s → 实际速度 ≤ 100 KB/s |
| 8 | **无任务名不崩** | 新建任务后立即搜索，不白屏 |
| 9 | 行选中 | 点击行左侧出现 3px accent 竖条 |
| 10 | 键盘可达 | Tab 遍历所有按钮，有 focus 环 |

### 🎯 第 7、8 条是本次重构必须内建修复的

因为它们**现在就是坏的**，重写时不修就还是坏的。

---

## 8. 工作量估算

| Step | 内容 | 估时 |
|---|---|---|
| 1 | `tokens.css` 12 主题 | **0.5 天**（从 preview html 抄） |
| 2 | `types.ts` 改类型 | 0.5 小时 |
| 3 | `store.ts` 修 bug | 0.5 小时 |
| 4 | `TasksTable.tsx` | **1 天** |
| 5 | `Toolbar.tsx` / `StatusBar.tsx` | 0.5 天 |
| 6 | `DlApp.tsx` 改布局 | 0.5 天 |
| 7 | `globals.css` 重写 | **1 天** |
| 8 | `SettingsView` / `TaskDetail` 适配 | 0.5 天 |
| 9 | 删除 qoder-ui + 清理 164 处引用 | 0.5 天 |
| 10 | 验收调试 | 0.5 天 |
| | **合计** | **≈ 6 个工作日** |

**Phase 2-4（功能分批）**：每批 0.5-1 天，共约 2 周。

---

## 9. 三个必须避免的坑

### 坑 1：`task.engine === "bt"` 是对的，别改

```typescript
// TaskDetail.tsx:157 —— 这是正确的
task.engine === "bt"
```

后端 `engine` 字段走 `eng.id()` 返回**小写** `"bt"`（`bt.rs:501`）。
**改成 `"Bt"` 会让 BT 专属控件全部消失。**

（`EngineKind` 枚举的 serde 默认是 `"Bt"` —— **两个不同口径**，别混淆。）

### 坑 2：`name` 有两处用法，必须一起改

```typescript
TasksView.tsx:60   搜索 .toLowerCase()   ← 改这里，否则一输入就崩
TasksView.tsx:167  渲染 {t.name}         ← 改这里，否则显示 "null"
```

### 坑 3：检查脚本必须用 pwsh 7

```powershell
pwsh -NoProfile -File scripts/check-ui-spec.ps1   # ✅
powershell -File scripts/check-ui-spec.ps1        # ❌ 中文乱码崩溃
```

**同时建议修脚本自身的 bug**：
- `G0-4` 在 0 个主题时假 PASS（空集合 `all()` 恒真）
- `D5` focus-visible 为 0 时只 WARN 应改 FAIL

---

## 10. 与后端契约的对照表（重写时逐条核对）

### 常用请求体字段名（**最容易搞错的地方**）

| 端点 | 字段 | 来源 |
|---|---|---|
| `POST /tasks` | `url` / `dest` / `torrent_b64` / `metalink_b64` / `down_kb_s` / `up_kb_s` / `sequential` / `proxy` / `headers` / `username` / `password` / `sha256` / `sha1` / `md5` / `backup_url` / `backup_md5` / `name` / `conflict_policy` / `start_at_unix` / `auto_retry` | `AddTaskReq` |
| `POST /tasks/:id/limit` | **`down_kb_s`** / **`up_kb_s`** | `LimitReq` ⚠️ |
| `POST /tasks/:id/sequential` | `sequential` | `SequentialReq` |
| `POST /tasks/:id/connections` | `max_connections` | `ConnectionsReq` |
| `POST /tasks/:id/proxy` | `proxy` | `SetProxyReq` |
| `POST /tasks/:id/piece-priority` | `priority` | `PiecePriorityReq` |
| `POST /tasks/:id/super-seeding` | `enabled` | `SuperSeedingReq` |
| `GET /tasks/:id/logs` | — | 返回 `{events:[{at_ms,op,detail}]}` ⚠️ |

**建议做法**：重写 `store.ts` 时，**每个方法都在注释里写上对应的 Rust struct 名**，
下次改动两边能对上。

---

## 11. 总结：为什么这个方案总时间最短

### 对比两条路

| 路线 | 步骤 | 总耗时 |
|---|---|---|
| **先修 bug 再重构** | 修 3 bug（0.5 天）→ 重构（6 天）= **6.5 天**，且 bug 修复会被重写覆盖（白做） | 6.5 天（含浪费） |
| **直接重构（本方案）** | 重构 + 内建 bug 修复 = **6 天** | **6 天** |

**本方案省的 0.5 天不只是时间，更是「不做白工」的确定性。**

### 关键设计

1. **骨架稳定后，功能分批只填不改布局** → 零返工
2. **3 个 P0 bug 内建在骨架里** → 不用单独修
3. **12 主题从 preview html 直接抄** → 不用重新设计色板
4. **契约对照表** → 避免再次出现 `max_download_kb_s` vs `down_kb_s` 这种错
