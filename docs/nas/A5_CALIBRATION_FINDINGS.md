# Task 31 — A5 校准实录：群晖平台伪装与最小配方定案（2026-08-31 云端执行）

> 接续 A4（`A4_CALIBRATION_FINDINGS.md` 90120 双嫌疑 + 配额触顶）。
> **结论先行：群晖平台伪装的最小配方 = `BinDir/envconfig`（YAML）注入
> `PLATFORM=群晖` + `ALLOW_CUSTOM_PLATFORM=true`，引擎即以 `Platform:群晖`
> 干净启动并保持 warm 凭据与全 API 面可用；synoinfo.conf / authenticate.cgi /
> /var/packages 等文件伪装经 7 阶段实验链证实全部非必需；
> 90120 终验与任务清理被每日配额阻断（北京时间 0 点重置），A6 执行。**

## 1. 静态侦察：平台检测到底查什么

### 1.1 XOR 全谱差分搜索（负结果，方法论留存）

自研 `a5_xorhunt.py`：常量 XOR 保持相邻字节差分（`D[i]=data[i]^data[i+1]`），
对目标串构造差分签名后在差分序列中单遍 `bytes.find`，一次扫描定位**任意单字节
XOR 变体**（纯 Python 逐 key 扫描 62MB×255 key 超时，差分法秒级）。

对引擎（62MB）与 launcher（19MB）双二进制全谱扫描：

| 目标串 | 引擎 | launcher |
|--------|------|----------|
| `synoinfo.conf` / `authenticate.cgi` / `platform_name` / `unique=synology` | 零 | 零 |
| `/etc/VERSION` / `synos-release` / `SYNOPLATFORM` / `SYNOPKG_*` | 零 | 零 |
| `/var/packages` / `pan-xunlei-com` | 零 | 零 |
| `OS_VERSION` | **1 处明文**（31677432） | 零 |
| `群晖`（UTF-8） | 仅内嵌 web UI bundle | 零 |

**定案：检测器不读任何 syno 特征文件**。附带发现：引擎混淆字符串池内含
`ynos-release` / `os_version` 可读孤岛（可变 XOR，未破）；前端平台代号映射
`联想:lev / 群晖:syn / 电脑:pcl`。

### 1.2 cnk3x/xunlei 与官方 SPK 解包（权威配方对照）

- cnk3x `mockEnv`：`SYNOPLATFORM/SYNOPKG_*/OS_VERSION/PLATFORM=群晖` 全套 env；
  `mockSyno`：`/etc/synoinfo.conf`（platform_name/synobios/unique 三行）+
  `/usr/syno/synoman/webman/modules/authenticate.cgi`（真实 ELF）。
- 官方 `service-setup` 实测引擎 env 仅 **6 个**：`DriveListen`、`PLATFORM=群晖`、
  `OS_VERSION="${SYNOPLATFORM} dsm ${MAJOR}.${MINOR}-${BUILD}"`、`ConfigPath`、
  `DownloadPATH`、`HOME`。SYNOPKG_*/SYNOPLATFORM 只在 DSM 侧使用，不传引擎。
- 官方 SPK（nasxunlei-DSM7-x86_64.spk, 3.23.5-0814080017）解包：bin/bin 内仅
  version/version_code/launcher/pan-cli 四件，**与我们 evidence 提取品 md5 逐字节
  一致**，SPK 不携带任何平台检测文件 → 检测必为 env/运行时式。

## 2. 实验矩阵（7+1 次引擎启动，`a5_boot.py <phase>`）

| 阶段 | env 注入 | 文件伪装 | 结果 |
|------|----------|----------|------|
| baseline | PipeLimit=10 | 无 | docker 平台正常；**dump DownloadPipeLimit=256 复验 ✓**；配额 403 实锤 |
| syno | cnk3x 全套 env | 无 | panic（零值候选） |
| syno-files | 官方极简 env | /etc shadow + synoinfo.conf | panic |
| syno-full | cnk3x 全套 env | /etc+/usr+/var 三树 shadow + authenticate.cgi + 真实 PKGDEST 布局 | panic |
| syno-envcfg① | + ENGINE_DIR/envconfig(KEY=VALUE) | 同上 | launcher **yaml unmarshal panic**（格式坑） |
| syno-envcfg② | + envconfig YAML 双落点 | 同上 | panic（缺 ALLOW_CUSTOM_PLATFORM） |
| syno-min | 同上 + 剥离 /etc 发行版指纹 | 同上 | panic |
| **syno-custom** | **+ ALLOW_CUSTOM_PLATFORM=true** | 同上 | **干净启动 Platform:群晖 ✓** API 全通 |
| syno-final | + PipeLimit=10 | 同上 | **群晖 + dump 256 ✓** + 配额 403 |
| cleanup | 无 unshare 无伪装，仅残留 BinDir envconfig | 无 | **群晖照常启动 → 最小配方反证** |

### 2.1 三树 shadow 技术实现（留档）

`unshare -Urm` 内（实测外层 overlay superblock 可 bind）：`/usr`→`/tmp/usr-real`
保留，假 `/usr` 用**软链农场**回指（引擎 ldd 仅依赖 `/lib*`，usrmerge 下
`/lib64→usr/lib64` 不能整树拷）；`/var` 同法 + `packages/pan-xunlei-com/target/
bin/bin→ENGINE_DIR` 真实布局；6 项事后校验全 True（loader/authcgi/pkgdest 可达）。

## 3. 机制定案

1. **launcher 启动链**：ENGINE_DIR launcher → 把 pan-cli 复制到 `HOME(=data/.drive)/bin`
   （BinDir，含 `.version` 校验）→ 运行**副本**（`panCliPath=.drive/bin/xunlei-pan-cli…`）
   → 副本从 `BinDir/envconfig` 读 **YAML `map[string]string`** 注入 env → spawn
   （launcher 的 `env=[…]` 日志行暴露全量传递 env）。
2. **平台白名单**：`platformdetect.Platform()`（detect.go:47）在
   `oauth2client.init.0` 调用；`PLATFORM=群晖` 在我们环境无法自动匹配白名单 →
   候选零值 → `panic: platform not suport: [{Name: … DefaultPipeLimit:0}]`。
3. **`ALLOW_CUSTOM_PLATFORM=true` 是唯一开关**（env 或 envconfig 均可）→ 白名单
   旁路 → 自定义平台注册：`config Platform:群晖`、`configEngine register
   PlatformPrivilege 6`、warm KV（storm.Open succ）无缝打开、pan-auth JWT 自举
   不变、`/drive/v1/tasks` 200。launcher 自身 report（platform:docker,
   client_id X9ibISwpIp8jQ4Ya）未变——平台身份切换的云端观感待 A6。
4. **文件伪装全部非必需**：synoinfo.conf/authenticate.cgi/PKGDEST 布局/发行版
   指纹剥离均为负结果链——cnk3x 成功的真正机制即其 mockEnv + （推断）docker
   环境；我们环境需显式 ALLOW_CUSTOM_PLATFORM 旁路。
5. **envconfig 格式 = YAML**（KEY=VALUE 触发 `cannot unmarshal !!str` panic——新坑）；
   `os.execvp` 后 `finally` 不执行 → ENGINE_DIR/envconfig 残留（已手动清除），
   BinDir envconfig 有意保留 = 持久化工作配方。
6. **PipeLimit 旋钮与群晖平台可叠加**：env=10 → dump 256（与 docker 平台行为一致）。

## 4. 配额与遗留

- 全程 `task_create_count_limit` 403（error_code 11，"任务创建次数达到上限"；
  02:26/03:08 UTC 双探针，UTC 午夜已过仍锁 → 与北京时间 0 点重置一致 ≈16:00 UTC）。
- 3 个 ERROR（90120）任务 DELETE 同步阻塞 >75s（复现 A4 >30s；脚本超时非引擎死锁），
  清理顺延 A6。
- try_speed get_info 200：`usage.total=3, used=0`（超速配额独立未扣）。

## 5. A6 计划（配方已就位，额度恢复即战）

1. 核对 BinDir envconfig 存续 → 直接 `cleanup` 清理 3 任务（DELETE 超时 100s+）。
2. `POST /drive/v1/task` → 轮询 phase 迁移（PENDING→RUNNING）→ RUNNING 即
   `apply` → 抓 `usage.used` 0→1 与加速回包 —— **90120 终验**。
3. 若仍 90120：嫌疑收窄至云端平台特权（docker 设备配额/权限），比对
   report 的 package_name/client_id 是否随平台切换（info.file 更新观测）。
4. 产出 NasRemoteEngine 下载链最终封装（smart-downloader 适配层）。

## 6. 本轮产物

| 资产 | 路径 |
|------|------|
| 7 阶段实验器（envconfig/unshare 三树/探针/清理） | `scripts/nas/a5_boot.py` |
| XOR 全谱差分搜索器 | `scripts/nas/a5_xorhunt.py` |
| 原始证据（7 份 JSON + 引擎日志摘录） | `docs/nas/evidence/a5/` |
