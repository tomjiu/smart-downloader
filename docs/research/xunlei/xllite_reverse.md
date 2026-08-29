# xllite.exe（桌面云盘引擎）逆向档案

> 2026-08-25。多代理接力（静态分诊 → Go 符号 → 运行时 Frida → 活体探测）。
> **总结论：凭证表受保护无法静态/轻动态提取；但组件架构已完整摸清，SMS 保底为当前最快解锁路径。**

## 1. 组件定位（A级）
- `xllite.exe`（50MB，Go 1.2x + 内嵌 Vue/Element 前端）= 桌面迅雷的**云盘/登录引擎**
- 包路径 `gitlab.xunlei.cn/xlppc/pan-cli/pkg/platformdetect`；符号区偏移 ~39706533
- 运行时以 thunder 子进程存活（PID 实测），家目录 `C:\Users\<user>\.drive\`

## 2. 平台→client 映射（A级·运行时日志确认）
`PLATFORM=pcxllite` 启动 → 日志打印 `pcxllite XW-G4v1H72tgfJym`
- client 家族（二进制常量池）：X9ibISwpIp8jQ4Ya / XVJVzaJv8vKHzVCk / XW-G4v1H72tgfJym / XW5SkOhLDjnOZP7J(登录页) / Xqp0kJBXWhwaTpB6(web pan) / Yd0*GrNJhCC2oX 系列(电视盒)
- **api-pan 白名单**（H 代理交叉确认）：X9ib / XVJV / Xqp0 / Yd0*；**XW5Sk 与 XW-G4 不在册**

## 3. 凭证存储（否定结论，A级）
- secret **非静态字面量**：全量字符串扫描、R1c JSON/YAML 字面量扫描、物理相邻假设、运行时 Memory.scan 全部落空
- `PlatformConfig.GetClientSecret(name)` 从**混淆/重定向结构**动态解析；错误串所在节未按需分页，轻量 hook 无法锚定
- `.drive\device_info` 为加密 blob；两个 KV 库（6c84…/cc7b…）被活实例独占锁定
- 剩余可行路径（遗留）：Go pclntab 完整解析+Stalker 全量 trace，或带符号 debug 构建——成本高，暂缓

## 4. 活体实例行为（A级·实测）
- 配置默认：DriveListen=127.0.0.1:5050 / LauncherListen=127.0.0.1:5051 / DrivePublicPort=21603
- 活实例实际只监听 **0.0.0.0:21603**（插件网关：未知路由一律 403 "handler not exists"）
- drive 真实 API 在 5050，但活实例**未监听**——等 Launcher(thunder 主程序) 经 5051 握手喂 token 后才起
- 组件间令牌：PluginTokenDuration=5h / RefreshTokenDuration=1h（本地互联协议，未破）
- 启动配方：cwd=可写空目录 + `PLATFORM=pcxllite`（否则 rename panic）；envconfig 文件可覆盖配置
- 家目录关键文件：`user.core.db`(SQLite) / device_info(加密) / 两个 hash 名 KV 库(被锁)

## 5. OAuth 结论（H 代理，A级·反证）
- 本域登录 = OAuth2 设备码流（已实现）；**不存在**"XW5Sk 票换 pan 票"的交换端点
- `pan.xunlei.com/yc/oauth-callback` = 阿里云盘挂载（第三方文件源），与迅雷 pan 无关

## 6. 行动结论
- 云盘功能的**当前最优解 = SMS 短信登录**（流程已验证至 verify 步，等新鲜验证码）
- 次优 = 账密（被 result:review 滑块风控阻塞，需 xl_al 指纹移植）
- xllite 路线归档：除非投入 Stalker 全量追踪，否则不再推进
