//! Passive directory remote. Every published file is complete and immutable.
use crate::{cas::sync_dir, Error, Result};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

const REF_PREFIX: &[u8] = b"\0TRANSFS-REF-1\n";
const PENDING_PREFIX: &[u8] = b"\0TRANSFS-PENDING-1\n";

#[derive(Clone, Debug)]
pub struct DirectoryRemote {
    root: PathBuf,
    capabilities: Arc<OnceLock<RemoteCapabilities>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemoteCapabilities {
    pub hard_links: bool,
    pub exclusive_create: bool,
    pub replace_existing: bool,
    pub directory_sync: bool,
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
        Self {
            root: root.into(),
            capabilities: Arc::new(OnceLock::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Probe the actual filesystem before its first write, including removable
    /// media and network mounts. The result is cached for this remote handle.
    pub fn check(&self) -> Result<RemoteCapabilities> {
        if let Some(capabilities) = self.capabilities.get() {
            return Ok(*capabilities);
        }
        let capabilities = probe(&self.root)?;
        let _ = self.capabilities.set(capabilities);
        Ok(capabilities)
    }

    fn write_capabilities(&self) -> Result<RemoteCapabilities> {
        let capabilities = self.check()?;
        if !capabilities.exclusive_create
            || !capabilities.directory_sync
            || !capabilities.replace_existing
        {
            return Err(Error::Storage(format!(
                "remote filesystem is not safe for publishing: {capabilities:?}"
            )));
        }
        Ok(capabilities)
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
        self.write_capabilities()?;
        write_content(&path, bytes)
    }

    pub fn get_blob(&self, hash: &str) -> Result<Vec<u8>> {
        let bytes = fs::read(self.blob_path(hash)?)?;
        check_hash(hash, &bytes)?;
        Ok(bytes)
    }

    pub fn put_pack(&self, bytes: &[u8]) -> Result<String> {
        let hash = hash_bytes(bytes);
        let path = self.pack_path(&hash)?;
        self.write_capabilities()?;
        write_content(&path, bytes)?;
        Ok(hash)
    }

    pub fn get_pack(&self, hash: &str) -> Result<Vec<u8>> {
        let bytes = fs::read(self.pack_path(hash)?)?;
        check_hash(hash, &bytes)?;
        Ok(bytes)
    }

    /// Returns false when another publisher already claimed this sequence.
    pub fn publish_ref(&self, writer: &str, sequence: u64, bytes: &[u8]) -> Result<bool> {
        write_once_ref(
            &self.ref_path(writer, sequence)?,
            bytes,
            self.write_capabilities()?,
        )
    }

    pub fn writers(&self) -> Result<Vec<String>> {
        let dir = self.root.join("refs");
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut writers = Vec::new();
        let mut folded = BTreeSet::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let writer = entry.file_name().to_string_lossy().into_owned();
            if valid_writer(&writer).is_err() {
                continue;
            }
            if !folded.insert(writer.to_ascii_uppercase()) {
                return Err(Error::Storage(format!(
                    "writer IDs differ only by case under refs/: {writer}"
                )));
            }
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
            if name.len() != 20 || !name.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if !entry.file_type()?.is_file() {
                return Err(Error::Storage(format!(
                    "ref is not a regular file for writer {writer}: {name}"
                )));
            }
            let sequence = name
                .parse::<u64>()
                .map_err(|e| Error::Storage(format!("invalid ref sequence: {e}")))?;
            refs.push((sequence, fs::read(entry.path())?));
        }
        refs.sort_by_key(|(sequence, _)| *sequence);
        let last = refs.len().saturating_sub(1);
        let mut complete = Vec::with_capacity(refs.len());
        for (index, (sequence, bytes)) in refs.into_iter().enumerate() {
            match decode_ref(&bytes) {
                Ok(Some(bytes)) => complete.push((sequence, bytes)),
                Ok(None) | Err(_) if index == last => {}
                Ok(None) => {
                    return Err(Error::Storage(format!(
                        "incomplete non-tip ref for writer {writer}/{sequence}"
                    )))
                }
                Err(error) => return Err(error),
            }
        }
        Ok(complete)
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

fn probe(root: &Path) -> Result<RemoteCapabilities> {
    fs::create_dir_all(root)?;
    let parent = root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let dir = root.join(format!(".transfs-probe-{}", hex::encode(nonce)));
    fs::create_dir(&dir)?;
    let result = (|| -> Result<RemoteCapabilities> {
        let source = dir.join("source");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&source)?;
        file.write_all(b"probe")?;
        file.sync_all()?;
        drop(file);
        let exclusive_create = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&source)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::AlreadyExists);
        let hard_links = fs::hard_link(&source, dir.join("link")).is_ok();
        let replacement = dir.join("replacement");
        fs::write(&replacement, b"old")?;
        let replace_existing = fs::rename(&source, &replacement).is_ok()
            && fs::read(&replacement).is_ok_and(|bytes| bytes == b"probe");
        let directory_sync =
            sync_dir(&dir).is_ok() && sync_dir(root).is_ok() && sync_dir(parent).is_ok();
        Ok(RemoteCapabilities {
            hard_links,
            exclusive_create,
            replace_existing,
            directory_sync,
        })
    })();
    let cleanup = fs::remove_dir_all(&dir);
    match result {
        Ok(mut capabilities) => {
            cleanup?;
            capabilities.directory_sync &= sync_dir(root).is_ok();
            Ok(capabilities)
        }
        Err(error) => Err(error),
    }
}

/// Content-addressed objects can replace an existing key with identical,
/// verified bytes. This also repairs a damaged object without a reservation.
fn write_content(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().expect("remote object path has a parent");
    ensure_dir(dir)?;
    match fs::read(path) {
        Ok(existing) if existing == bytes => {
            sync_dir(dir)?;
            return Ok(());
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut nonce = [0_u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let temp = dir.join(format!(
        ".tmp.{}.{}",
        std::process::id(),
        hex::encode(nonce)
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes).map_err(|error| {
            if error.kind() == std::io::ErrorKind::FileTooLarge {
                Error::Storage(
                    "object exceeds this filesystem's file-size limit (FAT32 limits files to 4 GiB)"
                        .into(),
                )
            } else {
                error.into()
            }
        })?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        sync_dir(dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A hard link publishes synced ref bytes. Filesystems without links reserve
/// the ref name with O_EXCL and replace the reservation after syncing data.
fn write_once_ref(path: &Path, bytes: &[u8], capabilities: RemoteCapabilities) -> Result<bool> {
    let dir = path.parent().expect("remote object path has a parent");
    ensure_dir(dir)?;
    if !capabilities.hard_links {
        let framed = encode_ref(bytes)?;
        let stored = framed.as_slice();
        let reservation_bytes = encode_reservation(bytes);
        return match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut reservation) => {
                reservation.write_all(&reservation_bytes)?;
                reservation.sync_all()?;
                sync_dir(dir)?;
                write_content(path, stored)?;
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = fs::read(path)?;
                if existing == stored {
                    sync_dir(dir)?;
                    Ok(true)
                } else if existing == reservation_bytes {
                    write_content(path, stored)?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(e) => Err(e.into()),
        };
    }
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
            // NFS may report a lost link reply as EEXIST even though our link
            // succeeded. Equal bytes are enough to accept that outcome.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(fs::read(path)? == bytes),
            Err(e) => Err(e.into()),
        }
    })();
    let _ = fs::remove_file(&temp);
    if result.is_ok() {
        sync_dir(dir)?;
    }
    result
}

fn encode_reservation(bytes: &[u8]) -> Vec<u8> {
    let mut marker = Vec::with_capacity(PENDING_PREFIX.len() + 64);
    marker.extend_from_slice(PENDING_PREFIX);
    marker.extend_from_slice(hash_bytes(bytes).as_bytes());
    marker
}

fn encode_ref(bytes: &[u8]) -> Result<Vec<u8>> {
    let length =
        u64::try_from(bytes.len()).map_err(|_| Error::Storage("writer ref is too large".into()))?;
    let mut framed = Vec::with_capacity(REF_PREFIX.len() + 8 + bytes.len() + 32);
    framed.extend_from_slice(REF_PREFIX);
    framed.extend_from_slice(&length.to_le_bytes());
    framed.extend_from_slice(bytes);
    framed.extend_from_slice(&Sha256::digest(bytes));
    Ok(framed)
}

fn decode_ref(bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    if bytes.is_empty()
        || (bytes.len() < REF_PREFIX.len() && REF_PREFIX.starts_with(bytes))
        || (bytes.len() < PENDING_PREFIX.len() && PENDING_PREFIX.starts_with(bytes))
        || bytes.starts_with(PENDING_PREFIX)
    {
        return Ok(None);
    }
    if !bytes.starts_with(REF_PREFIX) {
        if bytes.first() == Some(&0) {
            return Err(Error::Storage("invalid framed writer ref".into()));
        }
        return Ok(Some(bytes.to_vec())); // legacy plain JSON ref
    }
    let header = REF_PREFIX.len() + 8;
    if bytes.len() < header {
        return Ok(None);
    }
    let length = u64::from_le_bytes(bytes[REF_PREFIX.len()..header].try_into().unwrap());
    let length =
        usize::try_from(length).map_err(|_| Error::Storage("writer ref length overflow".into()))?;
    let end = header
        .checked_add(length)
        .and_then(|end| end.checked_add(32))
        .ok_or_else(|| Error::Storage("writer ref length overflow".into()))?;
    if bytes.len() < end {
        return Ok(None);
    }
    if bytes.len() != end
        || Sha256::digest(&bytes[header..header + length])[..] != bytes[header + length..]
    {
        return Err(Error::Storage(
            "writer ref checksum or length mismatch".into(),
        ));
    }
    Ok(Some(bytes[header..header + length].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    use tempfile::TempDir;

    #[test]
    fn exclusive_create_refs_hide_an_interrupted_tip() {
        let temp = TempDir::new().unwrap();
        let remote = DirectoryRemote::new(temp.path());
        let mode = RemoteCapabilities {
            hard_links: false,
            exclusive_create: true,
            replace_existing: true,
            directory_sync: true,
        };
        let first = remote.ref_path("writer", 1).unwrap();
        assert!(write_once_ref(&first, b"first", mode).unwrap());
        assert_eq!(
            remote.refs_for("writer").unwrap(),
            vec![(1, b"first".to_vec())]
        );
        let second = remote.ref_path("writer", 2).unwrap();
        fs::write(&second, []).unwrap(); // reservation before the synced rename
        assert_eq!(
            remote.refs_for("writer").unwrap(),
            vec![(1, b"first".to_vec())]
        );
        fs::write(&second, &encode_ref(b"second").unwrap()[..12]).unwrap();
        assert_eq!(
            remote.refs_for("writer").unwrap(),
            vec![(1, b"first".to_vec())]
        );
        assert!(!write_once_ref(&second, b"second", mode).unwrap());
        let third = remote.ref_path("writer", 3).unwrap();
        assert!(write_once_ref(&third, b"third", mode).unwrap());
        assert!(remote.refs_for("writer").is_err());
    }

    #[test]
    fn exclusive_create_reservation_has_one_winner() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("ref");
        let barrier = Arc::new(Barrier::new(2));
        let mode = RemoteCapabilities {
            hard_links: false,
            exclusive_create: true,
            replace_existing: true,
            directory_sync: true,
        };
        let handles: Vec<_> = (0..2)
            .map(|index| {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                thread::spawn(move || {
                    barrier.wait();
                    write_once_ref(&path, format!("candidate {index}").as_bytes(), mode)
                })
            })
            .collect();
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect();
        assert_eq!(outcomes.into_iter().filter(|won| *won).count(), 1);
        let value = decode_ref(&fs::read(path).unwrap()).unwrap().unwrap();
        assert!(value == b"candidate 0" || value == b"candidate 1");
    }

    #[test]
    fn exclusive_create_ref_checks_its_complete_frame() {
        let temp = TempDir::new().unwrap();
        let remote = DirectoryRemote::new(temp.path());
        let mode = RemoteCapabilities {
            hard_links: false,
            exclusive_create: true,
            replace_existing: true,
            directory_sync: true,
        };
        let path = remote.ref_path("writer", 1).unwrap();
        assert!(write_once_ref(&path, b"value", mode).unwrap());
        assert!(write_once_ref(&path, b"value", mode).unwrap());
        assert!(!write_once_ref(&path, b"other", mode).unwrap());
        let mut bytes = fs::read(&path).unwrap();
        bytes[REF_PREFIX.len() + 8] ^= 1;
        fs::write(path, bytes).unwrap();
        assert!(remote.refs_for("writer").unwrap().is_empty());
        let next = remote.ref_path("writer", 2).unwrap();
        assert!(write_once_ref(&next, b"next", mode).unwrap());
        assert!(remote.refs_for("writer").is_err());
    }

    #[test]
    fn pending_reservation_can_be_completed_by_its_exact_ref() {
        let temp = TempDir::new().unwrap();
        let remote = DirectoryRemote::new(temp.path());
        let mode = RemoteCapabilities {
            hard_links: false,
            exclusive_create: true,
            replace_existing: true,
            directory_sync: true,
        };
        let path = remote.ref_path("writer", 1).unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, encode_reservation(b"value")).unwrap();
        assert!(remote.refs_for("writer").unwrap().is_empty());
        assert!(!write_once_ref(&path, b"other", mode).unwrap());
        assert!(write_once_ref(&path, b"value", mode).unwrap());
        assert_eq!(remote.refs_for("writer").unwrap()[0].1, b"value");
    }

    #[test]
    fn unrelated_os_files_do_not_break_ref_listing() {
        let temp = TempDir::new().unwrap();
        let remote = DirectoryRemote::new(temp.path());
        fs::create_dir_all(temp.path().join("refs/writer")).unwrap();
        fs::write(temp.path().join("refs/.DS_Store"), b"metadata").unwrap();
        fs::write(temp.path().join("refs/writer/Thumbs.db"), b"metadata").unwrap();
        fs::write(temp.path().join("refs/writer/.DS_Store"), b"metadata").unwrap();
        assert_eq!(remote.writers().unwrap(), vec!["writer"]);
        assert!(remote.refs_for("writer").unwrap().is_empty());
    }

    #[test]
    fn case_variant_legacy_writer_directories_are_rejected() {
        let temp = TempDir::new().unwrap();
        let remote = DirectoryRemote::new(temp.path());
        fs::create_dir_all(temp.path().join("refs/writer")).unwrap();
        if fs::create_dir(temp.path().join("refs/WRITER")).is_ok() {
            assert!(remote.writers().is_err());
        }
    }
}
