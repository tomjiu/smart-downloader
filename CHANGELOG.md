# Changelog

本文件记录 Smart Downloader 的用户可感知变更。
格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)；
版本号语义：`0.x` 阶段以能力批次为单位推进，不承诺字段级兼容。

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
