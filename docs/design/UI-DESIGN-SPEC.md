# Smart Downloader UI 设计规范 v1.1

> 状态：**已确认（2026-10-19 布局重设计落地）** — 作为 UI 重构的实现依据
> 创建：2026-10-16 · v1.1：2026-10-19（用户反馈驱动：导航归侧栏、动作归工具栏、三级表面色阶）
> 范围：`ui/` 前端视觉体系重构（12 主题 + 密集表格布局）

---

## 0. 背景与目标

### 为什么重构

现有 UI 基于 `mmqz/qoder-ui` 组件库（175KB CSS），存在两个错配：

1. **视觉语言错配** —— Qoder 是 IDE 的设计语言（侧边栏/编辑器/面板），下载器的核心是**任务列表**
2. **布局错配** —— 任务用单列卡片网格（`grid-template-columns: 1fr`），10 个任务就要滚动很久；专业下载器应该用**密集表格**

### 重构目标

| 目标 | 衡量方式 |
|------|----------|
| 信息密度 | 单屏可见 **15+ 条**任务（当前 3-4 条） |
| 无 AI 味 | 无渐变按钮、无辉光、无玻璃拟态、无错峰入场动画 |
| 多主题 | **12 个**（6 方案 × 明暗），默认 Nord Dark |
| 跨平台 | 桌面优先，预留 Android（触摸目标 ≥44px） |
| 零依赖 | 移除 `qoder-ui.min.css`，自建 token 层 |

### 后端约束（不可破坏）

- **不改 daemon HTTP API**（工作台后期经 API 集成下载器能力）
- API 需保持 **AI 可调用**：结构化、错误清晰、可发现
- 现有 `GET/PUT /settings`、`/tasks`、`/tasks/:id/*`、`/events/stream` 全部保留

---

## 1. 设计原则

### 1.1 克制优先

AI 生成的 UI 通病是"每处都加效果"。本设计**只在必要时给视觉反馈**。

| 元素 | 规则 |
|------|------|
| 渐变 | ❌ 禁止（状态色除外，且仅限进度条） |
| 辉光/发光 | ❌ 禁止 |
| 玻璃拟态 | ❌ 禁止（模态遮罩可用纯色半透明） |
| 卡片 hover 位移 | ❌ 禁止 |
| 错峰入场动画 | ❌ 禁止 |
| 阴影 | ⚠️ 仅用于浮层（抽屉/下拉/toast） |
| 动效 | ⚠️ 仅状态反馈（进度条流动、live 呼吸点） |

### 1.2 密度优先

下载器是"挂着看"的工具，信息密度决定可用性。

```
行高        32px（紧凑）/ 36px（舒适）
表格内边距  水平 12px，垂直 8px
卡片间距    8px
分组间距    16px
```

### 1.3 状态可辨识

6 种任务状态必须在**色盲友好**的前提下可区分（色相 + 位置 + 文字三重编码）。

---

## 1.5 细节规范（⭐ 评审重点）

> **本节是区分「专业工具」与「AI 生成」的关键。**
> 配色可以照抄，细节不能。以下每条都标注了 **[AI 常犯]** 的典型错误，
> Code Review 时逐条对照。

### 1.5.1 边框与边缘

| # | 规则 | [AI 常犯] |
|---|------|-----------|
| B1 | 优先用**背景色差**分层，少用边框 | 到处 `border: 1px solid` |
| B2 | 表格**只用 `border-bottom`**，不要竖线 | `border: 1px solid` 画全网格 |
| B3 | 最后一行**必须**去掉下边框 | 行尾出现双线（表尾 + 容器） |
| B4 | 首行不要上边框（表头 `border-bottom` 已承担） | 表头上下都有线 |
| B5 | 圆角容器内的表格/列表需 `overflow: hidden` | 直角戳出圆角 |
| B6 | 边框色**独立于文字色**调节，比最浅背景深一档 | 直接用 `--fg-3` 当边框色 |
| B7 | 高 DPI 下 1px 发虚时用 `box-shadow: inset 0 0 0 1px` | 无处理意识 |
| B8 | 相邻元素边框叠加时用**负 margin** 或只画一侧 | 1px + 1px = 2px 粗线 |

```css
/* ✅ 表格边框的正确写法 */
.task-table { border-collapse: collapse; }
.task-table thead th {
  border-bottom: 1px solid var(--line);
  background: var(--bg-2);          /* 靠背景差分层，不靠边框 */
}
.task-table tbody td {
  border-bottom: 1px solid var(--line-2);   /* 更弱的分隔线 */
}
.task-table tbody tr:last-child td { border-bottom: none; }  /* B3 */
```

### 1.5.2 交互状态覆盖

**每个可交互元素必须覆盖 6 态。** [AI 常犯]：只做默认态和 hover，漏掉其余。

| 状态 | 要求 | 检查方式 |
|------|------|----------|
| **default** | 基础样式 | — |
| **hover** | 背景/边框变化，**120ms** 过渡 | 鼠标移上去 |
| **active** | 按压反馈（背景加深或下沉 1px） | 按住鼠标左键 |
| **focus-visible** | **2px 描边**，仅键盘触发 | 按 Tab 走一遍 |
| **disabled** | 降透明度 + `cursor: not-allowed` | 无任务时点操作按钮 |
| **selected** | 表格行选中态（左侧 3px accent 条） | 单击行 |

```css
/* ✅ 六态范式 */
.btn {
  transition: background-color .12s ease-out,
              border-color .12s ease-out,
              color .12s ease-out;        /* 只过渡真正变的属性 */
}
.btn:hover   { background: var(--bg-3); border-color: var(--line-3); }
.btn:active  { background: var(--bg-2); transform: translateY(1px); }
.btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
.btn:disabled { opacity: .45; cursor: not-allowed; }

/* ❌ AI 标志性坏习惯 */
.btn { transition: all .3s ease; }        /* 全属性过渡 + 300ms 太慢 */
```

**表格行必须覆盖**：

```css
.task-table tbody tr { transition: background-color .1s ease-out; }
.task-table tbody tr:hover { background: var(--hover); }
.task-table tbody tr.is-selected {
  background: var(--accent-bg);
  box-shadow: inset 3px 0 0 var(--accent);   /* 左侧选中条 */
}
.task-table tbody tr:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}
```

### 1.5.3 动效时序

**工具类软件的交互必须"不粘手"。** [AI 常犯]：`.3s ease` 一刀切。

| 场景 | 时长 | 缓动 |
|------|------|------|
| hover / focus 反馈 | **100-120ms** | `ease-out` |
| active 按压 | **60-80ms** | `ease-out` |
| 抽屉/弹层进出 | 180-220ms | `cubic-bezier(.22,1,.36,1)` |
| 进度条宽度变化 | 400ms | `ease-out` |
| toast 进出 | 160ms | `ease-out` |
| 主题切换 | **0ms**（瞬时） | — |

**禁止**：
- `transition: all`（[AI 常犯]，会过渡布局属性导致卡顿）
- 时长 > 300ms 的 hover 反馈
- 入场/错峰动画（现有 `dl-enter` 的 `animation-delay: calc(var(--i) * 42ms)` **必须删除**）

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: .01ms !important;
    transition-duration: .01ms !important;
  }
}
```

### 1.5.4 数值网格

**只用 4 的倍数。** [AI 常犯]：`padding: 15px`、`margin: 23px` 这类随手数。

允许值：`0, 2, 4, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64`

**例外**：`border-width: 1px`、`outline-width: 2px`、`line-height` 等。

### 1.5.5 排版细节

| # | 规则 | [AI 常犯] |
|---|------|-----------|
| T1 | 所有数字列 `font-variant-numeric: tabular-nums` | 速率/进度数字左右跳动 |
| T2 | 表格行高 `line-height: 1.25`，正文 `1.5` | 一刀切 1.5，表格虚胖 |
| T3 | 字重层次用 **400 / 500 / 600**，不用 700+ | 只会 400 和 bold |
| T4 | 文件名截断用 `text-overflow: ellipsis` + `title` 属性 | 直接溢出或强行换行 |
| T5 | 中英混排加空格（`速率 24.6 MB/s`） | `速率24.6MB/s` |
| T6 | 长数字用 `min-width` 防抖动 | 进度从 9%→100% 时列宽跳变 |

```css
.num { font-variant-numeric: tabular-nums; }   /* T1 */
.task-table td { line-height: 1.25; }          /* T2 */
.file-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
```

### 1.5.6 边界情况

[AI 常犯]：只做"理想路径"（有数据、正常尺寸）。**必须覆盖：**

| # | 场景 | 要求 |
|---|------|------|
| E1 | **0 个任务** | 空状态（现有 `EmptyState` ✅），且表头**不显示** |
| E2 | **1 个任务** | 单行无下边框（B3 已覆盖） |
| E3 | **超长文件名** | 省略号 + tooltip |
| E4 | **数值为 0** | 显示 `—` 而非 `0 B/s` |
| E5 | **总大小未知**（`total: 0`） | 进度显示"未知"而非 NaN% |
| E6 | **超长错误信息** | 抽屉内 `word-break: break-all`（现有 ✅） |
| E7 | **窄屏塌陷** | <1200px 按 §2.4 隐藏列 |
| E8 | **加载中** | 首次加载显示骨架屏或明确文案，不留白 |
| E9 | **断线** | 连接状态徽章变 err 色（现有 ✅） |

```tsx
// E5：避免 NaN%
const pct = task.total > 0 ? (task.done / task.total) * 100 : 0;
```

### 1.5.7 层级与阴影

**扁平优先。** [AI 常犯]：到处加柔和阴影。

| 元素 | 阴影 |
|------|------|
| 表格 / 工具栏 / 卡片 | **无阴影** |
| 抽屉 | `0 0 24px rgba(0,0,0,.18)` |
| 下拉菜单 / tooltip | `0 4px 12px rgba(0,0,0,.16)` |
| toast | `0 4px 16px rgba(0,0,0,.20)` |
| 模态遮罩 | 纯色半透明 `rgba(0,0,0,.42)`，**不加 blur** |

---

## 2. 布局架构

> **变更记录（2026-10-19）**：初版 v1.0 曾采用「去侧栏、全部功能入顶栏」方案，
> 用户实测反馈：功能按钮全堆顶栏、导航与动作混杂、组件颜色分界不明显。
> 本版重设计为「左侧导航栏 + 视图工具栏」，对标 qBittorrent（状态过滤侧栏）/
> Linear·VS Code（分组导航/底部设置）/ Figma（工具栏属内容区）——
> 导航（结构性，「我在哪」）归侧栏；动作（上下文性，「我能做什么」）归工具栏。
>
> **变更记录 v1.2（2026-10-19 二轮）**：用户实测反馈「UI 仍太轻、字体和按钮
> 背景糊在一起、颜色按钮配色与背景不适配、不要过细的字体」。三项对策：
> ① 内置 Inter 可变字体 + 交互件字重 ≥600（详见 §6.3）；② 上浮件
> `--line-3` 硬边框 + 1px 内嵌高光线 + hover 变亮；③ 主题卡 4 色平铺条
> 改为迷你窗口预览（§6.3）。

### 2.1 整体结构

```
┌──────────────┬──────────────────────────────────────────────┐
│ 侧栏 (216px) │ 视图工具栏 (48px)                            │
│  品牌区 48px  │ 新建任务 │ 暂停·恢复·删除 │    …    │ 搜索  │
│ ─────────── │──────────────────────────────────────────── │
│ 任务          │ 密集表格 (flex:1)                             │
│  全部     12 │ ┌──────┬────┬────┬────┬────┬────┬────┐     │
│  进行中    3 │ │名称  │状态│进度│速率│大小│剩余│操作│     │
│  排队      1 │ ├──────┼────┼┼────┼────┼────┼────┤     │
│  已暂停    2 │ │ …    │    │    │    │    │    │    │     │
│  已完成    5 │ └──────┴────┴────┴────┴────┴────┴────┘     │
│  失败      1 │                                              │
│ 数据          │（非任务视图：视图标题 + 副标题，行高不变） │
│  统计·RSS·日志│                                              │
│ ─────────── │                                              │
│ ↓速率 ↑速率   │                                              │
│ ●已连接      │                                              │
│ ⚙设置        │                                              │
├──────────────┴──────────────────────────────────────────────┤
│ 状态栏 (28px，跨全宽)                                       │
│ 共 N 任务 · 进行中 · 已完成 · 已选 N · 会话流量 · 版本      │
└──────────────────────────────────────────────────────────────┘
```

**三级表面色阶**（Material/Carbon 高度模型——组件分界的根基）：

| 层级 | Token | 元素 |
|------|-------|------|
| 下沉 | `--bg-2` | 侧栏 / 表头 / 状态栏 |
| 画布 | `--bg` | 内容区 / 表格体 / 输入框（比卡片暗一档，GitHub 模式） |
| 上浮 | `--bg-3` | 卡片 / 按钮 / 主题卡 / 浮层（抽屉·确认框） |

区域间一律 `1px --line` 硬边框；表格行分隔也用 `--line`
（旧 `--line-2` 在浅色主题几乎不可见——分界不明根因之一）。

### 2.2 导航与视图切换

侧栏承载全部导航（qBittorrent 式「过滤即导航」）：

- 「任务」组：6 个状态过滤项（全部/进行中/排队/已暂停/已完成/失败），
  每项右侧计数徽章；点击 = 切到任务视图 + 应用该过滤（跨视图直达）
- 「数据」组：统计 / RSS / 日志
- 底部固定：全局速率 · 连接状态 · 设置（Slack/VS Code 惯例：设置沉底）
- 当前项：`--accent-bg` 底色 + 左侧 3px 指示条 + `aria-current="page"`

快捷键：`Ctrl/Cmd+1..5` 切视图、`/` 聚焦搜索（Slack/GitHub 惯例）。

### 2.2.1 侧栏折叠

- 展开 216px ↔ 图标栏 56px（折叠钮在品牌区，`aria-expanded`）
- 持久化 `localStorage["dl.side"]`；≤900px 视口初始自动图标栏
  （一次性判断，不与用户选择打架）
- 图标栏模式：文案/计数入 `title` tooltip

### 2.3 任务详情

**保留右侧抽屉**（`dl-drawer`），560px 宽。现有实现良好，仅调整视觉。

### 2.4 响应式

| 断点 | 布局 |
|------|------|
| ≥1200px | 侧栏全展 + 全列显示 |
| 900-1200px | 侧栏全展；隐藏「剩余」「速率」列 |
| ≤900px | 侧栏自动图标栏（56px）；隐藏「剩余」「速率」列 |
| ≤760px | 批量按钮图标化（隐文案）；副标题隐藏（Android 准备） |

---

## 3. Token 体系

### 3.1 Token 命名

```
--bg          画布（内容区/表格体/输入框下沉字段）
--bg-2        下沉层（侧栏/表头/状态栏）
--bg-3        上浮层（卡片/按钮/主题卡/浮层）
--fg          主文字
--fg-2        次文字
--fg-3        弱文字（标签/占位）
--line        边框（主——区域分界与行分隔，2026-10-19 起行分隔开用此色）
--line-2      边框（卡内装饰性分隔，弱）
--line-3      边框（强调/按钮硬边框）
--hover       行 hover 背景
--side-w      侧栏宽 216px（可折叠为 --side-rail-w 56px）

--accent      强调色（主按钮/进度条/链接）
--accent-fg   强调色上的文字
--accent-bg   强调色浅底（徽章背景）

--ok          成功/做种      --ok-bg
--warn        警告/暂停      --warn-bg
--err         错误/失败      --err-bg
--info        信息/云兜底    --info-bg

--track       进度条轨道
--radius      圆角（主题可覆写）
```

### 3.2 尺寸 Token（跨主题统一）

```css
--radius-sm: 2px;
--radius:    var(--radius-theme, 3px);   /* 主题可覆写 */
--radius-lg: 4px;

--row-h:      32px;
--row-h-touch: 44px;   /* Android */
--toolbar-h:  44px;
--status-h:   28px;

--font-xs: 11px;
--font-sm: 12px;
--font-md: 13px;
--font-lg: 15px;
--font-xl: 20px;

--space-1: 4px;
--space-2: 8px;
--space-3: 12px;
--space-4: 16px;
--space-5: 24px;
```

---

## 4. 12 主题色板

> 所有色值取自**官方来源**，非自创：
> - Nord: [nordtheme.com](https://www.nordtheme.com/docs/colors-and-palettes)
> - Gruvbox: [morhetz/gruvbox](https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim)
> - Catppuccin: [catppuccin/palette](https://github.com/catppuccin/palette/blob/main/palette.json)
> - Solarized: Ethan Schoonover 官方规范
> - Carbon: [IBM Carbon Design System](https://carbondesignsystem.com/)

### 4.1 Nord Dark（默认）⭐

```css
[data-theme="nord-dark"] {
  --bg:#2e3440;  --bg-2:#292f3a;  --bg-3:#3b4252;
  --fg:#eceff4;  --fg-2:#d8dee9;  --fg-3:#7b88a1;
  --line:#434c5e; --line-2:#3b4252; --line-3:#4c566a;
  --hover:#363d4b;
  --accent:#88c0d0;  --accent-fg:#2e3440;  --accent-bg:rgba(136,192,208,.14);
  --ok:#a3be8c;      --ok-bg:rgba(163,190,140,.14);
  --warn:#ebcb8b;    --warn-bg:rgba(235,203,139,.14);
  --err:#bf616a;     --err-bg:rgba(191,97,106,.16);
  --info:#81a1c1;    --info-bg:rgba(129,161,193,.14);
  --track:#434c5e;   --radius-theme:3px;
}
```

### 4.2 Nord Light

```css
[data-theme="nord-light"] {
  --bg:#eceff4;  --bg-2:#e5e9f0;  --bg-3:#ffffff;
  --fg:#2e3440;  --fg-2:#4c566a;  --fg-3:#7b88a1;
  --line:#d8dee9; --line-2:#e5e9f0; --line-3:#c2cbd8;
  --hover:#e3e8ef;
  --accent:#5e81ac;  --accent-fg:#ffffff;  --accent-bg:rgba(94,129,172,.12);
  --ok:#6a8f4f;      --ok-bg:rgba(106,143,79,.12);
  --warn:#b58900;    --warn-bg:rgba(181,137,0,.12);
  --err:#bf616a;     --err-bg:rgba(191,97,106,.12);
  --info:#5e81ac;    --info-bg:rgba(94,129,172,.12);
  --track:#d8dee9;   --radius-theme:3px;
}
```

### 4.3 Carbon Dark

```css
[data-theme="carbon-dark"] {
  --bg:#161616;  --bg-2:#1c1c1c;  --bg-3:#262626;
  --fg:#f4f4f4;  --fg-2:#c6c6c6;  --fg-3:#8d8d8d;
  --line:#393939; --line-2:#2a2a2a; --line-3:#525252;
  --hover:#232323;
  --accent:#0f62fe;  --accent-fg:#ffffff;  --accent-bg:rgba(15,98,254,.16);
  --ok:#42be65;      --ok-bg:rgba(66,190,101,.16);
  --warn:#f1c21b;    --warn-bg:rgba(241,194,27,.16);
  --err:#fa4d56;     --err-bg:rgba(250,77,86,.16);
  --info:#4589ff;    --info-bg:rgba(69,137,255,.16);
  --track:#393939;   --radius-theme:2px;
}
```

### 4.4 Carbon Light

```css
[data-theme="carbon-light"] {
  --bg:#ffffff;  --bg-2:#f4f4f4;  --bg-3:#ffffff;
  --fg:#161616;  --fg-2:#525252;  --fg-3:#8d8d8d;
  --line:#e0e0e0; --line-2:#f4f4f4; --line-3:#c6c6c6;
  --hover:#f4f4f4;
  --accent:#0f62fe;  --accent-fg:#ffffff;  --accent-bg:rgba(15,98,254,.10);
  --ok:#24a148;      --ok-bg:rgba(36,161,72,.10);
  --warn:#f1c21b;    --warn-bg:rgba(241,194,27,.20);
  --err:#da1e28;     --err-bg:rgba(218,30,40,.10);
  --info:#0043ce;    --info-bg:rgba(0,67,206,.10);
  --track:#e0e0e0;   --radius-theme:2px;
}
```

### 4.5 Gruvbox Dark

```css
[data-theme="gruvbox-dark"] {
  --bg:#282828;  --bg-2:#1d2021;  --bg-3:#3c3836;
  --fg:#ebdbb2;  --fg-2:#d5c4a1;  --fg-3:#928374;
  --line:#504945; --line-2:#3c3836; --line-3:#665c54;
  --hover:#32302f;
  --accent:#83a598;  --accent-fg:#282828;  --accent-bg:rgba(131,165,152,.15);
  --ok:#b8bb26;      --ok-bg:rgba(184,187,38,.15);
  --warn:#fabd2f;    --warn-bg:rgba(250,189,47,.15);
  --err:#fb4934;     --err-bg:rgba(251,73,52,.16);
  --info:#8ec07c;    --info-bg:rgba(142,192,124,.15);
  --track:#504945;   --radius-theme:3px;
}
```

### 4.6 Gruvbox Light

```css
[data-theme="gruvbox-light"] {
  --bg:#fbf1c7;  --bg-2:#f2e5bc;  --bg-3:#ebdbb2;
  --fg:#3c3836;  --fg-2:#504945;  --fg-3:#928374;
  --line:#d5c4a1; --line-2:#ebdbb2; --line-3:#bdae93;
  --hover:#f2e5bc;
  --accent:#076678;  --accent-fg:#fbf1c7;  --accent-bg:rgba(7,102,120,.12);
  --ok:#79740e;      --ok-bg:rgba(121,116,14,.12);
  --warn:#b57614;    --warn-bg:rgba(181,118,20,.14);
  --err:#9d0006;     --err-bg:rgba(157,0,6,.12);
  --info:#427b58;    --info-bg:rgba(66,123,88,.12);
  --track:#d5c4a1;   --radius-theme:3px;
}
```

### 4.7 Catppuccin Mocha

```css
[data-theme="catppuccin-dark"] {
  --bg:#1e1e2e;  --bg-2:#181825;  --bg-3:#313244;
  --fg:#cdd6f4;  --fg-2:#bac2de;  --fg-3:#7f849c;
  --line:#45475a; --line-2:#313244; --line-3:#585b70;
  --hover:#2a2a3c;
  --accent:#89b4fa;  --accent-fg:#1e1e2e;  --accent-bg:rgba(137,180,250,.14);
  --ok:#a6e3a1;      --ok-bg:rgba(166,227,161,.14);
  --warn:#f9e2af;    --warn-bg:rgba(249,226,175,.14);
  --err:#f38ba8;     --err-bg:rgba(243,139,168,.16);
  --info:#94e2d5;    --info-bg:rgba(148,226,213,.14);
  --track:#45475a;   --radius-theme:5px;
}
```

### 4.8 Catppuccin Latte

```css
[data-theme="catppuccin-light"] {
  --bg:#eff1f5;  --bg-2:#e6e9ef;  --bg-3:#ffffff;
  --fg:#4c4f69;  --fg-2:#5c5f77;  --fg-3:#8c8fa1;
  --line:#ccd0da; --line-2:#e6e9ef; --line-3:#bcc0cc;
  --hover:#e6e9ef;
  --accent:#1e66f5;  --accent-fg:#ffffff;  --accent-bg:rgba(30,102,245,.10);
  --ok:#40a02b;      --ok-bg:rgba(64,160,43,.10);
  --warn:#df8e1d;    --warn-bg:rgba(223,142,29,.12);
  --err:#d20f39;     --err-bg:rgba(210,15,57,.10);
  --info:#179299;    --info-bg:rgba(23,146,153,.10);
  --track:#ccd0da;   --radius-theme:5px;
}
```

### 4.9 Solarized Dark

```css
[data-theme="solarized-dark"] {
  --bg:#002b36;  --bg-2:#00252e;  --bg-3:#073642;
  --fg:#eee8d5;  --fg-2:#93a1a1;  --fg-3:#657b83;
  --line:#0d4a58; --line-2:#0a3d49; --line-3:#13505f;
  --hover:#04323d;
  --accent:#268bd2;  --accent-fg:#fdf6e3;  --accent-bg:rgba(38,139,210,.16);
  --ok:#859900;      --ok-bg:rgba(133,153,0,.18);
  --warn:#b58900;    --warn-bg:rgba(181,137,0,.18);
  --err:#dc322f;     --err-bg:rgba(220,50,47,.18);
  --info:#2aa198;    --info-bg:rgba(42,161,152,.16);
  --track:#0d4a58;   --radius-theme:3px;
}
```

### 4.10 Solarized Light

```css
[data-theme="solarized-light"] {
  --bg:#fdf6e3;  --bg-2:#eee8d5;  --bg-3:#ffffff;
  --fg:#073642;  --fg-2:#586e75;  --fg-3:#93a1a1;
  --line:#eee8d5; --line-2:#f5efdc; --line-3:#d9d2c0;
  --hover:#eee8d5;
  --accent:#268bd2;  --accent-fg:#fdf6e3;  --accent-bg:rgba(38,139,210,.10);
  --ok:#859900;      --ok-bg:rgba(133,153,0,.12);
  --warn:#b58900;    --warn-bg:rgba(181,137,0,.14);
  --err:#dc322f;     --err-bg:rgba(220,50,47,.10);
  --info:#2aa198;    --info-bg:rgba(42,161,152,.10);
  --track:#eee8d5;   --radius-theme:3px;
}
```

### 4.11 极简灰 Dark（Monochrome）

```css
[data-theme="mono-dark"] {
  --bg:#1a1a1a;  --bg-2:#151515;  --bg-3:#242424;
  --fg:#e8e8e8;  --fg-2:#a8a8a8;  --fg-3:#6e6e6e;
  --line:#2e2e2e; --line-2:#242424; --line-3:#3d3d3d;
  --hover:#202020;
  --accent:#e8a33d;  --accent-fg:#1a1a1a;  --accent-bg:rgba(232,163,61,.14);
  --ok:#7fb069;      --ok-bg:rgba(127,176,105,.14);
  --warn:#d9a441;    --warn-bg:rgba(217,164,65,.14);
  --err:#c9564e;     --err-bg:rgba(201,86,78,.16);
  --info:#5a9fd4;    --info-bg:rgba(90,159,212,.14);
  --track:#2e2e2e;   --radius-theme:2px;
}
```

### 4.12 极简灰 Light

```css
[data-theme="mono-light"] {
  --bg:#fafafa;  --bg-2:#f0f0f0;  --bg-3:#ffffff;
  --fg:#1a1a1a;  --fg-2:#5a5a5a;  --fg-3:#8e8e8e;
  --line:#e0e0e0; --line-2:#ededed; --line-3:#cccccc;
  --hover:#f0f0f0;
  --accent:#b8721f;  --accent-fg:#ffffff;  --accent-bg:rgba(184,114,31,.10);
  --ok:#4f7a3d;      --ok-bg:rgba(79,122,61,.10);
  --warn:#946a15;    --warn-bg:rgba(148,106,21,.12);
  --err:#a83e36;     --err-bg:rgba(168,62,54,.10);
  --info:#3d6f96;    --info-bg:rgba(61,111,150,.10);
  --track:#e0e0e0;   --radius-theme:2px;
}
```

---

## 5. 组件规范

### 5.1 任务表格（核心）

| 列 | 宽度 | 对齐 | 内容 |
|----|------|------|------|
| 名称 | flex, min 200px | 左 | 省略号截断，hover 显示完整 |
| 状态 | 90px | 左 | 徽章 |
| 进度 | 160px | 左 | 进度条 + 百分比 |
| 速率 | 100px | 右 | ↓ 下载 / ↑ 上传（等宽数字） |
| 大小 | 90px | 右 | done / total |
| 剩余 | 80px | 右 | ETA |
| 操作 | 72px | 右 | 暂停/恢复 + 删除 |

**行行为**：单击选中，双击打开详情抽屉，右键菜单（后续）。

**虚拟滚动**：`@tanstack/react-virtual`，阈值 >100 条启用。

### 5.2 状态徽章

```
下载中  accent 色底 + accent 文字 + 呼吸点
做种中  ok 色底
云兜底  info 色底
排队中  bg-3 底 + fg-3 文字
已暂停  warn 色底
已完成  ok 色底
失败    err 色底
```

**无障碍**：状态同时通过**文字**表达，不依赖颜色。

### 5.3 进度条

- 高度 **4px**（表格内）/ 6px（详情）
- 圆角 2px
- 运行时：纯色填充 + 细微流动条纹（**去掉现有的扫光 `::before`**）
- 完成：ok 色
- 失败：err 色
- 排队：斜纹 + 半透明

### 5.4 按钮

| 类型 | 样式 |
|------|------|
| 主按钮 | accent 实心，**无辉光** |
| 次按钮 | 透明底 + line 边框 |
| 图标按钮 | 24×24，透明底，hover 显 bg-3 |

### 5.5 抽屉

现有 `dl-drawer` 结构保留，调整：
- 宽度 560px
- 去掉入场位移（或缩短至 120ms）
- 阴影仅用于层级表达

---

## 6. 主题切换机制

### 6.1 实现

```tsx
// types.ts
export const THEME_KEYS = [
  "nord-dark", "nord-light",
  "carbon-dark", "carbon-light",
  "gruvbox-dark", "gruvbox-light",
  "catppuccin-dark", "catppuccin-light",
  "solarized-dark", "solarized-light",
  "mono-dark", "mono-light",
] as const;
```

```css
/* tokens.css */
[data-theme="nord-dark"] { /* ... */ }
[data-theme="carbon-dark"] { /* ... */ }
/* ... */
```

**应用方式**：`document.documentElement.setAttribute("data-theme", theme)`

**持久化**：`localStorage["dl.theme"]`（现有机制不变）

### 6.2 默认与跟随系统

```ts
// 首次启动：无 localStorage → 跟随 prefers-color-scheme
const prefersDark = matchMedia("(prefers-color-scheme: dark)").matches;
const defaultTheme = prefersDark ? "nord-dark" : "nord-light";
```

### 6.3 主题选择器

设置页保留主题预览卡，**改为 2 列网格 × 6 行**（≥960px 3 列）。

**迷你窗口预览（2026-10-19 字重扎实化批重设计）**：每张卡的预览区
不再使用 4 色平铺条（bg/bg-2/bg-3 三色明度接近，读作「一坨灰」），
改为渲染目标主题的**真实层级切片**——容器挂 `data-theme={tk}`，
内部全部取该主题 token：

- 标题栏 + 左侧栏（`--bg-2`，1px `--line` 分界）
- 画布（`--bg`）
- 迷你按钮（`--bg-3` + `--line-3` 边框 + 1px 内嵌高光线，与真实按钮同款）
- 强调色块（`--accent`）
- 正文行两根（`--fg-2` / `--fg-3`）

预览外圈 8px 卡片色留白 + 目标主题 `--line` 边框（圆角 2px）：
浅色主题预览不再与浅色卡片底融合，深色预览同样有容器边界。
预览高 64px，几何定义见 `globals.css` `.dl-theme-swatch`。

**字重与字体（2026-10-19 二轮）**：

- 内置 **Inter 可变字体**（`public/fonts/inter-latin-wght.woff2`，
  48KB，latin unicode-range）接管拉丁字形；CJK 按栈回退
  苹方/雅黑/Noto——消除 Segoe UI/DejaVu 在 12-13px 下的细弱观感。
- 交互件（按钮/tabs/字段标签/主题名）字重 ≥ **600**；表头
  `--fg-2` + 600 + letter-spacing 0.03em；侧栏项 `--fg` + 500。
- 上浮件（按钮/搜索框）统一 `--line-3` 硬边框 + 1px 内嵌高光线
  （`inset 0 1px 0 color-mix(in srgb, var(--fg) 7%, transparent)`，
  GitHub/Bootstrap 模式）；hover 变亮（`color-mix(bg-3, fg 8%)`）
  而非变暗——上浮件 hover 应更亮。

---

## 7. 从现有代码迁移

### 7.1 保留（改动小）

| 文件 | 处理 |
|------|------|
| `store.ts` | ✅ 不动（API 客户端完善） |
| `types.ts` | ⚠️ 改 `THEME_KEYS` / `THEME_META` |
| `format.ts` | ✅ 不动 |
| `SettingsView.tsx` | ⚠️ 视觉调整 + **加目录浏览按钮** |
| `TaskDetail.tsx` | ⚠️ 视觉调整 |
| `StatsView.tsx` / `RssView.tsx` / `LogsView.tsx` | ⚠️ 视觉调整 |
| `SpeedChart.tsx` | ✅ 保留逻辑，配色改 token |

### 7.2 重写

| 文件 | 原因 |
|------|------|
| `globals.css` | 全部重写为 token 体系 |
| `DlApp.tsx` | 去掉侧边栏，改顶部工具栏 |
| `TasksView.tsx` → `TasksTable.tsx` | 卡片网格 → 密集表格 |
| `Common.tsx` | 简化，去掉 Qoder 依赖 |

### 7.3 移除

- `ui/public/qoder-ui/`（175KB CSS + 字体）→ 删除
- 所有 `qoder-*` class 引用

### 7.4 功能补齐（唯一确认的缺口）

**下载目录浏览按钮**：需要 Tauri dialog 插件

```tsx
import { open } from "@tauri-apps/plugin-dialog";
const dir = await open({ directory: true, multiple: false });
if (dir) patch("download", "dest_root", dir);
```

**注意**：浏览器模式下无此能力，需降级为纯文本输入。

---

## 8. 实施计划

| 阶段 | 内容 | 产出 |
|------|------|------|
| **P0** | token 层 + 主题定义 | `tokens.css`，12 主题可用 |
| **P0** | 工具栏 + 表格骨架 | `DlApp.tsx` + `TasksTable.tsx` |
| **P1** | 组件视觉统一 | 徽章/进度条/按钮/抽屉 |
| **P1** | 设置页 + 目录浏览 | `SettingsView.tsx` |
| **P2** | 虚拟滚动 | `TasksTable` 接入 |
| **P2** | 性能修复 | 去掉 `JSON.stringify` 比较；轮询降频 |
| **P3** | Android 适配 | 触摸目标 + 卡片视图 |

---

## 9. 验收清单（可客观判定）

> **用法**：云端提 PR 后，逐条勾选。每条都**可被脚本或肉眼客观判定**，
> 避免"看起来还行"这类主观结论。
> 标注 **[脚本]** 的条目由 `scripts/check-ui-spec.ps1` 自动检查。

### 9.1 阶段门禁（P0 - 必须先过）

| # | 检查项 | 判定方式 |
|---|--------|----------|
| G0-1 | `ui/public/qoder-ui/` 目录已删除 | **[脚本]** 目录不存在 |
| G0-2 | 全仓无 `qoder-` class 引用 | **[脚本]** grep 计数 = 0 |
| G0-3 | `tokens.css` 存在且定义 12 个 `[data-theme]` | **[脚本]** 匹配计数 = 12 |
| G0-4 | 每个主题含全部必需 token（`--bg/--fg/--accent/--ok/--warn/--err/--line/--track`） | **[脚本]** 逐个校验 |
| G0-5 | 无 `transition: all` | **[脚本]** grep 计数 = 0 |
| G0-6 | 无 `backdrop-filter`（除 modal 遮罩） | **[脚本]** grep 计数 ≤ 1 |
| G0-7 | 无渐变用于按钮/背景 | **[脚本]** 检查 `linear-gradient` 仅出现在进度条 |
| G0-8 | 无 `animation-delay: calc(var(--i)` 错峰入场 | **[脚本]** grep 计数 = 0 |
| G0-9 | `tasks.json` / daemon API **零改动** | **[脚本]** `git diff main -- crates/` 为空 |

### 9.2 视觉与布局

| # | 检查项 | 判定方式 |
|---|--------|----------|
| V1 | 单屏可见 ≥15 条任务（1440×900） | 截图人工确认 |
| V2 | 侧栏导航 + 可折叠（216px ↔ 56px 图标栏），导航与动作分离 | 代码 + 截图 |
| V2a | 三级表面色阶分界清晰（侧栏 --bg-2 / 画布 --bg / 卡片 --bg-3） | 代码 + 截图（明暗主题各一） |
| V3 | 任务以**表格**呈现（非卡片网格） | 代码 `TasksTable.tsx` 存在 |
| V4 | 表格行高 32px | **[脚本]** 检查 `--row-h: 32px` |
| V5 | 12 主题逐个切换视觉正确 | 逐个截图 |
| V6 | 默认主题为 `nord-dark` | 代码检查 `DEFAULT_THEME` |

### 9.3 细节规范（§1.5，⭐ 重点）

| # | 检查项 | 判定方式 |
|---|--------|----------|
| D1 | 表格无竖线（`border-left/right` 不用） | **[脚本]** 检查 |
| D2 | 最后一行无下边框 | **[脚本]** grep `last-child` |
| D3 | 行 hover 有背景变化 | 肉眼 |
| D4 | 行选中态有左侧 accent 条 | **[脚本]** grep `is-selected` |
| D5 | 所有按钮有 `:focus-visible` | **[脚本]** 统计 |
| D6 | 禁用态有 `cursor: not-allowed` | **[脚本]** grep |
| D7 | hover 过渡 ≤120ms | **[脚本]** 检查无 `.3s` / `300ms` |
| D8 | 间距值全为 4 的倍数 | **[脚本]** 正则扫 CSS |
| D9 | 数字列有 `tabular-nums` | **[脚本]** grep |
| D10 | `prefers-reduced-motion` 仍生效 | **[脚本]** grep |

### 9.4 功能

| # | 检查项 | 判定方式 |
|---|--------|----------|
| F1 | 下载目录可通过**文件对话框**选择 | 手动测试 |
| F2 | 任务暂停/恢复可用 | 手动测试 |
| F3 | 进度条实时更新（非 0%） | 手动测试 |
| F4 | 1000 任务滚动流畅 | 手动测试（虚拟滚动） |
| F5 | 0 任务时空状态正确，表头隐藏 | 手动测试 |
| F6 | `total=0` 时不显示 NaN% | **[脚本]** grep 判断逻辑 |
| F7 | 移动端 <768px 表格转卡片 | 手动测试 |

### 9.5 性能

| # | 检查项 | 判定方式 |
|---|--------|----------|
| P1 | 去掉 `JSON.stringify` 全量比较 | **[脚本]** grep 计数 = 0 |
| P2 | 轮询频率 ≤1s 且支持 SSE 时降频 | 代码审查 |
| P3 | 首屏 JS 体积较重构前减小或持平 | build 输出对比 |

---

## 10. 监督与协作流程

### 10.1 角色

| 角色 | 职责 |
|------|------|
| **云端开发** | fork `mmqz/smart-downloader`，开分支实现，提 PR |
| **我（监督）** | 按 §9 清单审查 PR，留言问题，通过后合并到 `tomjiu/smart-downloader` |
| **你** | 最终决策（视觉方向争议、范围变更） |

### 10.2 PR 门禁

**合并前置条件（全部满足才可合并）：**

1. §9.1 阶段门禁 **G0-1 ~ G0-9 全通过**
2. §9.3 细节规范 **D1 ~ D10 全通过**
3. 无 daemon API 改动（G0-9）
4. `bun run build`（或 `npm run build`）成功
5. 至少提供一个主题的**前后对比截图**

### 10.3 分阶段提交要求

| 阶段 | 提交内容 | 门禁 |
|------|----------|------|
| **P0-1** | `tokens.css` + 12 主题定义 | G0-3, G0-4 |
| **P0-2** | 删除 qoder-ui + 工具栏骨架 | G0-1, G0-2, G0-6, G0-7 |
| **P0-3** | `TasksTable.tsx` 密集表格 | V1, V3, V4, D1, D2 |
| **P1** | 组件视觉统一 + 设置页浏览按钮 | D3-D10, F1 |
| **P2** | 虚拟滚动 + 性能修复 | F4, P1, P2 |

**每个阶段单独 PR**，不要一次提一大坨 —— 便于定位问题和回滚。

### 10.4 审查留言格式

我审查时使用统一格式：

```
## 审查结果：<通过 / 需修改>

### 门禁检查
- [x] G0-1 qoder-ui 已删除
- [ ] G0-5 发现 3 处 `transition: all` ← 需修
      ui/src/app/globals.css:142
      ui/src/components/dl/Common.tsx:88

### 细节问题
1. <文件:行号> <问题> <依据条款>

### 结论
<可以合并 / 修复后重提>
```

---

## 11. 待确认事项

1. **是否需要浅色主题？** — 本方案包含 6 个浅色变体，如不需要可减半
2. **Tauri dialog 插件** — 需确认 `desktop/src-tauri` 是否已有该插件，无则需添加
3. **虚拟滚动库** — 建议 `@tanstack/react-virtual`（3KB），需加依赖
4. **是否保留 Next.js** — 建议保留，专注视觉重构；后续可评估迁 Vite
5. **顶栏布局** — 方案为"工具栏 + 分段过滤"，是否有其他偏好
6. **前端包管理器** — 仓库有 `bun.lock`，确认统一用 bun
