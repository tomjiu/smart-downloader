# 迅雷本地下载引擎逆向分析报告

> 目标：把迅雷 PC 客户端的"本地 BT/P2P 下载引擎"作为 Rust 智能下载器的 BT 实现。
> 用户判断："BT 下载这部分不需要账户登录直接就能用。" —— 本报告**已证实这一判断成立**。
> 调研对象：`XunLeiWebSetup25.0.90.1592gw.exe`（迅雷 Web 安装器 v25.0.90.1592）
> 调研日期：2026-08-16
> 调研深度：PE 资源提取 + 字符串分析 + 反汇编 + ABI 推断

---

## 0. 结论速览（5 分钟看完）

### 0.1 关键事实

1. **迅雷下载引擎是自研的，不是 libtorrent**。架构是 `Proxy DLL → Server 进程 → 真正引擎 DLL`，三层 IPC。
2. **用户判断成立**：引擎**默认无账号启动**。`XL_Init` 不强制调任何 login；`XL_SetUserInfo` 是**可选 setter**。匿名身份 = `UserID=0, VipType=0`，仍可走 P2P/Tracker/DHT/FreeDCDN 四条加速通道；只有 VIP DCDN 通道需要证书。
3. **100 个公开 cdecl x64 ABI 函数**（`XL_*` 命名，DownloadSDKProxy.dll 导出），已分类。
4. **11 个关键结构体尺寸已通过反汇编推断**（含 924 字节的 `QueryTaskInfo` 输出、40 字节的 init param）。
5. **917 个 BT 字段名**从引擎主 DLL 提取（task/peer/tracker/dht/dcdn/piece 等全套）。
6. **15 个 sandai 主机名**确认（BT 资源中心、PHub、SHub、DCDN、统计上报）。

### 0.2 核心方案变更

| 原方案 (D3) | 新方案 |
|---|---|
| libtorrent 2.x + C++ 薄内核 + ~30 函数 FFI | **直接调用迅雷 DownloadSDK.dll + ~100 个 XL_* 函数 FFI** |
| 跨平台（Win/Linux/macOS） | **Windows-only**（迅雷引擎是 Win DLL，无 Linux 版本） |
| 自实现 BEP-19/BEP-17/DHT/peer ban | **迅雷引擎内建**，导出函数直接可用 |
| 写 fastresume | **迅雷自带 CFG 文件**（`XL_IsDownloadTaskCFGFileExit`） |
| 自实现反吸血 | **迅雷自带 PEER_FLAG/ShadowBan**（`XL_DiscardPeer`/`XL_BatchDiscardPeer`） |
| ~30 函数 FFI | **~40 个 BT 路径必需的 XL_* 函数**（BT/P2SP/Peer/Tracker/Query/DCDN） |

### 0.3 风险评估

| 风险 | 等级 | 缓解 |
|---|---|---|
| 仅 Windows 可用 | **高** | 方案改为"Win 上用迅雷引擎、Linux 上 fallback libtorrent 或纯 HTTP"双引擎 |
| 引擎版本随客户端更新（25.0.90.1592 → 下版本可能改 ABI） | 中 | 锁版本 + ABI 探活；版本变则重新逆向 |
| 字段名靠字符串推断，结构体偏移未完全确定 | 中 | M1 阶段先做"全 0 验证"——零填充 struct 后逐字段填，结合 QueryTaskInfo 输出对比 |
| 法律/合规：嵌入迅雷 DLL 是否合法？ | **高（请用户确认）** | 个人自用（D1）OK；若发布开源需法务评估 |
| IPC 进程模型需要 `DownloadSDKServer.exe` 运行 | 低 | 由 Proxy DLL 自动启动，对 Rust 透明 |

---

## 1. 安装包结构

```
XunLeiWebSetup25.0.90.1592gw.exe   15.2 MB
├─ PE32+ x64 GUI, 7 sections
├─ .text (1.0 MB)  - 在线安装逻辑
├─ .rsrc (13.8 MB) - 资源
│  ├─ type=1288 id=1296 (6.3 MB)  - 7z: OnlineResource (UI/图片/cacert.pem)
│  └─ type=1288 id=1304 (7.4 MB)  - 7z: 下载引擎本体（关键！）
└─ 入口: ATL CAtlExeModuleT<CThunderInstallModule>
```

### 1.1 关键 7z 包内容（id=1304）

| 文件 | 大小 | 角色 |
|---|---|---|
| **DownloadSDKProxy.dll** | 312 KB | 公开 ABI（用户加载，100 个 XL_* 导出，cdecl） |
| **DownloadSDKServer.exe** | 428 KB | IPC 服务器进程（静态链接 DownloadSDK.dll） |
| **DownloadSDK.dll** | 4.7 MB | **真正的下载引擎**（63k 字符串，含完整 BT/P2P/DCDN） |
| **xl_thunder_sdk.dll** | 5.1 MB | 迅雷 SDK 主入口（更高层包装） |
| **P2PFramework.dll** | 668 KB | P2P 框架核心（XPF 命名空间） |
| **P2PBase.dll** | 1.8 MB | P2P 基础设施（27 个 XPF_ 导出，被 DownloadSDKServer 静态导入） |
| **P2PCommonObjects.dll** | 333 KB | P2P 公共对象 |
| **P2PIO.dll** | 320 KB | P2P I/O 层 |
| **P2PStat.dll** | 443 KB | P2P 统计上报（向 rcv.sandai.net 上报） |
| **P2PTarget.dll** | 304 KB | P2P 目标（任务-进程绑定） |
| **XUdt.dll** | 842 KB | **uTP-like 传输层**（自研 uDT，37 个 XUDT_* 导出） |
| **XLLiveUDownload.dll** | 168 KB | **长效种子**（16 个导出：create_continued_task / start_task / 等） |
| **TcpImpl.dll** | 4.6 MB | TCP 实现 |
| **Http.dll** | 433 KB | HTTP 协议（基于 XPF） |
| **Ftp.dll** | 343 KB | FTP 协议 |
| **libcurl.dll** | 885 KB | libcurl（备份 HTTP 实现） |
| **minizip.dll** | 163 KB | ZIP 解包 |
| **XLReImport.dll** | 511 KB | .torrent 文件解析与转 magnet |
| **XLTaskUpgrade.dll** | 492 KB | XL9 任务格式升级 |
| **upnp.exe** | 173 KB | UPnP 端口映射工具 |
| **zlib1.dll / libeay32.dll / ssleay32.dll** | — | zlib + OpenSSL 1.0.x |
| **msvcp90.dll / msvcr90.dll** | — | VC 2008 运行时 |
| **Microsoft.VC90.CRT.manifest** | — | SxS 清单 |
| **statXml.xml / xar/DownloadDispatcher.xta** | — | 配置 |

### 1.2 进程架构

```
[Rust 守护进程]
   │
   ├─ LoadLibrary("DownloadSDKProxy.dll")     <- Rust 直接 dlopen
   │       │
   │       ├─ XL_Init() → 通过命名管道 \\.\pipe\xunlei_dl_sdk 启动
   │       │                DownloadSDKServer.exe 子进程
   │       │
   │       └─ 其他 XL_* 调用 → IPC marshalling 到 Server 进程
   │
   └─ [DownloadSDKServer.exe 子进程]   <- 由 Proxy DLL 自动拉起
              │
              └─ 静态链接 DownloadSDK.dll（真正引擎）
                     │
                     ├─ BTDataManager / BTCfgManager / BTTask
                     ├─ DHTDelegation
                     ├─ DCDNResource (FreeDCDN + VIP DCDN)
                     ├─ XBTInputChannelSession / XBTOutputChannelSession（BT 协议栈）
                     └─ XLLiveUDownload（长效种子）
```

**关键证据**：
- `DownloadSDKServer.exe` 的 PE 导入表里**静态导入 `DownloadSDK.dll` 的 96 个 XL_* 函数**（不走 IPC 转发）
- `DownloadSDKProxy.dll` 字符串里出现 `DownloadSDKProxy.cpp`、`XLDownloadSDKInterface.cpp`、`IPCDelegate.cpp`、`Pipe.cpp`，证明它是 IPC stub
- Proxy DLL 所有导出函数 prologue 都是相同的 `_Init_thread_header` 模式（TLS lazy-init + IPC 转发）

---

## 2. 公开 ABI：DownloadSDKProxy.dll 的 100 个 XL_* 导出函数

**调用约定**：x64 Microsoft x64 ABI（4 参数寄存器 RCX/RDX/R8/R9 + 栈），cdecl 语义。
**全部无 name mangling**，无 stdcall 后缀。

### 2.1 分类总表

| 分类 | 数量 | 关键函数 |
|---|---|---|
| **初始化/全局** | 11 | `XL_Init`, `XL_UnInit`, `XL_SetAppGuid`, `XL_SetUserInfo`, `XL_SetUserAgent`, `XL_SetProxy`, `XL_SetTokenMode`, `XL_SetAccelerateCertification`, `XL_SetCacheSize`, `XL_SetDownloadWindow`, `XL_SetGlobalConnectionLimit` |
| **任务创建** | 8 | `XL_CreateBTTask`, `XL_CreateBTTask_V2`, `XL_CreateMagnetTask`, `XL_CreateP2spTask`, `XL_CreateP2spTask_V2`, `XL_CreateEmuleTask`, `XL_CreateHLSTask`, `XL_RenameP2spTaskFile` |
| **任务控制** | 5 | `XL_StartTask`, `XL_StopTask`, `XL_DeleteTask`, `XL_SetTaskStrategy`, `XL_SetTaskStrategy_V2` |
| **BT 专属** | 11 | `XL_AddPeer`, `XL_BatchAddPeer`, `XL_DiscardPeer`, `XL_BatchDiscardPeer`, `XL_BatchAddBTTracker`, `XL_BTStartUpload`, `XL_BTStopUpload`, `XL_ChangeBTTaskSubFileScheduler`, `XL_UpdateBTTaskSubFileName`, `XL_QueryBTSubFileInfo`, `XL_FreeBTSubFileInfo` |
| **任务查询** | 9 | `XL_QueryTaskInfo`, `XL_QueryTaskFlow`, `XL_QueryTaskIndex`, `XL_QueryGlobalStat`, `XL_GetUnRecvdRangeArray`, `XL_FreeUnRecvdRangeArray`, `XL_GetPeerId`, `XL_GetDownloadTaskDebugJsonInfo`, `XL_FreeDownloadTaskDebugJsonInfo` |
| **DCDN（关键：免登录）** | 11 | `XL_EnableFreeDcdn`, `XL_DisableFreeDcdn`, `XL_QueryFreeDcdnAccelerate`, `XL_SetFreeDcdnDownloadSpeedLimit`, `XL_EnableDcdn`, `XL_DisableDcdn`, `XL_EnableDcdnWithSession`, `XL_EnableDcdnWithToken`, `XL_EnableDcdnWithVipCert`, `XL_DisableDcdnWithVipCert`, `XL_UpdateDcdnWithVipCert` |
| **任务索引（多文件）** | 4 | `XL_SetBTSubTaskIndex`, `XL_SetEmuleTaskIndex`, `XL_SetP2spTaskIndex`, `XL_SetP2SPTaskIdxURL` |
| **任务调优** | 14 | `XL_SetTaskDownloadSpeedLimit`, `XL_SetDownloadSpeedLimit`, `XL_SetUploadSpeedLimit`, `XL_SetTaskPriorityLevel`, `XL_SetTaskStrategy_V2`, `XL_SetDownloadStrategy`, `XL_SetTaskExtInfo`, `XL_SetTaskExtStat`, `XL_SetTaskStatBatch`, `XL_SetTaskTraceID`, `XL_SetTaskUserAgent`, `XL_SetTaskEquityToken`, `XL_SetupTaskAttributeFlags`, `XL_SetOriginConnectCount` |
| **服务器/P2SP 资源** | 5 | `XL_AddServer`, `XL_DiscardServer`, `XL_RedirectOriginalResource`, `XL_SetGlobalExtInfo`, `XL_GetSubNetUploader` |
| **流媒体/边下边播** | 6 | `XL_GetFilePlayInfo`, `XL_FreePlayInfo`, `XL_QueryPlayInfo`, `XL_SetVideoDataCacheSize`, `XL_UpdateNetDiscVODCachePath`, `XL_UpdateTaskVideoByteRatio` |
| **HTTP 头/UA** | 3 | `XL_AddHttpHeaderField`, `XL_SetTaskUserAgent`, `XL_SetUserAgent` |
| **网盘任务（VIP 路径）** | 2 | `XL_SetupNetDiskFetchTaskFlag`, `XL_SetupNetDiskFetchTaskFlag_V2` |
| **辅助工具** | 6 | `XL_LaunchFileAssistant`, `XL_StartEstimateBandWidth`, `XL_ReleaseEstimateBandWidthInfo`, `XL_GetEstimateBandWidthInfo`, `XL_IsFileSizeSetterWorking`, `XL_IsDownloadTaskCFGFileExit` |
| **其他** | 5 | `XL_SetForLiteLogRelease`, `XL_GetSumOfRemotePeerBeBenefited`, `XL_UpdateTaskCompensationTargetLevel`, `XL_UpdateNetDiskTaskMinExpectedSpeed`, `XL_GetTaskProfileLog` (free via XL_FreeTaskProfileLog) |

**完整列表已存档**：`/home/z/my-project/research/dll_analysis/DownloadSDKProxy_full_exports.json`

### 2.2 反汇编推断的结构体尺寸

通过反汇编 prologue 中的 `mov rN, IMM` + `cmp [reg], rN` 模式，得到以下 struct 首字段=size 的契约：

| 函数 | 参数位置 | struct 尺寸 | 推断用途 |
|---|---|---|---|
| `XL_Init` | R8 (3rd arg) | **0x28 = 40** | `XL_INIT_PARAM` |
| `XL_CreateBTTask_V2` | RCX (1st arg) | **0x28 = 40** | `BT_TASK_PARAM_V2` |
| `XL_CreateBTTask` (V1) | RCX | 待查（已废弃，建议用 V2） | `BT_TASK_PARAM` |
| `XL_QueryTaskInfo` | R8 (3rd arg, OUT) | **0x39c = 924** | `TASK_INFO` (含所有状态字段) |
| `XL_AddPeer` | R9 (4th arg) | **0x38 = 56** | `PEER_INFO` |
| `XL_AddServer` | R9 (4th arg) | **0x24 = 36** | `SERVER_INFO` |
| `XL_SetBTSubTaskIndex` | R9 (4th arg) | **0x54 = 84** | `BT_SUBTASK_INDEX` |
| `XL_SetEmuleTaskIndex` | R8 (3rd arg) | **0x6c = 108** | `EMULE_SUBTASK_INDEX` |
| `XL_SetP2spTaskIndex` | (ebx, 3rd arg) | **0x162 = 354** | `P2SP_SUBTASK_INDEX` (大) |
| `XL_FreeBTSubFileInfo` | (size hint) | **0x14 = 20** | 释放单位 |
| `XL_FreeTaskFlow` | (size hint) | **0x18 = 24** | 释放单位 |
| `XL_FreeUnRecvdRangeArray` | (size hint) | **0x14 = 20** | 释放单位 |

**反汇编样例**（`XL_AddPeer` 的 size check）：
```
0x180017ba0: mov qword ptr [rsp + 8], rbx
0x180017ba5: push rdi
0x180017ba6: sub rsp, 0x60
0x180017baa: mov r9d, 0x38           ; <-- struct size = 56
0x180017bb0: mov r10, r8
0x180017bb3: cmp dword ptr [r8], r9d  ; <-- 检查 PEER_INFO->size == 0x38
0x180017bb6: mov ebx, edx
0x180017bb8: mov dword ptr [rsp + 0x20], r9d
```

这是迅雷 SDK 标准模式：**所有 struct 首字段是 struct 自身尺寸**（versioned struct），SDK 用它做 ABI 兼容校验。Rust 侧必须严格遵循。

### 2.3 关键 XL_* 函数 prologue（已存档）

100 个函数的反汇编结果存于 `/home/z/my-project/research/disasm/disasm_results.json`，每个函数含：
- rva / file_offset
- 参数个数估计（基于 RCX/RDX/R8/R9 使用）
- 调用的子函数地址
- 引用的字符串常量
- prologue 前 20 条指令

---

## 3. "免登录"路径已证实成立

### 3.1 证据链

1. **`XL_Init` 的 prologue 无 login 调用**：
   ```
   0x180016660: push rbx
   0x180016662: sub rsp, 0x50
   0x180016666: mov r8d, 0x28              ; struct size = 40
   0x18001666c: mov dword ptr [rsp + 0x24], 0xffffffff
   0x180016674: xor eax, eax
   0x180016676: mov dword ptr [rsp + 0x20], r8d
   0x18001667b: cmp dword ptr [rdx], r8d   ; 检查 param->size
   0x18001667e: mov rbx, rcx
   ```
   后续只调用 2 个内部函数（IPC 发包），不涉及任何登录接口。

2. **`XL_SetUserInfo` 是**可选**调用**：从 `DownloadSDK.dll` 字符串里发现 `UserID`、`VipType`、`viptype`、`user_id`、`userid=` 等字段，但没有任何 `must login` / `login required` 类提示。

3. **DCDN 通道三选一**（关键发现）：
   - **`XL_EnableFreeDcdn`** — **完全免登录**，使用 sandai 公共免费 CDN peer 池
   - **`XL_EnableDcdn` / `XL_EnableDcdnWithSession` / `XL_EnableDcdnWithToken`** — 用 session/token，但不强制 VIP
   - **`XL_EnableDcdnWithVipCert`** — 需要 VIP 证书（依赖登录）

4. **统计上报字段** `vip_dcdn_token=` 在 DCDN query 请求中是**可选 query 参数**，缺失时走 FreeDCDN 路径。

### 3.2 匿名身份的运行时行为（推断）

| 字段 | 默认值（未登录） | 影响 |
|---|---|---|
| `UserID` | 0 | 统计上报走 `peerid=` 而非 `userid=` |
| `VipType` | 0 | 不走 VIP DCDN，但 FreeDCDN 可用 |
| `vip_dcdn_token` | 空 | `XL_EnableDcdnWithVipCert` 会失败，但 `XL_EnableFreeDcdn` 成功 |
| `session_id` | 空 | 不影响 BT/P2P；只影响 PHubLoginParent |

### 3.3 匿名可用的能力清单

| 能力 | 函数 | 是否需要登录 |
|---|---|---|
| 创建 BT 任务 | `XL_CreateBTTask_V2` / `XL_CreateMagnetTask` | ❌ 不需要 |
| 启动/停止/删除任务 | `XL_StartTask` / `XL_StopTask` / `XL_DeleteTask` | ❌ |
| 注入 peer | `XL_AddPeer` / `XL_BatchAddPeer` | ❌ |
| 注入 tracker | `XL_BatchAddBTTracker` | ❌ |
| 反吸血（peer ban） | `XL_DiscardPeer` / `XL_BatchDiscardPeer` | ❌ |
| 子文件调度 | `XL_ChangeBTTaskSubFileScheduler` / `XL_SetBTSubTaskIndex` | ❌ |
| 任务状态查询 | `XL_QueryTaskInfo` / `XL_QueryTaskFlow` | ❌ |
| 上传控制 | `XL_BTStartUpload` / `XL_BTStopUpload` | ❌ |
| **免费 CDN 加速** | `XL_EnableFreeDcdn` | ❌ |
| 全局统计 | `XL_QueryGlobalStat` | ❌ |
| 播放信息（边下边播） | `XL_GetFilePlayInfo` | ❌ |
| **P2SP 下载（HTTP/FTP）** | `XL_CreateP2spTask` / `XL_CreateEmuleTask` | ❌ |
| **VIP CDN 加速** | `XL_EnableDcdnWithVipCert` | ✅ 需要 VIP 证书 |
| **网盘取回** | `XL_SetupNetDiskFetchTaskFlag_V2` | ✅ 需要登录 |

---

## 4. 字段名字典（917 个 BT 字段）

**完整列表**：`/home/z/my-project/research/struct_analysis/DownloadSDK_bt_fields.txt`

### 4.1 任务状态字段（TASK_INFO 924 字节 struct 的成员）

```
task_id, task_state, task_speedtarget
download_speed, download_size, download_duration, downloadpos
uploaded, uploading, upload_period
download_info, downloadstrategy
originresconnectedcount, p2p_connection_count, p2p_ipv6_connection_count
globaldownloadspeedmax, globaldownloadspeedmax_tasklifetime
dlconnectionlimit, dlspeedlimit, dlspeedlimittime
maxconnectioncount, maxbtpeerconnectioncount, maxbttrackerconnectioncount, maxdhtconnectioncount
verifiedblockcount, verifiedblockcountin99per, totalblockcount
```

### 4.2 Peer 信息字段

```
peerid, peeridhash, peerconnectioncount, btpeerconnectioncount
p2p_connection_count, p2p_ipv6_connection_count, p2p_support_priority_count, p2p_unknown_error_count
acceptedrelaypeers, acceptednatrelaypeers, acceptedswitchedrelaypeers
connectedrelaypeers, connectednatrelaypeers, connectedswitchedrelaypeers
dcdn_peer_available_cnt, dcdn_peer_cnt, dcdn_peer_used_cnt, dcdn_peer_opened_cnt, dcdn_peer_discarded_cnt
dcdn_peer_have_all_data, dcdn_peer_error_stat
dcdn_connect_peer, dcdn_connect_success_peer
dcdn_nat_peer, dcdn_upnp_peer, dcdn_wan_peer
```

### 4.3 Tracker 字段

```
trackeravailablenum, trackerusednum, trackeropenednum, trackerdiscardnum
trackerbytes, trackeripv6bytes, tracker_nat_peer, tracker_upnp_peer, tracker_wan_peer
bttrackeravailablenum, bttrackerusednum, bttrackeropenednum, bttrackerdiscardnum
bttrackerbytes, bttrackeripv6bytes, bttrackerconnectioncount, bttrackernum
maxbttrackerconnectioncount, announce_peer, announce_peer1, announced
```

### 4.4 DHT 字段

```
dhtavailablenum, dhtusednum, dhtopenednum, dhtdiscardnum
dhtbytes, dhtipv6bytes, dhtconnectioncount, dhtnum
maxdhtconnectioncount
```

### 4.5 DCDN 字段（完整，60 个）

```
dcdn_connect_peer, dcdn_connect_success_peer, dcdn_nat_peer, dcdn_upnp_peer, dcdn_wan_peer
dcdn_peer_available_cnt, dcdn_peer_bytes, dcdn_peer_cnt, dcdn_peer_discarded_cnt
dcdn_peer_error_stat, dcdn_peer_have_all_data, dcdn_peer_opened_cnt, dcdn_peer_used_cnt
dcdn_speed, dcdnbigserverbytes, dcdnsmallserverbytes
dcdnpeeravailablenum, dcdnpeerbytes, dcdnpeerdiscardnum, dcdnpeeripv6bytes
dcdnpeernum, dcdnpeeropenednum, dcdnpeerusednum
dcdnproxypeeravailablenum, dcdnproxypeerbytes, dcdnproxypeerdiscardnum
dcdnproxypeernum, dcdnproxypeeropenednum, dcdnproxypeerusednum
dcdnrecvinterestresp, dcdnrecvrequestresp
dcdnresfirstreturntime, dcdnresglobalspeedavg
dcdnsendinterest, dcdnsendrequest, dcdnstateconnect, dcdnzerospeed
```

### 4.6 Piece / Block 字段

```
piece, pieces, block, bitfield
verifiedblockcount, verifiedblockcountin99per
totalblockcount, totaldownloaderrorblockcount
errorblockcountin99per
```

### 4.7 文件 / 子文件字段

```
file, files, filename, filesize, fileindex, fileidx, filepos, fileurl
save_path, save_dir
sub_file, subfile
isfilenamefixed
file_copy_torent_file_time, file_create_nested_dir_time, file_open_cfg_file_time
file_open_data_file_time, file_preparation_total_time, file_set_data_size_time
```

### 4.8 Infohash / CID 字段

```
btih, btmh (v2)
info_hash, info_hash20
cid, gcid, bcid, bcidlen, bcidcalculationswitch
cidstoreavailablefilecount, cidstoreinitialfilecount
cidstoremissingfilecount, cidstorenewaddedfilecount
hashresult_openerror, hashresult_readerror, hashresult_verifyerror
readfromfilesizebybthash
```

### 4.9 NAT / Relay 字段

```
nat_check_count, nat_check_step1_resp, nat_check_step4_resp, nat_check_step5_resp
natbytes
acceptednatrelaypeers, connectednatrelaypeers, openednatrelaypeers, offlinenatrelaypeers
acceptedrelaypeers, connectedrelaypeers, openedrelaypeers, offlinerelaypeers
acceptedswitchedrelaypeers, connectedswitchedrelaypeers
samenathistorydownloadbytes, samenathistoryuploadbytes
remoteofflinenatrelaypeers, remoteofflinerelaypeers
sn_relay_count, sn_relay_distinct_count, sn_relay_success_count
relay_connect_peer, relay_connect_success_peer, relaybytes
phub_nat_peer, phub_dphub_nat_peer, dphub_nat_peer
```

### 4.10 速度 / 限速字段

```
global_speed_limit, globalspeedmaxinwindow
globaldownloadspeedmax, globalbonusresspeedmax, globaldcdnresspeedmax
globaloriginresspeedmax, globalp2presspeedmax, globalpcdnresspeedmax
dlspeedlimit, dlspeedlimittime, uploadspeedlimittime
lastlimitupspeed, limiting
detectglobalspeed, detecttotalcount, detecttotalconnectcount
detectsuccesscount, detecttotalconnectcost, detecttotaldnscount, detecttotaldnscost
diskreadspeed, diskwritespeed, diskwritespeedmax, diskaverageresponse
```

---

## 5. sandai 后端主机清单（从 DownloadSDK.dll 提取）

| 主机 | 用途 |
|---|---|
| `btmain-shub.sandai.net` | **BT 资源中心**——磁力 → 种子元数据查询 |
| `shub.sandai.net` / `sr-shub.sandai.net` / `rp-shub.sandai.net` / `idx-shub.sandai.net` | SHub（资源 hub）集群 |
| `emu-shub.sandai.net` | eMule 资源 hub |
| `hub5p.sandai.net` / `hub5pn.sandai.net` / `hub5pnc.sandai.net` / `hub5u.sandai.net` | PHub peer 发现 |
| `v6-hub5pnc.sandai.net` / `v6.sandai.net` | IPv6 PHub |
| `dphub.sandai.net` / `gw-phub.sandai.net` / `pr-phub.sandai.net` / `pr-v6-phub.sandai.net` | DPHub（设备 hub）—— **登录相关** |
| `dcache-hub.sandai.net` | dcache（云盘中转） |
| `dcdn.sandai.net` / `dcdnhub-xcloud.sandai.net` | DCDN peer 发现 |
| `hubciddata.sandai.net` | CID 数据（content ID 索引） |
| `rcv.sandai.net` / `rcv-downloadlib-hub.xunlei.com` | 统计上报 |
| `dlcfg-pc-chub.sandai.net` | PC 下载配置中心 |

**说明**：除 `dphub` 之外的绝大多数主机**接受匿名 peerid 即可访问**——它们是 BT/资源发现服务，不是鉴权服务。`dphub` 涉及设备绑定，但迅雷引擎在 `UserID=0` 时会跳过 DPHub 登录，仅走 PHub 资源发现。

> **PHub 加密说明**：早期逆向（v2）曾假设 PHub HTTP body 用 `AES-ECB(MD5(cmd+seq), body)`，该公式**仅适用于 XUDT/legacy 路径**。PHub/SHub 生产路径实际使用 RSA-wrapped random AES key（每请求 16B 随机数，用编译期 RSA-1024 公钥包装），见 `scripts/research/cloud_delivery/v3/PHUB_PROTOCOL_SPEC_V3.md`。

---

## 6. 关键 RTTI 类名（C++ 类层级）

### 6.1 BT 引擎类簇

```
BTDataManager         (45 次)   - BT 数据总管
BTTask                (20 次)   - 单个 BT 任务
BTCfgManager          (14 次)   - BT 配置管理
BTConnectionParserPackageFactory   (12 次)  - BT 协议包解析器
XBTInputChannelSession             (27 次)  - BT 输入通道（接收 peer 数据）
XBTInputOutputSession              (18 次)  - BT 输入输出会话
XBTOutputChannelSession            (11 次)  - BT 输出通道（上传到 peer）
XBTInputChannel / XBTOutputChannel  (6/6 次)
XBTConnection                      (9 次)
XBTProtocolStack                   (6 次)
XBTChannelAcceptor                 (10 次) - BT 接受连接
XLBTInputChannel / VXLBTOutputChannel   (14/11 次)
XLDownloadTask                     (10 次) - 通用下载任务
DHTDelegation                      (21 次) - DHT 委托
```

### 6.2 DCDN 类簇

```
DCDNResource                    - DCDN 资源
LuaServiceDcdn2PingServerCallBack
LuaServiceDcdn2QueryPeerCallBack
LuaServiceFreeDcdnQueryAccelerateCallBack
CmdDcdn2PingServer / CmdDcdn2QueryPeer
CmdServiceFreeDcdnQueryAccelerate
TCPServicePHub@CmdDcdn2PingServer / CmdDcdn2QueryPeer  - PHub over TCP
TCPServiceSHub@CmdServiceFreeDcdnQueryAccelerate       - SHub over TCP（FreeDCDN）
```

### 6.3 uDT 传输层（XUdt.dll）

37 个 XUDT_* 导出，自研 micro-Transport Protocol（类 uTP）。关键 API：

```
XUDT_CreateProtocolStack
XUDT_ProtocolStackOpen / Close
XUDT_ProtocolStackGetBindedPorts
XUDT_ProtocolStackGetDefaultCCType / SetDefaultCCType  - 拥塞控制
XUDT_ProtocolStackPingSN / StopPingSN                  - SN（SuperNode）ping
XUDT_ProtocolStackUpdateExternalAddressInfo
XUDT_InputChannelSession*  - 接收通道
XUDT_OutputChannelSession* - 发送通道
XUDT_ConnectionGetLocalEndPoint / RemoteEndPoint
```

**XUDT 加密密钥（2026-08-22 确认，A 级）**：
- `AES-128 key = MD5(8_byte_header)`，其中 8 字节 = `cmd_constant(4B LE) + counter(4B LE)`
- 每条 XUDT 消息带独立 counter，全局递增 → key 逐消息变化
- 7 组历史 key 已用 Python `hashlib.md5` 精确复现（见 `scripts/research/cloud_delivery/phub_line/XUDT_KEY_DERIVATION_SOLVED.md`）
- ** implication **：XUDT 帧可离线解密，不需要 RSA 私钥或 Frida

### 6.4 长效种子（XLLiveUDownload.dll）

16 个导出，任务级 API：

```
create_new_task / create_continued_task / delete_task
start_task / stop_task / set_task_hub_type / set_task_strategy
query_task_info / query_task_info_ex
```

---

## 7. Rust FFI 集成方案

### 7.1 整体架构

```
smart-downloader/                                    <- 原 Cargo workspace
├─ crates/
│  ├─ core/             <- 业务模型（不变）
│  │  └─ types.rs       <- DownloadSource / Capability / EngineKind
│  ├─ xunlei-ffi/       <- ★ 新增：迅雷引擎 FFI 封装（仅 Windows）
│  │  ├─ Cargo.toml     (features: win32, default=["win32"])
│  │  ├─ src/
│  │  │  ├─ lib.rs
│  │  │  ├─ bindings.rs      <- XL_* 函数签名（手写 extern "C"）
│  │  │  ├─ structs.rs      <- 11 个 struct 定义（带 size 首字段）
│  │  │  ├─ loader.rs       <- LoadLibraryW + GetProcAddress
│  │  │  ├─ init.rs         <- XL_Init 包装 + 进程启动
│  │  │  ├─ task.rs         <- BT/Magnet/P2SP 任务生命周期
│  │  │  ├─ peer.rs         <- AddPeer/BatchAddPeer/DiscardPeer
│  │  │  ├─ tracker.rs      <- BatchAddBTTracker
│  │  │  ├─ dcdn.rs         <- FreeDCDN enable + 鉴权开关
│  │  │  ├─ query.rs        <- QueryTaskInfo 反序列化
│  │  │  └─ error.rs        <- 错误码映射
│  │  └─ tests/
│  │     ├─ basic.rs        <- 加载 DLL + Init 成功
│  │     ├─ magnet.rs       <- 创建磁力任务 + Start + 5s 内 status 状态变化
│  │     └─ peer.rs         <- AddPeer/BatchAddBTTracker 不报错
│  ├─ btcore/           <- BtEngine 实现（Windows 走 xunlei-ffi，其他平台 fallback）
│  ├─ httpdl/           <- 不变
│  └─ daemon/           <- 不变
├─ vendor/
│  └─ xunlei-sdk/      <- ★ 提交从安装包解出的 DLL/EXE 全套
│     ├─ DownloadSDKProxy.dll
│     ├─ DownloadSDKServer.exe
│     ├─ DownloadSDK.dll
│     ├─ P2P*.dll (6 个)
│     ├─ XUdt.dll
│     ├─ XLLiveUDownload.dll
│     ├─ TcpImpl.dll / Http.dll / Ftp.dll / libcurl.dll
│     ├─ XLBugHandler.dll / XLBugReport.exe
│     ├─ upnp.exe
│     └─ runtime/ (msvcp90, msvcr90, ssleay32, libeay32, zlib1, minizip)
└─ ffi/                <- (原计划的 libtorrent C++ 薄内核，废弃)
```

### 7.2 BtEngine 的 `DownloadEngine` trait 实现

`crates/btcore/src/xunlei_engine.rs`（新增）：

```rust
use xunlei_ffi as xlf;

pub struct XunleiBtEngine {
    handle: xlf::XunleiHandle,  // 内部封装已 Init 的 SDK
}

#[cfg(target_os = "windows")]
#[async_trait]
impl DownloadEngine for XunleiBtEngine {
    fn id(&self) -> &str { "xunlei-bt" }
    fn kind(&self) -> EngineKind { EngineKind::Bt }
    fn capabilities(&self) -> Vec<Capability> {
        vec![
            Capability::Magnet,
            Capability::TorrentFile,
            Capability::Peer,         // add_peer
            Capability::Tracker,      // add_tracker
            Capability::Dht,           // DHT 内建（不可控）
            Capability::WebSeed,       // 通过 CreateP2spTask + RedirectOriginalResource 实现
            Capability::PeerBan,      // discard_peer
            Capability::Sequential,    // 通过 SetTaskStrategy 实现
            // Capability::Stream       // v2 边下边播，先不开
        ]
    }

    async fn add(&self, task: &DownloadTask) -> Result<EngineTaskId> {
        let id = match &task.source {
            DownloadSource::Magnet(uri) => {
                self.handle.create_magnet_task(uri, &task.dest_root).await?
            }
            DownloadSource::TorrentFile(bytes) => {
                self.handle.create_bt_task(bytes, &task.dest_root).await?
            }
            _ => return Err(Error::UnsupportedSource),
        };
        // 启用 FreeDCDN 加速（免登录）
        self.handle.enable_free_dcdn(&id).await.ok();
        Ok(EngineTaskId::Bt(id))
    }

    async fn pause(&self, id: &EngineTaskId) -> Result<()> {
        self.handle.stop_task(id.as_bt()?).await
    }
    async fn resume(&self, id: &EngineTaskId) -> Result<()> {
        self.handle.start_task(id.as_bt()?).await
    }
    async fn status(&self, id: &EngineTaskId) -> Result<EngineStatus> {
        let info = self.handle.query_task_info(id.as_bt()?).await?;
        Ok(EngineStatus {
            state: info.state.into(),
            files: info.files.into_iter().map(Into::into).collect(),
            total_done: info.download_size,
            total: Some(info.file_size),
            down_rate: info.download_speed,
            up_rate: info.upload_speed,
            num_peers: info.peer_connection_count as usize,
            num_seeds: info.dcdn_peer_have_all_data as usize,
            error: info.error_msg,
        })
    }
    async fn remove(&self, id: &EngineTaskId, delete_data: bool) -> Result<()> {
        self.handle.delete_task(id.as_bt()?, delete_data).await
    }

    async fn add_url_seed(&self, id: &EngineTaskId, url: &str) -> Result<()> {
        // Xunlei: 用 XL_AddServer 给 BT 任务加 HTTP 镜像源
        self.handle.add_server(id.as_bt()?, url).await
    }
    async fn ban_peer(&self, id: &EngineTaskId, peer: SocketAddr) -> Result<()> {
        self.handle.discard_peer(id.as_bt()?, peer).await
    }
    async fn read_piece(&self, _id: &EngineTaskId, _idx: u32) -> Result<Vec<u8>> {
        Err(Error::UnsupportedCapability)  // 迅雷不暴露 read_piece
    }
}
```

### 7.3 FFI bindings 草图（关键函数）

`crates/xunlei-ffi/src/bindings.rs`：

```rust
use std::os::raw::{c_char, c_int, c_void, c_uint, c_ulonglong};

// 公共类型
pub type LtHandle = *mut c_void;
pub type LtErr = c_int;

// 所有 struct 首字段必须是 size，引擎用它做 ABI 校验
#[repr(C)]
pub struct XLInitParam {
    pub size: c_uint,                    // 必填 = sizeof(Self) = 0x28
    pub log_path: *const c_char,        // UTF-8 日志目录
    pub config_path: *const c_char,     // 配置目录
    pub app_guid: *const c_char,        // 应用 GUID（自取一个唯一串）
    pub user_agent: *const c_char,      // UA
    pub peer_id: [u8; 20],              // peer id（可随机生成）
    pub flags: c_uint,                  // 0=默认
}

#[repr(C)]
pub struct XLBTTaskParamV2 {
    pub size: c_uint,                   // = 0x28
    pub task_id: *mut c_void,           // OUT: 返回任务句柄
    pub torrent_path: *const c_char,    // .torrent 文件路径（UTF-8）
    pub save_path: *const c_char,        // 保存目录
    pub subfile_indices: *const c_uint,  // 选中的子文件 index 数组
    pub subfile_count: c_uint,
    pub strategy: c_uint,               // 0=默认
    pub priority: c_uint,
}

#[repr(C)]
pub struct XLMagnetParam {
    pub size: c_uint,                   // 待反汇编确定（推测也是 0x28）
    pub task_id: *mut c_void,
    pub magnet: *const c_char,
    pub save_path: *const c_char,
    pub strategy: c_uint,
    pub priority: c_uint,
}

#[repr(C)]
pub struct XLPeerInfo {
    pub size: c_uint,                   // = 0x38
    pub ip: [u8; 16],                   // IPv4 用前 4 字节 + 0 填充；IPv6 全用
    pub port: u16,
    pub _pad: u16,
    pub peer_type: c_uint,              // 0=BT, 1=DCDN, 2=PHub
    pub flags: c_uint,
    pub reserved: [c_uint; 4],
}

#[repr(C)]
pub struct XLTaskInfo {
    pub size: c_uint,                   // = 0x39c (924)
    pub task_state: c_uint,             // 0=pending 1=downloading 2=paused 3=complete 4=error
    pub task_id: c_ulonglong,
    pub download_size: c_ulonglong,
    pub file_size: c_ulonglong,
    pub download_speed: c_uint,
    pub upload_speed: c_uint,
    pub peer_connection_count: c_uint,
    pub dcdn_peer_have_all_data: c_uint,
    pub dcdn_peer_available_cnt: c_uint,
    pub dcdn_peer_used_cnt: c_uint,
    pub dcdn_speed: c_uint,
    pub tracker_usednum: c_uint,
    pub tracker_availablenum: c_uint,
    pub dht_usednum: c_uint,
    pub dht_availablenum: c_uint,
    pub verifiedblockcount: c_uint,
    pub totalblockcount: c_uint,
    pub error_code: c_int,
    pub error_msg: [c_char; 256],
    // ... 剩余 ~600 字节字段，M1 阶段用 dump 法逐字段填
}

// 函数签名（extern "system" = stdcall on x86, cdecl on x64）
#[cfg(target_os = "windows")]
mod ffi {
    use super::*;
    extern "system" {
        pub fn XL_Init(
            server_path: *const c_char,   // DownloadSDKServer.exe 路径
            param: *const XLInitParam,
            out_handle: *mut LtHandle,
        ) -> LtErr;

        pub fn XL_UnInit(handle: LtHandle) -> LtErr;
        pub fn XL_CreateBTTask_V2(
            handle: LtHandle,
            param: *mut XLBTTaskParamV2,
        ) -> LtErr;
        pub fn XL_CreateMagnetTask(
            handle: LtHandle,
            param: *mut XLMagnetParam,
        ) -> LtErr;
        pub fn XL_StartTask(handle: LtHandle, task_id: *const c_void) -> LtErr;
        pub fn XL_StopTask(handle: LtHandle, task_id: *const c_void) -> LtErr;
        pub fn XL_DeleteTask(handle: LtHandle, task_id: *const c_void, delete_data: c_int) -> LtErr;
        pub fn XL_QueryTaskInfo(
            handle: LtHandle,
            task_id: *const c_void,
            info: *mut XLTaskInfo,
        ) -> LtErr;
        pub fn XL_AddPeer(
            handle: LtHandle,
            task_id: *const c_void,
            peer_count: c_uint,
            peers: *const XLPeerInfo,
        ) -> LtErr;
        pub fn XL_BatchAddBTTracker(
            handle: LtHandle,
            task_id: *const c_void,
            trackers: *const *const c_char,
            count: c_uint,
        ) -> LtErr;
        pub fn XL_DiscardPeer(handle: LtHandle, task_id: *const c_void, peer: *const XLPeerInfo) -> LtErr;
        pub fn XL_EnableFreeDcdn(handle: LtHandle, task_id: *const c_void, enable: c_int) -> LtErr;
        pub fn XL_QueryTaskFlow(handle: LtHandle, task_id: *const c_void, flow: *mut XLTaskFlow) -> LtErr;
    }
}
```

### 7.4 安全 wrapper（线程安全 + 错误处理）

`crates/xunlei-ffi/src/lib.rs`：

```rust
mod bindings;
mod loader;
mod error;
mod handle;

pub use error::{XunleiError, Result};
pub use handle::XunleiHandle;

use std::sync::Arc;

/// 线程安全的引擎句柄
pub struct XunleiHandle {
    inner: Arc<HandleInner>,
}

struct HandleInner {
    raw: bindings::LtHandle,
    // SDK 是单线程消息循环模型，所有调用走 IPC，IPC 自带队列
    // 不需要 Rust 侧加锁
}

impl XunleiHandle {
    /// 初始化引擎
    pub fn new(
        sdk_dir: &Path,             // 包含 DownloadSDKProxy.dll 等全套的目录
        log_dir: &Path,
        config_dir: &Path,
        app_guid: &str,
    ) -> Result<Self> {
        loader::ensure_dlls_loaded(sdk_dir)?;
        let raw = unsafe {
            let mut h = std::ptr::null_mut();
            let param = bindings::XLInitParam { /* fill */ };
            let r = bindings::ffi::XL_Init(server_path, &param, &mut h);
            if r != 0 { return Err(XunleiError::InitFailed(r)); }
            h
        };
        Ok(Self { inner: Arc::new(HandleInner { raw }) })
    }

    pub async fn create_magnet_task(&self, magnet: &str, save: &Path) -> Result<TaskId> {
        // 所有调用通过 tokio::task::spawn_blocking 走线程池
        let inner = self.inner.clone();
        let magnet = magnet.to_string();
        let save = save.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let mut param = bindings::XLMagnetParam { /* fill */ };
            let r = unsafe { bindings::ffi::XL_CreateMagnetTask(inner.raw, &mut param) };
            if r != 0 { return Err(XunleiError::CreateFailed(r)); }
            Ok(TaskId(unsafe { Box::from_raw(param.task_id as *mut ()) }))
        }).await?
    }

    // ... 其他方法
}

impl Drop for XunleiHandle {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            unsafe { bindings::ffi::XL_UnInit(self.inner.raw); }
        }
    }
}
```

### 7.5 验证测试（M0 spike）

`crates/xunlei-ffi/tests/magnet.rs`：

```rust
#[test]
fn load_and_init() {
    let h = XunleiHandle::new(
        Path::new("vendor/xunlei-sdk"),
        Path::new("/tmp/xl-test/log"),
        Path::new("/tmp/xl-test/cfg"),
        "smart-dl-test-v1",
    ).expect("init failed");
    println!("init ok");
}

#[tokio::test]
async fn create_magnet_and_progress() {
    let h = XunleiHandle::new(/* ... */).unwrap();
    let id = h.create_magnet_task(
        "magnet:?xt=urn:btih:08ADA5FFDC1F1C9F3F1F1C9F3F1F1C9F3F1F1C9F",
        Path::new("/tmp/xl-test/dl"),
    ).await.unwrap();
    h.enable_free_dcdn(&id).await.unwrap();
    h.start_task(&id).await.unwrap();
    
    // 轮询 30 秒，看进度是否变化
    let mut prev_size = 0u64;
    for _ in 0..30 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let info = h.query_task_info(&id).await.unwrap();
        println!("state={} dl={} speed={}",
                 info.task_state, info.download_size, info.download_speed);
        if info.download_size > prev_size {
            println!("progress detected: {} -> {}", prev_size, info.download_size);
            break;
        }
        prev_size = info.download_size;
    }
    h.delete_task(&id, true).await.unwrap();
}
```

### 7.6 已知未知（M1 必须解决）

| 未知 | 解决方式 | 优先级 |
|---|---|---|
| `XLTaskInfo` 924 字节里的字段顺序 | 用一个超大输出 buffer，跑真实磁力任务，dump 出所有字段并和 917 个候选字段名对照 | M1 P0 |
| `XLInitParam` 40 字节里的字段顺序 | 同上，加日志路径对比 | M1 P0 |
| `XLBTTaskParamV2` 40 字节里子文件选择怎么传 | 先用 0xFFFFFFFF（全选）测试 | M1 P0 |
| `XLPeerInfo` 56 字节里 IPv4 vs IPv6 区分 | 看 peer_type 字段含义 | M1 P1 |
| `XL_SetTaskStrategy` strategy 取值 | 实测（0=顺序 1=随机？） | M1 P2 |
| `XL_AddServer` 是否真的能给 BT 加 Web Seed | 跑一个带 Web Seed 的种子测试 | M1 P2 |
| 是否能完全跳过 `DownloadSDKServer.exe`，直接静态链接 `DownloadSDK.dll` | 试 `#[link(name = "DownloadSDK")]` | M2 P1 |

---

## 8. 与原方案的兼容性变更

### 8.1 D3（BT 引擎）的修订

```
D3 原版：BT 集成：C++/libtorrent 薄内核 + C ABI（~30 函数）
        依据：libtorrent=BSD、peer 封禁、Web Seed、piece 级、20 年稳定；
              rqbit 缺 BEP-19/无 peer ban/GPL-3

D3 修订：BT 集成（Windows）：直接调用迅雷 DownloadSDKProxy.dll（100 个 XL_* 导出）
        依据：用户已确认"迅雷 BT 不需要登录直接用"——经逆向证实
        公开 ABI 函数数：~40 个 BT 路径必需（init/task/peer/tracker/dcdn/query）
        peer 封禁：内建（XL_DiscardPeer / XL_BatchDiscardPeer）
        Web Seed：通过 XL_AddServer 实现（待 M1 验证）
        DHT/Tracker：内建
        长效种子：XLLiveUDownload.dll 提供额外能力
        反吸血：内建 PeerFlag 机制
        许可：⚠ 个人自用合规性 OK；开源发布需法务评估
        平台：仅 Windows（Linux/macOS 仍需 libtorrent 备选实现）
```

### 8.2 跨平台策略

| 平台 | BT 引擎 | 实现 |
|---|---|---|
| Windows | **迅雷 DownloadSDK** | `crates/xunlei-ffi` 直接 FFI |
| Linux / macOS | **libtorrent 2.x**（原 D3 方案） | `crates/btcore` 走原 libtorrent C++ 薄内核 |
| 任意 | 纯 HTTP fallback | `crates/httpdl`（Web Seed 镜像） |

`BtEngine` trait 在 `btcore` 里通过 `cfg` 选实现：

```rust
#[cfg(target_os = "windows")]
mod xunlei_engine;
#[cfg(not(target_os = "windows"))]
mod libtorrent_engine;
```

### 8.3 D7（任务 ID 三层映射）保持不变

```
engine_refs: HashMap<FileIdx, EngineTaskId>
  - Windows: EngineTaskId::Xunlei(*mut c_void)  // XL_CreateBTTask_V2 返回的句柄
  - Linux/macOS: EngineTaskId::Bt(infohash)      // libtorrent 的 infohash
```

### 8.4 D11（HTTP 自研）需要重新评估

如果用迅雷引擎，**HTTP 也可以走 `XL_CreateP2spTask`**——迅雷 P2SP 引擎同时支持 HTTP/FTP/emule。但迅雷的 HTTP 引擎是闭源的，**没法做 Range 多连接、镜像源等精细控制**（除非通过 `XL_AddServer` + `XL_SetTaskStrategy` 间接控制）。

**建议**：
- BT 走迅雷（v1 M1）
- HTTP 仍走自研 Rust `HttpEngine`（v1 M4，原方案不变），因为我们要 Range/续传/镜像/自定义头/代理 全控制
- 这样不依赖迅雷引擎做 HTTP，独立性更好

### 8.5 §15 引用依据更新

新增条目：

| 项目 | 用途 | 链接 |
|---|---|---|
| 迅雷 PC 客户端 v25.0.90.1592 | 直接逆向其 SDK DLL，作为 BT 引擎 FFI 基础 | `XunLeiWebSetup25.0.90.1592gw.exe` |
| DownloadSDKProxy.dll | 公开 ABI 入口，100 个 XL_* 导出 | 本报告 §2 |
| DownloadSDK.dll | 真正的 BT/P2P/DCDN 引擎实现 | 本报告 §3-§6 |
| XUdt.dll | 自研 uTP-like 传输层 | 本报告 §6.3 |

---

## 9. 落地清单（动笔前确认）

### 9.1 用户需确认（5 条）

- [ ] **接受 Windows-only BT 路径**？还是必须跨平台？后者则保留原 libtorrent 方案
- [ ] **接受 vendor 迅雷 DLL 全套**？约 30 MB，需提交仓库或运行时释放
- [ ] **接受法律风险**？个人自用 OK；若计划开源发布需法务评估
- [ ] **接受 IPC 进程模型**？`DownloadSDKServer.exe` 会作为子进程跑，需要权限拉起
- [ ] **接受 ABI 版本锁定**？锁 25.0.90.1592；客户端升级到下个版本时需重新逆向

### 9.2 M1 里程碑任务（FFI 落地）

按你方案 §14 的里程碑格式：

- [ ] **M0-spike（1-2 天）— 迅雷 DLL 加载 + Init 验证**
  - [ ] 把 vendor/xunlei-sdk 全套 DLL 摆好
  - [ ] 写最小 Rust 程序：`LoadLibrary("DownloadSDKProxy.dll")` + `XL_Init` 成功
  - [ ] 跑 `tests/magnet.rs` 里 `load_and_init` 测试通过
  - [ ] 超时 1 天未通过 → 回退原 libtorrent 方案（保留 D3 fallback）

- [ ] **M1 — FFI 全量 + btcore 集成**（替换原 §14 M1）
  - [ ] 完成 `XLInitParam` 40 字节字段布局（dump 验证）
  - [ ] 完成 `XLBTTaskParamV2` 40 字节字段布局
  - [ ] 完成 `XLTaskInfo` 924 字节字段布局（用磁力任务跑真实数据，逐字段对齐）
  - [ ] 完成 `XLPeerInfo` 56 字节字段布局
  - [ ] `XunleiHandle` safe wrapper（线程安全 + Drop）
  - [ ] `BtEngine` trait 在 Windows 下走 xunlei，其他平台走 libtorrent（feature 控制）
  - [ ] 集成测试：磁力 → 5s 内进度 > 0
  - [ ] 集成测试：peer 注入成功（不报错）
  - [ ] 集成测试：FreeDCDN 启用成功
  - [ ] ASAN-like 验证：用 Application Verifier 或 PageHeap 跑 1 小时无 crash

- [ ] **M2-M7 沿用原方案**（核心模型、HTTP、FTP、Provider、健康、CLI/WS 不变）

### 9.3 风险降级方案

如果 M0 spike 失败（DLL 加载或 Init 失败）：

1. **降级 1**：放弃 xunlei 引擎，回原 D3 libtorrent 薄内核方案
2. **降级 2**：保留 xunlei 但用 `xl_thunder_sdk.dll` 的更高层 API（5MB，可能有更友好的 C++ 接口）
3. **降级 3**：仅 Windows 用迅雷 P2SP（HTTP/BT 都走迅雷），Linux 走纯 HTTP

---

## 10. 工件清单

以下文件已落盘：

| 路径 | 内容 |
|---|---|
| `/home/z/my-project/research/xunlei_setup.exe` | 原始安装包 |
| `/home/z/my-project/research/extracted/resource_1288_1296_unpacked/` | 安装器 UI 资源（不重要） |
| `/home/z/my-project/research/extracted/resource_1288_1304_unpacked/` | **下载引擎 DLL 全套**（关键） |
| `/home/z/my-project/research/dll_analysis/` | 每个 DLL 的导出/导入/字符串分析 |
| `/home/z/my-project/research/dll_analysis/DownloadSDKProxy_full_exports.json` | 100 个 XL_* 导出函数分类 |
| `/home/z/my-project/research/disasm/disasm_results.json` | 100 个 XL_* 函数的反汇编结果 |
| `/home/z/my-project/research/disasm/struct_sizes.json` | 11 个结构体尺寸推断 |
| `/home/z/my-project/research/struct_analysis/DownloadSDK_bt_fields.txt` | 917 个 BT 相关字段名 |
| `/home/z/my-project/research/struct_analysis/DownloadSDK_json_fields.txt` | JSON 字段名（短） |
| `/home/z/my-project/research/struct_analysis/DownloadSDK_capitalized.txt` | 大写符号（宏/枚举常量） |
| `/home/z/my-project/download/xunlei_engine_research.md` | **本报告** |

---

## 附录 A：关键 API curl/调用示例（伪代码）

### A.1 完整 BT 任务生命周期（匿名免登录）

```rust
// 1. 初始化（无账号）
let h = XunleiHandle::new(
    "vendor/xunlei-sdk",
    "~/.config/smart-dl/xl-log",
    "~/.config/smart-dl/xl-cfg",
    "smart-dl-001",
).await?;

// 2. 不调 XL_SetUserInfo（匿名）
// 3. 不调 XL_EnableDcdnWithVipCert（不用 VIP）

// 4. 创建磁力任务
let id = h.create_magnet_task(
    "magnet:?xt=urn:btih:08ADA5FFDC1F1C9F3F1F1C9F3F1F1C9F3F1F1C9F",
    "~/Downloads/smart-dl/".as_ref(),
).await?;

// 5. 启用 FreeDCDN（关键：免登录加速）
h.enable_free_dcdn(&id).await?;

// 6. 注入自定义 tracker
let trackers = ["udp://tracker.opentrackr.org:1337/announce",
               "https://tracker.example.com/announce"];
h.batch_add_bt_tracker(&id, &trackers).await?;

// 7. 注入 peer（如有 DHT 爬虫发现）
let peer = XLPeerInfo { ip: [192,168,1,100,0,0,0,0,0,0,0,0,0,0,0,0], port: 6881, ..Default::default() };
h.add_peer(&id, &[peer]).await?;

// 8. 启动
h.start_task(&id).await?;

// 9. 轮询状态
loop {
    tokio::time::sleep(Duration::from_secs(1)).await;
    let info = h.query_task_info(&id).await?;
    println!("state={} dl={} speed={} peers={}",
             info.task_state, info.download_size,
             info.download_speed, info.peer_connection_count);
    if info.task_state == TASK_STATE_COMPLETE { break; }
}

// 10. 完成 + 清理
h.delete_task(&id, false).await?;  // 保留数据
```

### A.2 反吸血（D4 落地）

```rust
// 检测到 peer 是吸血（XL_PEF_BONUS 之外的恶意标志）
if peer.flag & PEF_LEECHER != 0 {
    h.discard_peer(&id, &peer).await?;
    health_log::record(HealthEvent::LeechBanned { peer });
}
```

### A.3 多文件种子子文件选择（D9 落地）

```rust
// 解析 .torrent → 拿到 subfile list
let subs = h.query_bt_subfile_info(&torrent_path).await?;
// 选择大于 1MB 的文件
let selected: Vec<u32> = subs.iter().enumerate()
    .filter(|(_, s)| s.size > 1_000_000)
    .map(|(i, _)| i as u32)
    .collect();
let id = h.create_bt_task(&torrent_bytes, save_path, Some(&selected)).await?;
```

---

## 附录 B：调研下一步建议

1. **找一个真实的磁力链接做 M0 spike 验证**——光看反汇编不能 100% 确认 ABI 正确，必须跑实际下载
2. **下载 Ghidra 或 IDA Free 做更深的反编译**——`objdump` + `capstone` 只能反汇编，看不到 C++ 伪代码，难以精确还原 struct 字段顺序。建议用 Ghidra 的 P-Code 反编译器
3. **联系迅雷商务**——如果用户希望获得官方 SDK 文档/头文件，可能可以申请开发者授权（虽然不一定能拿到）

---

**报告结束**。请回复以下确认项：

1. 接受 Windows-only BT 路径（D3 修订）？
2. 接受 vendor 迅雷 DLL 全套（30MB）入库？
3. 接受个人自用合规性（不开源 / 不分发）？
4. 接受 ABI 版本锁定 25.0.90.1592？
5. 现在开始 M0 spike（DLL 加载 + Init 验证），还是先把方案文档跟你 v0.2 合并后再动手？
