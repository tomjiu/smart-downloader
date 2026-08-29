# 扫描基线：已知发现清单（KNOWN_ITEMS）

> 用途：语料扫描代理的**比对基准**。语料中命中本清单任一条 → 标注「已知 K##」；
> 未命中的业务性内容 → 标「新发现」进 GAP_LIST；vendor/样板/运行时 → 标「SKIP」。
> 维护：2026-08-25 由主会话从六份研究文档浓缩。

## A. 端点 / URL
- K1 `api-pan.xunlei.com/drive/v1/{files,tasks,file,events,share/*,privilege/*,space}`
- K2 `api-gateway-pan.xunlei.com`：`/speed-center/v1|v2/{rule,trial}`、`/xlppc.searcher.api/drive_common_search|drive_file_search`、`/report/v1/config`
- K3 `xluser-ssl.xunlei.com/v1/auth/{signin,token,device/code,verification,verification/verify}`、`/v1/shield/captcha/init`
- K4 `speedup.xunlei.com/v1/{user_query,check_status,has_exporder,open,close,notify,change_bind}`
- K5 `dev-speedup.xunlei.com/v1/*`（fast_bird 白名单同族）
- K6 `speed.auth.vip.xunlei.com/speed/{speedup,res_status}`（经典引擎证书认证）
- K7 `conf-m-ssl.xunlei.com/external/<guid>` 全局配置下发
- K8 `api-shoulei-ssl.xunlei.com`、`infra-gateway-shoulei.xunlei.com`
- K9 `pan.xunlei.com/yc/*`（含远程设备登录页）、`xlaccsdk01://` scheme
- K10 `etl-xlmc-ssl.xunlei.com/api/stat/*`（上报）
- K11 `download-code-lixian-vip.xunlei.com`；`vod*-h05-vip-lixian.xunlei.com/download/` 直链形态
- K12 阿里云盘挂载链：xluser `/proxy/aliyundrive/*`、pan `/yc/oauth-callback`（client=947331beffa84e718adbd66b1732e748）
- K13 `mqtt.xbase.cloud`

## B. client 家族与鉴权
- K14 client_id：Xqp0(web pan,白名单)/XW-G4(桌面xllite)/XW5Sk(登录页)/X9ib/XVJV/Yd0*GrNJhCC2oX 系列
- K15 captcha/init 配方：meta{user_id,captcha_sign(9盐链),client_version:"1.92.91",package_name:"pan.xunlei.com",timestamp}+Bearer 头
- K16 OAuth 设备码流 grant_type=device_code；refresh_token a1. 前缀轮换
- K17 device_sign="div101."+did32+md5(sha1(did+包名+APPID40+APPKEY 34a06…))；包名 com.xunlei.downloadprovider
- K18 speedup 鉴权标签 withBearerSessionID+withUserID；POST+body 鉴权通过（GET ret16）
- K19 WEB_SALTS 9 盐链 captcha_sign="1."+hex

## C. 加速产品线
- K20 快鸟宽带提速（地域锁定，北京电信 ret1101）≠下载加速
- K21 TrySpeed/SuperSpeed 试用：trial_left/used_times、trial_key、total_sec(timeout_sec1800,total_count3)、apply 绑任务列表；前端 judgeCanTrySpeed/commitApplyTry/showPreTryBanner/tryTimeUsagePercentage
- K22 组队加速 VipTeamJoinUrl/team_times/teamTaskIDListRef
- K23 经典引擎证书注入 XL_SetAccelerateCertification/TokenMode/TaskEquityToken/EquityToken/AccelerateToken
- K24 函数名 VipSpeedUpUrl/VipTeamJoinUrl/superSpeedVipControl/queryResourceSuperSpeedInfo/checkSpeedUpResult 存在，远端路径未知
- K25 PLAY 直链 vip= 参数按账号档位限速（FREE≈150KB/s 单连接）
- K26 任务字段 is_super_speed/is_try_super_speed/status TRYING；全局配置 try_speed{timeout_sec1800,total_count3,total_sec60}、user_runner{600..1200}

## D. 组件 / 架构
- K27 登录唯一在 xllite.exe（Go）；SDK 全家族零登录代码、只消费 token
- K28 platformdetect.PlatformConfig.GetClientSecret(name)；secret 静态不可提取
- K29 平台 pcxllite→client XW-G4；标签 driveApiAllowLocalToken/withLocalControlApi/disableLauncherAuth/withoutHome
- K30 本地 inner-api 白名单 allow_inner_api_paths 25 条（含 /device/v1/try_speed、/drive/v1/files 等）
- K31 DriveListen 127.0.0.1:5050 / LauncherListen 5051 / PublicPort 21603 插件网关（403 handler not exists）
- K32 .drive KV 加密存储；日志泄露 coreEncryptKey eb5aa306…
- K33 DownloadSDK 导出面 = 纯下载引擎（100+105 导出无登录）；XL_SetUserInfo ABI 为 char*,char*
- K34 安装包 = OnlineInstaller 引导器（stream_fuzzy_encoder/decoder+bencoding）
- K35 云盘直下无私有格式：web_content_link=原始文件（MD5 全量一致铁证）
- K36 免费档单连接 ~150KB/s；多连接近线性至 8 并发饱和 ≈1MB/s
- K37 web seed(BEP19) 与迅雷 CDN 完全兼容；直链 URL query at= 签名禁改
