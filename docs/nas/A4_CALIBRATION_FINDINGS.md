# Task 31 — A4 校准实录：真实下载任务创建与 90120 排障（2026-08-31 云端执行）

> 接续 A3（`A3_CALIBRATION_FINDINGS.md` pan-auth 鉴权门突破）。
> **结论先行：任务创建/删除 API 已定案；引擎侧下载执行全部 `下载(90120)` 失败；
> 根因两大嫌疑（PipeLimit=0 配置 / 平台特权 PLATFORM_DOCKER），均已有实证与解法；
> 云端「每日 3 任务创建」限制触顶，执行面实测顺延 A5。**

## 1. 任务 API 定案（pan-auth 鉴权下全部实测）

| 操作 | 请求 | 结果 |
|------|------|------|
| 创建 URL 任务 | `POST /drive/v1/task`（**单数**） | **200**，返回完整 task 对象 |
| 查询任务 | `GET /drive/v1/tasks?space=<target>&filters=<json>` | 200；filters 支持 `{"id":{"in":"<id>"}}`、`{"type":{"in":"user#download-url,user#download"}}`、`phase:{in:...}` |
| 删除任务 | `DELETE /drive/v1/tasks?space=<target>&task_ids=<id>[&task_ids=...]` | 路由有效但**同步阻塞 >30s**（配合本地文件清理+远端同步），短超时必失败 |
| 超速查询 | `GET /device/v1/try_speed/get_info` | 200，`usage.total=3`（超级加速次数配额） |
| 超速申请 | `POST /device/v1/try_speed/apply`（body 可空/`task_id`/`file_id` 均可） | 200 `{"message":"NO_RUNNING_TASK"}`——仅对 RUNNING 任务生效 |

创建载荷（实测定案，`url` 必须为**对象形**）：

```json
{
  "space": "device_id#c7d089…",
  "type": "user#download-url",
  "file_size": "0",
  "name": "a4r3-10Mb.dat",
  "file_name": "a4r3-10Mb.dat",
  "url": {"url": "https://proof.ovh.net/files/10Mb.dat"},
  "parent_folder_id": "",
  "params": {"target": "device_id#c7d089…"}
}
```

任务对象字段：`id/kind/type/phase/message/params{spec,status,error,real_path}/space/…`。
注意 `params.spec`（目标态）与 `params.status`（实况）分离，曾观测
`spec=running → status=error` 的迁移痕迹。

## 2. 云端限制实锤

- **每日 3 任务创建上限**：第 4 次创建返回
  `403 {"error":"task_create_count_limit","error_code":11,"error_description":"任务创建次数达到上限"}`
  ——与 cnk3x/xunlei issue #229（"限制每日只能下载3个文件"）完全吻合；
  **失败任务也计入次数**。本日 3 次额度已耗尽。
- 超速配额独立于任务配额：`usage: {total:3, used:0}` 始终未扣减（未达 RUNNING）。

## 3. 90120 排障（三次创建 → 三次 `下载(90120)` 秒败）

### 3.1 现象与已排除项

- 任务创建 200 → 数秒内 `PHASE_TYPE_ERROR`，`params.error="下载(90120)"`，
  message="失败"；`real_path` 指向 downloads 目录（文件未落盘）
- 已排除：磁盘空间（8.6G 可用 >> SingleTaskReserveMB=20）；URL 形态
  （cloudflare 带参 URL 与普通直链 proof.ovh 同样失败）；沙盒网络能力
  （UDP bind/send、TCP 出网全通，eth0 公网 IP）；fork 网络栈
- 附带观测：任务失败期间创建 **debug 日志模式（DriveLogLevel=debug）可令引擎
  整进程冻结**（HTTP 全挂、日志静默），普通模式不冻结——debug 模式勿在生产链路使用

### 3.2 嫌疑一（已验证解法）：PipeLimit=0

- config dump：`DownloadPipeLimit:0 UploadPipeLimit:0`——**0 是未配置的异常值**；
  cnk3x issue #212 实锤同族错误 `设置下载和上传的连接数错误(9102)`
  出自 `download.SetPipeLimit (download_cgo.go:152)`
- **env 旋钮实测**：启动前设 `DownloadPipeLimit=10 UploadPipeLimit=10` →
  config dump 变为 **256**（引擎内部按档位换算），旋钮有效
- 群晖平台定义含 `DefaultPipeLimit` 字段（见 3.3 panic 结构）——
  正常平台自带动辄 256+ 的默认连接数，docker 平台缺省 0 → cgo 内核初始化拒绝

### 3.3 嫌疑二：平台特权（PLATFORM_DOCKER vs 群晖）

- A2 日志 `configEngine register PlatformPrivilege 6` + KV 条目
  `docker.860599297.privilege`——引擎以 **docker 平台**身份注册运行
- 引擎平台由 `pkg/platformdetect/detect.go:47` **检测**而非仅 env 决定：
  强设 `PLATFORM=群晖`（无配套文件特征）→ 启动即
  `panic: platform not suport: [{Name: … Privilege: … SuportDocker:false … DefaultPipeLimit:0}]`
- panic 暴露平台结构全字段：`Privilege/PrivilegeCode/CreatePrivilegeCode/
  RunnerSpace/Environ/SuportDocker/Labels/Config/UploadStrategy/Files/IgnoreFiles/
  RunnerType/DefaultPipeLimit` —— **平台特权体系决定下载能力**是引擎一等公民设计
- 对标 cnk3x/xunlei（Docker 里正常下载的社区实现，提取自同款 SPK）：
  - mockEnv 全套：`PLATFORM=群晖`、`SYNOPLATFORM/SYNOPKG_*/OS_VERSION`
  - **伪造 `/etc/synoinfo.conf`**（platform_name/synobios/unique=synology_geminilake_DS920+）
    + `authenticate.cgi` —— 平台 detect 的文件特征正是它们
  - launcher 用 unix socket、chroot 到模拟根
- 我们沙盒无 CAP_SYS_CHROOT/CAP_SYS_ADMIN（CapEff=0），直接 chroot 不可行；
  复用 A2 已验证的 `unshare -Urnm` + bind-mount 伪造 `/etc/synoinfo.conf`
  是可行路径（ns 内有 CAP_SYS_ADMIN）

### 3.4 资源限制记录（沙盒画像）

| 项 | 值 | 影响 |
|----|-----|------|
| Max open files | soft 1024 / hard 100000 | 子进程可无特权提升（`resource.setrlimit`） |
| Max processes(threads) | 1024 / **1024** | 硬限即 1024，不可提升——cgo 内核若需 >1024 线程将失败 |
| Capabilities | CapEff=0 | 无 chroot/mount 特权（ns 内除外） |

实测引擎常态线程仅 24-27、fd 58-70（静态运行时），任务失败前后无爆炸增长。

## 4. A5 计划（额度重置后，北京时间 0 点）

1. 拉起引擎（`DownloadPipeLimit=10 UploadPipeLimit=10`，dump 确认 256）
2. 清理 3 个残留 ERROR 任务（DELETE 长超时 60s+ 或等云端同步）
3. 创建任务观察 phase 迁移（PENDING→RUNNING）→ RUNNING 即 `apply`
   → 抓 `usage.used` 0→1 与加速回包
4. 若仍 90120 → unshare ns 内 bind-mount `/etc/synoinfo.conf` 走群晖平台伪装，
   观察平台特权注册变化后再测
5. 产出：apply 加速实测报告 + NasRemoteEngine 下载链最终封装

## 5. 本轮产物

| 资产 | 路径 |
|------|------|
| 任务创建/清理校准器 | `scripts/nas/a4_run3.py`（载荷矩阵+轮询+apply 抢跑） |
| 资源受限复测器 | `scripts/nas/a4_run4.py`（rlimit 提升+引擎线程/fd 监控） |
| 诊断器 | `scripts/nas/a4_diag.py`（filters 形态矩阵+apply 三载荷） |
| 原始证据 | `docs/nas/evidence/a4/`（a4_run3/run4.json、创建响应、403 配额限回执） |
