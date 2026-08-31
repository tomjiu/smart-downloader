# NasRemoteEngine — 迅雷 NAS 引擎远程托管适配层（附录 E 落地）

> 代码：`crates/btcore/src/nas_remote.rs`（feature `nas`）。
> 校准依据：本目录 `A2/A3/A4/A5_CALIBRATION_FINDINGS.md`（Task 31 实测链）。
> 状态：**v1 可用**（URL 任务创建/轮询/删除/超速申请）；`cargo check --features nas`
> 通过，纯逻辑单测 9/9 通过（`lt_kernel` 原生库缺失使 btcore 全量 test 需
> Windows 侧构建，与本模块无关——逻辑已用独立 crate 等价复刻验证）。

## 1. 架构定位

```
┌───────────────────────────── NAS 盒子 / 沙盒 / NAS 容器 ─────────────────────────────┐
│  ops 层（一次性）：                                                                    │
│    1. SPK 引擎落位（launcher + pan-cli，与官方 SPK 逐字节一致的 3.23.5 实测可用）        │
│    2. BinDir/envconfig (YAML):  PLATFORM: "群晖"                                      │
│                                 OS_VERSION: "geminilake dsm 7.2-64570"                │
│                                 ALLOW_CUSTOM_PLATFORM: "true"   ← 平台白名单旁路(A5)   │
│    3. 热启动：launcher -pid（凭据在 KV 中则免扫码；冷启动见 A2 注入链）                  │
└──────────────────────────────────────┬───────────────────────────────────────────────┘
                                       │ HTTP (DriveListen, 如 127.0.0.1:5050)
┌──────────────────────────────────────▼───────────────────────────────────────────────┐
│  NasRemoteEngine（本模块，Rust，全平台）                                               │
│   ensure_jwt: GET / → uiauth(value){return "eyJ…"} → pan-auth 头（A3 自举链）          │
│   add:        POST /drive/v1/task   （url 对象形载荷，A4 定案）                         │
│   status:     GET  /drive/v1/tasks?space=&filters={"id":{"in":"…"}} → 相位映射         │
│   remove:     DELETE /drive/v1/tasks?space=&task_ids=…（同步阻塞 >30s，超时 95s）       │
│   超速:       POST /device/v1/try_speed/apply（仅 RUNNING；usage 配额独立）             │
└──────────────────────────────────────────────────────────────────────────────────────┘
```

与既有 `XunleiBtEngine`（Windows-only FFI SDK，BT/P2SP）**互补不互斥**：
NAS 路线 = Linux/远程盒子上的迅雷官方下载内核（云加速/离线/超级会员加速）。

## 2. 引擎侧一次性配置（ops 备忘）

| 项 | 值 | 出处 |
|----|-----|------|
| 环境变量 | `DriveListen` / `LauncherListen` / `ConfigPath` / `DownloadPATH` / `HOME` | A2 |
| 平台伪装 | `BinDir/envconfig` YAML：`PLATFORM:"群晖"` + `ALLOW_CUSTOM_PLATFORM:"true"`（其余 SYNOPKG 串可选） | A5 |
| 下载并发 | env `DownloadPipeLimit/UploadPipeLimit=10` → 引擎内部换算 256（0 是异常值，触发 90120 嫌疑之一） | A4 |
| device_space | `device_id#<hex32>`，取自引擎 `bin/bin/info.file` 的 `device_id` | A2 |
| 坑位 | `envconfig` 必须 YAML（KEY=VALUE 触发 yaml panic）；`DriveLogLevel=debug` 冻结引擎；DELETE 长阻塞 | A4/A5 |

## 3. API 映射表（协议面 → trait 面）

| `DownloadEngine` 方法 | 迅雷本地 API | 备注 |
|------------------------|--------------|------|
| `add` | `POST /drive/v1/task` | `Http/Thunder` 已实弹；`Magnet` 静态定案与 URL 同构（A6_PREP §2，待实弹）；`TorrentFile` → 云端 file_id 型，本端不产（A6_PREP §3） |
| `status` | `GET /drive/v1/tasks?filters={"id":{"in":id}}` | `PHASE_TYPE_* → EngineState` 映射；`params.error`（如 `下载(90120)`）透出 |
| `remove` | `DELETE /drive/v1/tasks?task_ids=` | 引擎同步清理本地文件+远端同步，阻塞 >30s（超时 95s） |
| `pause/resume` | `PATCH /drive/v1/task`（`set_params.spec={"phase":"pause"/"running"}`，A6_PREP §4 静态定案；A2 的 404 系路由打错） | v1 `Unsupported`，v2 待实弹挂载 |
| `update_sources` | `POST /device/v1/try_speed/apply` | 仅 RUNNING 生效；body `{}`；403 配额/无任务 → `classify_error` |
| `peers/ban_peer/read_piece/add_url_seed` | — | v1 `Unsupported`/空 |

错误分类：`task_create_count_limit`（每日 3 次，失败也计数，北京时间 0 点重置）、
`permission_deny / invalid number of segments`（JWT 失效 → 自动重自举一次）、
其余透传（截断 160 字节）。

## 4. 验证记录（2026-08-31）

- `cargo check -p smart-dl-btcore --features nas`：通过（bindgen 走已提交
  bindings.rs 回退路径，无需 libclang）。
- 纯逻辑单测 9/9：`uiauth` 提取（含非 JWT 拒收）、相位映射全表、创建载荷黄金
  样本（对齐 A4 实测响应）、任务名推导、配额/鉴权错误分类、速度字段防御式解析、
  space URL 编码、config 归一化。
- 实弹联调：待 A6（每日创建配额北京时间 0 点重置后）——建任务 → RUNNING →
  `apply`（`usage.used` 0→1）→ 90120 终验。

## 5. v1.1 静态补充（2026-08-31，零额度）

引擎前端 bundle 静态校准完成（`A6_PREP_STATIC_CALIBRATION.md`）：magnet 与 URL
任务同构、pause/resume 真实路由为 `PATCH /drive/v1/task`、`device/btinfo`
端点、A6 验证矩阵 P1–P6 及额度预算（最省 1 次创建）。实弹探针
`scripts/nas/a6_probe.py`，工作区冷恢复 `scripts/nas/a6_ops.py`
（SPK 已归档 `research_bin/nas/spk/`，哈希见 `ASSET_MANIFEST.md`）。
