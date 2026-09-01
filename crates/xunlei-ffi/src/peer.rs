//! Peer 管理（AddPeer / DiscardPeer / BatchAddBTTracker）。
//!
//! 匿名模式下可用，用于：
//! - 注入已知 peer（测试/本地网络）
//! - 封禁吸血 peer（反吸血）
//! - 添加自定义 tracker

use std::net::SocketAddr;
use tokio::task;

use crate::bindings::{self, XLPeerInfo};
use crate::error::{Result, XunleiError};
use crate::handle::XunleiHandle;
use crate::task::TaskId;

impl XunleiHandle {
    /// 添加 peer（单条）。
    pub async fn add_peer(&self, id: &TaskId, peer: SocketAddr) -> Result<()> {
        let sym = self.inner.symbols;
        let peer_info = socket_addr_to_peer_info(peer, PeerKind::Bt);
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_AddPeer)(task_id_u32, 1, &peer_info);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_AddPeer failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 批量添加 peer。
    pub async fn batch_add_peer(&self, id: &TaskId, peers: &[SocketAddr]) -> Result<()> {
        if peers.is_empty() {
            return Ok(());
        }

        let sym = self.inner.symbols;
        let peer_infos: Vec<XLPeerInfo> = peers
            .iter()
            .map(|&addr| socket_addr_to_peer_info(addr, PeerKind::Bt))
            .collect();
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            let r =
                (sym.XL_BatchAddPeer)(task_id_u32, peer_infos.len() as u32, peer_infos.as_ptr());
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_BatchAddPeer failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 封禁 peer（反吸血）。
    pub async fn discard_peer(&self, id: &TaskId, peer: SocketAddr) -> Result<()> {
        let sym = self.inner.symbols;
        let peer_info = socket_addr_to_peer_info(peer, PeerKind::Bt);
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_DiscardPeer)(task_id_u32, &peer_info);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_DiscardPeer failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 批量封禁 peer。
    pub async fn batch_discard_peer(&self, id: &TaskId, peers: &[SocketAddr]) -> Result<()> {
        if peers.is_empty() {
            return Ok(());
        }

        let sym = self.inner.symbols;
        let peer_infos: Vec<XLPeerInfo> = peers
            .iter()
            .map(|&addr| socket_addr_to_peer_info(addr, PeerKind::Bt))
            .collect();
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            let r = (sym.XL_BatchDiscardPeer)(
                task_id_u32,
                peer_infos.len() as u32,
                peer_infos.as_ptr(),
            );
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_BatchDiscardPeer failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }

    /// 添加 HTTP 镜像源（web seed）。
    pub async fn add_server(&self, id: &TaskId, url: &str) -> Result<()> {
        let sym = self.inner.symbols;
        let url = url.to_string();
        let task_id_u32 = id.0 as u32;

        task::spawn_blocking(move || unsafe {
            // 2026-08-27 反汇编铁证：XLServerInfo.url 是 UTF-16 宽字符串
            let url_wide = {
                let mut v: Vec<u16> = url.encode_utf16().collect();
                v.push(0);
                v
            };

            let server = bindings::XLServerInfo {
                // 反汇编铁证 = 0x24(36)，非 size_of::<Self>()
                size: 0x24,
                port: 0,
                url: url_wide.as_ptr(),
                str2: std::ptr::null(),
                str3: std::ptr::null(),
                _reserved: 0,
            };

            // NOTE: XL_AddServer(task_id, param2, server) 3 参数；param2 语义待确认（暂 0）
            let r = (sym.XL_AddServer)(task_id_u32, 0, &server);
            if r != 0 {
                return Err(XunleiError::with_context(r, "XL_AddServer failed"));
            }
            Ok(())
        })
        .await
        .map_err(|e| XunleiError::Other(format!("spawn_blocking failed: {}", e)))?
    }
}

/// Peer 类型（对应 XLPeerInfo.peer_type）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerKind {
    /// 标准 BT peer。
    Bt = 0,
    /// DCDN peer（迅雷 CDN 加速节点）。
    Dcdn = 1,
    /// PHub peer（迅雷 P2SP 中心化发现节点）。
    Phub = 2,
}

impl From<PeerKind> for u32 {
    fn from(k: PeerKind) -> Self {
        k as u32
    }
}

/// 将 SocketAddr 转换为 XLPeerInfo。
fn socket_addr_to_peer_info(addr: SocketAddr, kind: PeerKind) -> XLPeerInfo {
    let mut ip_bytes = [0u8; 16];
    if let std::net::IpAddr::V4(v4) = addr.ip() {
        ip_bytes[0..4].copy_from_slice(&v4.octets());
    } else if let std::net::IpAddr::V6(v6) = addr.ip() {
        ip_bytes.copy_from_slice(&v6.octets());
    }

    XLPeerInfo {
        size: std::mem::size_of::<XLPeerInfo>() as u32, // = 0x38
        ip: ip_bytes,
        port: addr.port(),
        _pad: 0,
        peer_type: kind as u32,
        flags: 0,
        _reserved2: [0u8; 8],
        reserved: [0; 4],
    }
}
