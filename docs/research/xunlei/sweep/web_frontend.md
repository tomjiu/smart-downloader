# S1 前端业务码扫描（重试版）

> 语料：`scripts/research/cloud_delivery/login_reverse/node_modules_dump/`（907 个 .js，≈2.5MB，
> 新迅雷桌面 CEF 内嵌 Web 前端全集，含 ~76 个 obfuscator 混淆 chunk，字符串表在外部未入 dump）。
> 方法：`scripts/research/xunlei/sweep_s1.py`（一次读入逐文件正则 + `'..'+'..'` 拼接折叠还原被拆片的 URL/路径）。
> 比对基线：`KNOWN_ITEMS.md` K1~K37。命中→「已知 K##」；能力性新内容→ GAP_LIST；vendor/运行时→ SKIP。
> 明细产物：同目录 `s1_inventory.json` / `s1_endpoints.json` / `s1_switches.json` / `s1_report.txt`。

## 1. 清点

**总数 907 = SKIP(vendor) 4 + 业务候选(BIZ) 67 + 其他(OTHER) 836**
（OTHER 抽样复核均为 <1KB~8KB 的 webpack 运行时胶水/空壳 chunk，不满足 vendor 启发式票数故单列；
混淆 OBF 共 76 个，其中 31 个落入 BIZ。功能开关字段与 WebSocket：本 dump 中为零——开关值疑在被外部字符串表吞掉的查表层。）

### Top15 业务文件

| # | 文件 | KB | 判定功能 |
|---|------|-----|---------|
| 1 | m_1182.js | 227.7 | xluser SDK 全量 API 封装：v1/auth+user 全家桶（signin/signup/provider/sudo/authorize…） |
| 2 | m_704.js | 207.4 | xluser SDK 孪生件：错误码总表 + token/introspect + 安全验证文案 |
| 3 | m_1431.js | 204.7 | 上传管理页：流量限额、上传记录、暂停/清除、`xlUploadUnfinished` |
| 4 | m_81.js | 105.5 | 全局 UI 基座：弹框/会员身份徽章组件、分享路由 share/{manage,pc,mobile}、client_id=Xqp0(K14) |
| 5 | m_600.js | 56.5 | 客户端桥接层：登录弹窗态机、127.0.0.1:28317/yun_fetch_back 转存回传、getuserinfo、pay 跳转 |
| 6 | m_933.js | 33.3 | 网页播放器：web-vod-xdrive ts_downloader 直链、倍速、试看两分钟、自动播放引导 |
| 7 | m_1.js | 22.5 | 全局请求层(OBF)：五个网关 env baseURL 表 + Peer-Id/Guid/Credit-Key 请求头族 + 客户端原生桥分流 |
| 8 | m_495.js | 22.2 | 云盘主界面 store：回收站、加星、享特权目录树 |
| 9 | m_12.js | 8.7 | 会员/容量运营(OBF)：支付中心跳转、云添加次数限制、sandai 安装包分发、thunderx:// 拉起 |
| 10 | m_167.js | 6.3 | 协议唤端(OBF)：`xunlei://path?query` 组装 + oia-pan-ssl 跳板编码 |
| 11 | m_51.js | 5.3 | 分享访问态文案(OBF)：提取码/审核拦截/侵权下架/特权资格申请流 |
| 12 | m_329.js | 5.1 | VIP 图标组合规则引擎(OBF)：超会/白金/云盘会员过期规则合成 |
| 13 | m_132.js | 5.1 | drive-reader 文本预览(OBF)：api-pan/drive-reader/v1，乱码检测 |
| 14 | m_23.js | 2.0 | OAuth 授权配置(OBF)：i.xunlei.com/center/account/personal/oauth scope=profile、dev-xluser |
| 15 | m_1066.js | 1.2 | xmodels 开放平台(OBF)：api-xmodels.xunlei.com/models/v1/apikey 创建 |

次级业务件：m_90(订阅 API 族)、m_134(在线解压)、m_67+m_1127(书城/小说)、m_171+m_17(chitchat 圈子)、
m_100(笔记+快鸟取链)、m_34+m_1228(union 行为上报)、m_516(分享卡片聚合)、m_938(私密文件夹)、
m_520(手雷客户端登录桥+会员升级)、m_942/m_945(复制移动转存解压上传云添加)、m_845(图片消费)、
m_937(笔记/回收站列表操作)、m_943(客户端下载引导)、m_1299(密码服务)、m_239/m_999(xluser2/3 代网关)、
m_235(微信签名+yun_fetch_back)、m_99(pc_info)。

## 2. 端点总表

host=`-` 为相对路径字面量（经拼接折叠还原）；多数挂接 m_1.js 的 env 网关表。状态：K##=已知，G#=GAP 新发现，S=SKIP 性质。

| 路径 | host | 次数 | 文件 | 状态 |
|------|------|-----|------|------|
| /api/subscribe/own/info · /list · /detail(list) · /info · /delete · /search(POST) · /whites · /subscribe/public/news | 待定(查表网关) | 9+ | m_90 | **G1** |
| /decompress/v1/list · /decompress · /progress | pan 系(参数 file_space) | 各1 | m_134 | **G2** |
| /activity/v1/rewards | - | 1 | m_111 | **G21** |
| /chitchat/v1/group/query (POST group_id) | - | 1 | m_171 | **G9** |
| /mediahub/v1/events · /event · /events:delete；/books/hot·rankings·labels；getBookInfo/{bookId} | - | 2+3 | m_67 | **G8** |
| /xlppc.pan.note/api/getNoteList | - | 1 | m_100 | **G8 注**(笔记) |
| /dlj/{bird_key}/url | - | 1 | m_100 | **G7** |
| /models/v1/apikey | api-xmodels.xunlei.com | 3 | m_1066 | **G12** |
| /user_info/pc_info (uid) | - | 1 | m_99 | **G22** |
| /v1/auth/signin · signin/token · signup · signup/anonymously · token · revoke · reset · device/code · verification(/verify) · auth/provider/{uri,token} | xluser-ssl(-ssl2/3, dev-) | 4 | m_1182,m_704 | 核心 **K3**；anonymously+provider 族 **G4** |
| /v1/user/me · query · profile · contact · password · sudo · authorize{,/info,/detect} · device/authorize · trans/by/provider · provider(/bind) | xluser-ssl | 4 | m_1182,m_704 | 常规 **K3 同族**；sudo/authorize/detect **G5**，provider/bind **G4** |
| /v1/auth/token/introspect | xluser-ssl | 1 | m_704 | **G5** |
| /v1/shield/captcha/init | xluser-ssl | 2 | m_1181 | **K3** |
| /v1/getuserinfo (POST account/account_type=userid) | xluser-ssl | 1 | m_600 | **G6** |
| /v1/password (prod) | xluser-ssl (dev: password.office.k8s.xunlei.cn) | 1 | m_1299 | **G20** |
| /xlppc.searcher.api/drive_file_search | api-gateway-pan | 1 | m_10 | **K2** |
| /drive-reader/v1 | api-pan.xunlei.com | 1 | m_132 | **G17** |
| /share/manage · /share/pc · /share/mobile · /share/man | 前端路由 | 3 | m_81,m_12 | UI 路由(非 API) |
| https://api-xl9-ssl.xunlei.com/sl_dev · /sl | 五 env 网关 | - | m_1 | **G18** |
| easy-mock.com/mock/…/red-envelope | mock | 1 | m_1 | S(测试桩，暗示红包活动) |
| api-shoulei-ssl / test-api-shoulei-ssl | shoulei | 2 | m_1 | **K8** |
| api-gateway-pan.xunlei.com (+test-api-gat…) | gateway-pan | 2 | m_1 | **K2** |
| api-pan.xunlei.com (+alpha-drive.office.k8s) | pan | - | m_1,m_132 | **K1** |
| conf-m-ssl.xunlei.com/external/<guid …0ac1-9b0d-11ea-a017-0242ac140002> | conf | 1 | m_100 | **K7** |
| https://union-gateway-pan.xunlei.com/union/v3/…reporting/user… (+office k8s) | union | 1 | m_34,m_1228 | **G13** |
| http://127.0.0.1:28317/yun_fetch_back/ | 本地 | 2 | m_600,m_235 | **G3** |
| http://10.10.45.67:19099/ts_downloader + https://web-vod-xdrive.xunlei.com/ts_downloader | vod | 2 | m_933 | **G23** |
| https://pay.xunlei.com/pages/2020/web-pay-center/?default_tab= · ges-pay.xunlei.com/android-sl-pay | pay | 2 | m_600,m_12 | **G14** |
| https://weixinapi-m-ssl.xunlei.com/wx/sign/js_api?page_url= | weixin | 2 | m_235,m_24 | **G15** |
| https://oia-pan-ssl.xunlei.com/?query=<enc(xunlei://…)> &ct= | oia-pan | 1 | m_167 | **G16** |
| https://i.xunlei.com/xluser/auth/ · /center/account/personal/oauth/?scope=profile · /xluser/validate/enter/modifyphone_enter.html | i.xunlei | 4 | m_602,m_23,m_1431 | **G19** |
| xluser2-ssl / xluser3-ssl / dev-xluser-ssl.xunlei.com | xluser 代际 | 4 | m_239,m_999,m_23 | **G19** |
| down.sandai.net/thunder11/XunLeiWebSetup_pan.exe · /mac/thunder.dmg · m.down.sandai.net/mobile/yunpanh5.apk | CDN | 3 | m_12,m_627,m_624 | S(K34 安装包链) |
| thunderx:// (scheme) · a.app.qq.com(应用宝 CK1470052035866) | - | 2 | m_627,m_600,m_12 | S(唤端) |
| backstage-img-ssl.a.88cdn.com · static-xl.a.88cdn.com | 静态CDN | 2 | m_81,m_50,m_289 | S |
| w3.org/svg · github.com · feross.org · clipboardjs.com · baidu.com | 文档/外链 | - | 多处 | S |

本 dump 未出现（由桌面 Go 层持有，符合 K27/K30 架构判断）：speedup/dev-speedup、mqtt.xbase.cloud、本地 5050/5051/21603。

## 3. 功能模块地图

```
账号与鉴权   xluser-ssl(1/2/3代,dev) v1/auth+user 全家桶; captcha K3; token introspect;
             第三方 provider 登录/绑定(G4); 匿名注册(G4); sudo 提权会话(G5); 密码服务(G20);
             i.xunlei.com 授权页/手机号修改(G19)
客户端桥     登录弹窗态机(m_600); 手雷桥(m_520); 原生 request 分流(isAndroid/iOS/HarmonyNative);
             127.0.0.1:28317/yun_fetch_back 分享转存回传(G3); thunderx:// 唤端; oia-pan 跳板(G16)
网盘核心     主界面 store(m_495); 列表操作 复制/移动/转存/解压/上传/云添加(m_942/945);
             回收站/加星/私密 SAFE 文件夹+审核态(G11); 图片消费(m_845); drive-reader 文本预览(G17);
             在线解压 API(G2); 上传管理+流量限额(m_1431)
分享体系     分享路由 manage/pc/mobile; 提取码/审核/特权文案(m_51); 分享卡片聚合+推荐群组(G10);
             复制分享/口令(m_870,m_9 迅雷口令)
社交/内容    chitchat 群组查询+圈子投票(G9); 订阅/动态 feed(dst_uid+scene)(G1);
             书城 books/hot·rankings·labels+阅读历史 mediahub/events+小说章节解析(G8); 笔记 note(G8注)
会员增长     VIP 图标规则引擎(m_329); 身份徽章组件(m_81); 会员升级导购(m_520);
             pay/web-pay-center 跳转(G14); activity/v1/rewards 活动奖励(G21); red-envelope mock(G18)
开放平台     xmodels models/v1/apikey 密钥创建(G12)
基础设施     全局请求层五网关+Credit-Key/Space-Authorization 头(m_1); union 行为上报双通道(G13);
             conf-m-ssl 配置下发 K7; web-vod 播放直链+倍速+试看(G23); 微信 JS-SDK 签名(G15);
             客户端下载引导 sandai CDN(S)
```

## 4. GAP_LIST

| G# | 猜测能力 | 证据原文(≤120字符) | 置信度 | 建议动作 |
|----|---------|-------------------|--------|---------|
| G1 | 用户订阅/动态系统：按 dst_uid+scene 关注、sub_stat 状态、公开动态 news、白名单 | `/api/subsc'+'ribe/own/i'+'nfo`…`{'dst_uid':..,'scene':..,'with':['sub_stat']}` `subscribe/publi'+'c/news` | A | 后端探测 /api/subscribe/info GET；scene 取值枚举 |
| G2 | 云盘在线解压：列包/解压/进度，支持压缩包密码与 file_space 空间参数 | `/decompres'+'s/v1/decom'+'press',{'gcid':..,'password':..,'parent_full_path':..,'file_space':..}` | A | 抓包 decompress POST 全参；gcid 即 K35 体系哈希 |
| G3 | 本地回传服务 28317：网页把分享选中文件推给桌面端处理（新端口，K31 未录） | `fetch("http://127.0.0.1:28317/yun_fetch_back/",{body:{files,userId,fromShare,shareId,passCodeToken,shareUserId}})` | A | 定位 28317 监听进程与其余 /yun_* 路径 |
| G4 | 第三方账号 provider 体系：OAuth uri/token 交换、绑定、按 provider 查事务、匿名注册 | `/v1/auth/signin/with/provider` `'/v1/auth/signup'+'/'+'anonymously'` `/v1/user/trans/by/provider` | A | 枚举 provider 取值；测匿名注册可得无手机号 token |
| G5 | 会话提权面：user/sudo、authorize/detect、device/authorize、token/introspect | `'/v1/user/sudo'` `'/v1/auth/toke'+'n/introspect'` `'/v1/user/author'+'ize/detect'` | A | sudo 换取的 token 权限域验证 |
| G6 | 可查任意用户资料：account_type=userid → vip.isyear/avatar | `Ue("https://xluser-ssl.xunlei.com/v1/getuserinfo",{account:String(te),account_type:"userid",scene:o})` | A | 越权面测试：遍历 userid 是否可拉他人 VIP 态 |
| G7 | 快鸟取链端点 /dlj/{bird_key}/url（K5 家族新路径形态） | `'/dlj/'['concat'](bird_key,'/url')` (m_100) | A | bird_key 来源回溯；响应是否为提速后直链 |
| G8 | 书城/听书产品线：books hot/rankings/labels、mediahub 阅读历史 events(:delete)、小说章节解析 | `'/books/hot','ranking':'/books/ran'+'kings','addHistory':'/mediahub/'+'v1/events'`; 正则`第[..]{1,7}(章\|节\|集\|卷\|部\|篇\|回)` | A | mediahub 网关归属确认；books 域名探测 |
| G9 | 圈子社区(发帖/投票)：chitchat 群组查询 + has voted/not found circle 错误码 | `'/chitchat/'+'v1/group/q'+'uery',{group_id:Number(..)}`; `'not found circle':'未找到该圈子'` | A | chitchat 其余端点(发帖/feed)挖掘 |
| G10 | 分享卡片聚合：卡内容带 recommended_group 群组推荐 + public 分享批量拉取 | `c.resources.recommended_group.list.map(...)` `h.a.getSharedList({public:!0,filters:{id:{in:c.join(",")}}})` | B | 卡片接口定位(y.a("0",id))与 shareToken 流 |
| G11 | 私密文件夹与内容审核态暴露：folder_type=SAFE、audit.status 敏感禁止查看 | `"SAFE"===e.info.folder_type&&e.isSafeRed` `"STATUS_UNKNOW"===e.info.audit.status?"未知":"敏感"` | B | files 接口返回结构中 audit 字段取证 |
| G12 | xmodels 开放平台：API Key 创建/管理 | `'https://ap'+'i-xmodels.'+'xunlei.com'+'/models/v1'` `('/apikey',{'name':..,'description':..})` | A | apikey 增删改查面与鉴权方式 |
| G13 | union 行为上报网关(pan 版)+内嵌 key | `'union-gatewa'+'y-pan.xunl'+'ei.com/uni'+'on/v3'`; `_0x2ffa84='1a1b3c5d8a'+'91298'` | A | key 用途(签名/appkey)确认；上报 schema |
| G14 | 支付中心 PC/安卓双页跳转 | `'pay.xunlei.com/pages/2020/web-pay-center/?default_tab='` `'ges-pay.xunlei.com/pages/2020/android-sl-pay/'` | A | default_tab 枚举；订单回跳参数 |
| G15 | 微信 JS-SDK 签名服务 | `'weixinapi-m-ssl.xunlei.com/wx/sign/js_api?page_url='['concat'](..)` | A | 返回 wx.config 所需签名四件套 |
| G16 | xunlei:// 唤端跳板页：协议串 URL 编码包装成 https 兜底 | `'xunlei://'['concat'](path,'/')(query)` → `'oia-pan-ssl.'+'xunlei.com'+'/?query='(encodeURIComponent(..)+'&ct=')` | A | ct 参数含义；跳板可否携带任意 path(开放重定向面) |
| G17 | 云盘文本读取器 drive-reader（prod 挂 api-pan） | `'production':'https://ap'+'i-pan.xunl'+'ei.com/dri'+'ve-reader/'+'v1'` | A | 子路径枚举(file/content)；乱码检测逻辑 |
| G18 | xl9 网关 /sl(/sl_dev) 与红包活动痕迹 | `'development':'https://ap'+'i-xl9-ssl.'+'xunlei.com'+'/sl_dev',...'production':'…/sl'`; `easy-mock…'/red-envelope'` | B | sl 前缀语义(shoulei?)；线上红包活动端点 |
| G19 | xluser 多代网关与 i.xunlei 授权页族 | `'https://xluser2-ssl.xunlei.com'` `'https://xluser3-ssl'` `'i.xunlei.com/center/account/personal/oauth/','scope':'profile'` | B | 各代际差异；oauth 页支持的 scope/ client 列表 |
| G20 | 独立密码服务(prod 并入 xluser /v1/password) | `'password.office.k8s.xunlei.cn/v1/password',…'production':'https://xl'+'user-ssl…/v1/passwor'+'d'` | B | password 子操作(reset/change/verify) |
| G21 | 活动奖励领取 | `Object(_0x4490b3['e'])('/activity/'+'v1/rewards',_0x5a27e7,{})` | B | activity 网关与 rewards 前置任务接口 |
| G22 | PC 端用户扩展信息 | `'/user_info'+'/pc_info',{'uid':..}` (m_99, 含 clientId) | B | pc_info 返回字段；所属网关 |
| G23 | vod 直链下载器：公网 web-vod-xdrive + 内网 :19099 双通道 ts_downloader | `'web-vod-xdrive.xunlei.com/ts_downloader'` `'http://10.10.45.67:19099/ts_downloader'` | B | 该服务与 K25 PLAY 直链的关系；内网 IP 出现条件 |

> 备注：功能开关(switch/gray/experiment 字段)在本 dump 明文层为零，疑随外部字符串表缺失；S2 若拿到完整 HTML/入口 bundle 可补扫。SKIP 项(w3.org/github/88cdn/sandai/easy-mock/a.app.qq.com)均为静态资源、文档链接或分发 CDN，不计入 GAP。
