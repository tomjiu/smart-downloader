## 来源

用户在 **0.2.3 实测**（本轮全部 7 个 PR 合并后构建的前端）发现 **6 个问题**，其中 2 个是功能性 bug。

环境：Windows 11，浏览器直连 `http://127.0.0.1:8790`（daemon 内嵌 UI），默认主题跟随系统（浅色）。

---

## 问题清单（按严重度）

| # | 现象 | 类型 | 严重度 |
|---|---|---|---|
| 1 | **点「设置」后界面自动变暗** | 功能 bug | 🔴 P0 |
| 2 | **侧栏不固定，页面滚动时滚走** | 布局 bug | 🔴 P0 |
| 3 | **按钮与设置内容重叠** | 布局 bug | 🔴 P0 |
| 4 | **设置按钮点击后消失** | 布局 bug | 🔴 P0 |
| 5 | 点击交互有延迟 | 性能 | 🟡 P1 |
| 6 | **设置页可配置项太少、预制选项太少** | 功能缺失 | 🟡 P1 |

---

## 问题 1：点「设置」后界面自动变暗 🔴

### 复现

1. 系统主题设为**浅色**
2. 首次打开应用 → 界面是 `nord-light`（正确，启动脚本按 `prefers-color-scheme` 判定）
3. 点击侧栏「设置」
4. **界面瞬间变成深色**

### 根因（我已定位到具体代码）

`ui/src/components/dl/SettingsView.tsx:49`

```typescript
const [theme, setTheme] = useState<ThemeKey>(DEFAULT_THEME);
//                                          ^^^^^^^^^^^^^^ = "nord-dark" 硬编码
```

```typescript
// :65-69  第一个 effect
useEffect(() => {
  load();
  const savedTheme = (localStorage.getItem("dl.theme") as ThemeKey) || null;
  if (savedTheme && THEME_KEYS.includes(savedTheme)) setTheme(savedTheme);
  ...
}, [load]);

// :73-76  第二个 effect
useEffect(() => {
  document.documentElement.setAttribute("data-theme", theme);
  localStorage.setItem("dl.theme", theme);   // ← 挂载时立即执行
}, [theme]);
```

**时序**：

```
① 页面加载 → layout.tsx:25 启动脚本按系统偏好设 data-theme="nord-light"  ✅
② 点「设置」→ SettingsView 挂载
③ useState 初始化 = "nord-dark"（硬编码，未读当前 DOM）
④ 第二个 useEffect【首次运行】立即执行 → 写入 data-theme="nord-dark"     ❌ 变暗
                                        → localStorage 也被覆盖成 nord-dark ❌ 持久化污染
⑤ 第一个 useEffect 才读到 savedTheme，setTheme 回浅色 → 闪烁一下
```

### 修复

`useState` 初始值必须读**当前 DOM 实际值**，而不是硬编码常量：

```typescript
const [theme, setTheme] = useState<ThemeKey>(() => {
  if (typeof document === "undefined") return DEFAULT_THEME;
  const cur = document.documentElement.getAttribute("data-theme") as ThemeKey | null;
  return cur && THEME_KEYS.includes(cur) ? cur : DEFAULT_THEME;
});
```

**并建议**：多视图共享主题时，应提到 `DlApp` 层或用一个 `useTheme()` hook，
不要在每个视图组件里各自 `useState` —— 否则每个视图挂载都会覆盖一次。

---

## 问题 2/3/4：布局三个 bug（同一根因）🔴

### 现象

- 滚动任务列表 → **侧栏跟着滚走**
- 内容底部 → **按钮被固定定位的状态栏遮住**
- 内容长时 → **侧栏底部的「设置」按钮滚出视口消失**

### 根因

`ui/src/app/globals.css:53-78`

```css
.dl-root {
  min-height: 100vh;      /* ← 只设最小高度 */
  display: flex;
  flex-direction: row;
}
.dl-shell {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  /* ← 缺 min-height: 0 —— flex 子项默认 min-height:auto 会被内容撑开 */
}
.dl-body {
  display: flex;
  flex: 1;
  min-height: 0;
}
.dl-main {
  flex: 1;
  min-width: 0;
  padding: var(--space-4) var(--space-5) var(--space-5);
  overflow-x: hidden;     /* ← 只有横向，没有 overflow-y */
  display: flex;
  flex-direction: column;
}
```

**问题链**：

```
内容超过一屏
  → .dl-main 内容高度撑开（没有 overflow-y 约束）
  → .dl-shell 被撑高（缺 min-height: 0）
  → .dl-root 被撑高（min-height 只保证下限，不封顶）
  → 整个页面出现滚动条
  → 侧栏（flex 首列）跟着文档流滚走
  → 底部固定（position:fixed）的 .dl-statusbar 压住 .dl-main 尾部内容
```

### 修复：改为「pane 滚动」模式（VS Code / qBittorrent 同款）

```css
.dl-root {
  height: 100vh;
  height: 100dvh;         /* 移动端地址栏收展不跳动 */
  overflow: hidden;       /* 外壳不滚 */
  display: flex;
  flex-direction: row;
}
.dl-shell {
  flex: 1;
  min-width: 0;
  min-height: 0;          /* ← 关键：允许收缩 */
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.dl-body {
  display: flex;
  flex: 1;
  min-height: 0;
  overflow: hidden;
}
.dl-main {
  flex: 1;
  min-width: 0;
  min-height: 0;          /* ← 关键 */
  padding: var(--space-4) var(--space-5) var(--space-5);
  overflow-y: auto;       /* ← 滚动落在内容区自身 */
  overflow-x: hidden;
  overscroll-behavior: contain;
  display: flex;
  flex-direction: column;
}
```

**注意**：`.dl-side-nav` 已有 `flex:1; min-height:0; overflow-y:auto`，
`.dl-side-foot` 已有 `flex:none` —— **侧栏自身是对的**，问题全在父容器。

**验证方法**：造 100+ 个任务（或用一份长设置页），滚动时：
- 侧栏必须纹丝不动
- 状态栏必须固定在底部
- 内容最后一行必须能滚到状态栏上方

---

## 问题 5：点击交互有延迟 🟡

### 根因

`ui/src/components/dl/DlApp.tsx:189`

```typescript
const iv = setInterval(refresh, 1000);   // 每秒全量拉 /tasks + /stats
```

```typescript
// :146-148  refresh 内
if (t !== null) {
  setTasks(t);      // ← 每次都设新数组（即使数据完全相同）
  setLoaded(true);
}
```

**每秒无条件 `setTasks(新引用)`** → React 认为状态变了 → **整个 DlApp 及全部子组件每秒重渲染**。

任务数上千时：每秒重建上千个 React 元素 + DOM diff → 主线程被占满 →
**按钮 `onClick` 排队到下一个空闲帧，表现为点击滞后 100–300ms**。

### 修复

**A. 内容没有变化时不触发重渲染**

```typescript
const taskSigRef = useRef<string>("");
const applyTasks = useCallback((t: TaskListItem[]) => {
  let sig = "";
  for (const x of t) sig += `${x.task_id}\u0001${x.state}\u0001${x.name ?? ""}\u0002`;
  if (sig === taskSigRef.current) return;   // 无变化 → 跳过
  taskSigRef.current = sig;
  setTasks(t);
}, []);
```

**B. 任务行组件用 `React.memo` + 稳定 key**

`TasksTable` 的行组件建议 `memo()` 包裹，props 只传该行需要的数据
（当前 `snapshots` / `rates` 整个对象传入会让所有行都失效）。

**C. 列表与快照轮询频率分离**

现有：列表 1s、快照 2s。建议：
- 有 WS `Progress` 事件时（PR #117 已接线），列表轮询降到 5s 兜底
- 快照轮询只在「表格可见 + 有进行中任务」时运行

**D. 长列表虚拟滚动**（任务 > 200 时）

---

## 问题 6：设置页可配置项太少 🟡

### 用户原话

> 设置页能设置的东西太少了，预制设置也太少了

### 现状

`SettingsView.tsx` 已接的域（9 个）：

```
bandwidth / connection / bittorrent / download /
cleanup / post_download / webhook / scheduler / queue + meta
```

**但对照后端 `/settings` 的能力，仍有缺口：**

| 后端支持但 UI 未暴露 | 位置 |
|---|---|
| `scheduler.completion_action` | `settings.rs:120`（none/exit/shutdown/sleep/hibernate）→ **完成关机没有 UI** |
| 分类目录规则 | 后端无（需新做） |
| Tracker 订阅源 | 仅静态 `extra_trackers` 数组，无在线列表订阅 |
| 主题预设/快捷键自定义 | 无 |
| 语言/区域 | 无 i18n |
| 导入/导出配置 | 无 |
| 重置为默认值 | 无 |
| 配置项搜索 | 无（设置项多了会难找） |

### 用户要的「预制设置」

我理解是 **preset（预设方案）**——一键套用的配置组合。建议增加：

| Preset | 内容 |
|---|---|
| **省电/低负载** | 并发 1、限速 1MB/s、不做种 |
| **极速下载** | 并发最大、不限速、启用全部 P2P |
| **隐身/最安全** | 加密 require、不用 DHT/PEX/LSD、走代理 |
| **PT 站** | 私有种子优化、上传不限、比率 2.0、不做 DHT |
| **移动网络** | 限速、禁用 P2P、小并发 |
| **夜间定时** | 备用限速时段 22:00–07:00 |

### 建议的 UI 改进

1. **设置项分组导航**（左侧二级导航：带宽/连接/BT/下载/调度/通知/高级）
2. **每项右侧显示「当前值 / 默认值」+ 一键恢复默认**
3. **搜索框**（设置项多了必须有）
4. **Preset 卡片区**（顶部一排，点击套用）
5. **导出 / 导入 JSON**（便于备份和团队共享）
6. **危险区**（重置全部设置，二次确认）

---

## 我在本地已尝试修复（但效果不好，故交回）

我改过 `globals.css` / `SettingsView.tsx` / `DlApp.tsx` 三个文件
（pane 滚动 + 主题初值读 DOM + 任务签名去重），**已全部还原**
（`git checkout`），当前工作区干净，云端可从 `2a248d7` 直接开工。

**唯一保留**的是 `ui/src/components/dl/TasksView.tsx` 的 P0-2 修复
（`displayName()` 兜底）——那个是 `bun run build` 的**硬阻断**
（TypeScript 报 `'t.name' is possibly 'null' or 'undefined'`），
不修则整个 UI 构建不出来，且已与 `types.ts` 的 `name?: string | null` 对齐。

**我的修复没解决问题的可能原因**：
- 布局问题可能还有我没找到的第二处（如某个视图自身有 `overflow` 覆盖）
- 点击延迟可能主要来自 `TasksTable` 行组件未 memo（我只做了列表签名去重）

**建议云端用真实浏览器 + React DevTools Profiler 实测定位**，而不是只看代码。

---

## 验收标准

| # | 项 | 通过标准 |
|---|---|---|
| 1 | 主题不被设置页覆盖 | 系统浅色 → 打开设置 → 仍浅色；localStorage 不被改写 |
| 2 | 侧栏固定 | 造 100 个任务，滚到底，侧栏位置不变 |
| 3 | 无遮挡 | 内容最后一行能滚到状态栏上方，不被压住 |
| 4 | 设置按钮常驻 | 任何滚动位置都能看到并点击 |
| 5 | 点击无延迟 | React Profiler 单次点击 < 16ms；1000 任务下无掉帧 |
| 6 | 设置项完善 | 补 `completion_action` + 至少 4 个 preset + 搜索 + 恢复默认 |
