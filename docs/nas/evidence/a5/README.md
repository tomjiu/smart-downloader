# A5 原始证据索引（2026-08-31）

| 文件 | 内容 |
|------|------|
| a5_baseline.json | docker 平台基线：PipeLimit=10→dump 256 复验、配额 403 实锤 |
| a5_syno.json | cnk3x 全套 env 无文件伪装 → panic（零值候选） |
| a5_syno-full.json | /etc+/usr+/var 三树 shadow + authenticate.cgi + PKGDEST → panic |
| a5_syno-envcfg.json | envconfig YAML 双落点注入 → panic（缺 ALLOW_CUSTOM_PLATFORM）；含 KEY=VALUE 格式 yaml panic 现场 |
| a5_syno-custom.json | +ALLOW_CUSTOM_PLATFORM=true → **干净启动 Platform:群晖**，API 全通 |
| a5_syno-final.json | 群晖 + PipeLimit=10→dump 256 组合验证；配额 403 |
| a5_cleanup.json | 无 unshare 无伪装仅 BinDir envconfig → 群晖照常启动（最小配方反证）；DELETE >75s 阻塞复现 |

引擎完整日志存于工作区 `~/.nas-engine-test/logs/engine_a5_*.log`（不入库，含大体积调试输出）。
