# Soak Test 使用指南（#168）

真实场景长跑测试：补上「mock 覆盖不到」的最后一层验证。

## 组件

| 文件 | 用途 | 时长 |
|---|---|---|
| `soak-http.sh [分钟=60] [并发=8] [--local]` | HTTP 引擎长跑：波次下载 + SHA256 双重校验 + RSS 趋势回归 + FD 泄漏检测 | ≥ 60min |
| `soak-failure-inject.sh [kill9\|netdown\|diskfull\|unreachable\|all]` | 故障注入：kill -9 / 断网（短+长）/ 磁盘写满 / 不可达——每个场景断言「恢复后文件完整」 | 每场景 1-6min |
| `soak-bt.sh [分钟=30]` | BT 长跑（需 LT 环境）：官方种子 + kill -9 fastresume + 做种执法 | ≥ 30min |
| `soak-file-server.py [端口=9903]` | 本地确定性内容源（`i%251` 模式，SHA256 可离线预计算，Range + 节流） | - |
| `soak-rss-check.py <csv> <时长>` | RSS 线性回归：投影增长 >20% 且 >30MB 判泄漏（纯 stdlib） | - |

## 快速开始

```bash
cargo build --release -p smart-dl-daemon          # release 构建（soak 用）
./scripts/soak/soak-failure-inject.sh all          # 15 分钟内跑完全部故障注入
./scripts/soak/soak-http.sh 60 8                   # 60 分钟公网长跑
./scripts/soak/soak-http.sh 60 8 --local           # 无外网环境用本地确定性源
```

## 设计要点（对应 #168 四原则）

1. **断言「最终文件正确」，不只是「任务完成」**——daemon 侧 E6 `sha256`
   请求参数（任务完成时引擎内校验，不符即 Failed）+ 脚本侧独立
   `sha256sum` 比对参考值，双保险防「完成但静默损坏」。
2. **采样趋势，不只看终值**——RSS 用线性回归投影全程增长（<20%），
   FD 用波间空闲基线对比（+15 容差），不是绝对值判据。
3. **故障注入后必须验证「恢复」**——kill -9 后账本合法/进度不回退/
   续传后 SHA256 正确；断网恢复后文件完整；写失败解除后能续传。
4. **不进 PR CI**——nightly 由 `.github/workflows/soak.yml` 调度
   （每日 UTC 18:00 + 手动触发），产物（采样 CSV / daemon 日志）上传
   artifact 供事后分析。

## 已实测确认的契约（写脚本时踩出来的）

| 契约 | 实测依据 |
|---|---|
| HTTP 查重按 URL（D34 canonical，剥 token 参数），与 dest 无关 | 跨波复用 URL → DuplicateRejected；soak 每波 DELETE 释放 canonical |
| 不可达 URL 在**加任务时**同步探测、快速拒绝（连接超时 10s 内），无任务产生 | `unreachable` 场景 A |
| 下载中途失联：读超时 30s + 段级重试 4 次（退避 1-8s）→ 恢复则透明续传；永久失联 → Failed（`all mirrors failed for segment`） | `netdown` / `unreachable` 场景 B |
| kill -9 后 tasks.json 原子性保持；重启后 HTTP 任务自动恢复下载、进度不回退 | `kill9` 场景 |
| HTTP 引擎默认不发 User-Agent → thinkbroadband 等公网源 403；已修（`smart-dl/<ver>` 产品 UA，httpdl 单源常量） | soak 开发实测；见 PR 说明 |
| 公网免费测速端点会 429 限流高频请求（Cloudflare 4 分钟 21 波触发）——真实模式默认大文件 + 波间隔 15s | soak 开发实测 |
| 单文件 HTTP 任务的快照 `files[]` 为空（引擎不透出），终文件 = dest 下唯一非 `.part` 文件 | verify 逻辑 |

## 环境变量

| 变量 | 默认 | 说明 |
|---|---|---|
| `SOAK_DAEMON` | `target/release/smart-dl-daemon` | daemon 二进制路径 |
| `SOAK_SAMPLE_SEC` | 60 | 采样间隔（秒） |
| `SOAK_MAX_FAILED` | 0 | 允许的失败任务数上限（nightly 对公网 429 容忍用） |
| `SOAK_WAVE_TIMEOUT_SEC` | 900 | 单波等待上限 |
| `SOAK_WAVE_GAP_SEC` | 15 | 波间隔（对公网源礼貌） |
| `SOAK_KEEP` | 0 | 1 = 保留工作目录取证 |
| `SOAK_UNREACHABLE_BUDGET_SEC` | 300 | 永久失联达 Failed 的预算 |
| `SOAK_TORRENT_URL` / `SOAK_SUMS_URL` | Debian netinst | BT soak 的种子与校验和地址 |

## 手工验收清单

见 `docs/soak/ACCEPTANCE.md`（桌面端 24h、热拔插、休眠唤醒等自动化
覆盖不了的场景）。
