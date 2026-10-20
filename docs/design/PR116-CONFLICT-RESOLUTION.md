## PR 冲突解法：`TasksView.tsx` 请保留删除（我的修改是冗余的）

### 现状

`mergeState: DIRTY` —— 冲突文件里 `ui/src/components/dl/TasksView.tsx` 是 **delete/modify** 冲突：

| 侧 | 状态 |
|---|---|
| `main`（我 6ea4778 提交） | 修改了 `TasksView.tsx`（加 `displayName()` 兜底） |
| 本 PR | **删除** `TasksView.tsx`（被 `TasksTable.tsx` 替代） |

### 结论：照本 PR 的删除走，不要保留我的修改

我核对了本 PR 的 `TasksTable.tsx`：

```
L2   /// 密集任务表格（Issue #115 Phase 1 核心，替代 TasksView 卡片网格）
L8   /// - P0-2 name 可空兜底 displayName（搜索与渲染共用，绝不含 null）
L22  export function displayName(t: { name?: string | null; source?: string | null; task_id?: string }): string
L23      return t.name?.trim() || deriveNameFromSource(t.source) || t.task_id || "";
L283 {displayName(t)}                                          ← 渲染
L353 return displayName(t).toLocaleLowerCase("zh-Hans-CN");    ← 搜索
```

**本 PR 已经完整、正确地解决了 P0-2**，且实现比我那个更完善
（多了 `deriveNameFromSource` 处理 URL 末段、`toLocaleLowerCase` 中文友好）。

`git grep TasksView pr116-check -- ui/` 只剩 `TasksTable.tsx:2` 的一句注释，
**没有任何代码引用** —— 删除是安全的。

### 我的那个修复为什么存在

`main` 上 UI **从未成功构建过**。我第一次跑 `bun run build` 时直接失败：

```
./src/components/dl/TasksView.tsx:60:49
Type error: 't.name' is possibly 'null' or 'undefined'.
```

原因是 PR #124 已把 `types.ts` 的 `name` 改为 `string | null`
（对齐后端 `Option<String>`），但旧 `TasksView.tsx` 还是 `t.name.toLowerCase()`。

为了能构建出 UI 做实测（就是你后来看到的 6 个问题），我在 main 上打了最小补丁。

**本 PR 删除该文件后，这个补丁自然失效且不再需要。**

### 建议操作

```bash
# 冲突时选「删除」
git rm ui/src/components/dl/TasksView.tsx
```

其余三个冲突文件（`globals.css` / `DlApp.tsx` / `types.ts`）
**请以本 PR 版本为准**——它们是本 PR 的核心产物，main 上我没有改过。

---

## 补充：实测反馈（见 #127 与另一条评论）

我在 0.2.3 构建版上跑真实浏览器发现 6 个问题，已逐一定位到本 PR 的代码行：

| # | 问题 | 位置 |
|---|---|---|
| 1 | 点「设置」后界面自动变暗 | `SettingsView.tsx:49` `useState(DEFAULT_THEME)` 硬编码 |
| 2/3/4 | 侧栏不固定 / 内容重叠 / 设置按钮消失 | `globals.css:53-78` `.dl-root { min-height: 100vh }` |
| 5 | 点击延迟 | `DlApp.tsx:147` 每秒 `setTasks(新引用)` |
| 6 | 设置项过少 + 无 preset | 缺 `completion_action`、无预设方案 |

**这些在本 PR 的 `c37fd7c` 里一个都没修**（我已验证 `head` 版本的对应代码）。

解完冲突后建议一并修 —— 前两项合计不到 25 行代码，却影响每一个用户。

详见：[#127](https://github.com/tomjiu/smart-downloader/issues/127) 与我在本 PR 的另一条评论
[#issuecomment-6287380672](https://github.com/tomjiu/smart-downloader/pull/116#issuecomment-6287380672)。
