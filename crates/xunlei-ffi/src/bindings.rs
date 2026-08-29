//! FFI 类型与函数签名（ABI 层）。
//!
//! 所有结构体首字段必须是 size（versioned struct），SDK 用它做 ABI 校验。
//! 反汇编铁证给出的 C 侧 struct size：
//!   XLInitParam = 0x28(40), XLBTTaskParamV2 = 0x28(40), XLPeerInfo = 0x38(56),
//!   XLServerInfo = 0x24(36), XLTaskInfo = 0x39c(924)
//!   （`mov rN, IMM` + `cmp dword ptr [reg], rN` 模式，见 xunlei_research_complete.md §2.2）
//!
//! 2026-08-27 重新提取 DownloadSDKProxy.dll 并完整反汇编（scripts/research/xunlei/
//! disasm_xl_structs.py），确认 ABI 机制：Proxy DLL 导出函数只做「size 校验 +
//! memcpy(min(size, param->size)) + IPC 转发」，字段布局见各结构体注释。
//! 已修正 XLBTTaskParamV2（pack(1) + 3 字符串指针 + 12 保留，铁证）。
//! 其余结构体（XLInitParam/XLServerInfo/XLTaskInfo）仍待逐字段逆向，
//! 登记见 bindings::tests::abi_size_register_known_drift。

use std::os::raw::{c_char, c_int, c_void, c_uint, c_ulonglong, c_ushort};

// ========== 公共类型 ==========

/// SDK 句柄（XL_Init 返回，内部存储为 usize 以确保 Send）。
pub type LtHandle = usize;
/// 错误码（i32）。
pub type LtErr = c_int;

// ========== 结构体定义 ==========

/// XL_Init 参数（size = 0x28 = 40）。
///
/// 2026-08-27 真机验证铁证（server 端 `DownloadSDK.dll::XL_Init` → 0x18003e950 字段访问）：
///   size(4) + u32(4) + word(2) + JSON 字符串(30) = 40（pack(1) 紧凑）
///   - +0x04: u32（配置标志，语义待确认）
///   - +0x08: word（0xffff = 无 JSON，跳过 JSON 解析；否则 = JSON 长度）
///   - +0x0a: JSON 字符串（窄/UTF-8，格式 `{key:val,key:val,...}`，逗号分隔，
///     最长 30 字节；`{`/`}` 边界，字段名如 app_guid/token_mode/equity_token）
///
/// ⚠️ 之前「4 窄字符串 + flags」推断已被真机反汇编推翻。
#[repr(C, packed)]
#[derive(Debug, Clone)]
pub struct XLInitParam {
    pub size: c_uint,                  // +0x00 = 0x28(40)
    pub field4: c_uint,                // +0x04 u32（配置标志，待确认）
    pub field8: c_ushort,              // +0x08 word（0xffff = 无 JSON）
    pub json: [c_char; 30],            // +0x0a JSON 字符串 `{...}`（最多 30 字节）
}

/// BT 任务参数 V2（size = 0x28 = 40）。
///
/// 权威布局（2026-08-27 反汇编 server 端序列化 RVA 0xf620 铁证）：
///   size(4) + 3×字符串指针(24) + 12 字节保留 = 40（pack(1) 紧凑，无对齐 padding）
///   - +0x04: torrent_path（UTF-16 宽字符串，wcslen）
///   - +0x0c: save_path（UTF-16 宽字符串，wcslen）
///   - +0x14: 第三个字符串（UTF-8 窄字符串，strlen；语义待确认：任务名/infohash？）
///   - +0x1c: 12 字节保留（序列化时未使用，填 0）
///
/// ⚠️ 旧定义（size+task_id+torrent_path+save_path+strategy+priority+subfile_count+
/// subfile_indices）已被推翻：task_id 是独立函数参数（不在结构体），
/// strategy/priority/subfile 均不在结构体。
#[repr(C, packed)]
#[derive(Debug, Clone)]
pub struct XLBTTaskParamV2 {
    pub size: c_uint,                   // +0x00 = 0x28(40)
    pub torrent_path: *const u16,       // +0x04 UTF-16 宽字符串（.torrent 路径）
    pub save_path: *const u16,          // +0x0c UTF-16 宽字符串（保存目录）
    pub third_str: *const c_char,       // +0x14 UTF-8 窄字符串（语义待确认）
    pub _reserved: [u8; 12],            // +0x1c 保留（序列化未用）
}

/// Magnet 任务参数 —— ⚠️ 2026-08-27 反汇编铁证：**此结构体不存在**。
///
/// `XL_CreateMagnetTask` 的签名是 3 个独立参数（非结构体）：
///   `XL_CreateMagnetTask(magnet: *const u16, save_path: *const u16, out: *mut u32)`
/// 序列化 RVA 0x180010940 无 size 校验（对比 XL_CreateBTTask_V2 有 `cmp [param], size`），
/// 参数直接是 2 个宽字符串（wcslen）+ 1 个 out 指针。
/// 此结构体保留仅作为文档占位，实际调用应直接用 XLCreateMagnetTaskFn 的新签名。
#[repr(C)]
#[derive(Debug, Clone)]
pub struct XLMagnetParam {
    pub size: c_uint,                   // 占位（实际函数不接收结构体）
    pub task_id: *mut c_void,
    pub magnet: *const c_char,
    pub save_path: *const c_char,
    pub strategy: c_uint,
    pub priority: c_uint,
    pub _pad: c_uint,
}

/// P2SP 任务参数（size = 0x38 = 56，反汇编铁证）。
///
/// 2026-08-27 反汇编 `XL_CreateP2spTask`（0x18780，薄包装）打包布局铁证：
///   +0x00: size (8 字节，`mov qword ptr [r11-0x48], 0x38`；+8 才是第一个指针)
///   +0x08: 宽字符串指针（url?）
///   +0x10: 宽字符串指针
///   +0x18: 宽字符串指针
///   +0x20: 宽字符串指针（save_path?）
///   +0x28: 宽字符串指针
///   +0x30: flags (8 字节，`mov qword ptr [r11-0x18], 2`)
/// 5 个指针均为 UTF-16 宽字符串（序列化 0x18000d3a0 里 `cmp word ptr [rdi+rax*2]` wcslen）。
/// ⚠️ 无 versioned size 校验（序列化直接访问字段，对比 BT_V2 有 cmp [param], size）。
#[repr(C)]
#[derive(Debug, Clone)]
pub struct XLP2spParam {
    pub size: c_ulonglong,              // +0x00 = 0x38(56)，8 字节
    pub url: *const u16,                // +0x08 宽字符串（URL）
    pub field10: *const u16,            // +0x10 宽字符串
    pub field18: *const u16,            // +0x18 宽字符串
    pub save_path: *const u16,          // +0x20 宽字符串（保存目录）
    pub field28: *const u16,            // +0x28 宽字符串
    pub flags: c_ulonglong,             // +0x30 = 2（8 字节）
}

/// Peer 信息（size = 0x38 = 56）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XLPeerInfo {
    pub size: c_uint,                   // = 0x38
    pub ip: [u8; 16],                   // IPv4 用前 4 字节 + 0 填充；IPv6 全用
    pub port: u16,
    pub _pad: u16,
    pub peer_type: c_uint,              // 0=BT, 1=DCDN, 2=PHub
    pub flags: c_uint,
    pub _reserved2: [u8; 8],           // padding to reach 56 bytes
    pub reserved: [c_uint; 4],
}

/// HTTP 镜像源（size = 0x24 = 36）。
///
/// 权威布局（2026-08-27 反汇编 XL_AddServer 转发 RVA 0x9990 铁证）：
///   size(4) + u32(4) + 3×宽字符串指针(24) + 4 保留 = 36
///   - +0x04: 整数（`mov eax, [r14+4]`，4 字节，语义待确认：端口？）
///   - +0x08: 宽字符串（UTF-16，`wcslen`）
///   - +0x10: 宽字符串（UTF-16）
///   - +0x18: 宽字符串（UTF-16）
///   - +0x20: 4 字节保留（序列化未访问）
///
/// ⚠️ 必须 packed：C 侧严格 36 字节无尾随 padding；`#[repr(C)]` 会向上对齐到 40。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct XLServerInfo {
    pub size: c_uint,                   // +0x00 = 0x24(36)
    pub port: c_uint,                   // +0x04 整数（语义待确认，可能是端口）
    pub url: *const u16,                // +0x08 宽字符串（UTF-16）
    pub str2: *const u16,               // +0x10 宽字符串（UTF-16，语义待确认）
    pub str3: *const u16,               // +0x18 宽字符串（UTF-16，语义待确认）
    pub _reserved: c_uint,              // +0x20 保留
}

/// 任务状态查询输出（size = 0x39c = 924）。
///
/// ⚠️ 2026-08-27 真机 dump 铁证（P2SP 本地 HTTP 完整生命周期 + BT ubuntu iso）：
/// 真实字段布局（u32 视角）：
///   +0x00: size (u32) = 0x39c
///   +0x04: task_state (u32) —— 0=未启动 3=下载中 5=暂停 7=完成（铁证）
///   +0x08: field8 (u32) = 0（疑似 task_id 低 32 位）
///   +0x0c: file_size (u32) —— 文件总大小（非 u64！）
///   +0x14: download_size (u32) —— 已下载大小（从 0 增长，非 u64！）
///   +0x1c: download_size 副本 (u32)
///   +0x24: 计数 (u32) —— 随秒/进度递增
///   +0x2c: peer 数 (u32)
///   +0x30: 连接数 (u32)
///   +0x34: download_size 副本2 (u32)
///   +0x54: 任务名/文件名（窄字符串，如 "test_5mb.bin"，ASCII 铁证）
///   +0x268: 8 字节 = -1（proxy 初始化哨兵）
///   +0x270: 1（完成标志？）
///   +0x274: download_size 副本3 (u32)
///   +0x27c: MIME 类型（窄字符串，如 "application/octet-stream"，ASCII 铁证）
///   +0x390: 4 字节 = -1（proxy 哨兵）
///   +0x394: 1（完成标志？）
///
/// ⚠️ 旧定义（task_id/download_size/file_size 为 u64）**错误**，真实均为 u32 且偏移不同。
/// 速度字段（down_rate）**不在** XLTaskInfo（需 XL_QueryTaskFlow 单独查询）。
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct XLTaskInfo {
    pub size: c_uint,                   // +0x00 = 0x39c(924)
    pub task_state: c_uint,             // +0x04 0=未启动 3=下载中 5=暂停 7=完成
    pub field8: c_uint,                 // +0x08 疑似 task_id 低32位
    pub file_size: c_uint,              // +0x0c 文件总大小（u32，铁证）
    pub field10: c_uint,                // +0x10 待确认
    pub download_size: c_uint,          // +0x14 已下载大小（u32，铁证增长）
    pub field18: c_uint,                // +0x18 待确认
    pub download_size_dup: c_uint,      // +0x1c download_size 副本
    pub field20: c_uint,                // +0x20 待确认
    pub count24: c_uint,                // +0x24 计数（随秒/进度递增）
    pub field28: c_uint,                // +0x28 待确认
    pub peer_count: c_uint,             // +0x2c peer 数
    pub conn_count: c_uint,             // +0x30 连接数
    pub download_size_dup2: c_uint,     // +0x34 download_size 副本2
    // 剩余字段（+0x38 之后：+0x54 文件名、+0x27c MIME、+0x268/+0x390 哨兵等，
    // 已部分 dump 确认但未逐字段结构化，保留字节占位）
    pub _remaining: [u8; 924 - 0x38],
}

/// 任务流量信息（size 待定，暂按 0x18 = 24 设计）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XLTaskFlow {
    pub size: c_uint,                   // = 0x18?
    pub download_bytes: c_ulonglong,
    pub upload_bytes: c_ulonglong,
    pub _pad: c_uint,
}

/// BT 子文件索引（size = 0x54 = 84）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XLBTSubTaskIndex {
    pub size: c_uint,                   // = 0x54
    pub indices: *const c_uint,
    pub count: c_uint,
    pub reserved: [c_uint; 20],
}

/// Emule 子文件索引（size = 0x6c = 108）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XLEmuleSubTaskIndex {
    pub size: c_uint,                   // = 0x6c
    pub indices: *const c_uint,
    pub count: c_uint,
    pub reserved: [c_uint; 24],
}

/// P2SP 子文件索引（size = 0x162 = 354）。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XLP2spSubTaskIndex {
    pub size: c_uint,                   // = 0x162
    pub url: *const c_char,
    pub save_path: *const c_char,
    pub reserved: [c_uint; 100],
}

// 非 Windows 平台提供空实现（防止编译错误）
#[cfg(not(windows))]
mod ffi {
    use super::*;
    pub struct LtHandle {}
}

// ========== 函数指针类型别名（供 libloading 使用） ==========

// NOTE(2026-08-27 真机反汇编铁证): XL_Init 是 2 参数（server_path + param），
// **无 out_handle 参数**。返回的是错误码（0=成功），handle 通过 SDK 全局句柄
// 获取（内部 call 0x180004030），不是输出参数。旧签名 fn(server_path, param, out_handle) 是错的。
pub type XLInitFn = unsafe extern "system" fn(server_path: *const c_char, param: *const XLInitParam) -> LtErr;
// NOTE(2026-08-27 真机反汇编): XL_UnInit 是 0 参数（handle 是 SDK 全局状态）。
pub type XLUnInitFn = unsafe extern "system" fn() -> LtErr;
// NOTE(2026-08-27 反汇编铁证): XL_CreateBTTask_V2 的 param 是第 1 参数（cmp [rcx+4]），
// 第 2 参数是 out（task_id, u32），**无 handle 参数**（SDK 内部 call 0x180004030 取全局句柄）。
// 旧签名 fn(handle, param) 是错的。
pub type XLCreateBTTaskV2Fn = unsafe extern "system" fn(param: *mut XLBTTaskParamV2, out_task_id: *mut c_uint) -> LtErr;
// NOTE(2026-08-27 反汇编铁证): XL_CreateMagnetTask 是 3 个独立参数，**无结构体**：
//   (magnet: *const u16 UTF-16宽, save_path: *const u16 UTF-16宽, out: *mut u32)
// 序列化 0x180010940 无 size 校验（对比 XL_CreateBTTask_V2 有 cmp [param], size），
// 参数直接是 2 个宽字符串（wcslen）+ 1 个 out 指针。旧签名 fn(handle, param) 是错的。
pub type XLCreateMagnetTaskFn = unsafe extern "system" fn(magnet: *const u16, save_path: *const u16, out_task_id: *mut c_uint) -> LtErr;
// NOTE(2026-08-27 反汇编铁证): XL_CreateP2spTask（0x18780）是薄包装，6 参数：
//   (url: *const u16, referer: *const u16, ua: *const u16, save_path: *const u16, filename: *const u16, out: *mut u32)
// 5 个宽字符串打包成 XLP2spParam（56 字节）后调 XL_CreateP2spTask_V2(param, out)。
pub type XLCreateP2spTaskFn = unsafe extern "system" fn(
    url: *const u16,
    referer: *const u16,
    ua: *const u16,
    save_path: *const u16,
    filename: *const u16,
    out_task_id: *mut c_uint,
) -> LtErr;
// NOTE(2026-08-27 反汇编铁证): 所有 task 操作函数**无 handle 参数**，task_id 是 u32（mov ebx, ecx）。
pub type XLStartTaskFn = unsafe extern "system" fn(task_id: c_uint) -> LtErr;
pub type XLStopTaskFn = unsafe extern "system" fn(task_id: c_uint) -> LtErr;
pub type XLDeleteTaskFn = unsafe extern "system" fn(task_id: c_uint, delete_data: c_int) -> LtErr;
pub type XLQueryTaskInfoFn = unsafe extern "system" fn(task_id: c_uint, info: *mut XLTaskInfo) -> LtErr;
pub type XLAddPeerFn = unsafe extern "system" fn(task_id: c_uint, peer_count: c_uint, peers: *const XLPeerInfo) -> LtErr;
pub type XLBatchAddBTTrackerFn = unsafe extern "system" fn(task_id: c_uint, trackers: *const *const c_char, count: c_uint) -> LtErr;
pub type XLDiscardPeerFn = unsafe extern "system" fn(task_id: c_uint, peer: *const XLPeerInfo) -> LtErr;
pub type XLBatchAddPeerFn = unsafe extern "system" fn(task_id: c_uint, peer_count: c_uint, peers: *const XLPeerInfo) -> LtErr;
pub type XLBatchDiscardPeerFn = unsafe extern "system" fn(task_id: c_uint, peer_count: c_uint, peers: *const XLPeerInfo) -> LtErr;
pub type XLEnableFreeDcdnFn = unsafe extern "system" fn(task_id: c_uint, enable: c_int) -> LtErr;
pub type XLDisableFreeDcdnFn = unsafe extern "system" fn(task_id: c_uint) -> LtErr;
pub type XLAddServerFn = unsafe extern "system" fn(task_id: c_uint, param2: c_uint, server: *const XLServerInfo) -> LtErr;
pub type XLQueryTaskFlowFn = unsafe extern "system" fn(task_id: c_uint, flow: *mut XLTaskFlow) -> LtErr;
pub type XLSetTaskUserAgentFn = unsafe extern "system" fn(task_id: c_uint, ua: *const c_char) -> LtErr;
pub type XLAddHttpHeaderFieldFn = unsafe extern "system" fn(task_id: c_uint, name: *const c_char, value: *const c_char) -> LtErr;
pub type XLSetTaskDownloadSpeedLimitFn = unsafe extern "system" fn(task_id: c_uint, limit: c_uint) -> LtErr;
pub type XLSetUserInfoFn = unsafe extern "system" fn(user_id: *const c_char, vip_type: *const c_char) -> LtErr;
// NOTE(考古·2026-08-25 已修正): 反编译显示 XL_SetUserInfo 两参数均为 `const char*`
// （strlen + XPF_String 构造）；旧整数绑定存在 strlen(整数) 段错误风险，已按证据改为字符串。
// 参数语义（user_id/vip_type 文本内容）待真机实测澄清。
pub type XLSetTokenModeFn = unsafe extern "system" fn(mode: c_uint) -> LtErr;
pub type XLSetAppGuidFn = unsafe extern "system" fn(guid: *const c_char) -> LtErr;
pub type XLSetAccelerateCertificationFn = unsafe extern "system" fn(cert: *const c_char) -> LtErr;
pub type XLSetUserAgentFn = unsafe extern "system" fn(ua: *const c_char) -> LtErr;
pub type XLSetProxyFn = unsafe extern "system" fn(proxy: *const c_char) -> LtErr;
pub type XLSetCacheSizeFn = unsafe extern "system" fn(size_mb: c_uint) -> LtErr;
pub type XLSetDownloadWindowFn = unsafe extern "system" fn(window: c_uint) -> LtErr;
pub type XLSetGlobalConnectionLimitFn = unsafe extern "system" fn(limit: c_uint) -> LtErr;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    /// ABI size 断言：锁定每个 versioned struct 的字节大小与反汇编铁证一致。
    ///
    /// 反汇编铁证（`docs/research/xunlei/xunlei_research_complete.md` §2.2，
    /// `mov rN, IMM` + `cmp [reg], rN` 模式）给出的 C 侧 struct size：
    ///   XLInitParam = 0x28(40), XLBTTaskParamV2 = 0x28(40),
    ///   XLPeerInfo = 0x38(56), XLServerInfo = 0x24(36), XLTaskInfo = 0x39c(924)
    ///
    /// 2026-08-27 全部修复对齐（重新提取 DLL + 完整反汇编）：
    ///   - XLBTTaskParamV2：pack(1) + 3 字符串指针 + 12 保留（铁证）
    ///   - XLServerInfo：pack(1) + u32 + 3 宽字符串 + 保留（铁证）
    ///   - XLInitParam：pack(1) + 4 窄字符串 + flags（推断，待真机验证）
    ///   - XLTaskInfo：pack(1) 消除尾随 padding（字段布局仍推测，待 dump 还原）
    /// 若未来结构体布局漂移，此测试立即回归报警。
    #[test]
    fn abi_size_assert_aligned() {
        assert_eq!(size_of::<XLInitParam>(), 0x28, "XLInitParam size 漂移即 ABI 回归");
        assert_eq!(size_of::<XLBTTaskParamV2>(), 0x28, "XLBTTaskParamV2 size 漂移即 ABI 回归");
        assert_eq!(size_of::<XLPeerInfo>(), 0x38, "XLPeerInfo size 漂移即 ABI 回归");
        assert_eq!(size_of::<XLServerInfo>(), 0x24, "XLServerInfo size 漂移即 ABI 回归");
        assert_eq!(size_of::<XLTaskInfo>(), 0x39c, "XLTaskInfo size 漂移即 ABI 回归");
        assert_eq!(size_of::<XLP2spParam>(), 0x38, "XLP2spParam size 漂移即 ABI 回归");
    }
}
