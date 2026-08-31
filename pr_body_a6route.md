## A6 前置定性与路线裁定（2026-08-31，A6 实弹未执行）

### 群晖「停止」语义三类型（§9）

三层一手证据交叉定案：**引擎不存在"全局暂停"概念**——

| 类型 | 机制 | 范围 |
|---|---|---|
| 任务级暂停/恢复 | `PATCH drive/v1/task`（单数），body 严格单 id，`phase: pause/running` | 仅该任务 |
| 套件级停止 | DSM `stop_daemon`：`kill -TERM → -KILL` 引擎进程树 | 全部（进程级） |
| 会话级签退 | 同路由 `phase: signout`，space 清空 | 全部（会话失效） |

证据：UI bundle `operateTask({id,space,type,action})` 单任务签名、无批量暂停；引擎二进制 `set_params`×19 / `signout`×15 / **`pause_all`×0**。

### 90120 云端属性定案（§10）

- 产生环节：创建 200（接单成功）→ 数秒后云端置 `PHASE_TYPE_ERROR`，任务元数据记 `error:"下载(90120)"`，文件 0 字节落盘——**下载执行阶段被云端拒绝**，与 403 配额（创建阶段）是两道不同的闸；
- 码源 100% 云端：引擎二进制 62MB 扫描 0 命中、UI bundle 无映射，引擎/页面只是展示者；
- 最强模型：客户端档位（docker 预览档）执行权限裁剪，与 3/日配额同根不同环节；社区旁证 = 飞牛 fnOS 原版迅雷同款 90120（PC 发远程任务到 NAS 全失败）。

### 路线裁定（§11）：主线回归 Win/Mac 流程，NAS 线归档

- Win/Mac 主线（Xqp0 档）≈95% 已建成且下载动作不经迅雷云端闸门——90120/配额天然不适用；
- NAS 线唯一增量（迅雷私有 P2SP on Linux 无头）属 L3「永不通解」层，持续成本 > 价值；
- **A6 实弹降级为可选**（1 次扫码 + 1 次额度可拿永久定论）；A2–A5 成果与 §1–§10 定性全部保留为研究文献；深水区不做；
- 复活路径：WORKSPACE.md 冷恢复四命令链 + 本 PR 全套证据整机可复现。

### 文档增补

- `NATIVE_LOGIN_GUIDE.md` 新增 §六：微信/QQ/微博等第三方登录仅在官方授权页可用（appid 回调白名单绑死迅雷域）+ Win/Mac 主线（Xqp0）与 NAS 线（X9ibIS）两条设备码流辨析表。

Commits: 661ad82 (stop taxonomy) · a2c42f5 (90120 verdict) · 本段（route verdict + login guide §六）
