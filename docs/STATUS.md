# 项目状态（单一事实来源）

> **本文档是项目中「什么是真的」的唯一入口。**
> 其他 `docs/review/*.md` 是历史审查记录，**会过期**；冲突时以本文档为准。
>
> 最后更新：2026-10-21 · 核对基线 main @ `d6bb3ff` · 版本 **0.2.3**
>
> 数据口径：**所有数字均可复现**，命令见 §1.1「复现命令」；测试数取自
> CI（同命令同环境，避免本地漂移）。行数为 `wc -l` 口径（含空行）。
> ⚠️ Windows PowerShell `Measure-Object -Line` **不计空行**，同仓库会少
> ~5,700 行——跨平台核对请统一用 §1.1 的 git 命令。

---

## 1. 当前基线

| 项 | 值 |
|---|---|
| 版本 | **0.2.3**（workspace + desktop + tauri.conf 三处一致） |
| crates | 9 个：`core` `btcore` `httpdl` `provider` `daemon` `xunlei-ffi` `xunlei-convert` `rss` `storage`（#138 新增） |
| 源码 | `crates/` **83,762 行** Rust（含测试；其中 `src/` 28,709 行——旧值 57,942 系 glob 口径错误，#147 §二修正） |
| UI | `ui/src/` **20 文件 / 6,327 行** TS/TSX/CSS |
| 测试 | **192 套件 / 2,101 通过 / 0 失败**（CI ubuntu @ `ab254c6`，逐 crate 特性矩阵）+ **BT 集成 48 套件 / 523 通过**（libtorrent job） |
| 集成测试文件 | daemon 35 个、httpdl 34 个 |
| License | Apache-2.0 / MIT 双许可（#114 补） |

#### 1.1 复现命令（口径写死，任何人任何平台同结果）

```bash
# 行数（wc -l 含空行；git ls-files 保证与 checkout 平台无关）
git ls-files 'crates/**/*.rs' | xargs wc -l | tail -1        # crates 总行数
git ls-files 'crates/*/src/**/*.rs' | xargs wc -l | tail -1  # 仅 src/（不含 tests/）
git ls-files 'ui/src/**/*.ts' 'ui/src/**/*.tsx' 'ui/src/**/*.css' | xargs wc -l | tail -1

# 测试（与 CI ubuntu job 完全同口径；Windows 本地跑同命令因条件编译会少 ~10 例，属正常）
cargo test -p smart-dl-core
cargo test -p smart-dl-daemon
cargo test -p smart-dl-daemon --features "ftp,nas"
cargo test -p smart-dl-daemon --features sftp
cargo test -p smart-dl-httpdl --features ftp
cargo test -p smart-dl-httpdl --features sftp
cargo test -p smart-dl-provider
```

### 构建状态

| 目标 | 命令 | 状态 |
|---|---|---|
| 后端（无 BT） | `cargo build -p smart-dl-daemon --release --features ftp,sftp` | ✅ 通过 |
| 后端（含 BT） | 需 libtorrent 开发环境（vcpkg / `torrent-rasterbar.lib`） | ⚠️ 本机缺环境 |
| UI | `cd ui && bun run build` | ✅ 通过（First Load 128 KB） |

---

## 2. 能力清单（已可用）

### 传输引擎

| 引擎 | 状态 | 说明 |
|---|---|---|
| HTTP/HTTPS | ✅ | 分段多连接、断点续传（账本 + 双指纹）、镜像切换 |
| FTP/FTPS | ✅ | REST 续传、MDTM 指纹 |
| SFTP | ✅ | TOFU 主机密钥 |
| BitTorrent | ✅ | libtorrent 2.0.11 FFI；三重 fastresume |
| HLS / DASH | ✅ | 分段账本续传 |
| Metalink4 | ✅ | 主源 + 备源 |
| Provider 云兜底 | ✅ | 迅雷 + **夸克（#119 完成装配）** |

### 链接协议（`source_parse` 活链）

```
thunder://  qqdl://  flashget://  fs2ys://  ed2k://  magnet:?
ftp://  ftps://  sftp://  http(s)://  metalink  .torrent
```

> `flashget://` 是 #134 从已删死代码 `sniffer` 中**抢救接管**的，
> 见 `crates/core/src/source_parse/normalize.rs:79`。

### UI（#116 + #133 + #140 合并后）

| 项 | 状态 |
|---|---|
| 12 主题（nord/carbon/gruvbox/catppuccin/solarized/mono × 明暗） | ✅ 默认 nord-dark |
| 密集表格（7 列，行高 32px，含 ETA/大小） | ✅ |
| 虚拟滚动（1000 任务只渲染可视窗口） | ✅ |
| 设置页：9 域 + 搜索 + **4 个预设方案** + 每节恢复默认 + 重置全部 | ✅ |
| `scheduler.completion_action`（完成关机/休眠） | ✅ 已暴露 |
| 侧栏固定 + 状态栏 + pane 滚动 | ✅（#116 修 Issue #127） |
| Toast / 确认对话框 / 键盘快捷键（Ctrl+1..5、`/`） | ✅ |

---

## 3. 已修复的重大缺陷（本轮）

### 后端 P0（PR #114 + 后续）

| 缺陷 | 修复 |
|---|---|
| BT 零日：多文件 .torrent 偏移计算除零 panic | 延后到键真值读取之后 |
| `total_size` 溢出 | `checked_add` |
| 队列超卖 TOCTOU | 预留集 + 断言 |
| `pause_intents` 跨 FFI 竞态 | 独立锁 |
| 登录页零 Host/Origin 守卫 | 加 `local_origin_guard`（fail-closed） |
| loopback 前缀绕过（`127.*`） | `IpAddr::parse` 精确判定 |
| `cid_store` 二分边界错配 | 独立 `partition_point(off < i)` |
| **tasks/slot_reservations ABBA 死锁** | 统一锁序 `tasks → slot_reservations` |
| `remove_with` 静默失败（删任务仍在下载） | 先停引擎 → 传播错误 → 才删记录（#123） |
| **Basic auth 端到端坏死**（填了凭据仍 401） | `source.auth` → Authorization 头注入（#124 契约矩阵抓出） |
| RSS 并发丢失更新 | 锁内重新快照（#125） |
| fsync 每段开销 | 批量落盘策略（#128） |
| 乱序账本被拒 → 全量重下 | 账本合并策略（#128） |

### 能力接线（5 项「写了没接」全部完成）

| # | 能力 | PR |
|---|---|---|
| 1 | `should_retry` / `Backoff` 段级重试 | ✅ #122 |
| 2 | `Progress`/`Speed` 事件发布 | ✅ #117 |
| 3 | `QuarkProvider` 装配 | ✅ #119 |
| 4 | `sniffer` | ✅ #134（不接线，抢救 `flashget://`） |
| 5 | `web_seeds` | ✅ #118 |

### UI 实测问题（用户报告 → Issue #127 → #116 全修）

| # | 现象 | 修复 |
|---|---|---|
| 1 | 点「设置」后界面自动变暗 | 初始值读 DOM 实际 `data-theme` |
| 2/3/4 | 侧栏不固定 / 内容重叠 / 设置按钮消失 | pane 滚动（`height:100dvh` + `overflow:hidden` 外壳） |
| 5 | 点击延迟 | 任务签名去重 + 虚拟滚动 |
| 6 | 设置项过少 | 4 preset + 搜索 + 恢复默认 + `completion_action` |

---

## 4. 已知限制（不是 bug，是明确边界）

| 项 | 说明 |
|---|---|
| **DASH 仅视频轨** | `dash.rs:9` 明文记录，不含音频轨合流 |
| **Windows 空洞检测无作用** | `verify_nonempty_blocks` 依赖 `md.blocks()`，Windows 恒返回 Ok |
| **HTTP 未校验 `Content-Range /TOTAL`** | 分段响应只有起始位置校验 |
| **未发送 `If-Range`** | 强 ETag 已在账本里，但续传请求未带条件头 |
| **`bt.enabled` 默认行为** | 编译未带 `--features bt` 时配置文件启用会告警降级 |
| **HLS/DASH 指纹对临时 token 敏感** | 带签名 URL 的 m3u8 每次刷新 fingerprint 变化 → 视为新源 |

---

## 5. 待处理（open PR / issue）

### 2026-10-21 批次合并记录（响应 #143，全部已合）

| PR | 内容 | 备注 |
|---|---|---|
| #144 | STATUS.md 单一事实来源 + 文档一致性 | 本文件 |
| #145 | 契约矩阵 15 → **24 条**（超额完成 #143 要求的 23） | 边界+正例成对覆盖 |
| #146 | 快照引用比较补全 rates/limits/累计 | #147 §一勘误描述 + 渲染门补修 |
| #129 | Playwright e2e 5 条关键路径 | 真实 daemon 实跑 5/5 |
| #137 | BtEngine 适配器 → btcore | rebase 后合入 |
| #138 | TD-3 I/O 下沉 storage crate | crates 8 → 9 |

### Issue 未关闭

| # | 内容 | 状态 |
|---|---|---|
| 143 | 合并后检查四部分 | §一~§四 全部落地；余「main 分支保护」待 repo admin（tomjiu） |
| 147 | 合并结果核验 3 项修正 | 本 PR 处理（描述勘误 / 统计口径 / e2e 解耦） |

---

## 6. 下一步开发建议（按投入产出比）

### 第一梯队：收尾「验证闭环」（约 3 天）

| 任务 | 理由 |
|---|---|
| **ts-rs 生成 TS 类型**（REALTEST-PLAN §A-1 方案 1，#147 §六再次点名） | **根治字段名不一致**：编译期报错而非运行时静默。当前 UI 类型手写，`max_download_kb_s` vs `down_kb_s` 的错误无法被发现 |
| ~~补齐契约矩阵 8 条~~ | ✅ #145 完成 24 条（超额）；随 `cargo test -p smart-dl-daemon` 在 CI 常驻，回退即红 |

### 第二梯队：UI 功能补全（约 1 周）

**按用户价值排序：**

| 功能 | 后端路由 | 说明 |
|---|---|---|
| **种源管理** | `/trackers`(3) `/peers` `/webseeds` `/super-seeding` `/recheck` `/announce` | BT 用户 80% 排障动作在这；**tracker 管理是 RSS/PT 刚需** |
| **新建任务表单扩展** | `/tasks` 全字段 | **现在只能贴裸 URL**；`.torrent`/metalink/headers/auth/sha256/定时全缺 |
| **文件选择优先级** | `/files/priority` | 大种只下需要的文件 |
| **批量操作** | `/tasks/batch` | 多选 + 批量暂停/恢复/删除 |
| **队列优先级** | `/tasks/:id/priority` | 上移/置顶/置底 |
| IP 封禁页 | `/security/bans`(4) | PT 用户 |
| 云兜底手动触发 | `/tasks/:id/fallback` + `/providers` | |

### 第三梯队：架构重构收尾（约 2 周）

`ARCHITECTURE-REVIEW.md` §6 的 10 项建议，已完成 6 项：

| # | 建议 | 状态 |
|---|---|---|
| 1 | `common_defaults` 宏 → `Default` 契约 | ✅ #114 |
| 2 | 幽灵依赖 + 死代码治理 | ✅ #134（-1,080 行） |
| 3 | LICENSE 双许可落地 | ✅ #114 |
| 4 | 未接线模块处置 | ✅ 全 5 项 |
| 6 | TD-3 I/O 下沉 storage crate | ✅ #138 已合 |
| 8 | BtEngine → btcore | ✅ #137 已合 |
| 9 | RSS 独立 crate | ✅ #132 |
| 5 | **拆分 `DownloadEngine` trait**（40 方法 → EngineCore 13 + BtEngineOps + HttpEngineOps） | ❌ #131 仅提案 |
| 7 | `core/strategy.rs` 迁回 btcore | ❌ 未做 |
| 10 | **`state/ops.rs` 按域拆分**（4,534 行 → 5 个 ~900 行） | ❌ 未做 |

**建议顺序**：#10（纯移动，零风险）→ #7 → #5（影响面最大，最后做）。

### 第四梯队：长期

| 项 | 说明 |
|---|---|
| **BT v2（BEP 52）** | 需改 FFI 契约：infohash 从 40-char hex `char*` 改二进制。**建议单独立项** |
| 浏览器扩展 | 截获下载请求 |
| WebDAV / S3 目标 | 下载完成后上传 |
| 分类目录规则 | 按扩展名/metadata 自动归类 |
| 在线 tracker 订阅列表 | 当前只有静态 `extra_trackers` |

---

## 7. 工程流程改进（本轮暴露）

### 🔴 合并后必须跑全量测试

**本轮实际发生：** 连续 3 个 merge commit（`b0b0b3e`、`2939bca`、`b609522`）的 main CI **全部红**，
无人处理。根因是 #125 与 #132 的**语义冲突**：

```
#125 引入 state_tests/rss_persist_tests.rs，用 crate::rss::RssFeed
#132 把 RssFeed 移到 smart-dl-rss，daemon 侧改成私有 use
→ 两个 PR 各自 CI 绿，合并后 lib test 编译失败
```

Git 无文本冲突（不同文件），但代码层面断裂。

**已修：** `5dfd654`（改为 `pub(crate) use`）

**流程建议：**

1. **合并队列串行化** —— 不要连续合并多个大 PR 而不看 CI
2. **main 分支 CI 失败应阻断后续合并**（GitHub 分支保护规则）
3. **跨 crate 重构 PR 与功能 PR 分开批次合并**

### 🟡 契约测试应用「UI 的请求体」

`contract_matrix.rs` 确实是这么做的（注释标 `抄自 ui/src/lib/daemon/store.ts:96`），
**并抓出了 Basic auth 端到端坏死**，证明方法有效。

**建议推广**：所有与 UI 对接的端点测试都从 UI 源码抄字面量。

---

## 8. 文档地图

**入口文档只有本文档。** 以下为历史/专题参考：

| 文档 | 用途 | 时效性 |
|---|---|---|
| `docs/review/README.md` | 审查文档索引 | 长期 |
| `docs/review/ARCHITECTURE-REVIEW.md` | 结构分析 + §6 重构建议 | ⚠️ §6.5 已修正初版误判 |
| `docs/review/REALTEST-PLAN.md` | 实机测试方案 | 计划已大部分落地 |
| `docs/review/CAPABILITY-GAPS-FINAL.md` | 续传能力评级 | 长期 |
| `docs/design/UI-DESIGN-SPEC.md` | UI 设计规范（657 行） | ✅ #116 已对齐 |
| `docs/design/UI-REWRITE-PLAN.md` | 重构实施方案 | ✅ 已执行 |
| `docs/review/PR114-*.md` | PR #114 审查系列（7 份） | 归档 |
| `docs/research/xunlei/` | 迅雷逆向研究 | 长期 |
| `repro/p0repro/` | 24 个独立复现工程 | 可执行资产 |

**已过期，勿引用：**

| 文档 | 原因 |
|---|---|
| `docs/review/UI-STATUS-ASSESSMENT.md` | 描述 #116 合并**前**状态，现状完全不同 |
| `docs/review/UNWIRED-CAPABILITIES.md` | 5 项已全部处置 |
