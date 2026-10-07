//! Store settings, kept in the store itself (`<store>/.transfs/store.json`) so
//! every program that opens the store writes it the same way.
use crate::{cas::sync_dir, Error, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
};

/// Whether new versions are chunked (docs/chunked-storage.md). `Off` keeps
/// every file a whole blob: a simple store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Chunking {
    /// Chunk files as docs/file-types.md says.
    #[default]
    Auto,
    /// Store every file as a whole blob.
    Off,
}

impl FromStr for Chunking {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "auto" => Ok(Self::Auto),
            "off" => Ok(Self::Off),
            _ => Err(Error::Storage(format!(
                "chunking must be auto or off, not {s:?}"
            ))),
        }
    }
}

impl fmt::Display for Chunking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Off => "off",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    format: u8,
    pub chunking: Chunking,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            format: 1,
            chunking: Chunking::Auto,
        }
    }
}

impl StoreConfig {
    pub fn path(root: &Path) -> PathBuf {
        root.join(".transfs/store.json")
    }

    /// The store's settings; a store without a settings file uses the defaults.
    pub fn load(root: &Path) -> Result<Self> {
        match fs::read(Self::path(root)) {
            Ok(bytes) => {
                let config: Self = serde_json::from_slice(&bytes)
                    .map_err(|e| Error::Storage(format!("invalid store settings: {e}")))?;
                if config.format != 1 {
                    return Err(Error::Storage(format!(
                        "unsupported store settings format {}",
                        config.format
                    )));
                }
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let dir = root.join(".transfs");
        fs::create_dir_all(&dir)?;
        let mut random = [0_u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let tmp = dir.join(format!(".store.{}.tmp", hex::encode(random)));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            file.write_all(
                &serde_json::to_vec(self)
                    .map_err(|e| Error::Storage(format!("cannot encode store settings: {e}")))?,
            )?;
            file.sync_all()?;
            fs::rename(&tmp, Self::path(root))?;
            sync_dir(&dir)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}
