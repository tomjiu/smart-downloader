# Next Action - M9 集成完成，待端到端验证

## 状态: 转换器已集成进 btcore/daemon，单元测试通过

### 2026-08-17 里程碑
- ✅ 真实样本三件套到位 + 验证 V1-V8 全绿 (validate_xunlei_sample.py)
- ✅ 3 项核心反汇编推断被真实样本推翻并修正 (magic / section 数组 / bitfield)
- ✅ 转换器重构为真实格式, e2e 通过 (fastresume + 位图 + 物化)
- ✅ spec_pending_validation.md 升级为 A 级 (已验证版)

## 待办 (按优先级)

### P0: 转换器产品化 (对接 M 系列)
- [x] 真实任务端到端试跑: 用 audio-books-cjk 样本生成 fastresume,
      在 qBittorrent 加载验证 rehash 行为 (用户机器上有迅雷环境)
- [x] 与主项目集成: btcore 的"导入迅雷任务"入口 (M3+ 范畴)
      - `add_xunlei_resume` 已加入 `DownloadEngine` trait，`smart-dl-btcore` / daemon 均已实现
      - daemon 新增 `POST /tasks/xunlei-import` + `import-xunlei` CLI
      - `xunlei-import` feature 依赖 `bt`，默认关闭
- [x] 处理边界: 在途 piece (partial) 策略确认 = 视为未完成 (已实现)
- [x] 在途 piece (partial) 策略细化: nonzero 字节占比 >= 50% 视为完成 (`build_bitfield_lenient`)
- [x] peer 注入: 将 cfg 中的 `bt://ip:port` 通过 `engine.add_peer` 注入 BT 引擎
- [x] 多文件 torrent 支持: `TorrentMeta` 解析 `files` 数组；`add_xunlei_import_task` 接受
      `xltds: Vec<Vec<u8>>`（按文件顺序）；fastresume `file sizes` 嵌套列表；CLI/HTTP 多 xltd 输入

### P1: 格式补全 (B/C 级遗留, 不影响转换)
- [x] cfg 头部 0x08-0x3B 字段部分解码：0x18=30025 (piece 数相关候选)、
       0x34=262145=0x40001 (疑似 flag 位)；其余字段单样本无法定论，需第二样本对照
- [x] tag-02 key 2..2200：单样本全零，推断为 per-piece 下载状态表（key=piece_index），
       已完成 piece 清零、在途/未下载为 1；未真实验证
- [x] 64KB 块记录 (0x4968 起, `65536×n+2` 序列)：推断为 piece 字节范围索引
       (offset/span 对)，服务于快速定位 piece 在 .xltd 中的物理位置
- [ ] peer 缓存记录内部字段 (当前仅知为 `bt://<ip>:<port>` 字符串；
       未验证是否含 client_hash / last_seen 等扩展字段)

### P2: 样本扩充 (可选)
- [ ] 收集第二个不同任务样本 (不同 piece_length / 单文件种子), 验证公式通用性
      (用户可把新样本丢进 tools/xunlei-migrate/samples/ 重跑验证器)

## 决策记录（D-xunlei-link-route）

**直链路线选择（2026-08-24 用户决策）**：
- **现阶段只用云端 pan PLAY API**（`/drive/v1/files/{id}?usage=PLAY` → `web_content_link`，
  标准 HTTPS，httpdl 可直接消费）
- **App 内部直链留作后续增强**：担心 pan 云端直链限速；若实测限速明显，
  再逆向 App 私有链路（设备绑定签名 / 迅雷自有 CDN）
- 触发条件：resolve 验证后实际测速，若低于预期再启动 App 路线

## 登录态 client 一致性约束（2026-08-24 实测）

pan API 三要素校验要求 **captcha_token 与 access_token 同 client 签发**：
| 登录方式 | token 绑定 client | 与 pan captcha 兼容 |
|---|---|---|
| 网页提取 | `Xqp0kJBXWhwaTpB6`（pan） | ✅ |
| 扫码设备码 | `XW5SkOhLDjnOZP7J`（登录页） | ❌ `client_id not match` |
| 账密 signin() | `Xqp0kJBXWhwaTpB6`（pan） | ✅ 全链路自洽 |

→ **云盘功能统一走账密登录**；扫码流程保留用于登录页场景（如需网页会话）。

### 扫码 token 复用实验（2026-08-25，xunlei_client_probe.rs）— 坐死不可行

三件套全部统一到设备码 client（XW5Sk）后逐项排查：
1. ✅ refresh_token 同 client 续期成功（7200s）——扫码会话本身可长期保活
2. ✅ captcha/init 在 XW5Sk 下签发成功（无需签名，空 meta）
3. ❌ pan API 仍拒绝：`"no client info found"`

**定论**：`api-pan` 服务端有独立 client 白名单（仅注册 `Xqp0` web / `Xp6vsxz` app），
登录页 client `XW5Sk` 不在册。失败与 token 新鲜度、captcha 配对均无关。
→ 扫码登录态只适用于 xluser 域 API；**pan 云盘功能账密登录是唯一解**。

## 离线提交实现状态（2026-08-24）

- ✅ `Client::offline_submit` / `offline_tasks`：POST `/drive/v1/files`
  (`upload_type=UPLOAD_TYPE_URL`) + GET `/drive/v1/tasks?type=offline` 轮询，
  请求格式来自 verify_offline_submit.py 实测结论
- ✅ `XunleiProvider::submit`：磁力（dn= 提名）/ HTTP 链接 → 云端离线；
  status 按 phase 映射 Queued/Downloading/Ready/Failed
- ⏳ **待真实验证**：需 pan client 登录态（账密 `xunlei_pwd_login`）后跑通
  submit → 轮询 → resolve 全链路；扫码 token 因 client 不一致不可用于此路径
- 📌 torrent 字节直传云端留后续（v1 仅磁力/HTTP 链接）

## dump 考古结论（2026-08-25，详见 dump_mining_upload_hash.md）

**UPLOAD_TYPE 枚举全集**（web 端）：`FORM / RESUMABLE / UNKNOWN / URL`。
**没有 TORRENT/BT 类型** —— web 前端对 .torrent 的做法是本地解析出 info-hash
（bencode-worker）后拼磁力走 `UPLOAD_TYPE_URL`。

→ **torrent 云离线已按官方同构方式实现**：`Client::torrent_upload`
（解析 .torrent → magnet → 复用已实测的 `offline_submit`），默认关闭的
multipart 原始直传分支仅作 B 级备用。

**hash 秒传：证据不足**。dump 中 gcid 全部是"已有文件取链 / 在线解压"语义，
不存在持 hash 直接建文件/换直链的端点。BCID/GCID 保留作未来素材，不实现。

## 子代理产出（2026-08-24，均已审计）

**分享链接解析**（`xunlei/share.rs`，+14 单测）：
- `parse_share_link` / `Sharer::list` / `Sharer::resolve`，错误体全透传
- ❗否定结论固化：**匿名取链不可行**（api-pan 不认 xluser 匿名 captcha；
  pwd→pass_code_token 接口未找到）→ 必须复用账密登录态（Bearer + 用户 captcha）
- daemon 的 `XunleiShare` 分支接入待登录态就绪后实施

**短信验证码登录**（`client.rs` 追加，+3~4 单测）：
- `send_sms_code`（A级字段：target=ANY/usage=SIGN_IN）+
  `verify_sms_code`（双分支兼容：直接返 token 或 verification_token→signin）
- ⚠️ 已知缺口：① send 下发的 `verification_id` 未跨方法传递（用手机号关联，B级推断，
  实测若强依赖需扩展签名）；② 返回的 AuthState device_id/captcha 为空——
  **必须过 `provider::store_login` 补齐后才能调 pan API**
- 滑块挑战不处理，服务端错误原样透传

## 用户配合事项

- 保持 audio-books-cjk 任务完成下载后, 可复跑验证器确认"全量分配 + 尾部零区"最终态
- 若要试 qBittorrent 迁移: 转换器输出 fastresume + 数据文件 → 添加到 qBittorrent 验证

---

## 2026-08-25 战果清单（登录终结日）

### ✅ 登录问题正式解决（网页凭证路线）
- 票源：浏览器 pan.xunlei.com 的 localStorage `credentials_Xqp0kJBXWhwaTpB6`
  （aud=Xqp0 = api-pan 白名单正主；同账号 860599297）
- 此前全部失败的根因：captcha/init 的 meta 缺 `user_id + captcha_sign +
  client_version + package_name + timestamp`，且缺 Bearer 头
- refresh_token（a1. 格式）12h 续期实测通过 → **一次开户，永久自动续**
- 凭证文件：`xunlei_auth_web.json`（已 gitignore）；配方脚本：
  `scripts/research/xunlei/web_token_validate.ps1`

### ✅ Rust 转化落地
- auth.rs：`load()` 兼容网页导出格式；新增 `jwt_exp` / `from_web_credentials_str`
  / 随机 did32；`.gitignore` 补 `_web.json` 防泄漏
- provider.rs：submit/status/resolve 三入口前置 `refresh_auth()`（access+captcha
  自动续期并回写旋转凭据）
- client.rs：PlayResp.size 柔性反序列化（服务端返回字符串数字，F3 PoC 已知坑）
- 新 example `xunlei_live_check.rs`：**活票全链自检一次通过**
  （load→refresh→captcha→list→PLAY→Range206），provider 测试 77 绿、clippy 归零

### ✅ 加速体系逆向（详见 SPEEDUP_SYSTEM.md）
- 下载面板"会员加速"= TrySpeed/SuperSpeed 体验单系统（配额 trial_left/used_times
  + trial_key + 绑定任务 ID 列表）；"时有时无"= 配额+服务端发放策略；"启动约1分钟"
  = 首轮 get_info 轮询
- speedup.xunlei.com = 快鸟宽带提速（地域锁定，与下载加速无关）
- 经典引擎证书认证后端 = speed.auth.vip.xunlei.com/speed/*
- 我们的 Xqp0 票已被 speedup 服务接受（check_status 返回真实账号数据）

### ✅ xunlei-ffi 引擎带身份模式（2026-08-27 晚）
- `crates/xunlei-ffi/src/identity.rs` 已有三 setter：
  `set_token_mode(u32)` / `set_app_guid(&str)` / `set_accelerate_certification(&str)`
- `crates/btcore/src/xunlei_engine.rs` 新增 `XunleiBtEngineBuilder`：
  - `with_token_mode` / `with_user_id` / `with_vip_type` / `with_accelerate_cert`
  - `build()` 按顺序下发 `set_token_mode` → `set_app_guid` → `set_user_info` → `set_accelerate_certification`
- `crates/provider/src/xunlei/provider.rs` 新增 `user_id()` async 方法；
  `crates/daemon/src/serve.rs` 在装配 XunleiBtEngine 时若检测到同配置 xunlei provider 已启用，
  从 `auth::load()` 取 user_id 注入 builder。
- cert 来源仍未完全澄清：`speed.auth.vip.xunlei.com/speed/speedup` 的下发流程
  需 Frida 抓包或 dump 实测确认；当前 cert 字段留空，匿名/FreeDCDN 不受影响。

### 📌 待办
- F3.1 完整验收：daemon 挂真 provider 提交磁力→离线→resolve→下载比对
  （resolve 段已在 Rust 层活票验证通过）
- G1/G2 手动项（scripts/manual/）
- VipSpeedUpUrl 精确远端路径（Frida 抓官方触发试用）
- P1 格式补全：cfg 头部 0x08-0x3B 字段语义 / tag-02 key 2..2200 / peer 缓存内部字段
- P2 样本扩充：第二个不同 piece_length / 单文件种子
- 🔴 xunlei-ffi 结构体 ABI 未对齐（2026-08-27 发现，阻塞 SDK 引擎线）：
  反汇编铁证（xunlei_research_complete.md §2.2）给出 C 侧 struct size 为
  XLInitParam=0x28(40)、XLBTTaskParamV2=0x28(40)、XLServerInfo=0x24(36)、
  XLTaskInfo=0x39c(924)，但 Rust `#[repr(C)]` 因「4B size + 8B 指针」自然对齐
  隐式 padding，实际 size_of 为 64/56/40/928（仅 XLPeerInfo=0x38 对齐）。
  SDK 用首字段 size 做 ABI 校验（`cmp [r8], r9d`）→ 当前传参会被拒收。
  待办：重新逆向逐字段偏移（或确认 C 侧 `#pragma pack` 紧凑布局），修正
  bindings.rs 结构体；登记测试见 bindings::tests::abi_size_register_known_drift。

  **深挖追加（2026-08-27 晚，决定性证据）**：`XL_CreateBTTask_V2` prologue
  （xunlei_research_complete.md 第 11719-11723 / 15941-15945 行）显示 `param` 字段
  访问在 `[rcx+4]`、`[rcx+0xc]`、`[rcx+0x14]`（8 字节指针，间隔 8，**无 4 字节对齐
  padding**）→ **坐实 C 侧是 `#pragma pack(1)` 紧凑布局**。
  由此：`BT_TASK_PARAM_V2` 真实布局 = `size(4) + 3×指针(24) + 12 字节 = 40`，
  而当前 Rust 定义是 `size + 4×指针 + 3×u32 = 48`（pack 后）——**字段数量/类型
  本身猜错**，非仅对齐问题。剩余 12 字节语义（3×u32？1 指针+1 u32？）仍需
  精确逆向（需 DLL 或更完整 disasm）。

### 🔬 ABI 修复取得决定性突破（2026-08-27 深夜，DLL 重新提取 + 完整反汇编）

用户提供 `C:\Users\yezi6\Downloads\XunLeiWebSetup25.0.90.1592gw.exe`，我重新提取出
完整 DLL 全套（脚本 `scripts/research/xunlei/disasm_xl_structs.py` + pefile/py7zr）：

- 提取产物：`scripts/research/xunlei/extracted/resource_1288_1304_unpacked/`（26 DLL + 4 EXE）
  - 关键：`DownloadSDKProxy.dll`（312KB）、`DownloadSDK.dll`（4.7MB）、
    `xl_thunder_sdk.dll`（5.1MB）、`DownloadSDKServer.exe`、`XUdt.dll` 等
- 7z 资源：`resource_1288_1304.bin`（7.4MB，下载引擎全套）、`resource_1288_1296.bin`（6.3MB，UI）

**完整反汇编揭示的 ABI 机制（决定性）**：

所有 `XL_*` versioned struct 都是 **pack(1) 紧凑布局 + memcpy 整体传递**。Proxy DLL 的
导出函数只做「size 校验 + `memcpy(min(size, param->size))` 整体复制到栈上 + IPC 转发给
DownloadSDKServer.exe」，**真正的字段解析在 server 进程（DownloadSDK.dll）内**。

| 函数 | size 常量 | 字段访问证据 |
|------|----------|-------------|
| XL_Init | 0x28(40) | 仅 `cmp [rdx], r8d`，无字段偏移访问 |
| XL_CreateBTTask_V2 | 0x28(40) | `cmp [rcx+4]`/`[rcx+0xc]`/`[rcx+0x14]` 三指针非空校验 |
| XL_AddServer | 0x24(36) | 仅 size 校验 + memcpy |
| XL_AddPeer | 0x38(56) | 仅 size 校验 + memcpy |
| XL_QueryTaskInfo | 0x39c(924) | 输出结构体，server 填充后 memcpy 回 |

**修复方案已明确**：
1. 结构体改 `#[repr(C, packed)]`（或显式字节数组），消除隐式对齐 padding
2. `size` 字段填正确值（40/40/36/56/924），**不是** `size_of::<Self>()`
3. `XLBTTaskParamV2` 修正为 `size + 3×指针 + 12字节`（当前 `size+4指针+3u32` 字段数错）

**仍待决**：`XLBTTaskParamV2` 的 3 指针 + 12 字节具体语义，需逆向
`DownloadSDK.dll`（server 端）的字段解析逻辑（`call 0x180022f00` 之后的 IPC 序列化，
或 DownloadSDKServer.exe 的消息处理）。

**追加修正（布局非统一）**：`XL_CreateP2spTask_V2`（RVA 0x187d0）的字段访问在
`[rcx+8]`/`[rcx+0x20]`/`[rcx+0x28]`（8 字节对齐），**不同于** `XL_CreateBTTask_V2`
的 `[rcx+4]`/`[rcx+0xc]`/`[rcx+0x14]`（4 字节紧凑）。→ **不能一概而论「全部 pack(1)」**：
BT 任务结构体是 pack(1)（4B size + 3×8B 指针，紧凑），P2SP 任务结构体是 8 字节对齐
（可能 8B size 或自然对齐）。**修复必须逐结构体精确逆向，每个都单独验证**。

### 🎯 BT_TASK_PARAM_V2 布局彻底破解（2026-08-27 深夜，server 端序列化逆向）

逆向 `DownloadSDKProxy.dll` 的序列化函数（`XL_CreateBTTask_V2` 转发目标 RVA 0xf620，
`XLDownloadSDKInterface.cpp` 的 IPC 序列化）得到**铁证**：

- C++ 类 RTTI 泄露：`DownloadSDKProxy` / `IPCBytesStream` / `IPCPipe` / `ConnectClient`
  / `DataClientBase`，源码路径 `D:\jenkinsAgent\...\Downloadlib_33.2\PC_SDK_Master_VS2019\
  src\DownloadSDKProxy\XLDownloadSDKInterface.cpp`（有符号名，可精确逆向）
- 序列化函数 `0xf620` 只访问 param 的 **3 个字段**（grep 全函数 `[rdx+off]` 确认）：
  - `[rdx+4]` = 宽字符串（`wcslen`，`cmp word ptr [r+*2]`）→ **torrent_path（UTF-16 路径）**
  - `[rdx+0xc]` = 宽字符串（`wcslen`）→ **save_path（UTF-16 路径）**
  - `[rdx+0x14]` = 窄字符串（`strlen`，`cmp byte ptr`）→ **第三个字符串（UTF-8）**
  - **无** `[rdx+0x1c]`~`[rdx+0x27]` 访问 → 12 字节是 padding/保留

**权威布局**：
```rust
#[repr(C, packed)]  // pack(1)
struct XLBTTaskParamV2 {
    size: u32,              // +0x00 = 0x28(40)
    torrent_path: *const u16,  // +0x04 宽字符串（UTF-16）
    save_path: *const u16,     // +0x0c 宽字符串（UTF-16）
    third_str: *const c_char,  // +0x14 窄字符串（UTF-8，语义待确认：任务名/infohash？）
    _reserved: [u8; 12],       // +0x1c 序列化时未使用
}
```
当前 Rust 定义（`size+task_id+torrent_path+save_path+strategy+priority+subfile_count+subfile_indices`）
**完全错误**：task_id 不在结构体（是独立函数参数）、无 3 字符串、多出 strategy/priority 等。

**仍待确认（1 项）**：`third_str`（+0x14 窄字符串）的精确语义——需看 server 端
`DownloadSDK.dll` 如何使用，或真机 dump。

### ✅ XLBTTaskParamV2 已修复（2026-08-27 深夜，代码落地）

- `crates/xunlei-ffi/src/bindings.rs`：`XLBTTaskParamV2` 改为 `#[repr(C, packed)]` +
  `size + torrent_path(*const u16) + save_path(*const u16) + third_str(*const c_char) +
  _reserved([u8;12])`，size=40（铁证对齐）。
- 函数签名 `XLCreateBTTaskV2Fn` 修正为 `fn(param, out_task_id: *mut u64)`——
  反汇编确认 param 是第 1 参数、无 handle 参数（SDK 内部 `call 0x180004030` 取全局句柄）。
- `task.rs::create_bt_task` 同步重构：UTF-16 宽字符串（`path_to_wide`）+ size=0x28 常量
  + third_str 传 null（待确认）。
- size 登记测试更新：XLBTTaskParamV2 移入「已对齐」断言（40==40）。

**仍待逆向（3 个结构体）**：XLInitParam(64→40)、XLServerInfo(40→36)、XLTaskInfo(928→924)。
同样用 pack(1) + 逐字段偏移逆向，方法同 BT_TASK_PARAM_V2（序列化函数 RVA 里 grep
`[reg+off]` 访问）。XLInitParam 序列化在 `XL_Init` 转发目标（`call 0x180022f00` 之前），
XLServerInfo 在 XL_AddServer 转发（0x180009990），XLTaskInfo 是输出结构体（server 填充）。

### ✅ 全部 4 个结构体 ABI size 已对齐（2026-08-27 深夜，完成）

- **XLBTTaskParamV2**：铁证（序列化 RVA 0xf620），pack(1) + 3 字符串 + 12 保留。
- **XLServerInfo**：铁证（XL_AddServer 转发 RVA 0x9990），
  `size(4)+u32(4)+3×宽字符串(24)+保留(4)`，pack(1) 消除尾随 padding（自然对齐会到 40）。
- **XLInitParam**：推断（size=40 铁证 + 字段名推断），`size(4)+4×窄字符串(32)+flags(4)`，
  移除 peer_id（通过 XL_GetPeerId 单独获取；`generate_peer_id` 已删）。
- **XLTaskInfo**：pack(1) 消除 4 字节尾随 padding（928→924）；字段布局仍推测，
  需 server 端 dump 完整还原（反汇编已知 +0x268/-1、+0x390/-1 与当前定义不符）。

所有调用点 size 字段改为常量（0x28/0x24/0x39c），非 `size_of::<Self>()`。
`bindings::tests::abi_size_assert_aligned` 锁定 5 个结构体 size，漂移即回归。
测试全绿：xunlei-ffi 7 passed、xunlei-convert 13 passed、零警告。

**剩余未决（不影响编译/离线测试，但阻塞真机验证）**：
1. XLInitParam 字段顺序/编码（推断，待真机 dump）
2. XLTaskInfo 完整字段布局（推测，待 server 端 dump）
3. XLBTTaskParamV2 的 third_str（+0x14）语义
4. 其他函数签名参数顺序（XL_CreateBTTask_V2 已修正，其余待逐一确认）

### 🔬 真机验证进展（2026-08-27 深夜，DLL 可加载，XL_Init 受 server 启动阻塞）

新增脚本：`scripts/research/xunlei/verify_dll_load.py` + `verify_xl_init.py`。

- **DLL 加载验证 ✅**：`DownloadSDKProxy.dll` LoadLibrary 成功，10 个关键符号
  （XL_Init/XL_CreateBTTask_V2/XL_AddServer/XL_QueryTaskInfo 等）全部可解析。
  依赖完整（VC90 运行时、openssl、curl、P2P 全家桶均在解包目录）。
- **XL_Init 真机验证 ⚠️ 受阻**：返回错误码 2（实际是 server 启动失败）。
  `DownloadSDKServer.exe` 启动后立即退出（退出码 `0xE0000101`，迅雷自定义 HRESULT），
  无 stdout/stderr。手动启动也失败 → **不是结构体布局问题，而是 server 进程的
  运行时环境问题**。
- **根因定位（反汇编）**：Proxy 端 `XL_Init` → 序列化函数 `0x6550` → `0x6360`
  （启动 server）→ `0x4ce0`（CreateProcessW，STARTUPINFO size=0x68）。
  server 启动失败后 IPC 连接失败，返回错误码 `0x2160ec02`。
- **server 退出码精确定位**：`0xE0000101` 来自 server `0x180014c54`（`mov edi, 0xe0000101`），
  是某个核心组件初始化失败（配对 `0xE0000102` 在同一函数 `0x180014c4d`）。
  该函数深陷 P2P/DownloadSDK.dll 核心初始化链，需完整迅雷客户端运行时环境。
- **可能原因（待排查）**：当前非管理员权限（`IsInRole(Administrator)=False`）；
  server 可能需管理员权限 + 完整迅雷客户端环境（注册表项/全局互斥量/证书）；
  或 VC90 side-by-side 程序集（`msvcm90.dll`）未正确注册。
  server 导入：VERSION/SHLWAPI/P2PBase/DownloadSDK/KERNEL32/USER32/SHELL32/
  ADVAPI32/XLBugHandler（均在，依赖不缺）。

**结论**：结构体 ABI 修复已完成且正确（静态铁证 + DLL 可加载）。真机验证剩余阻塞是
`DownloadSDKServer.exe` 启动的环境问题（管理员权限 / VC90 运行时注册），需在管理员
环境或完整迅雷客户端环境重试，与结构体布局无关。

### 🔍 深度逆向：server 端结构体与 Proxy 端不同（2026-08-27 深夜，关键澄清）

反汇编 server 端 `DownloadSDK.dll::XL_Init`（RVA 0x5cfb0）→ `0x18003e950` 发现：

- **server 端 XL_Init 是 2 参数**：`XL_Init(窄字符串, std::string)`，与 Proxy 端 3 参数
  `XL_Init(server_path, XLInitParam*, out_handle)` **签名不同**。
- **第2参数是 `std::string`**（非 XLInitParam）：`[rdx+0x18]` 与 0x10 比较（SSO 检查）、
  `[rdx]` 是 data 指针、`[rdx+0x10]` 是 length。
- **第3参数（0x18003e950 的 rdi）字段**：`[rdi+4]`(u32)、`[rdi+8]`(word，与 0xffff 比较，
  疑似端口/哨兵)——**与 XLInitParam 布局（+4=指针）完全不同**。

**澄清结论**：Proxy 端结构体（XLInitParam 等 versioned size + memcpy）与 server 端
IPC 反序列化后的**内部结构体是两套独立布局**。xunlei-ffi 只通过 Proxy DLL 调用，
**只需保证 Proxy 端结构体正确（已做到）**。server 端内部结构是迅雷实现细节，
不构成 xunlei-ffi 的 ABI 契约。此发现**验证了修复方向的正确性**：

### 🎯 真机验证突破：找到 XL_Init 返回 2 的根因 + 真实 ABI（2026-08-27 深夜）

通过真机运行（Python ctypes 加载 DLL + 反汇编定位），**彻底解决**了此前的三个悬案：

1. **XL_Init 返回码 2 的根因**：`DownloadSDKServer.exe` 的 `server_path` 参数有
   **100 字符长度限制**（`0x18003e994: cmp r8, 0x64; ja → mov eax, 2`）。
   原路径 `E:\...\resource_1288_1304_unpacked\DownloadSDKServer.exe` = 111 字符 > 100。
   复制到短路径 `C:\xl\`（27 字符）后，**XL_Init 返回 0（成功），server 进程持续运行**。

2. **XLInitParam 真实布局（铁证，推翻"4 指针 + flags"推断）**：
   ```
   +0x00: size (u32) = 0x28
   +0x04: u32（配置标志，语义待确认）
   +0x08: word（0xffff = 无 JSON；否则 = JSON 长度）
   +0x0a: JSON 字符串（最多 30 字节，格式 `{key:val,key:val,...}`，
           `{`/`}` 边界 + 逗号分隔；字段名 app_guid/token_mode/equity_token 等）
   ```
   证据：`0x18003ea04: mov [rdi+4]`(u32)、`0x18003ea32: cmp word [rdi+8], 0xffff`、
   `0x18003ec44: cmp byte [rdi+0xa], 0x7b`('{')、`0x18003ec4e: cmp [rax+rdi+9], 0x7d`('}')。

3. **XL_Init 真实签名 = 2 参数（无 out_handle）**：
   `XL_Init(server_path, param) -> LtErr`。handle 是 SDK **全局状态**，
   所有 `XL_*` 函数**无 handle 参数**（内部 `call 0x180004030` 取全局句柄）。
   `XL_UnInit()` 也是 0 参数。这**推翻了 xunlei-ffi 的"handle 透传"模型**。

4. **server 启动机制**：Proxy 用 `CreateProcessW` 启动 server，命令行格式
   `"DownloadSDKServer.exe" BDAF7A63-568C-43ab-9406-D145CF03B08C:<PID>`，
   GUID 是 IPC 通道固定前缀，PID 是 Proxy 进程 ID（共享内存映射命名）。

**代码修正已落地**：`bindings.rs` 的 XLInitParam（新布局）+ XLInitFn（2 参数）。
**剩余重构**：所有 `XL_*` 函数签名需去掉 handle 参数（逐一反汇编确认），
`XunleiHandle.raw` 语义从"handle"改为"已初始化标志"。

### ✅ 真机验证彻底通过（2026-08-27 深夜，最终）

用 Python ctypes 加载 `C:\xl\DownloadSDKProxy.dll`（短路径）实测：

| 测试 | 结果 |
|------|------|
| `XL_Init(server_path, param)` 2 参数 + 新布局 | **返回 0（成功）** |
| `field8=0`（空 JSON） | **rc=0 ✅** |
| `field8=0xffff`（无 JSON 哨兵） | rc=1 ❌（理解反了：0xffff 不是"无 JSON"，是特殊值） |
| `field8=0` + `json={}` 全空 | rc=0 ✅ |
| server 进程 | 持续运行确认 |
| `XL_UnInit()` 0 参数 | 返回 0（成功清理） |

**最终确定的 XLInitParam 布局**：
```
+0x00: size (u32) = 0x28
+0x04: field4 (u32) = 0
+0x08: field8 (u16) = 0（空 JSON；0xffff 是特殊值会失败）
+0x0a: json [30 字节]（空 = 全 0，或 `{app_guid:xxx}`）
```

**关键结论**：xunlei-ffi 的 `XL_Init` 已能真机成功调用（返回 0，server 进程运行）。
此前「返回码 2」的根因是 server_path 超 100 字符（111>100），「handle 透传」模型
是错的（handle 是 SDK 全局状态）。剩余工作是其他 XL_* 函数签名的逐一确认重构。

### ✅ 系统性签名重构完成（2026-08-27 深夜，全部函数去 handle + task_id u32 化）

真机反汇编逐一确认所有 `XL_*` 函数的真实签名（**无 handle 参数，task_id 是 u32**）：

| 函数 | 真实签名（铁证） | 旧签名（错） |
|------|-----------------|-------------|
| XL_Init | `(server_path, param)` | `(server_path, param, out_handle)` |
| XL_UnInit | `()` | `(handle)` |
| XL_CreateBTTask_V2 | `(param, out_task_id: *u32)` | `(handle, param)` |
| XL_StartTask/StopTask | `(task_id: u32)` | `(handle, task_id: *void)` |
| XL_DeleteTask | `(task_id: u32, delete_data)` | `(handle, task_id: *void, ...)` |
| XL_QueryTaskInfo | `(task_id: u32, out)` | `(handle, task_id: *void, out)` |
| XL_AddPeer/BatchAddPeer | `(task_id: u32, count, peers)` | `(handle, task_id, ...)` |
| XL_DiscardPeer | `(task_id: u32, peer)` | `(handle, task_id, peer)` |
| XL_AddServer | `(task_id: u32, param2: u32, server)` | `(handle, task_id, server)` |
| XL_*Set* 系列 | 无 handle | 有 handle |

**核心规律**：handle 是 SDK **全局状态**（`call 0x180004030` 取全局句柄），
所有函数**无 handle 参数**；task_id 是 **u32**（`mov ebx, ecx` 32 位）。

**代码落地**：
- `bindings.rs`：全部函数签名去 handle + task_id u32 化
- `handle.rs`：`HandleInner` 删除 `raw` 字段（handle 全局，无需存储）
- `task.rs`/`peer.rs`/`tracker.rs`/`dcdn.rs`/`identity.rs`/`query.rs`：调用点同步修正
- `TaskId.0` 仍为 u64（外部 API 兼容），FFI 层 `as u32` 截断

**待逆向（未完成）**：
- `XLMagnetParam`/`XLP2spParam` 真实布局（含内联 std::string，`[rcx+0x18]`=length），
  签名暂用 `*mut c_void` 占位
- `XL_CreateMagnetTask` 是 3 参数（rcx=结构体, rdx=宽字符串, r8=第三参），
  与 XLBTTaskParamV2 的 2 参数不同

### 🎉 端到端真机验证通过 + Magnet/P2sp 签名确定（2026-08-27 深夜，最终）

**`XL_CreateMagnetTask` 真实签名（反汇编铁证，推翻"结构体"假设）**：
```c
int XL_CreateMagnetTask(const wchar_t* magnet, const wchar_t* save_path, uint32_t* out_task_id);
```
- **无 XLMagnetParam 结构体**！序列化 0x180010940 无 size 校验（对比 BT_V2 有 cmp），
  参数直接是 2 个宽字符串（wcslen）+ 1 个 out 指针。
- `XL_CreateP2spTask`（0x18780）是薄包装，打包成 **size=0x38(56)** 的结构体后调
  `XL_CreateP2spTask_V2`；V2 校验 `[rcx+8]`/`[rcx+0x20]`/`[rcx+0x28]` 三个指针非空。

**端到端真机验证（Python ctypes，C:\xl 短路径）**：
```
[1] XL_Init = 0                          ✅ server 启动
[2] XL_CreateMagnetTask = 0, task_id=1   ✅ 成功创建磁力任务，返回真实 task_id
[3] XL_UnInit = 0                        ✅ 清理
```
这是 xunlei-ffi SDK 引擎线**首次端到端真机验证通过**（Init → Create → UnInit 全链路）。

**代码落地**：
- `XLCreateMagnetTaskFn` 改为 `fn(magnet: *const u16, save_path: *const u16, out: *mut c_uint)`
- `task.rs::create_magnet_task` 重写为宽字符串 + out u32
- `XLMagnetParam` 标记为「不存在，函数直接传宽字符串」（保留占位文档）
- `XLP2spParam` size 更新为 0x38(56)

### ✅ XLP2spParam 布局确定 + P2SP 真机验证通过（2026-08-27 深夜）

**XLP2spParam 真实布局（反汇编铁证 + 真机验证）**：
```
+0x00: size (u64) = 0x38(56)
+0x08: url（宽字符串，UTF-16）
+0x10: 宽字符串（referer？）
+0x18: 宽字符串（user-agent？）
+0x20: save_path（宽字符串）
+0x28: 宽字符串（文件名，非空才成功）
+0x30: flags (u64) = 2
```
- `XL_CreateP2spTask`（0x18780）是薄包装，6 参数打包成 56 字节结构体调 V2
- `XL_CreateP2spTask_V2(param, out)` 校验 `[+8]`/`[+0x20]`/`[+0x28]` 非空
- **无 versioned size 校验**（序列化直接访问字段，对比 BT_V2 有 cmp）

**P2SP 真机验证**：
```
XL_CreateP2spTask_V2 = 0, task_id = 1  ✅（5 指针全非空时成功）
XL_CreateP2spTask_V2 = 2  ❌（field28 文件名 NULL 时失败）
```

**代码落地**：`XLP2spParam` 重写为 `size(u64) + 5×宽字符串指针 + flags(u64)` = 56，
`abi_size_assert_aligned` 加 XLP2spParam 断言（0x38）。

### ✅ XL_CreateP2spTask 薄包装 6 参数签名确定 + 真机验证通过（2026-08-27 深夜）

**`XL_CreateP2spTask`（0x18780，薄包装）真实签名**：
```c
int XL_CreateP2spTask(const wchar_t* url, const wchar_t* referer, const wchar_t* ua,
                      const wchar_t* save_path, const wchar_t* filename, uint32_t* out_task_id);
```
6 参数：5 个宽字符串 + out_task_id 指针。打包成 XLP2spParam（56 字节）后调 `XL_CreateP2spTask_V2`。

**薄包装真机验证**：
```
XL_CreateP2spTask = 0, task_id = 1  ✅（6 参数全传，空串 referer/ua）
```

**代码落地**：
- `XLCreateP2spTaskFn` 改为 6 参数 `(url, referer, ua, save_path, filename, out)`
- `task.rs::create_p2sp_task` 新增实现（url/save/filename 3 参数，referer/ua 传空串）
- 新增 `str_to_wide` 辅助函数

### ✅ 批量签名验证 + 完整导出表审计（2026-08-27 深夜，收尾）

**100 个 XL_* 导出函数全部列出**，逐一核对 bindings.rs 已定义的 30 个签名。

**批量反汇编验证（mov esi/edi, ecx 32 位模式 = task_id u32 + 无 handle）**：
- ✅ `XL_QueryTaskFlow`/`XL_QueryTaskIndex`/`XL_SetTaskUserAgent`/`XL_AddHttpHeaderField`
  /`XL_SetTaskDownloadSpeedLimit`/`XL_SetTaskPriorityLevel`/`XL_QueryBTSubFileInfo`
  /`XL_SetP2spTaskIndex`/`XL_RenameP2spTaskFile` —— 全部 `mov esi/edi, ecx`，task_id u32 无 handle

**特殊函数（非 task 操作，用指针参数）**：
- `XL_GetPeerId(in: *const, out: *mut)` —— 2 指针参数，NULL 检查，非 u32
- `XL_QueryGlobalStat(out: *mut)` —— 1 指针参数，size=0x1c(28)，全局统计
- `XL_SetTaskStrategy`（0x17450）= `or edx,8; jmp XL_SetTaskStrategy_V2` 薄包装

**核心规律（最终版）**：
1. handle 是 SDK 全局状态（`call 0x180004030`），所有函数无 handle 参数
2. task 操作函数：`task_id: u32`（`mov esi/edi, ecx` 32 位）
3. 全局/非 task 函数：指针参数（NULL 检查）
4. 字符串参数：task 操作窄字符串，创建任务宽字符串（wcslen）
Proxy 端 = versioned size 结构体（已对齐），server 端 = IPC 内部结构（无关）。

### 🎉 BT 下载全链路真机验证通过（2026-08-27 深夜，Rust 侧，收官）

**这是 xunlei-ffi SDK 引擎线核心价值（BT 下载）首次完整真机验证通过。**

**关键发现（XLTaskInfo 真实布局，dump 铁证）**：
```
+0x00: size (u32) = 0x39c
+0x04: task_state (u32) —— 0=未启动, 3=下载中（download_size 增长铁证）
+0x08: field8 (u32) = 0（疑似 task_id 低32位）
+0x0c: file_size (u32) —— 1174243328（1.17GB，**非 u64**）
+0x14: download_size (u32) —— 从 0 增长（**非 u64**）
+0x1c: download_size 副本 (u32)
+0x24: 计数 (u32) —— 随秒递增
+0x2c: peer 数 (u32) = 31
+0x30: 连接数 (u32) = 6
+0x34: download_size 副本2 (u32)
+0x268: 8 字节 = -1（proxy 初始化，server 未覆盖）
+0x390: 4 字节 = -1（同上）
```

**重大修正**：
1. **`task_state=3` = 下载中**（非"完成"！）——download_size 增长铁证推翻旧枚举
2. **download_size/file_size 是 u32**（非 u64），且偏移与旧定义不同
3. **`XLBTTaskParamV2.third_str`（+0x14）必须非空**（`cmp [rcx+0x14], 0; je 失败`），
   是任务显示名，传任意非空字符串即可（旧代码传 null → 返回 code=2）

**真机验证（Rust example `verify_bt_download.rs`，1.17GB ubuntu iso）**：
```
[1] XL_Init 成功
[2] XL_CreateBTTask_V2 成功，task_id = TaskId(1)
[3] XL_StartTask 成功
[4] state=Downloading, file_size=1174243328, download_size=739829 ↑ 增长中
```

**代码落地**：
- `bindings.rs::XLTaskInfo` 重写为 dump 铁证的真实布局（u32 字段，非 u64）
- `query.rs::TaskState` 枚举修正（3=Downloading，非 Completed）
- `query.rs::TaskInfo` 精简为已 dump 确认的字段（state/file_size/download_size/peer_count/conn_count）
- `task.rs::create_bt_task` 的 third_str 改为非空字符串（"smart-dl-task"）
- `btcore::xunlei_engine.rs` 的 map_state/status 同步修正
- 新增 example `crates/xunlei-ffi/examples/verify_bt_download.rs`

**测试状态**：xunlei-ffi 7 passed、xunlei-convert 13 passed、btcore（xunlei feature）编译通过，零警告。

**剩余（次要，不影响 BT 下载）**：
- XLTaskInfo 的 +0x38 之后字段（速度/error_msg/DHT 统计等）未 dump 还原，暂用 _remaining 占位
- task_state 完整枚举（1/2/4/5/6/7/8/9）未真机观察，暂归 Unknown
- 下载速度（down_rate）字段未定位，EngineStatus.down_rate 暂为 0

### ✅ task_state 完整枚举 + XLTaskInfo 剩余字段逆向（2026-08-27 深夜，最终）

**用本地 HTTP server + P2SP 任务做完整生命周期验证**（5MB 本地文件秒下）：

**task_state 完整枚举（真机铁证）**：
| 值 | 状态 | 证据 |
|----|------|------|
| 0 | 未启动 | start 前 |
| 3 | 下载中 | download_size 增长 |
| 5 | 暂停 | XL_StopTask 后 |
| 7 | 完成 | download_size == file_size（5MB 秒下） |

**XLTaskInfo 剩余字段（dump 铁证）**：
- `+0x54`：任务名/文件名（窄字符串，ASCII "test_5mb.bin"）
- `+0x270`：1（完成标志）
- `+0x274`：download_size 副本3
- `+0x27c`：MIME 类型（窄字符串 "application/octet-stream"）
- `+0x394`：1（完成标志）
- **下载速度不在 XLTaskInfo**（需 XL_QueryTaskFlow 单独查询，且该函数是 3 参数非 2 参数）

**代码落地**：
- `query.rs::TaskState` 补全：0=Pending 3=Downloading 5=Paused 7=Completed
- `btcore::xunlei_engine.rs` map_state 补全 Paused→Paused、Completed→Seeding 映射
- `bindings.rs::XLTaskInfo` 文档注释补全完整布局

**剩余（纯增强，不影响核心）**：
- 下载速度：需逆向 XL_QueryTaskFlow（3 参数）的真实签名 + XLTaskFlow 布局
- error 状态值（9=失败？）未验证（需构造失败任务）
- +0x38..+0x53 之间的字段语义

### 🐞 F3.1 验收发现的三只真 Bug（2026-08-25 夜，复现配方齐全）
| # | 现象 | 根因定位 | 修复方向 |
|---|------|---------|---------|
| A | 磁力任务秒暂停不保持：pause 后引擎仍下载至完成 | lt auto_managed 队列语义：metadata 到达即恢复；快照实时化又用引擎态覆盖记录态（视觉误导） | add_magnet 初始 flags 去 auto_managed，或 metadata alert 时尊重记录态 Paused |
| B | 特定生命周期后 runtime 全端点挂死（/config 也 hang，进程活着低 CPU） | 复现序：complete→seed→DELETE→add 挂 20s+；与 fallback 在飞叠加过一次。未定位到锁 | 复现后抓 minidump 分析线程栈（rundll32 comsvcs 可用） |
| C | 兜底传输撞上"BT 已抢先下完的同名文件"时挂起：记录停 Paused、无 fallback 事件、盘上文件已全量、provider busy=0 | Bug A 修复后 BT 能在 Paused 记录态下被引擎完成 → Seeding，但 fallback 只允许 Downloading/Queued 起始，旧文件路径与 rename/lock 冲突非唯一根因 | ① `transition_for` 放行 Paused→Seeding；② `OutputManager.finalize_to` 幂等短路改为"清理 .part 后 Ok"；③ `finalize_part` 删 dest 失败升级 warn |
复现配方：scripts/manual/f31_run.ps1（BBB 磁力+250ms 抢停+门禁校验+wedge 探针）

### 🐞 Bug B 精化 + Bug C 实锤（2026-08-26 晚，跨日复现追加）
新复现（配额跨日刷新后，token refresh 即刻恢复，配额表确认仍 free=3）：
- 时序：add→metadata@0s→pause(done=2.4MB<50%✓)→fallback firing→
  **传输完成**（276MB 落盘，mtime 实证）→ **handler 悬死**：
  记录态停 Paused、无 fallback 事件、providers busy=0、
  alert 循环与 watchdog 心跳持续存活（非全局冻结！与 8/25 版症状不同）
- 收敛结论：挂点在「sink 传输完成之后 → record Completed/bt.remove 之前」，
  高度疑似 OutputManager.finalize_to 与本地 seeding torrent 已写入的同名文件
  冲突路径（httpdl 与 BT 先后写同一目标）
- 现场日志：bugc_repro_20260826_204938.log（已归档本目录）；daemon.err 无 panic 输出
- 下轮专项入口：读 httpdl OutputManager.finalize_to 存在分支 +
  FallbackSink remove 时序；单变量二分（跳过 finalize / 改临时名落位）
另：8/26 凌晨的「全端点 hang 790s 后自发恢复」与云端离线完成事件精确同步，
与本次 handler-only hang 并存为两个表现形态；统一根因候选 = sink/移除段阻塞。

### 🐞 Bug B 升级：runtime 死锁实锤（2026-08-25 深夜追加）
- 复现时点：fallback 传输段进行中 + 本地 BT 完成进入 Seeding 的交汇窗口
- 症状：进程存活低 CPU、29 线程全部 Parked（23 Unknown/4 EventPairLow/2 UserRequest）、
  全端点（含 /config）hang；minidump 被 comsvcs 沙箱拒
- 附带确认：Bug A 的"队列复活"单次压制不够——需持续执法
  （已实现 enforce_pauses 每 500ms 对比 done 增长再压，实测可冻结进度，
   但与队列管理器拉锯下最终仍会完成→Seeding）
- 下轮专项：干净环境单变量复现 → minidump（需脱离当前沙箱限制）→ 线程栈定位持锁者

### ✅ Bug A 已修复 + F3.1 收尾（2026-08-27 晚）
- 根因（最终定位）：`crates/daemon/src/serve.rs` 中 `bt_typed` 变量遮蔽
  （`let bt_typed = Some(bt.clone())` 创建了 inner 局部变量，outer 仍为 `None`），
  导致 `spawn_alert_loop` 收到的 `guard` 始终为 `None`，`enforce_pauses`
  从未被调用。此前所有"持续压制"代码实际处于断开状态。
- 修复（三层）：
  1. **serve.rs 接线修复**：`bt_typed = Some(bt.clone())`（去掉 `let`），
     使 alert 循环正确拿到 `BtEngine` 句柄，`enforce_pauses` 真正生效。
  2. **内核 flags 修复**：`lt_add_magnet` / `lt_add_torrent_resume` 现在强制
     `paused=true` + `auto_managed=false`（ABI1 兼容写法：`flags &= ~auto_managed; flags |= paused`），
     从源头阻止 libtorrent queue_manager 接管。
  3. **持续压制逻辑保留**：`enforce_pauses` 每 500ms 对意图任务无条件 pause，
     将"保持暂停"从被动反应变成主动持续执法。
- 实测验证（本轮）：
  - 冷启动 + 无 fastresume + DHT 开启：metadata 0.25s 到达 → pause(done=0) →
    90s 后仍 Paused(done=0)，未复活。
  - 对照（修复前同场景）：15-20s 后自动复活并下载至完成。
  - Fallback 功能点：同日早些时候手动触发 fallback 曾返回 `transferred=1`，
    证明 provider 侧上传通路正常。
- F3.1 完整脚本：当日迅雷离线配额已用完（`task_create_count_limit`, error_code 11），
  标准化脚本的 fallback+MD5 段未跑到。上述两项实测已足够收尾；完整 F3.1 可待后续配额刷新后补跑。

### 🔑 重大发现：官方 C++ SDK 已在本机落地（2026-08-27，纠正旧认知）

此前多处文档（PUBLIC_INTEL_REPORT / xunlei_research_complete.md 等）把官方仓库
`xunlei-open/xunlei-dlsdk` 判定为「仅接入文档、无协议层、无头文件」——**该认知已过时/错误**。

本机实际存在完整官方 SDK 源码 + 头文件 + 预编译库（1.0.2）：

- 位置：`scripts/research/cloud_delivery/sdk_cpp/xunlei-dlsdk-cpp/`
- 头文件：`include/xl_dl/xl_dl_sdk.h`（**官方结构体定义，无 versioned size 首字段，无 ABI 逆向问题**）
- 预编译库：`prebuilt/windows-x64/bin/dk.dll`(7MB) / linux-x64 / macos-universal 全平台
- 官方 API：`xl_dl_init` / `xl_dl_login(login_token, session_id)` / `xl_dl_create_p2sp_task` /
  `xl_dl_create_batch_task` / `xl_dl_get_task_state` / `xl_dl_set_http_header` 等
- 登录：`get_login_token(api_key)`（`open.xunlei.com/api/v1/sdk/login_token`）→ `xl_dl_login`
  —— 需在 open.xunlei.com 申请 `api_key`（开发者凭证）

**与逆向 DownloadSDKProxy.dll（`XL_*`）的关键差异：**

| 维度 | 官方 SDK `xl_dl_*`（dk.dll） | 逆向 `XL_*`（DownloadSDKProxy.dll） |
|------|------------------------------|--------------------------------------|
| 来源 | open.xunlei.com 官方开放平台 | 逆向迅雷客户端内部 DLL |
| 结构体 | 官方头文件，无 ABI 问题 | versioned size 首字段，**有未对齐 bug** |
| 跨平台 | ✅ Win/macOS/Linux | ❌ Windows only |
| BT/磁力 | ❌ 仅 P2SP/HTTP 下载 | ✅ BT/磁力/P2SP |
| 登录 | 官方 `api_key` → login_token | 免登录匿名（UserID=0）|
| 加速 | 动态链加速（`set_dynamic_link_acceleration`）| FreeDCDN / VIP DCDN |

**战略结论**：官方 SDK 是「P2SP/HTTP 加速下载」的正道（无逆向风险、跨平台），
但它**不含 BT/磁力能力**，且需要 api_key。逆向 DLL 是「BT/磁力 + 免登录」的唯一来源，
但有 ABI bug。二者**互补，不可互相替代**。

**待决**：是否值得：
1. 基于官方 `xl_dl_*` SDK 新起一个 `xunlei-dlsdk` crate（P2SP 加速，替代逆向的 P2SP 部分），
2. 继续修逆向 `XL_*` 的 ABI（BT/磁力部分，仍需重新逆向字段偏移）。
需用户决策：是否有 open.xunlei.com 的 api_key 申请渠道/意愿。
