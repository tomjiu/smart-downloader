# 云盘在线解压 API 探测记录（DECOMPRESS_API）

> 任务日期：2026-08-25。性质：**只读探测**——全程未调用 `/decompress`（实际解压写操作），也未调用 `/decompress/v1/download`。
> 鉴权配方完全复用 [`web_token_validate.ps1`](../../../scripts/research/xunlei/web_token_validate.ps1)（token 读 `xunlei_auth_web.json`，captcha/init 的 `action` 换成对应路径）。
> 样本：云盘 RAR「看漫画学Python：有趣、有料、好玩、好用：全彩版.rar」，`file_id=VNsr8phmP9dWgr2owSrpPQGKA1`。

## 0. 一句话结论

**端点形状已从源码证据完整还原（含 S1 报告未录的第 4 端点 `/download`），但活体探测全部 404——该路由未挂载在 api-pan / api-gateway-pan 两个已知网关上；真实网关需浏览器活体抓包确认。**

## 1. 端点形状（源码证据，非猜测）

来源：CEF 前端 dump `m_134.js`（解压 API 封装模块，S1-G2）。前端请求工具第二参在 POST 下默认可能拼 query（`decompress/download` 带 `noAssignParams:true` 而 list 不带，语义见 §3 备注）。

| # | 方法 | 路径 | 参数（源码原样） | 备注 |
|---|---|---|---|---|
| 1 | **POST** | `/decompress/v1/list` | `{path, file_id, gcid, password, file_space}` | 列压缩包内条目树；本次目标 |
| 2 | **POST** | `/decompress/v1/decompress` | `{gcid, file_id, password, default_parent, parent_id, files, parent_full_path, file_space, parent_space}` | **实际解压写操作——本项目禁调** |
| 3 | **POST** | `/decompress/v1/download` | 同 #2 参数族 | **S1 报告未录的新端点**（混淆前缀 `_0x34a+'s/v1/downl'+'oad'` 还原）；疑似"解压并取下载"，同样按写操作对待，未调用 |
| 4 | **GET** | `/decompress/v1/progress` | `{task_id}` | 按 taskId 轮询进度 |

源码片段（m_134.js 原文，拼接折叠还原）：

```js
// list —— 注意是 POST：
Object(e)('/decompress/v1/list',
  {path, file_id, gcid, password, file_space}, {method:'POST'})
// decompress（禁调）：
Object(e)('/decompress/v1/decompress',
  {gcid, file_id, password, default_parent, parent_id, files,
   parent_full_path, file_space, parent_space},
  {method:'POST', noAssignParams:!0})
// download（新发现，未调）：
Object(e)('/decompress/v1/download',
  {gcid, file_id, password, default_parent, parent_id, files,
   parent_full_path, file_space}, {method:'POST', noAssignParams:!0})
// progress：
Object(e)('/decompress/v1/progress', {task_id}, {method:'GET'})
```

## 2. 活体探测实录（2026-08-25）

前置链路每轮均成功：captcha/init（action 分别用 `GET:/decompress/v1/list` 与 `POST:/decompress/v1/list`，均发 token len=701）→ Bearer + X-Captcha-Token + X-Client-Id/X-Device-Id 头族。

### 2.1 第 1 轮：api-pan.xunlei.com GET ×3 变体（任务规定动作）

```
GET https://api-pan.xunlei.com/decompress/v1/list?file_id=VNsr8phmP9dWgr2owSrpPQGKA1
→ HTTP 404（nginx HTML：<center><h1>404 Not Found</h1></center>…<hr><center>nginx</center>）
GET …/list?parent_id=VNsr8phmP9dWgr2owSrpPQGKA1   → HTTP 404（同上 nginx HTML）
GET …/list?gcid=VNsr8phmP9dWgr2owSrpPQGKA1        → HTTP 404（同上 nginx HTML）
```

### 2.2 第 2 轮：依源码证据改 POST（m_134.js 明示 method:'POST'）

```
POST https://api-pan.xunlei.com/decompress/v1/list   body={"file_id":"VNsr…A1"}
→ HTTP 404（nginx HTML，同上）
POST https://api-pan.xunlei.com/decompress/v1/list?file_id=VNsr…A1   body={}
→ HTTP 404（nginx HTML，同上）
GET  https://api-pan.xunlei.com/decompress/v1/progress?task_id=probe_no_such_task
→ HTTP 404（nginx HTML）※ list 未成功，此调用仅顺带记录 progress 路由同样不存在
```

### 2.3 第 3 轮：网关归属验证（K2 已知第二网关）

```
POST https://api-gateway-pan.xunlei.com/decompress/v1/list   body={"file_id":"VNsr…A1"}
→ HTTP 404 "404 page not found"（纯文本 Go 路由器风格）
POST https://api-pan.xunlei.com/decompress/v1/list           body={"file_id":"VNsr…A1"}
→ HTTP 404（nginx HTML，对照组复现）
```

**关键观察**：两网关都 404 但报错栈不同——api-gateway-pan 返回 Go 服务路由器的纯文本 404（请求已打到业务路由层），api-pan 返回 nginx 层 HTML 404。即：**该路径在两个已知网关的路由表里都没有注册**，不是鉴权/参数问题（鉴权链全程绿）。

### 2.4 附带验证：样本 file_id 有效性

```
GET https://api-pan.xunlei.com/drive/v1/files/VNsr8phmP9dWgr2owSrpPQGKA1?space=&usage=DISPLAY
→ 200: name="看漫画学Python：有趣、有料、好玩、好用：全彩版.rar"
       size=67026810  kind=drive#file  mime_type=application/x-rar-compressed
       parent_id=VMyGaQvV_2hn79IEbA7aOlyiA1   （gcid 字段顶层为空）
```

结论：样本存在且 file_id 正确 → 404 与输入无关，纯属路由未挂载。

### 2.5 压缩包内条目结构

**未能获取**（list 全变体 404，无条目数据可记）。

## 3. 参数猜测表（供后续抓包对照）

| 参数 | 出现端点 | 猜测语义 | 置信度 |
|---|---|---|---|
| `file_id` | list/decompress/download | 云盘文件 id（本样本 VNsr…A1 即此形态） | 高（源码+实测样本匹配） |
| `gcid` | 全部四端点 | K35 体系内容哈希（GCID）；离线秒传/去重入口 | 高（源码+S1 注记"gcid 即 K35 体系哈希"；注意 drive 文件对象顶层 gcid 为空，需另行计算/获取） |
| `password` | 全部四端点 | 压缩包密码（RAR 加密包） | 高 |
| `path` | 仅 list | 包内子目录相对路径（列包内子树） | 中 |
| `file_space` | 全部四端点 | 目标空间 id（私人盘/企业盘多空间语义，与 files 列表的 space 字段同族） | 中 |
| `parent_space` | decompress | 解压落点所在空间 | 中 |
| `parent_id` | decompress/download | 解压落点目录 id | 高 |
| `files` | decompress/download | 选择性解压的条目列表（选择性下载的关键参数） | 高 |
| `default_parent` | decompress/download | bool：落到默认目录 | 高（源码默认 false） |
| `parent_full_path` | decompress/download | 解压落点全路径（字符串形态备选） | 中 |
| `task_id` | progress | 解压任务 id（由 decompress/download 响应返回） | 高 |

备注：前端工具层对 POST 默认把参数拼 query（`noAssignParams:true` 才走 body）——故 list 的真实传输形态（query vs body）以浏览器抓包为准；两种形态本轮均已试，均 404（与传输形态无关）。

## 4. 生态线索（顺带收获，归档）

- **VIP 特权档位**（m_1431.js/m_81.js）：解压有会员身份门槛——`DECOMPRESS_FILE_SIZE_LIMIT` 配置 + `decompressLimit` 特权配置；档位判断出现 `vip.platinum` / `vip.super` 阈值字样，免费账号大概率受限或不可用（与本探测路由 404 无关，但接入前要过这关）。
- **UI 流**（m_494.js store `decompress.ts`）：`getDecompressList(path)` → 树选择 → `decompress(parent_id, parent_space, files, parent_full_path)`，进度页 `dialog-decompress-status`。
- 操作类型枚举（m_12.js）：`{3:'decompress'}` 出现在复制/移动/恢复/解压/上传/云添加的操作分类里——解压被当作一种云端文件操作流水。

## 5. 后续接入建议（下载器能力候选）

定位：作为 provider「**云端预览 / 选择性下载**」能力候选——先 list 出压缩包条目树让用户勾选，再走 decompress/download 只取选中条目，避免整包下载（64MB 样本是典型场景：只要其中几章 PDF）。

下一步（按性价比排序）：
1. **浏览器活体抓包**（唯一可靠路径）：登录 pan.xunlei.com 网页 → 对样本 RAR 点「在线解压」→ F12 Network 过滤 `decompress`，一次拿到真实网关 host + 参数传输形态 + 响应结构。若网页端按钮对本账号不可见（VIP 门槛），换会员账号或直接判定能力不可接入。
2. 抓到网关后回填本文档 §2/§3，并把 captcha/init 的 action 更新为真实方法+路径。
3. 若确认仅会员可用：评估降级方案——直链整包下载 + 本地解压预览（现有 HTTP 引擎即可覆盖，无额外 API 依赖）。
4. 接入实现时遵守约束：`/decompress`、`/download` 属写操作（会在云盘产生解压产物/消耗配额），必须显式用户触发，绝不可用于探测。

## 6. 探测合规声明

- 本轮全部请求均为只读（GET list/progress 变体、POST list 形态验证、drive 元信息查询）；**从未调用** `/decompress/v1/decompress` 与 `/decompress/v1/download`。
- 凭证只读使用 `xunlei_auth_web.json`（gitignore 内），无写盘、无代码改动；探测脚本存放于仓库外 TEMP 目录，未入库。
