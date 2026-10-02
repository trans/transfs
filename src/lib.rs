//! Portable claim-log and content store. Native indexing is a separate module.
pub mod cas;
pub mod check;
pub mod claim;
pub mod document;
pub mod library;
pub mod log;
pub mod query;

#[cfg(feature = "native")]
pub mod index;
#[cfg(feature = "native")]
pub mod mount;

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("{0}")]
    InvalidClaim(String),
    #[error("{path}:{line}: {reason}")]
    CorruptLog {
        path: std::path::PathBuf,
        line: usize,
        reason: String,
    },
    #[error("ambiguous id prefix '{prefix}' ({matches} matches)")]
    AmbiguousId { prefix: String, matches: usize },
    #[error("native library error: {0}")]
    Native(String),
    #[cfg(feature = "native")]
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
