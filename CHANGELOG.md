# Changelog

本文件记录 Smart Downloader 的用户可感知变更。
格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本号语义：`0.x` 阶段以能力批次为单位推进，不承诺字段级兼容。

## [0.2.3] - 2026-10-18

第十轮修复批次（batch10）：基于 2026-10-16 三轮后端审查 + 架构评审
（[docs/review/](docs/review/)，覆盖约 52,700 行）修复全部 14 个 P0、
30+ 个 P1 与架构/文档问题，每批修复均以真实测试验证
（复现工程 [repro/p0repro/](repro/p0repro/) 3 个缺陷实证全部转绿）。

### 崩溃与安全（P0）
- **多文件 .torrent 导入除零 panic**（P0-1，已实证复现）：bencode 字典序
  下 `files` 先于 `piece length` 出现，真实种子（mktorrent/libtorrent/
  Transmission 生成）全部命中——扫描时 piece_length 仍为 0 即计算偏移。
  修复：两阶段解析（偏移计算延后到校验后）+ piece_length 16MiB 上限；
  测试 fixture 修正为真实字典序键序
- **total_size 求和溢出绕过磁盘预检**（P0-11，已实证）：`[u64::MAX, 1]`
  回绕为 0 → 预检放行 → 写爆磁盘。修复：checked_add + TotalSizeOverflow
- **loopback 前缀绕过**（P1-a，已实证 4 项）：删除 `starts_with("127.")`
  兜底——`127.evil.com` 经 DNS 解析公网可骗过 fail-closed 闸门未鉴权绑公网
- **登录页 DNS rebinding 防护**（P0-7）：本地登录页（接收明文密码）原零
  HTTP 头校验，全路由套 Host 白名单 + Origin 同源守卫中间件
- **serde 部分表默认值错位**（P1-b，已实证）：子表手写 Default 与
  Config 单源——只写 `[server] http_token` 令 addr 变空串（daemon 起不来）、
  只写 `[bt]` 任一键令 BT 静默关闭
- **sanitize_rel 裸 CurDir**（P1-1）：`"."`/`"./"` 拒绝（dest 指向
  dest_root 自身的静默失败链）+ Windows 保留名尾空格等价形态
- **slot 预留超卖竞态**（flaky 实证 5 次 2 败）：gate_or_enqueue 检查+
  插入+登记原子化

### 数据完整性（P0）
- **空洞文件双层防线**（P0-4）：段数据 fsync 后才记账（WAL 顺序，崩溃后
  resume 不再交付长度正确的全零文件）+ 无摘要任务的完全稀疏检测
- **无 ETag 跨源混拼**（P0-5）：换源作废判定与段账本同口径（total 不一致/
  指纹消失/指纹不等均作废）——原实现主流无 ETag CDN 上恒放行，旧源账本
  续传新源即静默数据损坏
- **限速器三连**（P1-1/2/3）：持锁 sleep 串行化（并发段速率低于设定值）/
  短窗口丢字节（速率系统性低估）/ 热调不清旧速率债务
- **降级接受语义**（P2-19）：哈希不符不再标 Completed（文件落位保留 +
  Error 明示损坏，7+2 个测试断言同步）
- gen≥4 换源残留 .part 永久泄漏（枚举 0..=255）；finalize_part 预删
  dest 数据丢失窗口（直接 rename 原子覆盖，Bug C/P2-17 双语义收敛）

### Provider 与引擎
- **匿名设备号配对**（P0-6）：单一 did32 缓存——原三调用点各自重新生成，
  captcha_token 与 x-device-id 不匹配致匿名分享解析 100% 失败
- **夸克/迅雷任务 ID 碰撞**（P0-8/9）：原子序号——同分享并发任务不再
  互相覆盖句柄（文件内容错配根除）
- **provider 并发闸门生效**（P1-17）：coordinator 层 RAII 占位——原
  `busy < limit` 恒真（全仓无递增点），100 兜底任务可同时打进同一 provider
- **refresh_auth 持锁跨 await**：三段式（快照→锁外网络刷新→CAS 回写）
- **迅雷引擎孤儿任务**（P0-13）：start 失败回滚登记 + best-effort 删除
- **cid_store 二次方爆炸**（P0-14，已实证 384KB 输入 6s + 95 万候选）：
  二分查找 + 候选上限 + BTreeMap 区间（O(n²)→O(n log n)）
- **bdecode 截断输入**（P1-23）：unterminated dict/list 报错——被截断的
  fastresume 原「校验通过」
- **XLTaskFlow ABI 漂移**（P1-24）：补 packed（实际 size 32 与声明 0x18
  漂 8 字节）+ 断言补入回归
- **xunlei Unknown 状态伪装**（P1-19）：映射 Error 不再伪装下载中
- **xudt frame 32 位回绕**（P1-22）：checked_add

### daemon 与运维
- **设置即执行**（P0-10）：`PUT /settings` 配置 completion_action 不再
  立即关机/退出（动作只由任务完成事件触发）
- **RSS 持锁落盘**（P1-9，6 处）：锁内快照锁外写盘；**未处理条目上限**
  （P1-10，5000/feed 防内存爆）
- **热重载校验同口径**：负 share_ratio 等启动被拒的值不再热重载静默生效
- **持久化 fsync**（tasks.json/bans.json/quark cookie/NAS token）：0600
  先建后写 + sync_all（掉电丢全部任务/凭据可读窗口根除）
- **lockfile unlink 竞态**：Drop 只 unlock 不删文件（防双实例）
- **FTP PASV 值域校验**（P1-4）/ is_terminal 死代码（P1-5）/ SFTP 终态
  判定文案脆性（P1-6）
- **长驻循环启动盲区**（P2-8）：三处循环先工作后 sleep
- **`DELETE /security/bans` 带 body**（P2-1）：新增 POST unban 端点；
  **tokenless Host 缺失**（P2-3）改 fail-closed

### 架构与文档
- 幽灵依赖消除（xunlei-convert → xunlei-ffi 删除）；daemon nas feature
  真正转发 btcore 校准版
- 版本号全线统一 0.2.3（workspace/desktop/tauri.conf）；LICENSE-MIT +
  LICENSE-APACHE 补齐（deny.toml 门禁合规硬缺口）
- LOCK_MODEL.md 勘误：autosave 例外边如实登记 + 审计方法盲区章节
  （`_` 前缀命名漏报机制）；BACKLOG 登记 A0 技术债（bencode 统一/
  trait 拆分等 4 项架构演进）；D28 同号两义改名 DX-P2P-EXCL
- README 文档地图 5→12 条；xunlei-import feature 编译修复（ContentIdentity
  新字段未跟上，上游预存）

### 验证
- workspace（排除 btcore）997 passed / 0 failed（基线 983 + 14 新回归）
- daemon `--features bt,ftp,sftp,xunlei-import` 433 passed 全绿；
  btcore 42 passed；provider 164 passed；xunlei-convert 22 + ffi 8 passed
- repro/p0repro 23 passed（3 个缺陷实证全部转绿为回归守护）
- cargo clippy 0 error 0 warning（新引入代码）

## [0.2.2] - 2026-09-08

第七轮收尾批次（batch7）：batch6 遗留 5 项暂缓缺陷全部落地 + 回归测试；
第八轮全仓安全审计批次（batch8）：4×P1 全修 + 5×P2 修复（详见
[docs/SECURITY_AUDIT.md](docs/SECURITY_AUDIT.md)）；
第九轮工具化安全审计批次（batch9）：cargo-audit/cargo-deny/gitleaks 三件
行业标准工具全量扫描 + 依赖漏洞修复 + 审计门禁固化入库。

### 安全加固（batch9，工具化审计）
- **依赖漏洞清零**：h2 0.4.15→0.4.19（RUSTSEC-2026-0258 HTTP/2 空帧 DoS）、
  quick-xml 0.37→0.41（RUSTSEC-2026-0194/0195 清单解析 DoS）——cargo-audit
  复扫 0 漏洞
- **quick-xml 0.41 事件模型适配**：DASH/Metalink4/RSS 三解析器补
  `GeneralRef` 实体事件分支与文本累积语义；新增实体切分回归测试
- **审计门禁固化**：`deny.toml`（advisories/bans/licenses/sources）+
  `.gitleaks.toml`（精准 allowlist，逐条取证理由）入库；CI 新增 security
  job（cargo-deny-action + gitleaks-action 全历史扫描）
- **许可与发布元数据**：workspace 统一 `MIT OR Apache-2.0` + `publish=false`

### 安全修复（batch8，P1 全部）
- **空 token 击穿 fail-closed**（P1）：config `http_token = ""` 可同时骗过
  非回环启动检查（视为已配置）又令鉴权全放行（注入时过滤成 None）——
  resolve 双侧过滤空串，回归锚定
- **DNS rebinding → tokenless 回环 RCE 链**（P1）：新增 Host/Origin 同源
  守卫——tokenless 模式下 Host 必须回环（或 `[server] extra_allowed_hosts`
  白名单），跨站 Origin（CSWSH/表单 CSRF）同口径 403；HTTP/1.0 无 Host
  放行（CLI 兼容）；配置 token 后守卫自动关闭（反代域名不受影响）。
  实弹：`Host: evil.com` → 403、正常访问 200
- **FTP PASV 数据面 SSRF**（P1）：数据连接目标校验——内网/回环/链路本地
  （含 v4-mapped）且异于控制对端 → 拒绝；双公网 NAT 场景放行（curl
  同款取舍）；RETR/LIST 全覆盖
- **SFTP host key 全接受**（P1）：TOFU——首录 `sftp_known_hosts.json`
  （0600 原子写）+ 接受；失配拒绝（换钥 = MITM/重装，日志指明恢复方式）；
  缺省内存 TOFU
- `/settings` 代理凭据脱敏回显（P2，与 /config 同口径）；全局并发请求
  上限 256（P2 slowloris 面）；provider 云端文件名 sanitize 后才 join
  （P2 dest_root 外建目录面）；ftp:// 明文凭证显式告警（P2）；hook 子进程
  剥离敏感环境变量（P3）；`.part.progress`/fastresume 唯一 tmp + 0600
  （P3）；`is_loopback_addr` v4-mapped 语义判定（P3）；sanitize_rel 补
  Windows 保留设备名与尾点尾空格（P3）

### 并发与竞态（同根因闭环）
- **配额闸门 TOCTOU 根治（预留槽位改造）**：add 路径改「预留占位记录 + 引擎接入」
  两段式——有槽位（或 0 = 不限）时同步插入 Queued 占位记录并登记
  slot_reservations 集合，engine.add 成功 attach 句柄 / 失败回滚删除。
  add await 在途窗口任务对并发 add 可见且占槽，关闭「记录未插入前并发 add
  双双过闸」的超卖窗口（magnet/.torrent/HTTP/FTP/SFTP 五入口统一）。
  attach 双检竞态间隙用户意图：暂停/移除/resume 抢先激活 → 尽力回滚引擎
  任务不留孤儿；resume 在预留窗口内幂等成功（不二次 engine.add）
- **完成动作全表快照竞态根治**：终态判定与 execute_completion_action 收进
  同一 tasks 锁临界区（execute 同步：exit 内联、power 动作仅 dispatch），
  间隙插入的新任务（RSS 自动建任务/并发 add）不再被 exit/shutdown 误杀

### 正确性
- **ban 重放失败脏标记重试**：replay_bans 单条失败/引擎不可用不再仅 warn
  后整会话丢失——挂入 ban_replay_failed 待重试集，serve 30s tick 经
  retry_pending_bans 重发至成功；全程持 ban_ops 串行锁（防与用户 unban
  交错把已解封 IP 重新封回），用户已解封条目自动丢弃
- **FTP 续传 MDTM 指纹（G2 类 FTP 残留根治）**：下载轮起点经 RFC 3659
  MDTM 探测远端 mtime 写入段账本 last_modified 位，恢复时按 HTTP 双指纹
  同款 fingerprint_ok 核对——远端文件同长被替换 → 账本作废整文件重下
  （旧实现只比 total，同长不同内容拼出静默混合文件）；服务器不支持 MDTM
  → 降级旧 size-only 语义；账本有指纹而探测消失 → 宁枉勿纵重下

### 队列
- **created_at 持久化（重启 FIFO 保序）**：TaskMetadata 新增 added_at_ms
  （unix 毫秒，serde default 兼容旧档），add 路径统一写入；排队递补/队列
  位置排序键升级为 (queue_priority, added_at_ms, created_at)——created_at
  是单调时钟不可持久化，重启后恢复任务 FIFO 并列退化 + HashMap 无序；
  恢复路径对旧档（0 值）按加载序回填 1..n（恒小于真实墙钟 = 恢复任务排在
  新任务前，跨重启稳定）

### 测试
- 新增 9 例：并发 add 不超卖（multi_thread + 引擎延迟注入）/ add 失败预留
  回滚 / 预留窗口 resume 幂等 / ban 重试三态（失败入集-恢复收敛-unban
  不复活）/ added_at_ms 跨重启 FIFO 保序（E23 定时任务链路）/ FTP MDTM
  四态（一致续传-失配重下-不支持降级-指纹消失宁枉勿纵）

## [0.2.1] - 2026-09-07

第六轮子智能体批量代码审查修复（batch6）+ 桌面端三平台打包矩阵首跑验证。

### 审查修复（安全与正确性）
- **RSS 解析器 CDATA 事件丢弃**（P0）：quick-xml 将 `<![CDATA[...]]>` 投递为
  独立事件（非 Text），旧实现静默吞掉 → WordPress 等主流 CMS feed 条目全丢；
  现已接取（原始文本不转义）+ 混合内容累加语义 + 回归锚测试
- **HLS/DASH 流式循环缺 epoch 单写者闸门**（P0）：暂停→恢复可产生双写者并发
  append 同一 `.part` → 静默交付损坏文件；现已引入 is_aborted 闭包
  （暂停 ∪ epoch 过期 ∪ 任务已移除）段间检查 + 落位前终检 + HTTP resume
  运行态不重 spawn（与 FTP 同口径）
- **BT 内核首尾块优先级 0 语义颠倒**（P1）：契约 0=恢复默认，内核透传为
  libtorrent 的「不下载」→ 首末块（及小于一块的小文件）永不完成；
  现映射 `lt::default_priority`
- **内核 fill_ih 补幂等/errored 句柄重建**（P1）：`.torrent`/fastresume 路径
  重试全程 duplicate 死锁（磁链路径已有修复，此为补齐）
- **重启后 Seeding 任务脱管**（P1）：记录强置 Queued 退出轮询管道（速率/
  名称回填/做种限制执法失效）+ 完成事件可能重发；现已恢复后回登记做种态
- **做种任务 pause→resume 后计时失效**（P1）：内核不重发 finished alert，
  `max_seeding_time` 永久失效；resume 后按引擎实况收敛记录态并重起计时
- rss.json 唯一 tmp+0600+损坏留存（对齐 tasks.json 加固配方）；RSS 刷新
  互斥（并发重复建任务）；规则首见命中去重；DASH 单文件表示改流式下载
  （120s 总超时+全量内存 → 大文件必挂）；FTPS 全局共享 TLS connector
  （vsftpd `require_ssl_reuse` 默认配置兼容）

### 其他修复
- 激活窗口暂停意图守卫；pause/resume 广播 from 态实值化
- downloaded==0 视作分享率 ∞（qbit 口径）；空白 RSS 关键词 add 即拒；
  RSS 已处理条目上限 0 值防护；集数过滤全量提取（多集标题漏检）
- recheck/announce/超级种子句柄缺失 404（原 500）；封禁区间 v4-mapped 归一
- HLS 段数/key 缓存/清单体积上限；显式全零 IV 保留（Option 根治哨兵混淆）；
  SegmentTemplate Rep 级逐属性继承（ISO 23009-1）
- 暂缓项（配额闸门预留槽位改造等 5 项）记入 BACKLOG

### 桌面端
- desktop CI 三平台打包矩阵首跑验证：Linux/macOS 构建链修复（brew
  libtorrent 2.1.0 RTC 构建态宏一致性探测兜底），Windows vcpkg 缓存复跑
- 桌面安装包以 `desktop-v0.2.1` 标签发布（含本批全部修复）

版本号语义：`0.x` 阶段以能力批次为单位推进，不承诺字段级兼容。

## [0.2.0] - 2026-09-07

第五轮全库审查修复（12 项）+ qBittorrent/BitComet 对标补齐 + RSS 订阅 UI。

### 审查修复（安全与正确性）
- **重启后 IP 封禁重放时序失效**（P1）：重放早于 bans.json 回读，重启后
  封禁从未下发引擎——现已修正顺序并在冒烟中实证 3/3 重放
- **BT 内核 ip_filter 读改写竞态**（P1）：并发封禁/解封丢失过滤规则，全程收锁
- magnet 导出种子 metadata 未就绪 409（原 500）；引擎侧任务缺失 404
- 队列优先级相邻移动溢出保护；强制宣告部分失败不再静默；封禁操作串行化
- BT/迅雷引擎同时启用启动期互斥报错（原静默覆盖致 BT 能力失效）
- 完成动作热更后补判；exit 补日志落盘窗口；电源动作移出 tokio worker
- `::ffff:x.x.x.x` v4-mapped IPv6 封禁归一

### 功能补齐（qB/BitComet 对标）
- **auto_retry 贯通全引擎**：BT/magnet/.torrent/FTP 任务失败自动重试（指数退避）
- **完成后移动支持 BT 多文件目录任务**：目录整体 rename + 跨盘递归回退
- **首尾块优先**：`POST /tasks/:id/piece-priority`（边下边播场景）
- **存储模式**：`[bt] storage_allocate` 新任务预分配（fastresume 保留原模式）
- **IP 段批量导入**：`POST /security/bans/import`（PeerGuardian DAT/P2P 格式；
  区间持久化 + 重启重放 + 对称解封）
- **RSS 订阅增强**：规则正则匹配（use_regex）、集数过滤（episode_filter，
  如 `1x02;1x04-1x06`）、每订阅独立刷新间隔
- `/stats` 新增会话累计上下行流量；`[download] max_redirects` 重定向深度可配

### 界面
- 新增 **RSS 视图**：订阅管理（增删/刷新）+ 规则表单（正则/集数过滤/目录）+
  条目命中状态（已建任务/未匹配）
- 设置面新增：存储模式（预分配）开关、HTTP 重定向跳数
- 统计页展示会话累计流量；任务详情新增首尾块优先操作（BT 任务）

### 已知边界
- 桌面版当前为 no-BT profile（原生 libtorrent 三平台打包矩阵待 CI 迭代）
- 云盘 F3.2（云解压）等抓包依赖项未落地

## [0.1.0] - 2026-09-06

首个对外能力批次。三引擎下载内核 + 设置运行时面 + Web UI + 桌面壳。

### 引擎与传输
- **HTTP(S)**：断点续传（双指纹校验）、动态分段（P0 方案A）、多源并行/备用 URL、
  代理（http/socks5/socks4，全局+任务级）、Cookie 会话、限速（全局/备用窗口/任务级）、
  RFC 7233 容错（非 Range 服务器 200 全量响应兜底）、HLS 直链、metalink4 引导
- **FTP**：单文件 + 目录递归；**FTPS**：RFC 4217 显式 TLS（AUTH TLS→PBSZ→PROT P）
- **BT**：libtorrent 2.0 内核（native FFI）、fastresume 持久化、magnet、
  DHT/LSD/UPnP/PEX/uTP/MSE 加密三态配置面、tracker 运行时增删、webseed、
  文件级优先级、顺序下载、监听端口/连接数上限（会话级）、分享率统计
- 通用：任务暂停/恢复/移除、重试与 auto_retry、定时错峰启动、并发队列配置、
  完成后 Webhook/移动/钩子程序、冲突策略、磁盘预检、任务改名/标签/备注

### 云盘线
- 迅雷：设备码登录 + 二维码、云盘任务导入、captcha_sign、provider 冷却降级、
  手动兜底（`POST /tasks/:id/fallback`）
- 百度：分享免登录解析（verify→BDCLND→meta→share/list）

### 观测与 API
- REST API（回环默认无 token；非回环强制 fail-closed）+ WS/SSE 事件三通道
- Prometheus `/metrics`、结构化日志、任务级日志

### 界面与桌面
- `ui/`：Next.js 静态导出 + qoder-ui 8 主题 + 四视图（任务/统计/日志/设置）+
  qbit 式设置八组 + 生效徽标；daemon `--ui-dir` 内嵌同源服务
- `desktop/`：Tauri v2 壳（sidecar daemon + 托盘 + 三平台打包 CI）；
  `serve --addr` 端口契约 + 应用数据目录收纳

### 已知边界
- 桌面版当前为 no-BT profile（原生 libtorrent 三平台打包矩阵待 CI 迭代）
- 云盘 F3.2（云解压）等抓包依赖项未落地
