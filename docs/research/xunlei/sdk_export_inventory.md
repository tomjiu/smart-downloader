# 迅雷 DownloadSDK 导出表普查与「登录/用户身份」能力考古

> 目标：从真实 DLL 导出表 + 反编译 C 语料中，挖掘 DownloadSDK（DownloadSDKProxy.dll / DownloadSDK.dll）是否具备「SDK 登录 / 用户身份」能力，并评估是否可落地到 `xunlei-ffi`。

---

## 0. 结论速览（TL;DR）

**结论：DownloadSDK 是一套纯下载 / P2P 加速引擎，不具备任何「账号登录 / 鉴权 / OAuth / Passport」能力。**

- 导出表中**不存在** `XL_Login` / `XL_Passport` / `XL_OAuth` / `XL_VerifyUser` / `XL_GetUserInfo` 之类函数。
- 唯一与「身份」相关的导出是**身份注入器（setter）**与**加速凭证注入器**，它们假设调用方**已经从别处（迅雷 Pan/云盘登录体系）拿到了 user_id / token / 证书**，SDK 本身不做任何网络登录。
- 反编译中出现的 `CmdDPHubLoginParent` 是 **P2P 节点握手（DPHub 协议）**，不是账号登录。
- 项目 `crates/provider/src/xunlei/` 已经独立实现了迅雷云盘的 OAuth 式登录（access_token / device_id / captcha_token / user_id），那是 **Pan API 体系**，与 DownloadSDK DLL 无关——二者是两套不同的登录栈。

**因此：「通过 DownloadSDK DLL 实现 SDK 登录」是一条死路，已被本次考古封死。** 但 SDK 暴露的用户身份/凭证**注入**面（以下 A 级函数）仍有工程价值，已在 `xunlei-ffi` 中以安全封装落地（见 §5）。

---

## 1. 普查对象

| 文件 | 内部名 | 导出总数 |
|---|---|---|
| `C:\Program Files\Thunder Network\Thunder\program\SDK\DownloadSDKProxy.dll` | `DownloadSDKProxy.dll` | **100** |
| `C:\Program Files\Thunder Network\Thunder\program\SDK\DownloadSDK.dll` | `DownloadSDK.dll` | **105** |

解析工具：手写最小 PE 解析器 `scripts/research/xunlei/dump_exports.py`（无 pefile 依赖，按段表把 RVA 换算成文件偏移后读取导出目录），复用了 `pe_iat_probe.py` 的导出目录遍历思路。

### 1.1 登录 / 身份 / 凭证 相关导出命中清单（两 DLL 共有）

以下函数名命中 `Login/User/Token/Session/Passport/SetUserInfo/OAuth/Verify/PeerId` 关键字：

| 函数 | 证据等级 | 角色判定 |
|---|---|---|
| `XL_SetUserInfo` | **A 级**（逻辑可读，但见 §4 的 ABI 风险） | 身份注入器：把 user_id / vip_type（实际为两个字符串，见 §4）写入 SDK 全局状态。**非登录**。 |
| `XL_SetTokenMode` | A 级 | 全局 token 模式开关（`undefined4` 单参数）。**非登录**，仅切换 SDK 内部 token 处理模式。 |
| `XL_SetAppGuid` | A 级 | 注入应用 GUID 字符串。身份/来源标识，**非登录**。 |
| `XL_SetAccelerateCertification` | A 级 | 注入加速证书字符串（caller-supplied）。**非登录**。 |
| `XL_EnableDcdnWithToken` | B 级 | 用调用方提供的 token 字符串激活 DCDN 加速；整型参数语义未完全确认。 |
| `XL_EnableDcdnWithSession` | B 级 | 用 session 字符串激活 DCDN；含 3 个字符串参数，整型参数语义未确认。 |
| `XL_EnableDcdnWithVipCert` | B 级 | 用 VIP 证书激活 DCDN。 |
| `XL_SetTaskEquityToken` | B 级 | 给具体任务注入 equity/token 字符串。 |
| `XL_GetPeerId` | A 级 | 返回 P2P **peer id** 内部缓冲指针（20 字节级），是 P2P 网络身份，**非账号 user_id**。 |
| `XL_SetUserAgent` / `XL_SetTaskUserAgent` | A 级 | UA 字符串，无关登录。 |

> 两 DLL 导出集合几乎一致（DownloadSDK.dll 多了 `XL_ContinueTask`、`XL_GetUniversalPlayInfo`、`XL_QueryAccelerateInfo`、`XL_SetHLSDownloadStrategy`、`XL_SetTaskExtraKey`、`XL_SetWindowsSleepInfo`），但**均不含任何登录/鉴权函数**。

### 1.2 反编译中出现的「Login」字样

```
downloadsdk_key_funcs.c:1354   *param_1 = TCPServiceDPHub<class_CmdDPHubLoginParent, ...>::vftable;
downloadsdk_combined.c:660     *param_1 = TCPServiceDPHub<class_CmdDPHubLoginParent, ...>::vftable;
```

`CmdDPHubLoginParent` / `CmdDPHubGoAway` / `CmdDPHubPingParent` 是 **DPHub（Download/Peer Hub）P2P 控制协议** 的命令类，用于 P2P 节点间握手/保活/退出。**与账号登录无关 —— 属于 C 级（仅字符串线索）。**

grep 全语料 `https?://|passport|xlogin|token\.xunlei|login` 等登录端点字符串：**0 命中**。说明 SDK 内没有任何登录 RESTful 端点硬编码。

---

## 2. 反编译语义挖掘（关键函数）

### 2.1 `XL_SetUserInfo`（@0x18005ed80，DownloadSDK_DECOMPILED.c:11201）
```c
undefined8 XL_SetUserInfo(longlong param_1, longlong param_2)
{
  if ((param_1 == 0) || (param_2 == 0)) return 2;   // 两参数均按指针判空
  ...
  do { lVar16 = lVar16 + 1; } while (*(char *)(lVar16 + param_1) != '\0'); // 对 param_1 做 strlen
  FUN_180004730(local_78, param_1, lVar16);          // XPF_String 构造（指针+长度）
  do { lVar19 = lVar19 + 1; } while (*(char *)(param_2 + lVar19) != '\0'); // 对 param_2 做 strlen
  FUN_180004730(local_58, param_2);                  // XPF_String 构造（指针）
  ...
}
```
**语义**：接收**两个窄字符串指针**，分别构造 `XPF_String` 后写入 SDK 全局用户态。名字虽叫 `UserInfo`，但它是**身份数据注入**，不是「执行登录」。无网络调用、无鉴权。

> ⚠️ **ABI 风险（重要）**：反编译显示两个参数都是 `const char*`（strlen + XPF_String 构造），但 `crates/xunlei-ffi/src/bindings.rs:210` 当前绑定为
> `pub type XLSetUserInfoFn = unsafe extern "system" fn(handle: usize, user_id: c_ulonglong, vip_type: c_uint) -> LtErr;`
> 即按整数传参。在 x64 `extern "system"` 下整数与指针都占 8 字节寄存器，但 C 侧会把 `user_id` 数值当成指针去解引用 → **极易崩溃或静默损坏**。现有 `handle.rs::set_user_info(user_id: u64, vip_type: u32)` 调用存在 ABI 错配风险。本次**不改动既有声明**（任务要求「只追加」），仅在此标注，建议后续用 dump 法或实测确认真实参数类型。

### 2.2 `XL_SetTokenMode`（@0x180067f80，:18762）
```c
undefined8 XL_SetTokenMode(undefined4 param_1)
{ lVar2 = FUN_180045c90(); *local_28 = FUN_18005b370; *(undefined4 *)(local_28+1) = param_1; ... }
```
**语义**：单一 `undefined4` 参数，写入全局 token 模式标志。**是模式开关，不是登录。**

### 2.3 `XL_SetAppGuid`（@0x180068000，:18790）
```c
undefined4 XL_SetAppGuid(longlong param_1)
{ if (param_1 == 0) return 2;
  do { ... } while (*(char *)(param_1 + lVar5) != '\0');  // strlen
  FUN_180004730(local_28, param_1);                        // XPF_String 构造
  ... }
```
**语义**：接收应用 GUID 字符串并存储。**来源标识，非登录。**

### 2.4 `XL_SetAccelerateCertification`（@0x180062320，:13956）
```c
undefined8 XL_SetAccelerateCertification(int param_1, longlong param_2)
{ if (param_1 == 0) return 2;
  do { ... } while (*(char *)(param_2 + lVar18) != '\0');  // strlen on param_2
  FUN_180004730(local_60, param_2);                        // XPF_String 构造
  ... if (*(int *)(lVar8 + 0x30) == 2) { cVar7 = FUN_18003ffe0(lVar8, param_1); ... } }
```
**语义**：`(handle, cert_string)` —— 注入加速证书字符串。整型 param_1 经 `FUN_18003ffe0` 校验后写入。**凭证注入，调用方需先拿到证书，非登录。**

### 2.5 `XL_EnableDcdnWithToken` / `WithSession` / `WithVipCert`（:13296 / :13563 / :14105）
- `WithToken(int, int, longlong, longlong)`：param_3 / param_4 均为字符串（strlen + XPF_String）。两个整型参数语义未确认。
- `WithSession(int, int, longlong, longlong, longlong)`：param_3/4/5 均为字符串。
- `WithVipCert(int, int, longlong)`：param_3 为字符串。

**语义**：在调用方已提供 token / session / vip 证书的前提下，激活 DCDN 加速通道。**它们是「凭证消费」而非「凭证获取」。**

### 2.6 `XL_GetPeerId`（@0x18005d5e0，:9997）
```c
undefined4 * XL_GetPeerId(longlong param_1, uint *param_2)
{ if ((param_1 != 0) && (param_2 != 0)) { lVar1 = FUN_1800287a0();
    XPF_ThreadLockLock(*(undefined8 *)(lVar1 + 0x738));
    plVar2 = (longlong *)(lVar1 + 0x748); ... } }
```
**语义**：返回 P2P **peer id** 内部缓冲指针（受线程锁保护）。这是 P2P 网络节点身份，**不是迅雷账号 user_id**。

---

## 3. 结论分级

| 等级 | 函数 | 说明 |
|---|---|---|
| **A 级** | `XL_SetUserInfo`、`XL_SetTokenMode`、`XL_SetAppGuid`、`XL_SetAccelerateCertification`、`XL_GetPeerId` | 导出实存 + 反编译可见完整逻辑 |
| **B 级** | `XL_EnableDcdnWithToken`、`XL_EnableDcdnWithSession`、`XL_EnableDcdnWithVipCert`、`XL_SetTaskEquityToken` | 导出实存 + 逻辑可见，但整型参数语义未完全确认，未落地封装 |
| **C 级** | `CmdDPHubLoginParent`（P2P 握手，非账号登录） | 仅字符串线索，与登录无关 |
| **（无）** | `XL_Login` / `XL_Passport` / `XL_OAuth` / `XL_VerifyUser` / `XL_GetUserInfo` | 导出表与反编译中均**不存在** → 否定结论 |

**最终判定**：DownloadSDK 无登录能力（纯下载引擎）。本次考古封死「通过 SDK DLL 实现登录」这条路。

---

## 4. 与既有仓库实现的对照

- `crates/xunlei-ffi/src/handle.rs::set_user_info` 已封装 `XL_SetUserInfo`，但如上 §2.1 所述，**参数类型（整数 vs 字符串）存在 ABI 错配风险**，需后续用 dump/实测澄清。本次不改动。
- `crates/provider/src/xunlei/{client,auth}.rs` 实现的是**云盘 Pan API 登录**（captcha/init、access_token JWT 解 `sub` 得 user_id、device_id、captcha_token）。这是另一个登录栈，与 DownloadSDK DLL 解耦。DownloadSDK 只需被「喂」入 user_id / token / 证书即可，自身不发起登录。

---

## 5. 落地（仅追加，不动既有声明）

依据 §3 的 A 级清单，把 SDK 真实暴露的「用户身份 / 加速凭证」注入面以安全异步封装追加进 `xunlei-ffi`（仅在既有风格上**追加**，不修改 `XL_SetUserInfo` 既有声明）：

- `bindings.rs`：新增类型别名 `XLSetTokenModeFn` / `XLSetAppGuidFn` / `XLSetAccelerateCertificationFn`。
- `loader.rs`：`Symbols` 追加 3 个字段 + `lib.get` 解析（仅追加）。
- `identity.rs`（新增模块）：`XunleiHandle` 上提供 `set_token_mode(u32)` / `set_app_guid(&str)` / `set_accelerate_certification(&str)`，对齐 `dcdn.rs` 的 `spawn_blocking` + `with_context` 风格。
- `lib.rs`：注册 `pub mod identity;`。

> 对 B 级的 `XL_EnableDcdnWithToken/Session/VipCert` 与 `XL_SetTaskEquityToken`，因整型参数语义未确认，**本次不封装**，仅在本文档记录，待 dump/实测确认参数布局后再追加，以避免错误 ABI 导致运行时崩溃。

---

## 附录 A：`DownloadSDKProxy.dll` 完整导出（100）

```
XL_AddHttpHeaderField
XL_AddPeer
XL_AddServer
XL_BatchAddBTTracker
XL_BatchAddPeer
XL_BatchDiscardPeer
XL_BTStartUpload
XL_BTStopUpload
XL_ChangeBTTaskSubFileScheduler
XL_CreateBTTask
XL_CreateBTTask_V2
XL_CreateEmuleTask
XL_CreateHLSTask
XL_CreateMagnetTask
XL_CreateP2spTask
XL_CreateP2spTask_V2
XL_DeleteTask
XL_DisableDcdn
XL_DisableDcdnWithVipCert
XL_DisableFreeDcdn
XL_DiscardPeer
XL_DiscardServer
XL_EnableDcdn
XL_EnableDcdnWithSession
XL_EnableDcdnWithToken
XL_EnableDcdnWithVipCert
XL_EnableFreeDcdn
XL_FreeBTSubFileInfo
XL_FreeDownloadTaskDebugJsonInfo
XL_FreePlayInfo
XL_FreeTaskFlow
XL_FreeTaskProfileLog
XL_FreeUnRecvdRangeArray
XL_GetDownloadTaskDebugJsonInfo
XL_GetEstimateBandWidthInfo
XL_GetFilePlayInfo
XL_GetPeerId
XL_GetSubNetUploader
XL_GetSumOfRemotePeerBeBenefited
XL_GetTaskProfileLog
XL_GetUnRecvdRangeArray
XL_Init
XL_IsDownloadTaskCFGFileExit
XL_IsFileSizeSetterWorking
XL_LaunchFileAssistant
XL_QueryBTSubFileInfo
XL_QueryFreeDcdnAccelerate
XL_QueryGlobalStat
XL_QueryPlayInfo
XL_QueryTaskFlow
XL_QueryTaskIndex
XL_QueryTaskInfo
XL_RedirectOriginalResource
XL_ReleaseEstimateBandWidthInfo
XL_RenameP2spTaskFile
XL_SetAccelerateCertification
XL_SetAppGuid
XL_SetBTSubTaskIndex
XL_SetCacheSize
XL_SetDownloadSpeedLimit
XL_SetDownloadStrategy
XL_SetDownloadWindow
XL_SetEmuleTaskIndex
XL_SetForLiteLogRelease
XL_SetFreeDcdnDownloadSpeedLimit
XL_SetGlobalConnectionLimit
XL_SetGlobalExtInfo
XL_SetOriginConnectCount
XL_SetP2SPTaskIdxURL
XL_SetP2spTaskIndex
XL_SetProxy
XL_SetSubTaskConcurrency
XL_SetTaskDownloadSpeedLimit
XL_SetTaskEquityToken
XL_SetTaskExtInfo
XL_SetTaskExtStat
XL_SetTaskPriorityLevel
XL_SetTaskStatBatch
XL_SetTaskStrategy
XL_SetTaskStrategy_V2
XL_SetTaskTraceID
XL_SetTaskUserAgent
XL_SetTokenMode
XL_SetUploadSpeedLimit
XL_SetupNetDiskFetchTaskFlag
XL_SetupNetDiskFetchTaskFlag_V2
XL_SetupTaskAttributeFlags
XL_SetUserAgent
XL_SetUserInfo
XL_SetVideoDataCacheSize
XL_StartEstimateBandWidth
XL_StartTask
XL_StopTask
XL_UnInit
XL_UpdateBTTaskSubFileName
XL_UpdateDcdnWithVipCert
XL_UpdateNetDiscVODCachePath
XL_UpdateNetDiskTaskMinExpectedSpeed
XL_UpdateTaskCompensationTargetLevel
XL_UpdateTaskVideoByteRatio
```

## 附录 B：`DownloadSDK.dll` 完整导出（105）

（在 DownloadSDKProxy.dll 基础上多出：`XL_ContinueTask`、`XL_GetUniversalPlayInfo`、`XL_QueryAccelerateInfo`、`XL_SetHLSDownloadStrategy`、`XL_SetTaskExtraKey`、`XL_SetWindowsSleepInfo`；其余 99 个相同，此处不重复罗列，仅列出差异项）

```
XL_ContinueTask
XL_GetUniversalPlayInfo
XL_QueryAccelerateInfo
XL_SetHLSDownloadStrategy
XL_SetTaskExtraKey
XL_SetWindowsSleepInfo
```
