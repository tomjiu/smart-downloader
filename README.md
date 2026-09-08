# smart-downloader

Rust 多引擎下载器：HTTP(S) / FTP / BitTorrent（libtorrent 基座）统一调度，axum daemon 提供 REST API + WebSocket 事件流，内置任务队列、断点续传、限速、代理与备用源兜底。

> 当前状态：主线能力已封版（HTTP 动态分段、BT fastresume、任务级限速/文件优先级、运维 API 三件套、CI 双平台矩阵）；常规能力增强线 E1–E33 已全部合并（2026-09-02 ~ 09-04：任务管理面/事件三通道/速率全链路/重试体系/定时错峰/完成 Webhook 与钩子/BT tracker 运行时等，逐批档案见 [`docs/IMPLEMENTED.md`](docs/IMPLEMENTED.md) #21）。整体路线图与未实现清单见 [`docs/BACKLOG.md`](docs/BACKLOG.md)。

## 能力总览

| 引擎 | 能力 |
|------|------|
| **HTTP/HTTPS**（`smart-dl-httpdl`） | 多连接 Range 并行、动态分段（SegmentManager 动态领取 + 流式写盘）、段账本断点续传（`<part>.progress` 记录已完成段，跨重启恢复；**ETag + Last-Modified 双指纹**确认服务器文件未变，任一变化/账本非法即作废重下）、真实进度与真暂停（段边界生效）、镜像与换源、`backup_url`/`backup_md5` 备用源兜底、**多源并行**（双源强 ETag 相等 + Range/总长一致才启用跨源分段混拼，worker 轮转分摊；严于 aria2 的无条件多源）、失败缩小粒度重试、429 退避、跨段共享限速、sha256/sha1/md5 可选校验（互斥）、文件冲突策略（overwrite/rename/skip）、顺序下载（在飞窗口收紧，边下边播） |
| **FTP**（httpdl `ftp` feature） | 单文件 + 目录递归下载、断点续传、生命周期管理 |
| **BitTorrent**（`smart-dl-btcore`） | libtorrent FFI 薄核：magnet / .torrent 建任务、元数据抓取（`POST /bt/metadata`）、fastresume 持久化与恢复、web seed 注入（P2SP）、任务级限速（双向）、子文件优先级（持久化 + 恢复重放）、校验/做种停止、DHT/LSD/UPnP 开关、tracker 运行时增删查、magnet 任务名回填、累计上传/下载/分享率透出（all_time 计数） |
| **daemon**（`smart-dl-daemon`） | 任务生命周期（add/pause/resume/remove/list/status/logs；终态 Failed 可 resume 手动重试）、并发队列（BT≤3 / HTTP·FTP≤8）、事件三通道（WS 双向 / REST 补拉 / SSE 尾随）、配置热重载、Provider fallback 兜底、运维 API（`/stats` `/version` `/health` `/metrics`）、全局代理 + 双引擎限速（运行中热改）、失败自动重试（指数退避）、定时启动 + 错峰、完成 Webhook + 完成后钩子、已完成任务自动清扫 |
| **链接解析**（`smart-dl-core`） | `thunder://`、`qqdl://`、`fs2you://`、magnet URI（v1/v2 识别）、ed2k 链接（name/size/md4 结构化）、迅雷网盘分享链接 |

## Workspace 结构

```
crates/
├── core/            # 下载引擎 trait、类型、链接解析（source_parse）、任务状态机
├── httpdl/          # HTTP/FTP 下载引擎（分段、限速、续传、校验）
├── btcore/          # libtorrent FFI 绑定 + BT 引擎（feature: bt）
├── daemon/          # axum 服务 + CLI（smart-dl-daemon / smart-dl 客户端二合一）
├── provider/        # 下载源 Provider 抽象（fallback 调度、探活、冷却降级）
├── xunlei-ffi/      # 迅雷 SDK FFI（Windows-only，可选）
└── xunlei-convert/  # 迅雷任务迁移（xlbt.cfg + .bt.xltd + .torrent → fastresume）

ffi/                 # C++ FFI 内核（lt.h + lt_kernel.cpp，CMake + vcpkg）
tests/integration/   # 跨 crate 集成测试
scripts/ci/          # CI native 环境脚本（bt-linux-setup.sh）
docs/                # 全部设计与验收文档（见下方文档地图）
tools/ scripts/research/ docs/research/   # 研究资产（见「仓库说明」）
spike/cxx/           # 临时 C++ 对比 spike（独立构建，排除出 workspace）
research_bin/        # 各平台预编译产物
```

## 构建与测试

### 纯 Rust 基线（无 BT）

```bash
cargo test --workspace --exclude smart-dl-btcore
```

### 启用 BT（Linux，无需 root）

BT 需要链接 libtorrent（FFI 内核按 2.1 API 编写，2.0.x 由源内 `#if LIBTORRENT_VERSION_NUM >= 20100` 守卫兼容）。一键脚本完成 native 环境：头文件/动态库（rootful apt 或 `--no-root` 本地前缀）、FFI 内核静态库、e2e seeder、vcpkg 契约仿真：

```bash
scripts/ci/bt-linux-setup.sh --no-root "$HOME/.local/smart-dl-native"

export LT_KERNEL_LIB_DIR="$HOME/.local/smart-dl-native/lib"
export LT_VCPKG_LIB_DIR="$HOME/.local/smart-dl-native/fakevcpkg"
export SEED_MAIN="$HOME/.local/smart-dl-native/lib/seed_main"
export LD_LIBRARY_PATH="$HOME/.local/smart-dl-native/prefix/usr/lib/x86_64-linux-gnu:$HOME/.local/smart-dl-native/fakevcpkg"

cargo test -p smart-dl-daemon --features bt
```

### 启用 BT（Windows / vcpkg）

```powershell
scripts/m0/01_vcpkg.ps1     # vcpkg 安装 libtorrent/boost/openssl
scripts/m0/02_build.ps1     # 构建 ffi 内核 + workspace
```

### Feature 矩阵

| feature | 说明 |
|---------|------|
| `bt` | libtorrent 薄核（`smart-dl-btcore`），默认关 |
| `ftp` | FTP 引擎（httpdl） |
| `webseed` | P2SP web seed 注入端点（隐含 bt） |
| `xunlei-import` | 迅雷任务导入（隐含 bt） |
| `nas` | NAS 版迅雷引擎托管（Linux-only） |

### CLI 速查

```bash
smart-dl-daemon serve                       # 启动 daemon（默认 127.0.0.1:8787）
smart-dl add <URL> [--dest DIR]             # 建任务（HTTP/FTP/magnet/.torrent）
smart-dl list / status <id> / logs <id>     # 查询
smart-dl pause|resume|remove|fallback <id>  # 生命周期
smart-dl-daemon xunlei-login [--qr|--browser]  # 迅雷登录（可选）
smart-dl import-xunlei <xlbt.cfg ...>       # 迅雷任务导入（可选）
```

## REST API 速查

默认 `http://127.0.0.1:8787`；配置 token 后所有端点要求 `Authorization: Bearer <token>`（fail-closed，无例外路径）。

| 端点 | 方法 | 说明 |
|------|------|------|
| `/tasks` | GET / POST | 任务列表（`?state=`/`?engine=` 过滤、`?search=` 关键字、`?tag=` 标签 any-of、`?limit`/`?offset` 分页 + `X-Total-Count`）/ 建任务（HTTP、FTP 目录、magnet、.torrent；能力字段：任务级代理/自定义 headers/Basic 凭据/顺序下载/trackers/限速/校验 sha256·sha1·md5/`backup_url`+`backup_md5`/显式 `name`/`conflict_policy`/`start_at_unix` 定时/`auto_retry` 自动重试预算） |
| `/tasks/batch` | POST | 批量操作：显式 `ids`（≤100，pause/resume/remove）或条件 `select`（state/engine/tag/search 任一，仅 pause/resume 非破坏动作） |
| `/tasks/:id` | GET / DELETE | 快照（files 明细 + 实时速率 + 累计流量/分享率 + 标签 + 顺序下载标志）/ 删除（`?delete_data=true` 引擎侧同删已下载数据） |
| `/tasks/:id/pause` · `/resume` | POST | 生命周期（定时等待中任务：pause = 取消自动启动，resume = 立即激活；终态 Failed 任务 resume = 手动重试重新接入引擎） |
| `/tasks/:id/name` | POST | 任务重命名（显示层；`{"name": null}` 清除回退派生链） |
| `/tasks/:id/tags` | POST | 任务标签设置（替换式；trim/去重，≤16 个×64 字符；null/空表清除） |
| `/tasks/:id/logs` | GET | 任务日志 |
| `/tasks/:id/proxy` | POST | 任务级代理运行中热改（仅 HTTP；epoch 重入 + 段账本续传，换代理不断传） |
| `/tasks/:id/sequential` | POST | 顺序下载切换（边下边播：HTTP 在飞窗口收紧；BT sequential flag；FTP 不支持 409） |
| `/tasks/:id/trackers` | GET / POST / DELETE | BT tracker 运行时管理（列举 announce 表 / 批量追加 / URL 精确删除，无匹配 404） |
| `/tasks/:id/limit` | POST | 任务级限速（BT 双向 / HTTP 下载向） |
| `/config/limit` | POST | 全局限速总阀门热改（合计下行 + BT 上行；缺省字段 = 沿用当前值，双缺省 = 查询） |
| `/tasks/:id/files/priority` | POST | BT 子文件优先级（持久化 + 恢复重放） |
| `/tasks/:id/webseeds` | POST | web seed 注入（P2SP，feature webseed） |
| `/tasks/:id/fallback` | POST | 手动 Provider 兜底（BT→直链→HTTP） |
| `/bt/metadata` | POST | magnet 元数据抓取（摘要 + torrent_b64） |
| `/config` | GET | 当前配置（限速两键随运行中热改同步） |
| `/stats` · `/version` · `/health` | GET | 运维三件套 |
| `/metrics` | GET | Prometheus 指标（text/plain；任务按状态/引擎计数 + 聚合速率） |
| `/providers` | GET | Provider 状态 |
| `/probe` | POST | 探测预览：不建任务取 URL 元数据（大小/服务端文件名/Range/ETag/Last-Modified/Content-Type + `suggest_name`；headers/代理/Basic 凭据同 add 语义；v1 仅 HTTP 源） |
| `/events` | GET | 事件历史查询（seq 游标分页 + task_id/type 过滤 + 缺口报警） |
| `/events/stream` | GET | 事件流（SSE：历史重放 + 活流尾随，Last-Event-ID 断线续传） |
| `/ws` | GET | 事件流（WebSocket，背压保护） |

## 配置

TOML 配置（默认 `./config.toml`，任务状态 `./tasks.json`）：`[download]`（dest_root/并发/限速/代理/disk_precheck_strict 磁盘预检严格模式）、`[bt]`（save_path/DHT/LSD/UPnP/做种）、`[provider]`（fallback 链）、`[server]`（bind/token）、`[webhook]`（任务完成通知 URL，POST JSON，fire-and-forget）、`[cleanup]`（auto_remove_completed_days 已完成任务 N 天后自动清扫，0 = 禁用；auto_remove_keep_data 默认保留文件）、`[post_download]`（完成后自动处理：move_to 移动到目标目录 + hook 外部程序，环境变量 SD_TASK_ID/SD_TASK_NAME/SD_FILE_PATH/SD_ENGINE 传任务上下文；空 = 禁用）、`[scheduler]`（start_jitter_seconds 错峰随机延迟上限，秒；0 = 关）。支持热重载。

## 文档地图

| 文档 | 内容 |
|------|------|
| [`docs/BACKLOG.md`](docs/BACKLOG.md) | 未实现总清单（路线图） |
| [`docs/IMPLEMENTED.md`](docs/IMPLEMENTED.md) | 已实现档案（行为契约 / API / 验证证据） |
| [`docs/PROJECT_STATUS.md`](docs/PROJECT_STATUS.md) | 四条主线现状交接报告 |
| [`docs/CAPABILITY_MAP.md`](docs/CAPABILITY_MAP.md) | 远期能力地图（BiglyBT/aria2/eMule 对标） |
| [`docs/nas/`](docs/nas/) | NAS 场景校准结论（A2–A6） |
| [`docs/ANDROID_TERMUX_DEPLOY.md`](docs/ANDROID_TERMUX_DEPLOY.md) | Android/Termux 部署 |

## 仓库说明

`docs/research/`、`scripts/research/`、`scripts/nas/`、`tools/` 下的 Python 脚本与文档是**逆向研究资产与原型参照实现**（迅雷协议考古、BitComet 加速设计工具包、NAS 校准脚本），不参与构建与 CI；Rust 生产代码是其中部分原型的移植（如 `crates/provider/src/xunlei/url_class.rs` 移植自 Python 原型常量表）。仓库的自动化测试全部为 Rust（`cargo test`），CI 不依赖任何 Python。
