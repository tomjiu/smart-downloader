# Task 31 — A2 校准全程实录（2026-08-31 云端执行）

> 执行环境：Debian 13 trixie x86_64 / Python 3.12.14 / 沙盒（进程回收型）
> 校准对象：xunlei-pan-cli.3.23.5.amd64（3.23.5，62,765,544 字节）
> 结论先行：**#8 否决 JSON 预置、确立 KV 热启动；#9/#10 路由面实测定形；登录门可外部注入**

## 0. 灾备恢复（沙盒重置）

会话续接时发现全新沙盒（磁盘 86M、workspace/repo/引擎全失）。恢复路径：

1. `git clone --branch feat/nas-remote-identity` fork（含上轮超时前推送的 9ecdb05 sync_l1_token 对齐 commit）
2. 引擎二进制不在 git（62MB 从未入库）→ 从上游 Release `v0.1.0-cross-platform` 的
   `cross-platform-evidence.zip`（190MB）提取 `spk-x64/payload/bin/bin/xunlei-pan-cli.3.23.5.amd64`
3. ldd 零缺库，尺寸与 BuildID 与 Task 14 实测一致

## 1. 假设区 #8（预置 token 免扫码）——三轮实测全部否决，第四轮确立正解

| 轮次 | 手段 | 结果 |
|------|------|------|
| R1 | `auth_token.json` 预置 + `DriveAuthorizationTokenPath` env（过期 token） | 引擎同秒走 `DoLoginQrcode`，无任何 refresh 尝试日志 |
| R2 | 同 R1（用户二次授权换 fresh token，expires_in=7200） | 同秒 `DoLoginQrcode`，与 token 时效无关 |
| R3 | `AuthTokenPATH` env | config dump 中 `AuthTokenPATH:` 仍空——该字段**不是 env 旋钮** |
| R4 | **HostXluser 重定向 MITM**（见 §3） | `login ok`——引擎自登录，凭据入内部 KV |

关键实证：
- config.init dump 双字段并存：`AuthTokenPATH:`（空）与 `DriveAuthorizationTokenPath:<我们的路径>`
  ——后者仅是配置回显，auth 模块不读它
- 引擎凭据真身：**内部加密 KV**（storm DB，`CredentialStorage.GetItem` / `FileKV` /
  `PanAuthTokenSecret` / `BackupMgr.Load itemName:credential`）
- `auth_token.json` 是引擎**写出**的导出物（`service.Start.StartWriteAuthToken`），非读入物

**#8 定案**：JSON 预置方案否决（PR #1 中 L1→xllite 桥接的 token 注入设计需按 §3 重构）；
免扫码登录的正解 = **凭据 KV 库热启动**（登录一次后永久免扫码）。

## 2. 用户凭据链（两次设备码授权）

- 第一次授权（Task 30，用户 Windows 侧）：token 已死——access_token 过期 7h，
  refresh_token 换新实测 `invalid_grant / 4126 / invalid refresh token`
- 第二次授权（本任务，用户浏览器 /yc/）：`a2_device_flow.py` 解耦流程——
  request（申码出 URL 后进程即退）→ 用户点击 → poll（拿 device_code 换 token）✓
  - 设备码时效实测 `expires_in:120`；105s 龄时 poll round-1 即命中
  - 沙盒实证：工具调用结束后 ~12-135s 无差别回收后台进程（setsid 无效），
    **RFC 8628 的等待状态在服务端**是解耦可行性的根基

## 3. 无头登录注入（本任务核心工程成果）

引擎登录路径的三道门与对应解法：

| 门 | 症状 | 解法 |
|----|------|------|
| `/dev/tty` 打开失败 | panic at login.go:48 | `pty.fork()` 真 PTY |
| gocui 0x0 窗口 | panic：SetView nil view（login.go:74） | `TIOCSWINSZ(40,120)` |
| 凭据无从注入 | 三旋钮全败 | **HostXluser 白名单重定向** |

HostXluser 注入细节：
- `HostXluser` 是 env 旋钮但**白名单校验**，仅允许
  `https://xluser-ssl.xunlei.com` / `https://dev-xluser-ssl.xunlei.com`（panic 信息实证）
- 无 sudo（443 不可绑、/etc/hosts 不可写）→ `unshare -Urnm` 用户命名空间全通：
  ns 内 uid=0 → `ip link set lo up` + `mount --bind fake_hosts /etc/hosts`
  + 443 自签 TLS MITM（`TLSInsecureSkipVerify:true` 为 config 默认，自签直通）
- MITM 行为：`/v1/auth/device/code` → 假码；`/v1/auth/token` → 200 真 token（用户二次授权所得）
- 结果：`startWatch RawRequest succ!` → **`login ok`**（auth.go:282），
  credential 全量入 KV；`dy1-vip-ssl` DNS 失败被引擎优雅容忍（ns 内无外网属预期）

## 4. 调用姿势（重要工程发现）

引擎正确服务调用：**`xunlei-pan-cli-launcher.amd64 -pid <file>`**（SPK 官方 .sh 即此）
或 `xunlei-pan-cli.3.23.5.amd64 run -pid <file>`。

矩阵实测（`a2_invocation_matrix.py`）：

| 调用 | 结果 |
|------|------|
| `run -pid` | startService ✓ 无 help ✓（但 -pid 值传不进 initPidFile，会 panic，弃用） |
| `-pid … run` | startService ✓ 后打印 help 退 0（凭据在时暴露） |
| `launcher -pid` | startService ✓ 无 help ✓ **采用** |
| 裸 `-pid` | 无凭据时可用；凭据就绪后同 `-pid … run` 病灶 |

注：裸 `-pid` 此前"可用"是因为从未通过登录门；门通过后必现 help 退 0。

## 5. 热启动验证 + #9/#10 校准（launcher 入口，纯在线零 MITM）

**热启动: 成功 | 登录门: 绕过**——`BackupMgr.Load credential` 后无任何 DoLoginQrcode，
DriveListen t=0s 就位。runner 注册 user#nfo / user#app，api-pan 在线调用正常。

#9 路由面（DriveListen 127.0.0.1:5050）：

| 探测 | 状态 | 判定 |
|------|------|------|
| `GET /` | 200 | web UI（内嵌 Vue 应用） |
| `GET /drive/v1/tasks` | 403 | 存在·需本地鉴权 |
| `GET /device/v1/try_speed/get_info` | 403 | 存在·需本地鉴权 |
| `POST /device/v1/try_speed/apply` | 403 | 存在·需本地鉴权 |
| `POST /device/v1/try_speed/get_info` | 404 | 方法不匹配（gin 无 405 透出） |
| `/webman/3rdparty/pan-xunlei-com/index.cgi/` 等 | 404 | 不存在/未挂载 |

#10 try_speed：路由在**设备内 API**（远端 api-pan 公网面 404 实证），
`get_info`/`apply` 双路由存活，参数面探测被 403 本地鉴权门拦截。

403 鉴权门（本地 API）：`{"error":"permission…","error_code":403}`——
鉴权源**不是** xunlei access_token（带 Bearer 仍 403）也**不是** `auth_token.json`
（引擎登录后未重写该文件），而是 KV 内 secret（`GetOrCreateAuthTokenSecret` /
`CheckPluginAuthToken` / `service.apiAuth GinVerifyToken` 字符串族）→ **A3 任务**。

## 6. 产物与资产

| 资产 | 路径 |
|------|------|
| 设备码解耦工具 | `scripts/nas/a2_device_flow.py`（request/poll/status） |
| ns 登录注入器 | `scripts/nas/a2_nsd_login.py` |
| 热启动校准器 | `scripts/nas/a2_warmboot_run.py` |
| 调用姿势矩阵 | `scripts/nas/a2_invocation_matrix.py` |
| 凭据取证归档 | `scripts/research/xunlei/extracted/cross-platform/xllite_token.json` |
| 热态工作区 | `~/.nas-engine-test/data/.drive/`（KV 三库 + .backup，**沙盒重置即失**） |
| 校准报告 | `~/.nas-engine-test/a2_result_warmboot.json` / `ns_login_result.json` 等 |

## 7. 对 PR #1 的影响

- `sync_l1_token`（9ecdb05）桥接对齐引擎原生 token 形的**方向作废**：
  引擎不接受任何外部 token 形预置
- 桥接重构方向：L1 完成设备码授权后，**首次登录走引擎自登录**（有头环境）或
  HostXluser 注入（无头环境，本任务已验证全链路）；此后永久热启动
- merge 前建议：A2 校准结论以本文件为准补入 PR 描述；`a2_calibrate.py` 保留为
  历史探针（其 step2 预置分支已实证不可行，可在 A3 中改造为热启动验证器）
