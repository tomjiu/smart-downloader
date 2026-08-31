# Task 31 — A6 预备：Magnet/TorrentFile 载荷静态校准（2026-08-31，零额度消耗）

> 数据源：引擎前端 bundle `docs/nas/evidence/a3/assets/index-1ded6b9a.js`（1,521,802 字节，
> A3 提取入库）逐偏移切片分析。**全程未触碰引擎与配额**，纯静态推导。
> 结论先行：**magnet 任务与 HTTP URL 任务共用同一载荷结构（type=user#download-url，
> url 对象形）**；BT 型任务（user#download）依赖云端文件 file_id，NAS UI 自身不产；
> **pause/resume 真实路由 = PATCH /drive/v1/task**——A2"未挂载（404）"结论系路由打错，
> 推翻 A2 定案表对应行。

## 1. 任务创建载荷总表（bundle offset 1211059 `addUrlTask` 完整还原）

```js
const b = {
  type: r,                       // "user#download-url"(uy) 或 "user#download"(VM)
  name: <trim 后 name||file_name||"unnamed">,
  file_name: <trim 后 file_name||name||"unnamed">,
  file_size: String(l),
  space: a,                      // device_id#hex32（A4 定案）
  params: {
    target: a,                   // URL 型必有
    url: o.url,                  // 仅 type=user#download-url（r===uy 分支）
    file_id: o.id,               // 仅 BT 型（else 分支）——云端已存在文件的 id
    total_file_count: c ? String(c) : "0",
    parent_folder_id: p || t.treeNodeId,
    parent_folder_path: v != null ? v : undefined,
    sub_file_index: d,           // BT 选择性下载，falsy 时 delete
    mime_type: o.mime_type || "",
    file_id: m                   // 顶层 params.file_id 并存（o.file_id）
  }
};
POST drive/v1/task（单数）
```

与 A4 实测 HTTP 载荷完全兼容（A4 实测仅填了 URL 型必需子集）。
**字符串清洗规则**：name/file_name trim 后 `replaceAll(" ","")`（全角空格）。

## 2. Magnet 定案（零结构差异）

- URL 校验正则（offset 1444519 附近）：`/^(magnet|http[s]?|ftp|ed2k|emule):/i.test(e)`
  ——**magnet/ed2k/emule 与 http 同列白名单**。
- infohash 归一化函数 `Yrt`（offset 1444689 附近）：输入 40 位 hex 或 32 位 base32
  → 自动包成 `magnet:?xt=urn:btih:<hash>`；原串含 scheme 则透传。
- NAS UI 的 BT 入口（`Qrt`，offset 1445627）：`.torrent` 文件 → `device/btinfo`
  POST `{file_id}` → 返回 `{url}`（磁力链）→ 路由带 `taskLink` → 走 URL 型建任务。
  控制台文案"正在从BT种子中解析磁力链接"。
- **推论（A6 实弹验证点 P1）**：引擎创建 magnet 任务 =
  `POST /drive/v1/task` + `type:user#download-url` + `url:{url:"magnet:?xt=urn:btih:…"}`，
  载荷其余字段与 A4 HTTP 任务一致。ed2k 链接同通道（验证点 P2）。

## 3. BT 型任务（user#download）——NAS UI 不产，仅消费

- 任务列表 filters（offset 1206771）：`type:{in:"user#download-url,user#download"}`，
  两型并存；`user#download` 任务在
  PENDING/RUNNING/COMPLETE/PAUSED 相位下被读取 `params.file_id`（offset 1207030）。
- 建任务 action `addUrlTask` 的 else 分支（`s.file_id=o.id`）在 bundle 内无
  UI 调用方传入 VM 型——**本端不产 BT 型任务**，file_id 型任务由 App/云盘侧下发。
- **A6 策略（验证点 P3）**：不硬造 BT 型任务（file_id 需真实云端文件）；
  磁力链走 P1 通道已覆盖 BT 下载能力。

## 4. pause/resume 真实路由——推翻 A2"未挂载"结论

offset 1208365 `actionTask` 与 1211602 `operateTask` 双实现同构：

```js
PATCH drive/v1/task        // 单数！A2 实测 404 用的是复数/其他路由
body = {
  space: <target>,          // signout 时强制 space:""
  type: <任务 type>,
  id: <task_id>,
  set_params: { spec: JSON.stringify({ phase: <action> }) }
}
```

- **phase 动作值实锤**（offset 1202000 `pauseTasks` / 1202222 `resumeTasks`
  本地乐观镜像）：暂停 `{"phase":"pause"}`、恢复/启动 `{"phase":"running"}`、
  签退 `{"phase":"signout"}`。注意是 `pause` 不是 `paused`。
- 语义镜像：暂停时 `spec=phase:pause, status=phase:running`（目标态/实况分离，
  与 A4 观测的 spec/status 双轨一致）。
- **A6 验证点 P4**：RUNNING 任务上 PATCH pause → 轮询 phase 变 PAUSED →
  PATCH running → 回 RUNNING。成功则 NasRemoteEngine v2 补挂
  `pause/resume` 方法（现 v1 为 Unsupported）。

## 5. 新解锁端点与常量

| 项 | 内容 | 出处 offset |
|----|------|------------|
| `POST device/btinfo` | body `{file_id}` → `{url:<magnet>}`，种子→磁力解析 | 1212428 / 1445883 |
| `GET drive/v1/resource/list/{list_id}` | 任务文件分页列表（page_token） | 1211059 前 |
| `PATCH drive/v1/task` | pause/running/signout 相位控制 | 1208365 |
| 平台常量 | `Ynt={synology:"PAN_CLI_PREVIEW",linux:"PLATFORM_LINUX"}`、`jM={lenovo,ugreen,lex,jkj}`、`Xnt="PLATFORM_DOCKER"` | 1081900 |
| 进度分档 | ly=[{0,0,60},{16,60,500},{32,500,1024},{48,1024,5K},{64,5K,50K},{80,50K,100K},{100,100K,150K}]（速度档→百分比 UI 映射） | 1081900 |
| 相位全表 | PENDING/RUNNING/PAUSED/COMPLETE/ERROR（Unt 枚举，对齐 A4 实测） | 1081900 |

## 6. A6 实弹验证矩阵（额度预算：3 次/日）

| # | 验证点 | 依赖 | 消耗 |
|---|--------|------|------|
| P1 | magnet URL 型建任务（magnet:?xt=urn:btih: 公链） | 无 | 1 次配额 |
| P2 | ed2k 链接建任务 | P1 通过 | 1 次配额（可选，优先级低） |
| P3 | BT 型任务观测（等 App 侧下发或放弃） | 云端文件 | 0 |
| P4 | PATCH pause→running 相位迁移 | 任一 RUNNING 任务（P1 产物） | 0（复用 P1 任务） |
| P5 | try_speed/apply 终验 90120（A6 原计划） | P1 任务 RUNNING | 0（加速配额独立） |
| P6 | DELETE 清理 3 个历史 ERROR 任务（100s+ 超时） | 无 | 0 |

预算：最省路径 P1→P4→P5→P6 共 **1 次创建配额**。若 P1 即 90120，
直接转 A5 FINDINGS §5 第 3 步（云端平台特权终判）。

## 7. 产物

| 资产 | 路径 |
|------|------|
| 本文档 | `docs/nas/A6_PREP_STATIC_CALIBRATION.md` |
| A6 探针（P1/P4/P5/P6 一体化，单进程引擎链） | `scripts/nas/a6_probe.py` |
| 引擎前端 bundle（证据本体） | `docs/nas/evidence/a3/assets/index-1ded6b9a.js` |

## 8. 配额归属再校准（用户域知识输入，2026-08-31）

**修正前的粗归因**："本地下载每日 3 次限制"——不准确。
**修正后模型**（对齐 Windows 客户端用户实测经验）：

| 通道 | 限制 | 归属 |
|------|------|------|
| Windows 客户端：URL/磁力下载到本地 | **无次数限制** | PC client_id 档 |
| 磁链提交云盘离线下载 | 每日限次 | 云盘空间配额 |
| 会员试用加速 | `try_speed usage.total=3`（A4 实测吻合） | 试用配额，独立 |
| **我们实测的 403** | 每日 3 次，`task_create_count_limit` | **pan-cli/docker 引擎档的云端提交策略** |

**证据**：A4 全部任务对象 `params.client_id=X9ibISwpIp8jQ4Ya`、
`package_name=pan.xunlei.cli.docker`、`platform=docker`（a4_run3.json）；
403 发生在 device 空间提交（real_path 落本地 downloads），故限制绑的是
**客户端身份档**而非"本地下载"行为本身；cnk3x #229（NAS 引擎用户同款 3/日）
属同一档位策略。**90120 与 3/日配额大概率同根**：云端按 docker 预览档
权限面裁剪（A4 嫌疑二 PLATFORM_DOCKER 的强化版）。

**对 A6 实验设计的直接影响**：
1. "云端身份观感"从附带观测升级为**第一观测点**——创建响应 200 body 的
   task.params 自带 client_id/package_name/platform（零额外成本）：
   群晖伪装后若任务对象仍报 docker → 身份未随引擎配置切换 → 90120/配额
   大概率照旧；若报 syn/群晖 → 立即复测下载执行。
2. 若身份未切换且 90120 照旧，下一杠杆 = launcher report 的
   client_id/package_name 载荷本身（深水区，观测优先、动手术慎重）。
3. a6_probe.py 已加 `cloud_identity` 提取（建任务响应 + 既有任务列表双路）。

## 9. 「群晖的停止」三类型定性（用户问询驱动核查，2026-08-31）

> 用户问："群晖的下载停止，是所有的下载都停滞了吗？到底是什么类型的？"
> 三层一手证据核查定案：**不存在任何"全局暂停"API；"全停"只有进程级与会话级两种**。

### 9.1 三类型对照表

| 类型 | 触发面 | 真实机制 | 影响范围 |
|------|--------|----------|----------|
| **任务级暂停/恢复** | 迅雷 web UI 单任务按钮 | `PATCH drive/v1/task`（单数），body `{space,type,id,set_params:{spec:JSON.stringify({phase:"pause"\|"running"})}}` | **仅指定 id 的那一个任务** |
| **套件级停止** | DSM 套件中心「停止」 | `start-stop-status` 的 `stop_daemon`：`kill -TERM $PID` → 超时 `kill -KILL`（杀 launcher→pan-cli 进程树，service-setup L88 经 `xunlei-pan-cli.sh --pid` 拉起） | **全部下载立刻停**（进程生死），重启套件后任务自 storm db 断点恢复 |
| **会话级签退** | UI 解绑/退出登录 | 同一 PATCH 路由 `{phase:"signout"}`，bundle 中 `a==="signout"&&(d.space="")` | **全部任务失去宿主**，需重新扫码绑定 |

### 9.2 证据链（三层独立来源交叉）

1. **UI bundle**（a3 证据 index-1ded6b9a.js）：store action 签名
   `operateTask({id,space,type,action})` —— **严格单任务**，一次只带一个 `id`；
   全 bundle 无「全部暂停」批量操作（仅"全部文件"字样，无批量任务操作）。
2. **引擎二进制**（xunlei-pan-cli.3.23.5.amd64 全文扫描）：
   `set_params`×19、`signout`×15、`phase→pause/running`×14，
   **`pause_all`/`PauseAll` 0 命中** —— 内核层不存在全局暂停概念。
3. **DSM 脚本**（spk-unpacked/scripts/start-stop-status + service-setup）：
   套件停止 = 对 PID_FILE 直接 `kill -TERM`/`kill -KILL`，无任务层优雅下坡。

### 9.3 与 A6 实弹的关系

- P4 的 PATCH pause 测试属**类型 1**（单任务），沙盒自建任务 + 独立登录，
  与用户真实 NAS 上的任何任务零交集。
- bundle 中 `pauseTasks`/`resumeTasks`（offset 1202000/1202222）为 UI 方法名，
  复数命名不改变 wire format 单 id 的事实。
- 引擎进程级停止（类型 2）即 A2 起复用的 pty.fork 生命周期的真实对应物；
  v2 代码面 pause/resume 只需覆盖类型 1。

## 10. 90120 云端属性定案（用户问询驱动，2026-08-31）

> 用户问："群晖上的报错到底是干嘛的？"——定案：**90120 是迅雷云端在
> "下载执行"环节下发的失败码，不是本地错误，引擎/页面都只是展示者**。

### 10.1 产生环节（a4_final.json 任务对象还原）

```
POST drive/v1/task → 200（任务已建立，接单成功）
  ↓ 数秒
云端置 phase=PHASE_TYPE_ERROR，status={"phase":"error"}
  params.error="下载(90120)"   ← 唯一错误信息，顶层 error 字段为 null
  real_path 文件 0 字节落盘
```

即：**创建（接单）成功 → 真正下载执行被云端拒绝**。与 403 配额（创建时拒）
是两个不同环节的两道闸。

### 10.2 码源 100% 云端（三路独立验证）

| 验证面 | 结果 |
|--------|------|
| 引擎二进制 62MB 全文扫描 | `90120` **0 命中**（set_params×19/signout×15 作对照全命中） |
| 群晖 UI bundle（a3 证据） | 无 90120 文案映射 |
| 任务对象 | `params.error` 为云端写入的任务元数据，非引擎本地字段 |

### 10.3 含义最强模型 + 社区旁证

- **模型**：云端按客户端档位（docker 预览档）裁剪"下载执行"权限面 →
  执行被拒记 90120；与 3/日创建配额（403 task_create_count_limit）同根
  不同环节（§8 档位模型的两道闸）。
- **旁证 1**：飞牛 fnOS 原版迅雷同款——PC 迅雷发远程任务到 NAS 端
  全部 90120（club.fnnas.com tid=51254，2026-01）→ 该码是 NAS 执行
  远程任务场景的系统性错误，非我们沙盒特有。
- **旁证 2**：cnk3x/xunlei #197 的 `下载(9102)` 同为"下载(码)"格式，
  但那是本地 PipeLimit 配置错（#212，A4 已修）——格式同族、成因不同。
- **官方体系**：迅雷远程下载帮助（xlyc.client.xunlei.com）即"任务失败→
  列错误码"模式（4112=URL 异常等同族码），错误码由云端任务系统下发。

### 10.4 对 A6 的意义：身份试金石

群晖伪装配方已落位（A5：envconfig，引擎日志已现 sdk_version=synology_3.23.5），
但云端身份是否随引擎配置切换未验。A6 第一发额度建任务时：
- task.params 四件套仍报 docker → 90120 大概率照旧 → 下一杠杆 =
  launcher report 身份载荷（深水区）；
- 报 syn/群晖 → 立即观察下载执行——**RUN 即 90120 终验通过**。
