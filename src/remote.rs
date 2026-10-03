//! Passive directory remote. Every published file is complete and immutable.
use crate::{cas::sync_dir, Error, Result};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct DirectoryRemote {
    root: PathBuf,
}

/// Portable remote operations. Implementations may use a directory, an object
/// store, or a transfs service while keeping the same object and ref format.
pub trait RemoteStore: Send + Sync {
    fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<()>;
    fn get_blob(&self, hash: &str) -> Result<Vec<u8>>;
    fn put_pack(&self, bytes: &[u8]) -> Result<String>;
    fn get_pack(&self, hash: &str) -> Result<Vec<u8>>;
    fn publish_ref(&self, writer: &str, sequence: u64, bytes: &[u8]) -> Result<bool>;
    fn writers(&self) -> Result<Vec<String>>;
    fn refs_for(&self, writer: &str) -> Result<Vec<(u64, Vec<u8>)>>;
}

impl DirectoryRemote {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn blob_path(&self, hash: &str) -> Result<PathBuf> {
        valid_hash(hash)?;
        Ok(self.root.join("blobs").join(&hash[..2]).join(hash))
    }

    fn pack_path(&self, hash: &str) -> Result<PathBuf> {
        valid_hash(hash)?;
        Ok(self.root.join("packs").join(hash))
    }

    fn ref_path(&self, writer: &str, sequence: u64) -> Result<PathBuf> {
        valid_writer(writer)?;
        Ok(self
            .root
            .join("refs")
            .join(writer)
            .join(format!("{sequence:020}")))
    }

    pub fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<()> {
        check_hash(hash, bytes)?;
        let path = self.blob_path(hash)?;
        if !write_once(&path, bytes)? {
            check_hash(hash, &fs::read(path)?)?;
        }
        Ok(())
    }

    pub fn get_blob(&self, hash: &str) -> Result<Vec<u8>> {
        let bytes = fs::read(self.blob_path(hash)?)?;
        check_hash(hash, &bytes)?;
        Ok(bytes)
    }

    pub fn put_pack(&self, bytes: &[u8]) -> Result<String> {
        let hash = hash_bytes(bytes);
        let path = self.pack_path(&hash)?;
        if !write_once(&path, bytes)? {
            check_hash(&hash, &fs::read(path)?)?;
        }
        Ok(hash)
    }

    pub fn get_pack(&self, hash: &str) -> Result<Vec<u8>> {
        let bytes = fs::read(self.pack_path(hash)?)?;
        check_hash(hash, &bytes)?;
        Ok(bytes)
    }

    /// Returns false when another publisher already claimed this sequence.
    pub fn publish_ref(&self, writer: &str, sequence: u64, bytes: &[u8]) -> Result<bool> {
        write_once(&self.ref_path(writer, sequence)?, bytes)
    }

    pub fn writers(&self) -> Result<Vec<String>> {
        let dir = self.root.join("refs");
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut writers = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                return Err(Error::Storage("unexpected entry under refs/".into()));
            }
            let writer = entry.file_name().to_string_lossy().into_owned();
            valid_writer(&writer)?;
            writers.push(writer);
        }
        writers.sort();
        Ok(writers)
    }

    pub fn refs_for(&self, writer: &str) -> Result<Vec<(u64, Vec<u8>)>> {
        valid_writer(writer)?;
        let dir = self.root.join("refs").join(writer);
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut refs = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(".tmp.") {
                continue;
            }
            if !entry.file_type()?.is_file()
                || name.len() != 20
                || !name.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(Error::Storage(format!(
                    "unexpected ref entry for writer {writer}: {name}"
                )));
            }
            let sequence = name
                .parse::<u64>()
                .map_err(|e| Error::Storage(format!("invalid ref sequence: {e}")))?;
            refs.push((sequence, fs::read(entry.path())?));
        }
        refs.sort_by_key(|(sequence, _)| *sequence);
        Ok(refs)
    }
}

impl RemoteStore for DirectoryRemote {
    fn put_blob(&self, hash: &str, bytes: &[u8]) -> Result<()> {
        DirectoryRemote::put_blob(self, hash, bytes)
    }

    fn get_blob(&self, hash: &str) -> Result<Vec<u8>> {
        DirectoryRemote::get_blob(self, hash)
    }

    fn put_pack(&self, bytes: &[u8]) -> Result<String> {
        DirectoryRemote::put_pack(self, bytes)
    }

    fn get_pack(&self, hash: &str) -> Result<Vec<u8>> {
        DirectoryRemote::get_pack(self, hash)
    }

    fn publish_ref(&self, writer: &str, sequence: u64, bytes: &[u8]) -> Result<bool> {
        DirectoryRemote::publish_ref(self, writer, sequence, bytes)
    }

    fn writers(&self) -> Result<Vec<String>> {
        DirectoryRemote::writers(self)
    }

    fn refs_for(&self, writer: &str) -> Result<Vec<(u64, Vec<u8>)>> {
        DirectoryRemote::refs_for(self, writer)
    }
}

pub(crate) fn valid_writer(writer: &str) -> Result<()> {
    if writer.is_empty()
        || writer.len() > 128
        || !writer
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(Error::Storage(format!("invalid writer ID: {writer}")));
    }
    Ok(())
}

pub(crate) fn valid_hash(hash: &str) -> Result<()> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(Error::Storage(format!("invalid SHA-256 hash: {hash}")));
    }
    Ok(())
}

pub(crate) fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn check_hash(expected: &str, bytes: &[u8]) -> Result<()> {
    valid_hash(expected)?;
    if hash_bytes(bytes) != expected {
        return Err(Error::Storage(format!("object hash mismatch: {expected}")));
    }
    Ok(())
}

pub(crate) fn ensure_dir(dir: &Path) -> Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    let parent = dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_dir(parent)?;
    match fs::create_dir(dir) {
        Ok(()) => {
            sync_dir(parent)?;
            sync_dir(dir)?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// A hard link publishes already-synced bytes only if the destination is free.
fn write_once(path: &Path, bytes: &[u8]) -> Result<bool> {
    let dir = path.parent().expect("remote object path has a parent");
    ensure_dir(dir)?;
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let temp = dir.join(format!(
        ".tmp.{}.{}",
        std::process::id(),
        hex::encode(nonce)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let result = (|| -> Result<bool> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temp, path) {
            Ok(()) => {
                sync_dir(dir)?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e.into()),
        }
    })();
    let _ = fs::remove_file(&temp);
    if result.is_ok() {
        sync_dir(dir)?;
    }
    result
}
