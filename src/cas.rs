use crate::Result;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Cas {
    root: PathBuf,
}

impl Cas {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }
    pub fn path_for(&self, hash: &str) -> PathBuf {
        self.blobs_dir()
            .join(hash.get(..2).unwrap_or(hash))
            .join(hash)
    }
    pub fn exists(&self, hash: &str) -> bool {
        self.path_for(hash).exists()
    }
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(hash);
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = hex::encode(Sha256::digest(bytes));
        let path = self.path_for(&hash);
        if path.exists() {
            return Ok(hash);
        }
        let dir = path.parent().expect("blob path has parent");
        fs::create_dir_all(dir)?;
        sync_dir(&self.root)?;
        sync_dir(dir.parent().expect("fan-out has parent"))?;
        sync_dir(dir)?;
        let mut random = [0_u8; 4];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let tmp = dir.join(format!(
            "{hash}.tmp.{}.{}",
            std::process::id(),
            hex::encode(random)
        ));
        let result = (|| -> Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, &path)?;
            sync_dir(dir)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        Ok(hash)
    }
}

pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
