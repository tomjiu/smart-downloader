# Phase 2 主计划 — 登录原生 UX / 跨平台通解 / 能力吸收 / 主线增强

> 日期：2026-08-30
> 前置：Phase 1 已完成（见 docs/PROJECT_STATUS.md、docs/IMPLEMENTED.md）
> 执行：4 个并行子代理（5-a/5-b/5-c/5-d），主代理审核集成 + 全链路测试 + 打包交付

---

## 0. 本轮要回答的三个问题（来自用户）

| # | 问题 | 回答产出 |
|---|------|----------|
| Q1 | 迅雷登录原生：希望是和迅雷 App 一致的 OAuth 式登录页——点击直接跳转官方页面，或本地渲染与 App 一样的页面 | 5-b：三种原生登录模式（官方页跳转 / 本地 App 同款页面 / 终端二维码）+ 文档 NATIVE_LOGIN_GUIDE.md |
| Q2 | 迅雷跨平台能做到通解吗？整个迅雷的下载能力能完全取下来了吗？ | 5-c：CROSS_PLATFORM_UNIVERSAL_SOLUTION.md——分层通解架构 + 能力抽取矩阵（完全/部分/不可 取得三张清单）+ 各平台路线图 |
| Q3 | 比特彗星/夸克等分析文档、结果、分析代码都转化成可吸收的能力了吗？有哪些能力？ | 5-d：CAPABILITY_ABSORBED.md 吸收能力总清单 + 按 ROI 把高价值能力落地进主工作区代码 |

## 1. 任务分解与边界（防文件冲突）

| Task | 范围 | 拥有的文件（独占编辑权） |
|------|------|--------------------------|
| 5-a 主线修复增强 | Linux 编译修复（xunlei-ffi cfg 门控）、workspace 测试修复、ed2k 链接解析 | crates/xunlei-ffi/**、crates/core/src/source_parse/**（mod.rs 注册归 5-a）、crates/daemon/src/{main,serve,http}.rs 仅限 5-a 必要接线 |
| 5-b 迅雷原生登录 | DEVICE_CLIENT_ID 对齐、三种登录模式、离线下载 API、示例更新 | crates/provider/src/xunlei/**、crates/provider/examples/**、crates/provider/Cargo.toml、crates/daemon/src/cli.rs（新增 xunlei-login）、crates/daemon/Cargo.toml |
| 5-c 跨平台通解 | 纯研究文档（不写代码） | docs/research/xunlei/CROSS_PLATFORM_UNIVERSAL_SOLUTION.md |
| 5-d 能力吸收 | 吸收矩阵 + 夸克 Provider + 嗅探引擎 + BT 策略建议器 | docs/CAPABILITY_ABSORBED.md、crates/provider/src/quark/**、crates/core/src/sniffer.rs、crates/btcore/src/strategy*.rs、crates/core/src/lib.rs（sniffer 注册） |

共同约束：
- 冲突规避：docs/{PROJECT_STATUS,IMPLEMENTED,BACKLOG}.md 由主代理收口更新；worklog.md 只用 `cat >>` 追加。
- 测试：一律 mock / 本地服务器，禁止真实外网 API 调用与真实凭证。
- 合规：仅互操作性研究；不复制专有代码；不泄露凭证；迅雷私有 P2P 加速引擎维持 D28 排除决策。
- cargo：先 `source $HOME/.cargo/env`；并行构建遇 package lock 等待属正常。

## 2. 依赖与顺序

```
5-a(编译绿) ─┐
5-b(登录)    ├─ 全部并行 → 主代理集成（cargo check/test --workspace 全绿）
5-c(通解文档)┤              → 主代理收口三文档 → 打包 zip → 交付
5-d(吸收)   ─┘
```

## 3. 验收标准（Definition of Done）

1. `cargo check --workspace` Linux 通过（Windows-only 代码全部 cfg 门控）。
2. `cargo test --workspace` 全绿（默认 features；mock 测试离线可跑）。
3. 三种迅雷登录模式代码 + 测试 + 用户手册齐备；client_id 对齐 Xqp0…。
4. 夸克 Provider 可编译、mock 测试通过、与 provider trait 对接。
5. CAPABILITY_ABSORBED.md 覆盖 BitComet r1/r2、qBittorrent、FileCentipede、FlashGet、Tixati、夸克全部已分析能力并标注落地状态。
6. CROSS_PLATFORM_UNIVERSAL_SOLUTION.md 给出 Q2 的诚实完整回答。
7. 全部更新回 PROJECT_STATUS.md / IMPLEMENTED.md / BACKLOG.md。
8. 交付压缩包：smart-downloader-phase2-YYYYMMDD.zip（代码 + 文档，不含凭证/大二进制）。
