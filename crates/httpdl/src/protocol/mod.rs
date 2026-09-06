//! 协议层（M4c：FTP 子集，feature-gated；C-S1：SFTP 子集）。

#[cfg(feature = "ftp")]
pub mod ftp;
#[cfg(feature = "sftp")]
pub mod sftp;
