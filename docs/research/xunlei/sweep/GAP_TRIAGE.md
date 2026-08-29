# 盲区终审分诊报告（GAP_TRIAGE）

> 合并来源：[`decompiled_c.md`](decompiled_c.md)（S2 反编译 C 扫描，17 条 G#）+ [`web_frontend.md`](web_frontend.md)（S1 前端业务码扫描，23 条 G#），共 **40 条**。
> 判定基线：`KNOWN_ITEMS.md` K1~K37（两份扫描的"已知"项不在本表）。
> 分层定义：**T1** 下载器核心相关（可立项变现为能力/解锁后续协议面）；**T2** 生态知识归档（写入知识库即可，不立项）；**T3** 安全面观察（记录/报备，不做主动越权测试）；**T4** 暂无行动。
> 分诊时间：2026-08-25。语料只读。

## 统计

**总 40 条 = S1 23 + S2 17；分层：T1 立项 8 / T2 归档 20 / T3 安全 6 / T4 暂无 6。**

## Top5 推荐立项

| # | 编号 | 一句话理由 |
|---|---|---|
| 1 | S1-G2 云盘在线解压 API | 唯一能直接变成产品能力的云盘 API（云端预览/选择性下载）；本次已启动只读探测 → [`../DECOMPRESS_API.md`](../DECOMPRESS_API.md) |
| 2 | S2-G6 + S2-G7 Hub 信封自建 | 魔数 `0x26035888` + RSA+AES 封装是自建 PHub/SHub 通信的总闸门，解锁 G4/G16 全部命令面 |
| 3 | S2-G8 DownloadSDKServer IPC 服务面 | 命名管道 + CommandID 分发 XL_* 桩 = 不逆向协议直接驱动官方引擎能力的捷径，成本最低收益最高 |
| 4 | S1-G3 本地回传端口 28317 | 分享转存自动化入口；新端口 K31 未录，定位监听进程与 `/yun_*` 路径面即可评估 |
| 5 | S2-G15 全局限速治理落点 | 直接服务现有产品痛点：对照 K36 实测 150KB/s 单连接结论，hook 定位 LimitSpeedQuota 服务端配额字段落点 |

---

## T1 下载器核心相关（建议立项，8 条）

| 编号 | 一句话 | 建议动作 |
|---|---|---|
| S1-G2 | 云盘在线解压三端点 `/decompress/v1/{list,decompress,progress}`，支持 password/gcid 参数 | **立项**：只读探测已完成首轮（list 形状与实录见 [`../DECOMPRESS_API.md`](../DECOMPRESS_API.md)）；后续作为「云端预览/选择性下载」候选能力接入 provider |
| S1-G3 | 本地回传服务 `127.0.0.1:28317/yun_fetch_back`：网页把分享选中文件推给桌面端处理（新端口 K31 未录） | **立项**：定位 28317 监听进程、枚举其余 `/yun_*` 路径——分享转存自动化的天然入口 |
| S2-G4 | PHub/SHub 命令全集目录：SHub 10 命令对 + PHub ReportRCList/IsRCOnline/NeedSyncCidStore，含命令 id（如 ReportCorrection=0x7df） | **立项**：补全 phub_line 协议文档命令表；IsRCOnline 可用于主动探测 CDN 资源存活 |
| S2-G6 | Hub 请求封装格式：魔数 `0x26035888` + key_id + 256B RSA-PKCS1(随机 AES-128 key) + AES-ECB body，公钥为编译期 hex 常量 | **立项**：提取 FUN_180285de0 栈立即数还原公钥模数，写独立 PHub 报文编码器（信封自建前置） |
| S2-G7 | Hub 查询响应体加密：AES-128-ECB，key=MD5(命令头 8 字节 seg 字段)，带 seg 回显一致性校验 | **立项**（并入 S2-G6 信封专项）：用已知 seg 做 known-plaintext 验证明文结构 |
| S2-G16 | Hub 命令采用路径风格操作名 + 1 字节命令类型：/ping=0x00、/query=0x11、/report=0x0d、/invalid=0x13 | **立项**（并入 S2-G4 命令表）：操作路径/类型映射辅助流量快速分类 |
| S2-G8 | DownloadSDKServer.exe = 本地 IPC 服务进程：命名管道 + 1MB 共享内存 + Event，按 CommandID 分发 XL_* 调用 | **立项**：运行期枚举管道名（Process Explorer/handle.exe），伪造客户端直调 IPC 面——绕过协议逆向直接复用官方引擎能力 |
| S2-G15 | 全局限速治理：LimitSpeedQuota 类 + GlobalSpeedRegulator（全局上限变更通知、BT 子策略通道数截断） | **小成本立项**：hook 枚举配额字段，对照 K36 实测 150KB/s 结论定位服务端限速字段落点 |

## T2 生态知识归档（不立项，写入知识库即可，20 条）

| 编号 | 一句话 | 建议动作 |
|---|---|---|
| S2-G1 | 视频预加载清单服务：SDK 直连 dcache-hub.sandai.net 拉 PreloadDeploy 清单，走 /ping /query /report /invalid 四路命令 | 归档（播放线知识；PreloadDeploy 配置节实际下发值可选抓包确认） |
| S2-G2 | Hub 配置下发加密通道：CmdHubGetConfigResp 响应体 AES-128-ECB，密钥含 "X-GMT-Date" 派生串 MD5 | 归档（动态 hook 抓 GetConfig 明文为可选项，非必需） |
| S2-G3 | DPHub 父节点协议（未记录的第三 Hub 平面）：TCP LoginParent/GoAway + UDP PingParent | 归档（迅雷私有 P2P 加速已在 BACKLOG D 段明确排除，仅记录协议面存在） |
| S2-G5 | IPv6 专属服务族：独立的 ServiceIPv6QueryRes / ServiceIPv6PhubReportRCList 处理器 | 归档（提醒：既有抓包监控点可能漏 IPv6 endpoint） |
| S2-G11 | 本地持久化存储集：cid_store.dat / pub_store.dat / bt_uncomplete_record_store.dat / Profiles\ / GlobalSetting.ini | 归档（与 K32 .drive KV 并列收录；离线解析 cid_store 对 xunlei-migrate 迁移工具潜在有用） |
| S2-G12 | XUdt 支持 `relay://` 中继地址与 SN（超级节点）ping 事件 | 归档（补 XUDT 协议文档传输章节：NAT 回退场景待观察） |
| S2-G13 | XPF 框架级 PeerTracker/认证查询导出：XPF_PeerTrackerBeginTrack、DNS 查询缓存 | 归档（纯知识：确认是否对接迅雷自有 tracker，区别于 BT tracker） |
| S2-G14 | 引擎级统计埋点 API：XLSTAT4_TrackEvent 已见 createchannel/createconn 两事件 | 归档（可选 hook 枚举全部事件名作行为遥测清单） |
| S1-G1 | 用户订阅/动态系统：按 dst_uid+scene 关注、公开动态 news、白名单 | 归档（内容生态，与下载器无关） |
| S1-G4 | 第三方账号 provider 体系：OAuth uri/token 交换、绑定、匿名注册 | 归档（注记：signup/anonymously 理论可得无手机号 token，研究价值留档不主动测） |
| S1-G5 | 会话提权面：user/sudo、authorize/detect、device/authorize、token/introspect | 归档（xluser 权限域知识；sudo token 权限域验证属敏感操作，仅留档） |
| S1-G7 | 快鸟取链端点 /dlj/{bird_key}/url（K5 家族新路径形态） | 归档（bird_key 来源回溯留待快鸟线需要时再挖） |
| S1-G8 | 书城/听书产品线：books hot/rankings/labels、mediahub 阅读历史、小说章节解析 | 归档（内容产品线知识） |
| S1-G9 | 圈子社区 chitchat：群组查询 + 圈子投票错误码 | 归档（社交生态） |
| S1-G10 | 分享卡片聚合：recommended_group 群组推荐 + public 分享批量拉取 | 归档（分享体系补充知识） |
| S1-G11 | 私密文件夹与内容审核态暴露：folder_type=SAFE、audit.status 敏感禁止查看 | 归档（注记 files 接口返回结构含 audit 字段，做列表功能时需处理该态） |
| S1-G12 | xmodels 开放平台：API Key 创建/管理 models/v1/apikey | 归档（开放平台知识） |
| S1-G17 | 云盘文本读取器 drive-reader（prod 挂 api-pan.xunlei.com/v1） | 归档（文本预览能力参考，乱码检测逻辑可借鉴） |
| S1-G19 | xluser 多代网关（xluser2-ssl/xluser3-ssl/dev-）与 i.xunlei.com 授权页族 | 归档（账号基础设施演进史注记） |
| S1-G23 | vod 直链下载器：公网 web-vod-xdrive + 内网 :19099 双通道 ts_downloader | 归档（与 K25 PLAY 直链的关系一句话确认后并入播放线知识） |

## T3 安全面观察（记录/报备，不做主动测试，6 条）

| 编号 | 一句话 | 建议动作 |
|---|---|---|
| S1-G6 | getuserinfo 可按 account_type=userid 查任意用户资料（vip.isyear/avatar） | **安全报备**：越权面（水平越权拉他人 VIP 态）——记录证据原文，不遍历实测，可考虑负责任披露 |
| S1-G13 | union 行为上报网关 union-gateway-pan + 前端内嵌 key 明文（混淆拆片还原） | **安全报备**：key 用途（签名/appkey）确认后报备；前端硬编码密钥属常规泄露面 |
| S1-G16 | xunlei:// 唤端跳板页 oia-pan-ssl：协议串 URL 编码包装成 https 兜底 | **安全报备**：跳板可否携带任意 path = 开放重定向面；ct 参数含义一并确认 |
| S1-G22 | PC 端用户扩展信息 /user_info/pc_info 按 uid 查询 | **安全观察**：与 S1-G6 同族越权面，记录即可 |
| S2-G9 | 双模块内嵌 Lua 控制面：XLLRT 注册 30+ 引擎类，Server 有 Lua 栈诊断弹窗 | **安全观察**：脚本化控制面的注入面评估（本地攻击面，非远程） |
| S2-G17 | 子网上传器导出 XL_GetSubNetUploader（局域网互传/上传能力面） | **安全观察**：确认是否有独立局域网发现/上传协议——潜在隐蔽内网行为，用户知情权视角 |

## T4 暂无行动（6 条）

| 编号 | 一句话 | 建议动作 |
|---|---|---|
| S1-G14 | 支付中心 PC/安卓双页跳转（pay.xunlei.com / ges-pay） | 暂无行动（运营页面，扫描文档已有记录） |
| S1-G15 | 微信 JS-SDK 签名服务 weixinapi-m-ssl/wx/sign/js_api | 暂无行动（营销分享链路，与项目无关） |
| S1-G18 | xl9 网关 /sl(/sl_dev) 与红包活动痕迹（easy-mock 测试桩） | 暂无行动（活动性内容，时效性强不值得跟进） |
| S1-G20 | 独立密码服务（dev: password.office.k8s.xunlei.cn，prod 并入 xluser /v1/password） | 暂无行动（prod 已收敛进 xluser 主面；dev 内网域名仅在归档中保留） |
| S1-G21 | 活动奖励领取 /activity/v1/rewards | 暂无行动（运营活动接口） |
| S2-G10 | FileAssistant 伴生进程拉取（XLFileAssistant.exe / FileAssistant.exe） | 暂无行动（职责大概率文件修复/预览，不影响本项目） |

---

## 备注

- 两份源报告的 SKIP 项（vendor/CRT/静态 CDN 等）与本表无关；"已知 K#" 命中项也不重复收录。
- T1 各条若立项，建议先在 `docs/research/xunlei/` 下开专项文档（参照 phub_relay_task_v* 系列），避免散落。
