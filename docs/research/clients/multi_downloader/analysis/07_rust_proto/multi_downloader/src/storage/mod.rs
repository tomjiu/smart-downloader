//! Storage backends — piece store, SQLite-WAL resume DB, and atomic file IO.

pub mod file_io;
pub mod piece_store;
pub mod resume_db;

pub use file_io::AtomicFile;
pub use piece_store::{PieceHash, PieceStore, Sha256Hash};
pub use resume_db::ResumeDb;
