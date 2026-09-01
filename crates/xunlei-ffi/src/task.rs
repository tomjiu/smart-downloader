//! 任务生命周期管理（Magnet/BT/P2SP 创建、启停、删除）。

use std::path::Path;
use tokio::task;

use crate::bindings::{self};
use crate::error::{Result, XunleiError};
use crate::handle::XunleiHandle;

/// 任务 ID（迅雷引擎返回的 opaque 句柄的 Rust 包装）。
///
/// ⚠️ 2026-08-27 真机铁证：task_id 在 ABI 层是 u32（`mov ebx, ecx`），
/// 但此类型用 u64 存储以兼容外部 API；FFI 调用点用 `as u32` 截断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u64);

impl XunleiHandle {
    /// 创建磁力任务（免登录）。
    ///
    /// 匿名身份 UserID=0 可直接使用。
    ///
    /// ⚠️ 2026-08-27 反汇编铁证：签名是 3 个独立参数（非结构体）：
    ///   `XL_CreateMagnetTask(magnet: *const u16, save_path: *const u16, out: *mut u32)`
    pub async fn create_magnet_task(&self, magnet: &str, save: &Path) -> Result<TaskId> {
        let sym = self.inner.symbols;
        let magnet = magnet.to_string();
        let save = save.to_path_buf();
        task::spawn_blocking(move || unsafe {
            // magnet 和 save_path 都是 UTF-16 宽字符串（反汇编 wcslen 铁证）
            let magnet_wide = {
                let mut v: Vec<u16> = magnet.encode_utf16().collect();
                v.push(0);
                v
            };
            let save_wide = {
                let mut v: Vec<u16> = save.to_string_lossy().encode_utf16().collect();
                v.push(0);
                v
            };

            let mut task_id: u32 = 0;
            let r =
                (sym.XL_CreateMagnetTask)(magnet_wide.as_ptr(), save_wide.as_ptr(), &mut task_id);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_CreateMagnetTask failed"));
            }

            if task_id == 0 {
                return Err(XunleiError::Other(
                    "XL_CreateMagnetTask returned null task_id".into(),
                ));
            }

            Ok(TaskId(task_id as u64))
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 创建 BT 任务（需要 .torrent 文件内容）。
    ///
    /// 匿名身份 UserID=0 可直接使用。
    /// 子文件索引通过 XL_SetBTSubTaskIndex 单独设置。
    ///
    /// ⚠️ 2026-08-27 反汇编铁证重构：BT_TASK_PARAM_V2 = size + torrent_path(UTF-16宽) +
    /// save_path(UTF-16宽) + third_str(UTF-8窄，语义待确认) + 12 padding。
    /// 函数签名 XL_CreateBTTask_V2(param, out_task_id)，无 handle 参数。
    pub async fn create_bt_task(&self, torrent_bytes: &[u8], save: &Path) -> Result<TaskId> {
        let sym = self.inner.symbols;
        let torrent_bytes = torrent_bytes.to_vec();
        let save = save.to_path_buf();
        task::spawn_blocking(move || {
            // 将 torrent 写入临时文件
            let temp_dir = std::env::temp_dir().join("xunlei-ffi");
            let _ = std::fs::create_dir_all(&temp_dir);
            let torrent_path = temp_dir.join(format!("task-{}.torrent", std::process::id()));
            std::fs::write(&torrent_path, &torrent_bytes).map_err(|e| {
                XunleiError::Other(format!("failed to write temp torrent file: {}", e))
            })?;

            let mut param = bindings::XLBTTaskParamV2 {
                size: 0x28, // 反汇编铁证 = 40，非 size_of::<Self>()
                torrent_path: std::ptr::null(),
                save_path: std::ptr::null(),
                third_str: std::ptr::null(),
                _reserved: [0u8; 12],
            };

            let torrent_path_wide = path_to_wide(&torrent_path);
            let save_wide = path_to_wide(&save);
            // third_str（+0x14 窄字符串）反汇编铁证**必须非空**（cmp [rcx+0x14], 0; je 失败）。
            // 语义 = 任务显示名（真机验证传任意非空字符串即可成功）。
            let third_cstr =
                std::ffi::CString::new("smart-dl-task").expect("static str has no null byte");

            param.torrent_path = torrent_path_wide.as_ptr();
            param.save_path = save_wide.as_ptr();
            param.third_str = third_cstr.as_ptr();

            unsafe {
                let mut task_id: u32 = 0;
                let r = (sym.XL_CreateBTTask_V2)(&mut param, &mut task_id);
                if r != 0 {
                    return Err(XunleiError::with_context(r, "XL_CreateBTTask_V2 failed"));
                }

                if task_id == 0 {
                    return Err(XunleiError::Other(
                        "XL_CreateBTTask_V2 returned null task_id".into(),
                    ));
                }

                Ok(TaskId(task_id as u64))
            }
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 创建 P2SP 任务（HTTP/普通 URL 下载，走迅雷 P2SP 加速网络）。
    ///
    /// ⚠️ 2026-08-27 反汇编铁证：`XL_CreateP2spTask`（0x18780）是 6 参数薄包装：
    ///   `(url, referer, ua, save_path, filename, out)`，5 个宽字符串打包成
    ///   XLP2spParam（56 字节）后调 `XL_CreateP2spTask_V2`。
    /// 真机验证：5 指针全非空时返回 0 + task_id=1。
    pub async fn create_p2sp_task(&self, url: &str, save: &Path, filename: &str) -> Result<TaskId> {
        let sym = self.inner.symbols;
        let url = url.to_string();
        let save = save.to_path_buf();
        let filename = filename.to_string();
        task::spawn_blocking(move || unsafe {
            let url_wide = str_to_wide(&url);
            let save_wide = path_to_wide(&save);
            let filename_wide = str_to_wide(&filename);
            // referer / user-agent 语义待确认，传空串（非 NULL，避免薄包装校验失败）
            let empty_wide = [0u16; 1];

            let mut task_id: u32 = 0;
            let r = (sym.XL_CreateP2spTask)(
                url_wide.as_ptr(),
                empty_wide.as_ptr(),
                empty_wide.as_ptr(),
                save_wide.as_ptr(),
                filename_wide.as_ptr(),
                &mut task_id,
            );
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_CreateP2spTask failed"));
            }

            if task_id == 0 {
                return Err(XunleiError::Other(
                    "XL_CreateP2spTask returned null task_id".into(),
                ));
            }

            Ok(TaskId(task_id as u64))
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 启动任务。
    pub async fn start_task(&self, id: &TaskId) -> Result<()> {
        let sym = self.inner.symbols;
        let task_id_u32 = id.0 as u32;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_StartTask)(task_id_u32);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_StartTask failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 停止任务。
    pub async fn stop_task(&self, id: &TaskId) -> Result<()> {
        let sym = self.inner.symbols;
        let task_id_u32 = id.0 as u32;
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_StopTask)(task_id_u32);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_StopTask failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 删除任务。
    ///
    /// `delete_data` — 是否同时删除已下载的文件。
    pub async fn delete_task(&self, id: &TaskId, delete_data: bool) -> Result<()> {
        let sym = self.inner.symbols;
        let task_id_u32 = id.0 as u32;
        let delete_data = if delete_data { 1 } else { 0 };
        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_DeleteTask)(task_id_u32, delete_data);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_DeleteTask failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}

/// 路径转 UTF-16 宽字符串（`Vec<u16>`，带 NUL 结尾）。
///
/// 2026-08-27 反汇编铁证：BT_TASK_PARAM_V2 的 torrent_path/save_path 是 UTF-16
/// 宽字符串（`wcslen` 校验，`cmp word ptr [r+*2]`）。Windows 路径用宽字符。
///
/// 平台门控（2026-08-30 Task 5-a）：Windows 用 `OsStr::encode_wide`（保留非 UTF-8
/// 路径字节）；非 Windows 用 `to_string_lossy` 兜底（仅保证可编译，运行时该 crate
/// 在非 Windows 由 loader 层短路返回 Err，不会真正走到这里）。
#[cfg(windows)]
fn path_to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    // 末尾加 NUL
    let mut out = wide;
    out.push(0);
    out
}

/// 非 Windows 编译兜底：路径按 UTF-8 lossy 转 UTF-16（保持类型/签名不变）。
#[cfg(not(windows))]
fn path_to_wide(path: &Path) -> Vec<u16> {
    str_to_wide(&path.to_string_lossy())
}

/// 字符串转 UTF-16 宽字符串（`Vec<u16>`，带 NUL 结尾）。
fn str_to_wide(s: &str) -> Vec<u16> {
    let mut out: Vec<u16> = s.encode_utf16().collect();
    out.push(0);
    out
}
