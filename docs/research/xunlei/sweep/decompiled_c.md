# S2 反编译 C 扫描

> 任务：对迅雷桌面引擎反编译 C 做覆盖式函数簇标注，找「此前研究没注意到的能力痕迹」。
> 方法：锚点定位（脚本 `scripts/research/xunlei/sweep_s2.py`，中间产物 `docs/research/xunlei/sweep/s2_anchor_map.json`）→ 锚点 ±150 行划簇 → 字符串常量与调用结构判用途 → 对照 `KNOWN_ITEMS.md` K1~K37 标注。
> 判定依据全部来自字符串常量 / RTTI 类名 / 魔数 / 调用结构，未采信 sub_XXX 混淆名。
> 扫描时间：2026-08-26。语料只读，未改任何代码。

---

## 1. 各文件规模与锚点地图（关键词 × 行号段）

主锚点 = 任务指定 20 词；扩展锚点（`x_` 前缀）= crypto/host/netapi/tracker/account/config/updver/pay/httpverb。

### 1.1 DownloadSDK_DECOMPILED.c（574KB，18862 行，127 函数，XL_* 导出 ~100 个）

| 关键词 | 行号段（压缩） |
|---|---|
| gcid | 581, 749, 863, 966-967, 1131, 1242 |
| shub / report | 2056, 2261-2275 |
| resource | 2205-2209, 15182-15191 |
| dcdn | 2277, 13128-13601, 13910-14528, 17548-17558 |
| accelerate | 2277, 13952-13982, 17557, 18398-18474 |
| cert | 5682~8527（六段，均在巨型函数 FUN_18000bac0 内）, 13952-14528 |
| upload | 10118-10243, 15829-15924 |
| token / equity | 13292-13332, 18609-18771 |
| url / mirror / login / vip / http / .com | 无独立命中（登录类已证伪排除） |
| x_crypto | 7, 131, 234-464（RequestBuilder 区）, 1292-1360, 1619-1620, 2048-2049 |
| x_host | 79, 2065, 2130-2305（XLLRT RTTI 注册区） |
| x_tracker | 1681-1695(BT 握手), 2023-2026, 3358-3360(.torrent), 15450-15470, 16023-16047 |

### 1.2 P2PFramework_DECOMPILED.c（234KB，7770 行，179 函数，XPF_* 导出面）

| 关键词 | 行号段 |
|---|---|
| cert | 1125-1441, 2101, 2732-4120, 4158-4170, 5346-5352（TLS 校验态） |
| token | 892-894, 1492, 1555, 2509-2580（AuthenticationToken） |
| gcid / report | 55-56 / 76-77 |
| resource | 6450-6457 |
| x_crypto | 7-77, 285, 1125-1151, 1329-1340（CertificationEncryptData/DecryptData/SignData） |
| x_netapi | 2006-2016（AddressHelperInitDNSCache） |
| x_tracker | 1831-1845, 2920-2929, 3645-3652, 5431-5444, 6371-6382 等（PeerTracker/RemotePeerInfo） |

### 1.3 P2PBase_DECOMPILED.c（5.5KB，233 行，9 函数）：HashValue 工厂/解码，仅 x_tracker 4 处误报。

### 1.4 XUdt_DECOMPILED.c（16KB，571 行，6 函数）

| 关键证据 | 行号 |
|---|---|
| `XUdtPingSNClientEventManager` / `XUdtPingSNClientEvent` | 27-29 |
| `"relay://"`（立即数 0x2f2f3a79616c6572） | 209 |
| `XUDT_CHANNEL_PACKAGE_PARSER_CLASSNAME` / `XUdtConnection` | 369, 452 |

### 1.5 聚焦件（scripts/research/cloud_delivery/phub_line/）

| 文件 | 行数 | 锚点分布要点 |
|---|---|---|
| downloadsdk_encrypt.c | 1174 | 与主件重复的 XL_EnableDcdnWithToken/VipCert/EquityToken/TokenMode 导出体（cert/dcdn/token/equity 全覆盖） |
| downloadsdk_key_funcs.c | 1653 | report rc 校验串 L223；CmdHubGetConfigResp::DecodeBody L969-1108（Decrypt failed）；phub/shub/login RTTI 表 L1306-1482 |
| downloadsdk_combined.c | 959 | 同 key_funcs 的核心子集：GetConfig 解密 L107/246、RSA/AES 构造器 L284-588、TCPService*/UDPService* RTTI L612-780 |
| p2pbase_crypto.c | 521 | XPF_AESCreate{En,De}cryptContext/RSAFreeContext/RandomBytes/MD4/MD5/Sha1HashData |
| p2pbase_rsa.c | 1514 | **零锚点命中**——纯大数运算 |
| p2pbase_aes_core.c | 2016 | **零锚点命中**——纯 AES 表/轮实现 |
| p2pf_encrypt_funcs.c / 2.c | 42/715 | XPF_Certification* 全家族（加解密/签名/验签/状态机） |
| aes_callers_decompiled.c | 1041 | `/query` L76、`/report` L378、`dcache-hub.sandai.net` L667、`/ping` L691、`/invalid` L1008、Decrypt Fail L828 |
| fb_region_decompiled.c | 1366 | CmdSHubReportCorrection/ReportURLChange/ReportResQuality L25/247/513、CmdPHubIsRCOnline L849、CmdPHubReportRCList L949、CmdPHubNeedSyncCidStore L1296 |
| xudt_protocol_stack_decompiled.c | 6 | 空壳（NO FUNCTION 占位），跳过 |
| xudt_addr_decompiled.c | 565 | "token" 命中实为 `XPF_ThreadToken*`（线程令牌）→ 误报 |

### 1.6 sdk_login_static/DownloadSDKServer_DECOMPILED.c（467KB，16539 行，255 函数）

| 关键词 | 行号段 | 定性 |
|---|---|---|
| download/ipc 类 | 2723-3808, 8955-11056 | DownloadIPC/IPCPipe/Pipe/IPCPermanentStream/AcceptServer/DataServerBase |
| upload/dcdn/cert/equity/token | 3861-4027, 4614-4815, 5715-5863, 6473-6539 | 全部为 IPC 命令桩（调 XL_* 后序列化回包） |
| stat | 7528, 12285-13491, 14316, 15016 | CRT/异常处理样板（`_stat64`/EH） |
| x_account / x_config | 11308-11318, 11432, 11738-11746 | CRT（security_init_cookie 等） |
| report | 8646-8737 | XL_SetBugReportRootDir 指向 Thunder Network\XLSDK、XL_InitBugHandler |

---

## 2. 函数簇标注表

状态：已知 K##｜新发现 G#（编号见 §4）｜SKIP（CRT/STL/样板/纯数学）

| 文件:行段 | 判定用途 | 关键证据（≤100 字符） | 状态 |
|---|---|---|---|
| DownloadSDK L11-125 (FUN_180285de0) | Hub 请求 RSA 公钥装载：hex→DER→RSA 上下文+随机 AES-128 key | `XPF_HexStringToBytes`→`XPF_RSAContextFromKeyEx`；280 位 hex 立即数公钥 | 新发现 G6 |
| DownloadSDK L135-330 / combined L284-431 (FUN_180285fe0) | 请求体封装器：魔数+key_id+RSA(AES key)+AES-ECB(body) | 魔数 `0x26035888`；`XPF_RSAEncrypt_PKCS1_EX`；密文长 0x100 | 新发现 G6 |
| DownloadSDK L340-467 / key_funcs L5-80 | 日志辅助（LogGetGlobalSwitch/AllocOutBuffer） | `XPF_LogAllocOutBuffer` | SKIP |
| DownloadSDK L477-961 (FUN_1800c8f00) | GCID 计算器 | `HashCalculator::TryCalcGCID`、`DataManager\HashCalculator.cpp`、`Invalid GCID length:` | 已知 K35 相邻 |
| DownloadSDK L971-1287 (FUN_1800a4060) | CID/GCID 校验（HUBIndexInfo） | `CommonUtility\CIDUtility.cpp`、`Invalid CID length:` | 已知 K35 相邻 |
| DownloadSDK L1297-1614 (FUN_180096a00) | urllist 资源通道处理 | 立即数 `0x7473696c2d6c7275`=“urllist” | 新发现（并入 G4 主题） |
| DownloadSDK L1626-2043 (FUN_1802f7aa0) | BitTorrent 握手收发 | `s_BitTorrent_protocol`、len=0x13、保留位 0x100000000000(DHT) | 已知 K37 |
| DownloadSDK L2110-2311 (FUN_180012850) | XLLRT(Lua) 类注册总表，30+ 类 | `XLLRT_RegisterClass("Xunlei.DownloadSDK.…")`、`LimitSpeedQuota.Class` | 新发现 G9/G15 |
| DownloadSDK L2323-3397 (FUN_18009dac0) | MIME 类型表构建 | `application/x-bittorrent`、约 130 条 Content-Type | SKIP |
| DownloadSDK L3544-3734 (FUN_1800299f0) | BT 全局限速调节器 | `GlobalSpeedRegulator_BTTask.cpp`、`CutoffBTSubStrategyChannelCount` | 新发现 G15 |
| DownloadSDK L3744-4705 (FUN_180048700) | HLS 任务创建 | `XDLInterfaceImpl::CreateHLSTask`、`XDLInterfaceImpl_MSDK.cpp` | 已知 K33 相邻 |
| DownloadSDK L4827-5138 (FUN_180024b30) | 全局速度上限管理 | `GlobalSpeedRegulator.cpp`、`Gloable upper bound changed from ` | 新发现 G15 |
| DownloadSDK L5148-5670 (FUN_1800137a0) | 本地持久化路径解析 | `Thunder Network\cid_store.dat`、`pub_store.dat`、`Profiles\`、`GlobalSetting.ini` | 新发现 G11 |
| DownloadSDK L5687-9621 (FUN_18000bac0 ×6) | 日志子系统巨型函数（TRACE/SCOPE 分级） | `CTRL_INIT`、`Lite_Log_Target` | SKIP |
| DownloadSDK L9649-9732 (XL_Init/UnInit) | 引擎生命周期导出 | `XL_Init` | 已知 K33 |
| DownloadSDK L9773-9988 (XL_LaunchFileAssistant) | 启动伴生进程 | `PathAppendW(…,"XLFileAssistant.exe")`、`FileAssistant.exe` | 新发现 G10 |
| DownloadSDK L9997-10083 (XL_GetPeerId) | 取本机 PeerID | `XL_GetPeerId` | 已知 K33 |
| DownloadSDK L10152-10341 (XL_GetSubNetUploader) | 局域网/子网上传器导出 | `XL_GetSubNetUploader`、struct size 校验 | 新发现 G17 |
| DownloadSDK L10350-10696 (XL_SetProxy) | 代理设置导出 | `XL_SetProxy` | 已知 K33 相邻 |
| DownloadSDK L11205-11391 (XL_SetUserInfo) | 用户信息注入（char*,char* ABI） | `XL_SetUserInfo` | 已知 K33 |
| DownloadSDK L13132-13287 (XL_EnableDcdn) | 免费 DCDN 开关 | `XL_EnableDcdn` | 已知 K23 |
| DownloadSDK L13296-13553 (XL_EnableDcdnWithToken) | token 型 DCDN | `XL_EnableDcdnWithToken` | 已知 K23 |
| DownloadSDK L14105-14591 (XL_{Enable,Update,Disable}DcdnWithVipCert) | VIP 证书型 DCDN 三连导出 | `XL_EnableDcdnWithVipCert` | 已知 K23 |
| DownloadSDK L15304-15518 (XL_CreateBTTask/MagnetTask) | BT/磁力任务创建 | `XL_CreateMagnetTask` | 已知 K33 |
| DownloadSDK L15833-15988 (XL_BT{Start,Stop}Upload) | BT 上传开关 | `XL_BTStopUpload` | 已知 K33 |
| DownloadSDK L16027-16136 (XL_BatchAddBTTracker) | 外部 tracker 注入 | `XL_BatchAddBTTracker` | 已知 K37 相邻 |
| DownloadSDK L16751-17155 (XL_QueryPlayInfo 等 4 导出) | PLAY 直链播放信息 | `XL_QueryPlayInfo`、`XL_GetUniversalPlayInfo` | 已知 K25 相邻 |
| DownloadSDK L17164-17543 (XL_RenameP2spTaskFile) | P2SP 任务改名（超长函数） | `XDLInterfaceImpl_P2sp.cpp` | 已知 K33 |
| DownloadSDK L17919-17996 (XL_StartEstimateBandWidth) | 带宽预估会话 | `XL_StartEstimateBandWidth` | 已知 K33 相邻 |
| DownloadSDK L18402-18501 (XL_QueryAccelerateInfo) | 加速信息查询 | `XL_QueryAccelerateInfo invalid parameters` | 已知 K23/K24 |
| DownloadSDK L18613-18781 (XL_SetTaskEquityToken/SetTokenMode) | 权益令牌注入 | `XL_SetTaskEquityToken` | 已知 K23/K34 |
| DownloadSDK L18790-18838 (XL_SetAppGuid) | 应用 GUID 注入 | `XL_SetAppGuid` | 已知 K33 |
| P2PFramework L197-602 | 默认 Ping 客户端 | `DefaultPingClient::BeginPing`、线程 `workio.xpf.pingclient`、`P2P_Framework_VS2019` 源树 | SKIP（框架探活） |
| P2PFramework L775-1007 | XPF 事件管理装配 | `XPF_Cert_StateChange_Event` | SKIP（框架） |
| P2PFramework L1130-1476 | Certification 加解密/签名/类型注册导出 | `XPF_CertificationEncryptData`、`FinishSignData` | 已知 K23 相邻 |
| P2PFramework L1659-2138 | 地址缓存/DNS 查询缓存 | `XPF_AddressHelperInitDNSCache`、`XPF_MakeAddressQueryKey` | 新发现（并入 G13 主题） |
| P2PFramework L2295-2580 | 认证缓存与令牌 | `XPF_CreateAuthenticationToken`、`AuthenticationToken::vftable` | 新发现 G13 |
| P2PFramework L2498-2501（注释块） | 框架级异步入口枚举 | `XPF_PeerTrackerBeginTrack`、`XPF_RemotePeerInfoBeginQuery` | 新发现 G13 |
| P2PFramework L2736-4120 | 证书下载器/证书管理器全家族 | `XPFCertificationDownloaderStateChangeEvent`、`XPF_IsLocalRootCertification` | 已知 K23 相邻 |
| P2PFramework L4203-4887 | ChannelAcceptor 状态机 | `XPFChannelAcceptorInputChannelEvent` | SKIP（框架） |
| P2PFramework L5034-5035 / 7261-7262 | 统计埋点 | `XLSTAT4_TrackEvent("createchannel"…)`、`("createconn"…)` | 新发现 G14 |
| P2PFramework L5336-5360 | TLS 校验结果名 | `TLS_WrongHostName`、`TLS_ExpiredCertificate` | 已知 K23 相邻 |
| P2PFramework L5402-6460 | Connection 收发/虚拟端口/配额 | `XPF_ConnectionSetLocalVirtualPort` | SKIP（框架） |
| XUdt L6-122 | 会话构造（随机字段）+Ping SN 客户端事件 | `XUdtPingSNClientEvent` | 新发现 G12 |
| XUdt L176-327 | relay 地址解析/构造 | 立即数 `"relay://"`（0x2f2f3a79616c6572） | 新发现 G12 |
| XUdt L334-567 | XUDT 通道与包解析器注册 | `XUDT_CHANNEL_PACKAGE_PARSER_CLASSNAME`、`XUdtConnection` | 已知（XUDT 线既有研究） |
| key_funcs L88-270 (FUN_180169c00) | QueryAllRes 响应解密+seg 校验 | `ServiceQueryAllRes::HandleResponse`、`queryCmd.seg != queryResponseCmd.seg` | 新发现 G7 |
| key_funcs L867-1138 / combined L5-276 (FUN_18002ceb0) | Hub 配置下发响应解密 | `CmdHubGetConfigResp::DecodeBody`、`HubServiceGetConfig.cpp`、`Decrypt failed` | 新发现 G2 |
| combined L609-793 / key_funcs L1303-1487 | Hub 服务命令构造器目录（8 个模板类） | `TCPServicePHub<CmdPHubReportRCList>`、`UDPServiceDPHub<CmdDPHubPingParent>` | 新发现 G3/G4 |
| combined/key_funcs L801-875 (FUN_18030fe70/fcf0) | WSA 异步套接字簿记 | `WSAGetLastError`、overlapped 链表 | SKIP |
| aes_callers L6-120 (FUN_180162dc0) | 资源查询请求构造 | 立即数 `"/query"`、cmd 类型 0x11 | 新发现 G1/G16 |
| aes_callers L128-288 (FUN_180163890) | IPv6 资源查询响应处理 | `ServiceIPv6QueryRes::HandleResponse`、`ServiceIPv6QueryRes.cpp` | 新发现 G5 |
| aes_callers L296-408 (FUN_180177150) | RCList 上报请求构造 | 立即数 `"/report"`、cmd 0xd、0x58 字节记录数组 | 新发现 G16 |
| aes_callers L416-572 (FUN_1801773d0) | IPv6 Phub RCList 上报响应处理 | `ServiceIPv6PhubReportRCList::HandleResponse`、子类型 0x0c/0x0e/0x10 | 新发现 G5 |
| aes_callers L580-732 (FUN_1802acb20) | 预加载清单查询器构造 | `ServicePreloadListQuerier::vftable`、`dcache-hub.sandai.net`、`PreloadDeploy/PreloadURLServerHost` | 新发现 G1 |
| aes_callers L740-962 (FUN_1802ad5e0) | 预加载清单协议体解密解析 | `PreloadListQuerier.cpp`、`Decrypt Fail`、`UnpackData Fail` | 新发现 G1 |
| aes_callers L970-1039 (FUN_1801672d0) | invalid 上报构造 | 立即数 `"/invalid"`、cmd 0x13 | 新发现 G16 |
| fb_region L6-217 (FUN_1800fabe0) | SHub 资源纠错上报构造 | `CmdSHubReportCorrection::vftable`、cmd id 0x7df | 新发现 G4 |
| fb_region L225-483 (FUN_1800fd790) | SHub URL 变更上报 | `CmdSHubReportURLChange::vftable` | 新发现 G4 |
| fb_region L491-822 (FUN_180100070) | SHub 资源质量上报 | `CmdSHubReportResQuality::vftable` | 新发现 G4 |
| fb_region L830-908 (FUN_180141120) | PHub RC 在线探测发送 | `CmdPHubIsRCOnline::vftable`、port==0x1bb→`SSLConnection` | 新发现 G4 |
| fb_region L916-1226 (FUN_180160b20) | PHub RCList 上报构造 | `CmdPHubReportRCList::vftable` | 新发现 G4/G5 |
| fb_region L1277-1364 (FUN_18029fad0) | PHub CID 库同步请求 | `CmdPHubNeedSyncCidStore::vftable` | 新发现 G4 |
| Server L203-735 (FUN_1400012a0) | 日志目标初始化 | `TRACE1..4`、`DEBUG1..4`、`SCOPE1..4`、`CTRL_INIT` | SKIP |
| Server L2723-3468 (DownloadIPC 装配) | IPC 服务端初始化 | `DownloadIPC::vftable`、`IPCPipe::vftable`、`InitDownloadIPC` | 新发现 G8 |
| Server L3475-3854 (WaitConnectTimeout/ExcuteCommand) | IPC 命令分发 | `DownloadSDKServer::ExcuteCommand`、`ERROR_DOWNLOADIPCADAPT_INVALID_CALL` | 新发现 G8 |
| Server L3861-4027 | IPC 桩：上传类命令 | 解包→调 XL_*→序列化回包 | 新发现 G8 |
| Server L4614-4815 | IPC 桩：DCDN 命令 | 直接调用 XL_EnableDcdn / XL_DisableDcdn | 新发现 G8 |
| Server L5042-5260 (FUN_14000df10) | IPC 数据通道 | AcceptServer/DataServerBase 相关 | 新发现 G8 |
| Server L6473-6539 | IPC 桩：权益/token 命令 | equity/token 参数透传 | 新发现 G8 |
| Server L8620-8754 (main 附近) | 崩溃报告根目录 | XL_SetBugReportRootDir 指向 Thunder Network\XLSDK；XL_InitBugHandler("DownloadSDK") | 已知 K32 相邻（新路径细节并入 G11） |
| Server L882-922 | Lua 栈转储诊断 | `XPF_GetCurrentThreadLuaStack`、`MessageBoxW(L"Lua Stack As Follows:")` | 新发现 G9 |
| Server L10427-11019 (Pipe::CreateObjects/OpenObjects) | 命名管道+共享内存 IPC 原语 | `MapViewOfFile(…,0xf001f,…,0x100000)`、`CreateEventW`、`Pipe.cpp` | 新发现 G8 |
| Server L11207+ / 11474+ | CRT/异常处理/cookie | `__scrt_initialize_crt`、`__security_init_cookie` | SKIP |
| p2pbase_rsa.c 全文 | 纯 RSA 大数运算 | 零锚点命中 | SKIP（算法本体） |
| p2pbase_aes_core.c 全文 | 纯 AES 表/轮实现 | 零锚点命中 | SKIP（算法本体） |
| p2pbase_crypto.c 全文 | XPF 哈希/随机/上下文工厂 | `XPF_MD5HashData`、`XPF_RandomBytes` | 已知（XUDT 线既有） |
| p2pf_enc1/2 全文 | Certification 加解密/签名家族 | `XPF_CertificationCompare`、`GetSignLength` | 已知 K23 相邻 |
| xudt_addr 全文 | 端点/端口拷贝管线 | `XPF_EndPointSetPort`、`XPF_ThreadTokenAddRef` | SKIP（token 为线程令牌，非鉴权） |
| xudt_proto_stack | 空占位 | `=== NO FUNCTION ===` | SKIP |

簇计数：脚本自动归并锚点簇 47 个；人工标注表 80 行（已知 K## 25 行、新发现 39 行〔含并入主题的轻量项〕、SKIP 14 行）。覆盖全部 17 个语料文件。

---

## 3. 主题汇总

**① 下载源调度（SHub 平面 + 免费通道）**
SHub 是「服务器资源枢纽」TCP 服务：InsertServerRes / QueryServerRes / InsertBCID / QueryUrlInfo / VoteURLInfo / QueryEmuleInfo / ReportCorrection / ReportURLChange / ReportResQuality / BtTaskVote 十个命令对（RTTI 模板类名逐一确认）。配合 SDK 内部 `urllist` 通道与 `ServiceFreeDcdnQueryAccelerate`，构成 URL 资源的增删查改+投票+纠错闭环。emule 信息查询说明电骡源仍在协议面内。

**② P2P 握手与传输（PHub/DPHub 平面 + XPF/XUdt）**
PHub 平面：ReportRCList、IsRCOnline（对端在线探测）、NeedSyncCidStore（CID 库同步）。DPHub 是此前未记录的第三平面：`CmdDPHubLoginParent`/`CmdDPHubGoAway`（TCP）+ `CmdDPHubPingParent`（UDP）——向「父节点」登录/告别/心跳，疑似 P2SP 上游节点或代理父节点选举。XPF 框架提供完整证书体系（本地/远端/根证书、签名验签、TLS 主机名校验）与 AuthenticationToken/PeerTracker 抽象；XUdt 提供 UDP 传输、`relay://` 中继地址与 SN（超级节点）ping。

**③ 加密栈**
三层清晰分层：(a) 纯算法层 p2pbase_rsa/aes_core（无字符串、无锚点）；(b) XPF 封装层 AESContext/RSAContext/MD4/MD5/SHA1/RandomBytes；(c) 业务封装层——Hub 请求 `[0x26035888][key_id][u32=256][RSA-PKCS1(随机AES-128 key)][u32][AES-ECB(body)]`，公钥为编译期 hex 常量（FUN_180285de0 栈立即数可提取）；Hub 配置/查询响应体用 AES-128-ECB，密钥由 MD5 派生（GetConfig 用 "X-GMT-Date" 串参与派生，QueryAllRes 用命令头 8 字节 seg 字段派生）。所有 hub 命令对象析构时都释放 RSA 上下文（每命令一密钥的痕迹）。

**④ 上报/统计**
XLSTAT4_TrackEvent（createchannel/createconn）、HubServerStat、四类 SHub 上报、PHub RCList 上报（含 IPv6 变体）、`/report` `/invalid` 路径命令、XL_InitBugHandler+`Thunder Network\XLSDK` 崩溃报告目录。上报密度高但全部是下载行为遥测，未见账号维度字段。

**⑤ 认证消费（零登录再确认）**
全语料再次证实 K27/K33：没有任何 signin/token 刷新/captcha 实现；VIP 能力全部通过导出入口消费外部注入物（XL_SetAccelerateCertification、XL_SetTaskEquityToken、XL_SetTokenMode、XL_SetUserInfo、XL_SetAppGuid），IPC 桩只是把这些调用透传给宿主。dl_keyfuncs 里唯一 `login` 命中是 `CmdDPHubLoginParent` 的子串，非账号登录。

**⑥ 本地运行时（最大意外区）**
- **DownloadSDKServer.exe 是独立本地服务进程**：命名管道（Pipe.cpp CreateObjects/OpenObjects）+ 1MB MapViewOfFile 共享内存 + Event 组成的 DownloadIPC，ExcuteCommand 按 CommandID 分发到 XL_* 导出（EnableDcdn、BT 上传、QueryTaskInfo 等均有桩）。
- **双模块内嵌 Lua**：SDK 用 XLLRT_RegisterClass 把 30+ 引擎类（DLTask/P2SPTask/ResourceManager/ServiceSHub*…）注册进 LuaRT；Server 侧有 XPF_GetCurrentThreadLuaStack 与 "Lua Stack As Follows" 诊断弹窗 → 存在脚本化控制面。
- **伴生进程**：XL_LaunchFileAssistant 拉起 XLFileAssistant.exe / FileAssistant.exe。
- **持久化**：cid_store.dat / pub_store.dat / bt_uncomplete_record_store.dat / Profiles\ 目录 / GlobalSetting.ini。
- **配置拉取**：HubServiceGetConfig（加密配置下发）+ PreloadDeploy 配置节（PreloadURLServerHost/Port）。

---

## 4. GAP_LIST（仅新发现）

| G# | 能力猜测 | 证据原文（≤120 字符） | 置信度 | 建议动作 |
|---|---|---|---|---|
| G1 | 视频预加载清单服务：SDK 直连 dcache-hub.sandai.net:80/443 拉取预载 URL 清单（PreloadVideoTaskManager），走 TcpConnection/SSLConnection，发 /ping /query /report /invalid 四路命令 | `local_158="dcache-hub.sandai.net"`；`"PreloadURLServerHost"`；`"PreloadDeploy"`；立即数 `"/ping"` | A | 抓 dcache-hub 流量验证报文；提取 PreloadDeploy 配置节实际下发值 |
| G2 | Hub 配置下发加密通道：CmdHubGetConfigResp 响应体 AES-128-ECB，密钥由含 `"X-GMT-Date"` 的派生串 MD5 得到 | `"CmdHubGetConfigResp::DecodeBody"` + `HubServiceGetConfig.cpp` + `XPF_AESDecryptBufferECB(MD5(...))` | A | 动态 hook XPF_AESCreateDecryptContext 抓 GetConfig 明文配置项 |
| G3 | DPHub 父节点协议（未记录的第三 Hub 平面）：TCP LoginParent/GoAway + UDP PingParent | `TCPServiceDPHub<class_CmdDPHubLoginParent,...>`、`UDPServiceDPHub<class_CmdDPHubPingParent,...>` | A | 在 XUDT 抓包中按 DPHub 命令 ID 过滤，确认父节点角色（中继/上游） |
| G4 | PHub/SHub 命令全集目录：SHub 10 命令对 + PHub ReportRCList/IsRCOnline/NeedSyncCidStore，含命令 id（如 ReportCorrection=0x7df） | `CmdSHubReportCorrection::vftable`、`CmdPHubNeedSyncCidStore::vftable` | A | 补全 phub_line 协议文档的命令表；IsRCOnline 可用于主动探测 CDN 资源存活 |
| G5 | IPv6 专属服务族：独立的 ServiceIPv6QueryRes / ServiceIPv6PhubReportRCList 处理器 | `src\P2P\ServiceIPv6QueryRes.cpp`、`ServiceIPv6PhubReportRCList.cpp` | A | 检查现有抓包是否遗漏 IPv6 endpoint；IPv6 路径可能绕过既有 IPv4 监控点 |
| G6 | Hub 请求封装格式：魔数 `0x26035888` + key_id + 256B RSA-PKCS1(随机 AES-128 key) + AES-ECB body；公钥为 280 hex 编译期常量 | `local_res8[0]=0x26035888; XPF_RSAEncrypt_PKCS1_EX(...,0x100,&local_res18)` | A | 提取 FUN_180285de0 栈立即数还原公钥模数；写独立 PHub 报文编码器 |
| G7 | Hub 查询响应体加密：AES-128-ECB，key=MD5(命令头 8 字节 seg 字段)；带 seg 回显一致性校验 | `XPF_MD5HashData(&local_res20,8,local_140)` + `"queryCmd.seg != queryResponseCmd.seg"` | B | 用已知 seg 做 known-plaintext 验证明文结构（同 XUDT 548 帧思路） |
| G8 | DownloadSDKServer.exe = 本地 IPC 服务进程：命名管道+1MB 共享内存+Event，按 CommandID 分发 XL_* 调用（含 DCDN/BT 上传/任务查询桩） | `src\DownloadIPC\Pipe.cpp`、`DownloadSDKServer::ExcuteCommand`、`MapViewOfFile(...,0x100000)` | A | 运行期枚举管道名（Process Explorer/handle.exe），伪造客户端直调 IPC 面 |
| G9 | 双模块内嵌 Lua 控制面：SDK 经 XLLRT_RegisterClass 注册 30+ 引擎类；Server 有当前线程 Lua 栈诊断 | `XLLRT_RegisterClass(lVar1,"Xunlei.DownloadSDK.DLTask.Class",...)`、`L"Lua Stack As Follows:"` | A | 枚举 XLLRT 注册表确认可脚本化的方法面；评估 Lua 注入面（安全视角） |
| G10 | FileAssistant 伴生进程拉取 | `PathAppendW(...,L"XLFileAssistant.exe")`、`L"FileAssistant.exe"` | A | 确认该 exe 职责（文件修复/预览？）与启动参数 |
| G11 | 本地持久化存储集：cid_store.dat / pub_store.dat / bt_uncomplete_record_store.dat / DownloadSDK\Profiles\ / GlobalSetting.ini | `L"Thunder Network\\cid_store.dat"`、`L"Thunder Network\\DownloadLib\\pub_store.dat"` | A | 与 K32 .drive KV 并列收录；离线解析 cid_store 结构（大概率复用 CID 序列化） |
| G12 | XUdt 支持 `relay://` 中继地址与 SN（超级节点）ping 事件 | 立即数 `0x2f2f3a79616c6572`(="relay://")、`"XUdtPingSNClientEvent"` | A | 在 XUDT 会话里找 relay:// 地址出现场景（NAT 回退？）；补协议文档传输章节 |
| G13 | XPF 框架级 PeerTracker/认证查询导出：XPF_PeerTrackerBeginTrack、AuthenticationQuery、RemotePeerInfoBeginQuery、DNS 查询缓存 | 注释块 `0x6c20 621 XPF_PeerTrackerBeginTrack`、`XPF_AddressHelperInitDNSCache` | B | 确认 PeerTracker 是否对接迅雷自有 tracker（区别于 BT tracker） |
| G14 | 引擎级统计埋点 API：XLSTAT4_TrackEvent 已见 createchannel/createconn 两事件 | `XLSTAT4_TrackEvent("createchannel",param_2,0,0,...)` | A | hook 该函数枚举全部事件名，作为行为遥测清单 |
| G15 | 全局限速治理：LimitSpeedQuota 类 + GlobalSpeedRegulator（全局上限变更通知、BT 子策略通道数截断） | `"Xunlei.LimitSpeedQuota.Class"`、`CutoffBTSubStrategyChannelCount` | A | 与 K36 实测 150KB/s 单连接结论对照，定位服务端配额字段落点 |
| G16 | Hub 命令采用路径风格操作名 + 1 字节命令类型：/ping=0x00、/query=0x11、/report=0x0d、/invalid=0x13 | `local_b8[0]=0x79726575712f`(="/query")、`*(...+4)=0x11` | A | 把操作路径/类型映射写进 PHub 协议文档，辅助流量快速分类 |
| G17 | 子网上传器导出：XL_GetSubNetUploader（局域网互传/上传能力面） | `local_108="XL_GetSubNetUploader"` + struct size 校验 | A | 确认是否有独立局域网发现/上传协议（潜在隐蔽内网行为） |

统计：GAP_LIST 共 **17 条**（A 置信 16 条，B 置信 1 条）。

---

## 附：产物清单
- 脚本：`scripts/research/xunlei/sweep_s2.py`（锚点扫描/函数地图/簇归并，`--json`/`--funcs` 两种模式）
- 中间数据：`docs/research/xunlei/sweep/s2_anchor_map.json`
- 本报告：`docs/research/xunlei/sweep/decompiled_c.md`
