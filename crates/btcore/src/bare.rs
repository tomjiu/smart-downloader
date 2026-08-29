//! M0 门面：`Bare`（最小内核，6 函数；unsafe 归 ffi 层）。
//! 契约对齐 ffi/lt.h（M0 子集）；完整富 API 见 engine::BtCore（M1）。

use std::os::raw::c_char;
use std::path::Path;

use crate::ffi::Session;

pub use crate::ffi::{Error, Result};

/// M0 最小内核句柄（libtorrent session 的 safe 包装）。M1 起由 BtCore 取代。
pub struct Bare {
    sess: Session,
}

impl Bare {
    pub fn new(save_path: &Path, session_id: &str) -> Result<Self> {
        Ok(Bare {
            sess: Session::new(save_path, session_id)?,
        })
    }

    pub fn add_magnet(&self, magnet: &str, web_seeds: &[String]) -> Result<String> {
        self.sess.add_magnet(magnet, web_seeds)
    }

    pub fn add_peer(&self, ih: &str, ip: &str, port: u16) -> Result<()> {
        self.sess.add_peer(ih, ip, port)
    }

    /// (progress, state)；state 0 下载 1 完成 3 错误 4 元数据获取中
    pub fn status(&self, ih: &str) -> Result<(f32, i32)> {
        let st = self.sess.status(ih)?;
        Ok((st.progress, st.state))
    }

    /// 完成即停（做种停止，§10.1）；同步点 = torrent_paused alert（D19/D32）
    pub fn pause(&self, ih: &str) -> Result<()> {
        self.sess.pause(ih)
    }

    pub fn resume(&self, ih: &str) -> Result<()> {
        self.sess.resume(ih)
    }

    /// 诊断辅助（M0 调试用）：(metadata_received, num_peers, num_seeds)
    pub fn status_extra(&self, ih: &str) -> Result<(i32, i32, i32)> {
        let st = self.sess.status(ih)?;
        Ok((st.metadata_received, st.num_peers, st.num_seeds))
    }

    /// 诊断辅助（M0 调试用）：设置 alert mask
    pub fn diag_set_mask(&self, mask: u32) -> Result<()> {
        self.sess.set_alert_mask(mask)
    }

    /// 诊断辅助（M0 调试用）：弹出并扁平化 alert 文本
    pub fn diag_pop_alerts(&self) -> Result<Vec<String>> {
        Ok(self
            .sess
            .pop_alerts(64)?
            .iter()
            .map(|a| format!("kind={} msg={}", a.kind, fstr(&a.msg)))
            .collect())
    }
}

fn fstr<const N: usize>(arr: &[c_char; N]) -> String {
    let bytes: Vec<u8> = arr
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
