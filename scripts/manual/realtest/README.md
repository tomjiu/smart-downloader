# realtest —— daemon 真实功能测试（手工实机脚本）

与 `ui/e2e/`（浏览器级）互补的 **API 级真实功能测试**：每个脚本单调用内
编排「起真实 daemon（debug 二进制）+ 本地源服务器 → 造多状态任务 →
断言 → 清理」，不依赖 mock 引擎，走真实下载路径。

来源：2026-10-26 真实功能测试战役（R4-R11，8 轮 90+ 断言），发现并修复
1 个真实缺陷（CleanupCfg 默认值，PR #175）+ 4 项契约观察（见各脚本头注）。

## 运行

```bash
cargo build -p smart-dl-daemon          # debug 即可
scripts/manual/realtest/rt-01-filter-stats.sh   # 单跑任意一条
```

- 全部脚本自带临时工作目录（daemon 状态文件隔离，防跨跑恢复）与退出清理
- daemon 端口各脚本固定且互不冲突（8820-8827 段），可连跑
- 断言失败非零退出，输出逐条 PASS/FAIL

## 套件清单

| 脚本 | 覆盖 | 关键断言 |
|---|---|---|
| `rt-01-filter-stats.sh` | #173 列表过滤 + /stats + /metrics | state 单/多值过滤、search 大小写不敏感（名+URL）、非法 state 400、by_state 聚合一致、limit/offset 分页 |
| `rt-02-global-limit.sh` | #164 全局限速总阀门 + /providers | 行为学口径（对照组 ~14s 完成 vs 限制 25s 不完成）、/config 回显、放宽恢复、MockProvider 注册 |
| `rt-03-rss-e2e.sh` | RSS 端到端 | 订阅解析（item_count）→ 规则匹配/排除 → 自动建任务 → 真实下载完成 → 消费幂等 → guid 兜底解析 |
| `rt-04-retry-backoff.sh` | E30 失败自动重试 | auto_retry 语义（=重试次数）、事件铁证（「第 1/1 次…2s 后执行」）、复活续传 64MB、404 命中计数 |
| `rt-05-webhook-move-hook.sh` | E17/E27 完成回调 | webhook POST 载荷、move_to 落位 + 同名冲突改名 `(1)`、hook SD_* env、post_move 事件 |
| `rt-06-cleanup-settings.sh` | E20 清扫设置面 | PUT /settings 热更立即生效 + config.toml 持久化回写、保留期内不清扫 |
| `rt-07-queue-quota.sh` | S1 并发队列配额 | max_active_http=2 门控（6 任务恰 2 传）、暂停 FIFO 递补、配额释放递补 |
| `rt-08-events-channels.sh` | 事件三通道 | SSE data 帧、REST 补拉 type 过滤（单/多值/400）、WS 建连 + 帧推送 |
| `rt-09-bt-multifile-priority.sh` | BT 多文件子文件优先级（需 LT 环境 + bt 构建） | 创建时 file_priorities 暂停窗口应用（readback [4,0,4]/total 排除/SHA256/三 400/set 意图合并）——**#178 修复的 e2e 实证** |
| `rt-10-bt-restart-replay.sh` | BT 优先级跨 kill -9 重启重放（需 LT 环境 + bt 构建） | kill -9 → 重启 → 续传完成 → skip 跨重启存活（#178 缺陷①闭环） |
| `rt-11-bt-sequential.sh` | BT 顺序下载（需 LT 环境 + bt 构建） | sequential=true 建任务完成 + 双文件 SHA256 + 运行中端点幂等切换 |
| `rt-12-bt-webseed.sh` | BT web seed BEP-19 P2SP（需 LT 环境 + bt 构建） | 零 peer 死 tracker 纯 HTTP 源完成下载 + SHA256 一致 + added 回执 |
| `rt-13-bt-magnet-public.sh` | BT 磁力链接公网 swarm 抓取（需 LT 环境 + bt 构建 + 公网） | Debian 官方 swarm magnet → 元数据（infohash bencode 交叉校验/total/files/save_to 同 hash）+ 抓取产物建任务真实下载（磁盘余量不足自动 SKIP 下载段） |

## 组件服务器（本地、确定性）

- `rss-server.mjs`：RSS 2.0 feed（link=下载目标，torrent-feed 惯例；含 guid 兜底条目）
- `revive-server.mjs`：三态源（活→死→marker 复活），重试场景专用
- `webhook-server.mjs`：webhook POST 记录器 + 同名文件源（冲突改名路径）
- 复用 `ui/e2e/file-server.mjs`（确定性内容 + Range + 按路径限流）

## 已知契约观察（不判失败，供决策）

1. **rss crate v1 不读 enclosure**：条目 URL 取 link → link href → http 形态
   guid（对齐 torrent feed 惯例）。播客型 feed（媒体在 enclosure）的附件被
   忽略——可作 v2 增强（enclosure 优先或可配）
2. **重试窗口状态抖动**：失败→重试排定的间隙，任务短暂显示终态 Failed
   再回 Queued（事件流正确；轮询方可能看到 Failed→Completed 跳变）
3. **`GET /events` 过滤参数名为 `type`**（serde rename）：拼成 `event_type`
   会被静默忽略（serde 标准行为，但易踩）
4. **add 路径同步探测源站**：死源 URL 直接 400 拒绝（auto_retry 无从触发；
   重试只覆盖「添加时活、下载中死」场景——符合语义，文档化于 rt-04 头注）
5. **BT 任务级 dest 不支持**（v1 全局落盘语义）：显式 dest ≠ 全局目录 →
   engine error（文档口径；rt-09/10/11 均用全局目录）
6. **peer 表不跨 daemon 重启持久**：hermetic 测试（无 tracker）重启后须
   重注入；真实场景由 tracker/PEX 重发现兜底（rt-10 头注）
7. **纯做种者 + 有限 max_share_ratio = 立即暂停**（batch6-P2「qbit 同口径」：
   downloaded==0 && uploaded>0 → ratio=∞ ≥ 任意阈值）——导入完整数据做种
   的场景配 ratio limit 会被秒停；rt 系列配置一律不设 max_share_ratio
8. **优雅停机后做种任务重启不自动恢复**（Paused + engine_tid=None）
