//! 迅雷 BT 任务迁移转换器。
//!
//! 基于 `docs/research/xunlei/spec_pending_validation.md` A 级验证规格实现：
//! - `xlbt_cfg`：解析 `.xlbt.cfg` 文件（magic / infohash / tag-02 记录 / peer 缓存）
//! - `xltd`：验证 `.bt.xltd`  piece 数据（偏移公式 + SHA1 交叉验证）
//! - `fastresume`：生成 libtorrent fastresume（JSON 报告 + bencode 编码）
//! - `cid_store`：cid_store.dat 假设解析器（附录 A #7 解封，三形态自适应，待真实样本校准）
//!
//! 验证状态（2026-08-17）：真实样本三件套到位，验证器 V1-V8 全绿
//! （magic / section 数组 / bitfield / fastresume 位图 / 物化），转换器已重构为
//! 真实格式并 e2e 通过。见 `docs/research/xunlei/NEXT_ACTION.md` 里程碑记录。
//! 残留待验证项仅剩 peer 缓存内部扩展字段（P1，不影响转换）。

pub mod cid_store;
pub mod fastresume;
pub mod xlbt_cfg;
pub mod xltd;

#[cfg(test)]
pub mod integration_tests;

pub use cid_store::{analyze_cid_store, CidStoreEntry, CidStoreReport};
pub use fastresume::{build_bitfield, build_bitfield_lenient, FastresumeConverter, PartialPieceInfo};
pub use xlbt_cfg::{XlbtCfg, XlbtCfgError};
pub use xltd::{XltdAnalysis, XltdError};
