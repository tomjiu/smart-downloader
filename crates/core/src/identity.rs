//! 身份模型（§7 + D33/D34）：CanonicalId（去重键）与 ContentIdentity（内容指纹）。

use serde::{Deserialize, Serialize};

/// canonical 键种类（§7）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CanonicalKind {
    /// BT: btih(40hex)
    Bt,
    /// TorrentFile: SHA256(bytes)
    TorrentFile,
    /// HTTP: normalized URL（D34 token 剔除）
    Http,
    /// FTP: URL+size+mtime
    Ftp,
}

/// 去重验证器（带 token 的 URL 必须有一致 validator 才认重，D34）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Validator {
    Size(u64),
    SizeAndEtag(u64, String),
}

/// 去重键（§7）：kind + normalized identity + 可选 validator。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct CanonicalId {
    pub kind: CanonicalKind,
    pub identity: String,
    pub validator: Option<Validator>,
    /// URL 含签名/token 参数（D34）：无 validator 时不自动去重。
    #[serde(default)]
    pub token_sensitive: bool,
}

/// 内容指纹（D33：v1 两态；PieceHashed 属 v2，v2 走 schema version 升级）。
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ContentIdentity {
    InfoHash([u8; 20]),
    SingleFile {
        size: u64,
        etag: Option<String>,
        sha256: Option<String>,
        /// 主源 SHA1 校验（E25，40 位十六进制）。与 sha256/md5 互斥
        /// （add 层 validate 保证，至多一个 Some）。
        #[serde(default)]
        sha1: Option<String>,
        /// 主源 MD5 校验（E25，32 位十六进制）。与 sha256/sha1 互斥
        /// （add 层 validate 保证，至多一个 Some）。
        #[serde(default)]
        md5: Option<String>,
        /// 备用源内容 MD5（夸克 backup_md5 机制：切换备用源后以其校验）。
        #[serde(default)]
        backup_md5: Option<String>,
    },
}
