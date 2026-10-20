## 实测反馈：本 PR 的 UI 在真浏览器里有 6 个问题（详见 #127）

在 **0.2.3 构建版**（本 PR + 已合并的 7 个后端 PR）上跑真实浏览器（Windows 11，系统主题=浅色），
发现 6 个问题。**逐个定位到了本 PR 的代码行**，请在本 PR 内一并修复。

> 关联：#127。本 PR 是 UI 骨架重构，这 6 个缺陷就出在这些文件里，
> 分两个 PR 改会互相冲突，建议合并修。

---

## 🔴 P0-1：点「设置」后界面自动变暗（主题被覆盖）

**`ui/src/components/dl/SettingsView.tsx:49`**

```typescript
const [theme, setTheme] = useState<ThemeKey>(DEFAULT_THEME);   // DEFAULT_THEME = "nord-dark"
```

**:73-76**

```typescript
useEffect(() => {
  document.documentElement.setAttribute("data-theme", theme);
  localStorage.setItem("dl.theme", theme);   // ← 挂载时立即执行
}, [theme]);
```

**时序**：

```
① layout.tsx:25 启动脚本按 prefers-color-scheme 设 data-theme="nord-light"  ✅
② 点「设置」→ SettingsView 挂载
③ useState 初始化 = "nord-dark"（硬编码，未读当前 DOM）
④ useEffect【首次运行】立即写入 data-theme="nord-dark"   ❌ 界面变暗
                             并写 localStorage            ❌ 持久化也被污染
⑤ 第一个 useEffect 才读 savedTheme → setTheme 回浅色 → 闪一下
```

**修复**：初始值读当前 DOM 实际值。

```typescript
const [theme, setTheme] = useState<ThemeKey>(() => {
  if (typeof document === "undefined") return DEFAULT_THEME;
  const cur = document.documentElement.getAttribute("data-theme") as ThemeKey | null;
  return cur && THEME_KEYS.includes(cur) ? cur : DEFAULT_THEME;
});
```

**更彻底的做法**：主题不该由 SettingsView 独占管理。抽成 `useTheme()` hook
或提到 `DlApp` 层，否则将来每个视图挂载都会覆盖一次。

---

## 🔴 P0-2/3/4：侧栏不固定 + 内容被遮挡 + 设置按钮消失

三个现象，**同一个根因**。

**`ui/src/app/globals.css:53-78`**

```css
.dl-root {
  min-height: 100vh;      /* ← 只有下限，不封顶 */
  display: flex;
  flex-direction: row;
}
.dl-shell {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  /* ← 缺 min-height: 0 */
}
.dl-body {
  display: flex;
  flex: 1;
  min-height: 0;
}
.dl-main {
  flex: 1;
  min-width: 0;
  overflow-x: hidden;     /* ← 只有横向，没有 overflow-y */
  display: flex;
  flex-direction: column;
}
```

**问题链**：

```
内容超一屏
  → .dl-main 无 overflow-y 约束，内容高度撑开
  → .dl-shell 被撑高（flex 子项默认 min-height:auto）
  → .dl-root 被撑高（min-height 只保证下限）
  → 整个文档出现滚动条
  → 侧栏（flex 首列）跟着文档流滚走          ← 问题 2
  → .dl-statusbar（position:fixed）压住内容尾部 ← 问题 3
  → 侧栏 .dl-side-foot 里的「设置」按钮滚出视口 ← 问题 4
```

**修复**：改为 pane 滚动（VS Code / qBittorrent 同款）。

```css
.dl-root {
  height: 100vh;
  height: 100dvh;
  overflow: hidden;
  display: flex;
  flex-direction: row;
}
.dl-shell {
  flex: 1;
  min-width: 0;
  min-height: 0;
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
  min-height: 0;
  overflow-y: auto;
  overflow-x: hidden;
  overscroll-behavior: contain;
  display: flex;
  flex-direction: column;
  padding: var(--space-4) var(--space-5) var(--space-5);
}
```

**注意**：`.dl-side-nav` 已有 `flex:1; min-height:0; overflow-y:auto`，
`.dl-side-foot` 已有 `flex:none` —— **侧栏自身是对的**，问题全在父容器。

---

## 🟡 P1-5：点击交互有延迟

**`ui/src/components/dl/DlApp.tsx:147`**

```typescript
setInterval(refresh, 1000);   // 每秒全量拉 /tasks + /stats

// refresh 内
if (t !== null) {
  setTasks(t);        // ← 每次都是新数组引用，即使数据完全相同
  setLoaded(true);
}
```

**每秒无条件 `setTasks(新引用)`** → React 认为状态变了 →
整个 `DlApp` 及全部子组件每秒重渲染一次。

**3000 任务时**：每秒重建 3000 个 React 元素 + DOM diff → 主线程占满 →
`onClick` 排到下一个空闲帧 → **点击滞后 100–300ms**。

**修复建议（三条一起做）**：

**A. 内容签名去重**

```typescript
const taskSigRef = useRef<string>("");
const applyTasks = useCallback((t: TaskListItem[]) => {
  let sig = "";
  for (const x of t) sig += `${x.task_id}\u0001${x.state}\u0001${x.name ?? ""}\u0002`;
  if (sig === taskSigRef.current) return;   // 无变化 → 不触发重渲染
  taskSigRef.current = sig;
  setTasks(t);
}, []);
```

**B. 行组件 `React.memo`**

`TasksTable` 的行目前接收整个 `snapshots` / `rates` 对象 →
任一任务速率变化会让**所有行**失效。应只传该行数据，并 `memo()` 包裹。

**C. 轮询降频**

PR #117 已接线 `Progress`/`Speed` 事件。列表轮询可降到 5s 兜底，
实时数据走 WS 推送；快照轮询仅在「表格可见 + 有进行中任务」时运行。

---

## 🟡 P1-6：设置页可配置项过少 + 无预设方案

### A. 后端支持但本 PR 未暴露

| 项 | 位置 |
|---|---|
| `scheduler.completion_action`（none/exit/shutdown/sleep/hibernate） | `settings.rs:120` |

**用户完全没法用「下载完成后关机」。**

### B. 用户要的「预制设置」（preset）

一键套用的配置组合，建议 6 个：

| Preset | 内容 |
|---|---|
| **省电 / 低负载** | 并发 1、限速 1MB/s、停止做种 |
| **极速下载** | 并发最大、不限速、全 P2P（DHT/PEX/LSD/μTP） |
| **隐身 / 最安全** | 加密 `require`、禁 DHT/PEX/LSD、强制代理 |
| **PT 站** | 私有种子优化、上传不限、比率 2.0、禁 DHT |
| **移动网络** | 限速、禁 P2P、小并发 |
| **夜间定时** | 备用限速时段 22:00–07:00 |

### C. 设置页结构建议

1. **二级分组导航**（带宽 / 连接 / BT / 下载 / 调度 / 通知 / 高级）
2. **每项显示「当前值 / 默认值」+ 一键恢复默认**
3. **搜索框**（设置项多了必须有）
4. **Preset 卡片区**（顶部一排）
5. **导出 / 导入 JSON**（备份与团队共享）
6. **危险区**：重置全部设置（二次确认）

---

## 说明

我在本地试过改 P0-1/P0-2/P1-5（globals.css + SettingsView + DlApp），
但**实测效果不佳**，已全部 `git checkout` 还原，保持干净基线。

**我的修复没生效的可能原因**：
- 布局可能还有第二处覆盖（某视图自身有 `overflow`）
- 点击延迟主要来自 `TasksTable` 行未 memo，我只做了列表签名去重

**建议用真实浏览器 + React DevTools Profiler 实测定位**，而不是只看代码。
另外 `react-scan` / Lighthouse 的 INP 指标可以直接量化点击延迟。

---

## 验收标准

| # | 项 | 标准 |
|---|---|---|
| 1 | 主题不被覆盖 | 系统浅色 → 打开设置 → 仍浅色，localStorage 不被改写 |
| 2 | 侧栏固定 | 造 100 个任务，滚到底，侧栏位置不变 |
| 3 | 无遮挡 | 内容最后一行能滚到状态栏上方 |
| 4 | 设置按钮常驻 | 任何滚动位置可见可点 |
| 5 | 点击无延迟 | Profiler 单次点击 < 16ms；1000 任务无掉帧 |
| 6 | 设置完善 | 补 `completion_action` + ≥4 个 preset + 搜索 + 恢复默认 |
