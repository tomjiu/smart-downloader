//! Piece storage + integrity verification.
//!
//! Borrows FlashGet's part-state-machine (analysis §4.3) and adds SHA-256
//! piece hashes (FlashGet used MD5; we use SHA-256 for collision resistance).
//!
//! Pieces here are an **auxiliary** construct for HTTP downloads: each slice
//! (`core::task::Slice`) is associated with a `Piece` for hash tracking. For
//! BT downloads, the same abstraction serves as the on-disk piece cache that
//! BT protocols expect.

use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A 32-byte SHA-256 piece hash.
pub type Sha256Hash = [u8; 32];

/// Trait-erased hash type (SHA-256 / SHA-1 / MD5 / etc.).
pub trait PieceHash: Send + Sync {
    /// Hash the given bytes and return the digest.
    fn hash(&self, data: &[u8]) -> Vec<u8>;
    /// Algorithm name (e.g. `"sha256"`).
    fn algorithm(&self) -> &'static str;
    /// Expected digest length in bytes.
    fn digest_len(&self) -> usize;
}

/// Default SHA-256 hasher.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sha256Hasher;

impl PieceHash for Sha256Hasher {
    fn hash(&self, data: &[u8]) -> Vec<u8> {
        let mut h = Sha256::new();
        h.update(data);
        h.finalize().to_vec()
    }
    fn algorithm(&self) -> &'static str {
        "sha256"
    }
    fn digest_len(&self) -> usize {
        32
    }
}

/// On-disk piece record. Each piece maps to a byte range and an expected hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Piece {
    /// Piece index within the torrent / file plan.
    pub index: u32,
    /// Byte offset in the destination file.
    pub offset: u64,
    /// Piece length in bytes (final piece may be shorter).
    pub length: u32,
    /// Expected SHA-256 hash (None = unverified).
    pub expected_hash: Option<Sha256Hash>,
    /// True if the piece has been written to disk and hash-verified.
    pub verified: bool,
}

/// In-memory piece store (per-task). A separate `ResumeDb` handles
/// persistence.
pub struct PieceStore {
    pieces: RwLock<Vec<Piece>>,
    dest: PathBuf,
    /// Optional hasher override (defaults to `Sha256Hasher`).
    hasher: Box<dyn PieceHash>,
}

impl PieceStore {
    /// Build a piece store for the given destination file.
    #[must_use]
    pub fn new(dest: PathBuf) -> Self {
        Self {
            pieces: RwLock::new(Vec::new()),
            dest,
            hasher: Box::new(Sha256Hasher),
        }
    }

    /// Install a freshly-computed piece plan (typically from BT metadata or
    /// from `core::task::DownloadTask::plan_slices`).
    pub fn install(&self, pieces: Vec<Piece>) {
        *self.pieces.write() = pieces;
    }

    /// Get the number of pieces currently tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pieces.read().len()
    }

    /// True if the store holds zero pieces.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pieces.read().is_empty()
    }

    /// Return the indices of pieces still needing verification.
    #[must_use]
    pub fn pending_pieces(&self) -> Vec<u32> {
        self.pieces
            .read()
            .iter()
            .filter(|p| !p.verified)
            .map(|p| p.index)
            .collect()
    }

    /// Return the indices of pieces already verified.
    #[must_use]
    pub fn verified_pieces(&self) -> Vec<u32> {
        self.pieces
            .read()
            .iter()
            .filter(|p| p.verified)
            .map(|p| p.index)
            .collect()
    }

    /// Mark a piece as verified (after a successful hash check).
    pub fn mark_verified(&self, index: u32) {
        if let Some(p) = self.pieces.write().iter_mut().find(|p| p.index == index) {
            p.verified = true;
        }
    }

    /// Verify a piece against its expected hash, given freshly-read bytes.
    ///
    /// Returns `Ok(true)` if verified, `Ok(false)` if no expected hash was set,
    /// or `Err` with an `Integrity` error if the hash did not match.
    pub fn verify(&self, index: u32, bytes: &[u8]) -> crate::error::Result<bool> {
        let pieces = self.pieces.read();
        let piece = pieces
            .iter()
            .find(|p| p.index == index)
            .ok_or_else(|| {
                crate::error::DownloadError::new(
                    0,
                    crate::error::ErrorCategory::Integrity,
                    format!("piece {index} not found"),
                )
            })?;
        let Some(expected) = piece.expected_hash else {
            return Ok(false);
        };
        let actual = self.hasher.hash(bytes);
        if actual.as_slice() == expected.as_slice() {
            drop(pieces);
            self.mark_verified(index);
            Ok(true)
        } else {
            Err(crate::error::DownloadError::new(
                0,
                crate::error::ErrorCategory::Integrity,
                format!("piece {index} hash mismatch"),
            ))
        }
    }

    /// Snapshot all pieces (cloned).
    #[must_use]
    pub fn snapshot(&self) -> Vec<Piece> {
        self.pieces.read().clone()
    }

    /// Build a hash of `index → Piece` for fast lookup.
    #[must_use]
    pub fn as_map(&self) -> HashMap<u32, Piece> {
        self.pieces
            .read()
            .iter()
            .map(|p| (p.index, p.clone()))
            .collect()
    }

    /// Destination path (read-only).
    #[must_use]
    pub fn dest(&self) -> &std::path::Path {
        &self.dest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_hash(bytes: &[u8]) -> Sha256Hash {
        let mut h = Sha256::new();
        h.update(bytes);
        let out = h.finalize();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out);
        arr
    }

    #[test]
    fn verify_accepts_matching_hash() {
        let store = PieceStore::new(PathBuf::from("/tmp/x"));
        let data = b"hello world";
        let p = Piece {
            index: 0,
            offset: 0,
            length: data.len() as u32,
            expected_hash: Some(mk_hash(data)),
            verified: false,
        };
        store.install(vec![p]);
        let r = store.verify(0, data).unwrap();
        assert!(r);
        assert_eq!(store.verified_pieces(), vec![0]);
    }

    #[test]
    fn verify_rejects_mismatched_hash() {
        let store = PieceStore::new(PathBuf::from("/tmp/y"));
        let p = Piece {
            index: 0,
            offset: 0,
            length: 5,
            expected_hash: Some(mk_hash(b"wrong")),
            verified: false,
        };
        store.install(vec![p]);
        let r = store.verify(0, b"hello");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().category, crate::error::ErrorCategory::Integrity);
    }

    #[test]
    fn verify_returns_false_when_no_hash() {
        let store = PieceStore::new(PathBuf::from("/tmp/z"));
        let p = Piece {
            index: 0,
            offset: 0,
            length: 5,
            expected_hash: None,
            verified: false,
        };
        store.install(vec![p]);
        let r = store.verify(0, b"hello").unwrap();
        assert!(!r);
    }
}
