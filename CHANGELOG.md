# Changelog

本文件记录 Smart Downloader 的用户可感知变更。
格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本号语义：`0.x` 阶段以能力批次为单位推进，不承诺字段级兼容。

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
