# 迅雷加速体系逆向笔记（SuperSpeed / TrySpeed / 快鸟 / 经典引擎认证）

> 2026-08-25。来源：xllite.exe 静态字符串挖掘 + 全局配置 dump + speedup 服务实测。

## 一、四个产品线对照（澄清易混概念）

| 产品 | 后端 | 说明 |
|------|------|------|
| **快鸟宽带提速** | `speedup.xunlei.com/v1/{check_status,open,close,user_query,has_exporder,change_bind}` | 提升物理宽带带宽，需运营商/地区支持（北京电信实测 ret:11 err:1101 不支持）；与下载面板的会员加速**无关** |
| **下载试用加速（TrySpeed/SuperSpeed）** | 桌面本地 inner-api：`GET device/v1/try_speed/get_info`、`GET get_config`、`POST apply`；远端经 `HostHighSpeedFlow`(=api-pan) 的 VipSpeedUpUrl（精确路径待抓包） | **下载面板"会员加速"的真身**：按次数配额发放体验单，绑定到任务 ID 列表 |
| **组队加速** | VipTeamJoinUrl / team_times | 姊妹产品，teamTaskIDList 同构 |
| **经典引擎速度认证** | `speed.auth.vip.xunlei.com/speed/{speedup,res_status}` | DownloadSDK 用 XL_SetAccelerateCertification 注入的证书即与此交互 |

## 二、试用加速（用户问的"有时有有时没有"）机制

**服务端字段**（Go struct json tags）：
```
trial_left_times / trial_used_times   ← 次数配额
trial_key                              ← 体验凭证
total_sec / timeout_sec                ← 时长上限
speed_res_status                       ← 结果状态
is_speed_trial_queried                 ← 查询标记
TeamTimes                              ← 组队次数（姊妹产品）
```
全局配置样例：`"try_speed":{"is_disabled":false,"timeout_sec":1800,"total_count":3,"total_sec":60}`

**前端状态机**：`judgeCanTrySpeed`（查资格）→ `showPreTryBanner`（横幅）→
`commitApplyTry`（申请）→ `superSpeedTaskIDListRef`（绑到任务）→
任务状态 `TRYING`；倒计时结束回退。任务字段 `is_super_speed/is_try_super_speed`。

**行为解释**：
- 「时有时无」= trial_left_times 配额消耗 + 服务端发放策略（灰度/营销）
- 「启动后约 1 分钟」= 启动首轮 get_info/check_status 往返后自动套用
- 「面板在 P2P 加速和会员加速间切换」= 体验单生效窗口内额外源走 DCDN 高速通道
  （计为会员加速），窗口外回落普通 P2P/镜像

## 三、鉴权要点（对自动化友好）

- `speedup.xunlei.com/v1/check_status`（POST {user_id}）**接受 Xqp0 Bearer 票**，
  返回真实账号数据：`vas_id:14, is_vip:false, is_exp:false, probation:0,
  speed_open:false, basic_rate_down/up`
- GET 形态报 ret:16 登录验证失败 → 该族接口走 POST+body 鉴权
- `ctrl_check_auth.go` / `ctrl_open.go` 为其控制层（错误栈泄露）
- 桌面本地 inner-api 路由 `/device/v1/try_speed` 在 allow_inner_api_paths 白名单内

## 四、遗留未知

1. VipSpeedUpUrl 的完整远端路径（HostHighSpeedFlow+路径拼接；字符串常量未直出）
   → 需 Frida 在官方 App 实际触发试用时抓包，或反汇编 pkg/drive 相关函数
2. open 体验单的完整参数（本次误打的是快鸟 open，返回地域不支持；TrySpeed 的
   apply 在本地 inner-api 后面）
3. speed.auth.vip.xunlei.com 证书的下发流程（哪个接口产出 certification 字符串）

## 五、对我们项目的可操作结论

- 免费档确认可用：P2P（核心卖点）+ 基础镜像；会员加速为配额制试用
- daemon 可做：启动时若持有有效票 → 查询 check_status/get_info，将「有体验单」
  作为多源策略的开关信号
- 若要自动领试用：优先走本地 inner-api（自跑 xllite 实例）而非直连远端（路径未明）
