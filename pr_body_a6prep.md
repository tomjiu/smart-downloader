## A6 预备段（2026-08-31, 未实弹）——资产重获归档 + Magnet/暂停路由静态定案 + ops 冷恢复链

**背景**：沙盒重建事故后热态工作区全灭，本轮零额度消耗，三线预备 A6。A6 主体（90120 终验 / ERROR 任务清理 / v1 实弹联调）继续挂起，但已备至"额度恢复即战"状态。

| 线 | 定案 |
|----|------|
| **资产归档** | 官方源重获 `nasxunlei-DSM7-x86_64.spk`（**3.23.5-0814080017**，与 A2–A5 校准品逐字节同规格）入仓 `research_bin/nas/spk/`（27MB 直存）；SHA256 权威清单 `docs/nas/ASSET_MANIFEST.md`（pan-cli `fb1fe340…` / launcher `fb59f7b4…`）；package.tgz 实为 xz 流（新坑） |
| **Magnet 定案**（静态，bundle 偏移取证） | `POST /drive/v1/task` 的 magnet 任务与 HTTP **同构**（type=user#download-url + url 对象形）；URL 白名单正则 `/^(magnet|http[s]?|ftp|ed2k|emule):/i`；infohash 32/40 位自动包 magnet 前缀；BT 型（user#download）依赖云端 file_id，NAS UI 不产仅消费 |
| **pause/resume 推翻 A2** | 真实路由 = `PATCH /drive/v1/task`，body `{space,type,id,set_params:{spec:JSON.stringify({phase:"pause"/"running"/"signout"})}}`——A2 的 404 系路由打错；phase 值是 `pause` 非 `paused`（bundle 双实现 actionTask/operateTask + pauseTasks/resumeTasks 乐观镜像四重印证） |
| **新解锁端点** | `POST device/btinfo`（file_id→磁力解析）；`GET drive/v1/resource/list/{list_id}`；相位全表 + 平台常量（synology:PAN_CLI_PREVIEW）补全 |
| **ops 冷恢复链** | `scripts/nas/a6_ops.py`（extract/envconfig/status/snapshot 四命令，SHA256 校验落位实测通过）；`docs/nas/WORKSPACE.md` 持久化规约（L1-L5 分层 + 会话收尾五步 + 脱敏红线 + 配额日历） |
| **A6 实弹就绪** | `scripts/nas/a6_probe.py`（P1 magnet 建任务→轮询→P4 PATCH pause/resume→P5 try_speed 终验→P6 ERROR 清理一体，单进程引擎链）；额度预算：最省路径 **1 次创建配额**（P1 产物复用 P4/P5） |

产物：`docs/nas/A6_PREP_STATIC_CALIBRATION.md`、`ASSET_MANIFEST.md`、`WORKSPACE.md`、`scripts/nas/a6_probe.py`、`a6_ops.py`、`pr_update.py`、`research_bin/nas/spk/*.spk`。
