//! Representation records: how the content with a given hash is stored, when
//! it is not a whole blob (docs/chunked-storage.md). A record lives at
//! `.transfs/reps/<hh>/<content hash>/<record id>.json` and never changes; its
//! id is the SHA-256 of its bytes.
use crate::{cas::sync_dir, remote::valid_hash, Error, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rep {
    pub format: u8,
    /// SHA-256 of the complete content, as version claims record it.
    pub content: String,
    /// The content's length in bytes.
    pub length: u64,
    /// How it was cut: `fastcdc-2020/<min>/<avg>/<max>` or `fixed/<page>`.
    pub chunking: String,
    /// The chunk list's identity: the value `Sequence::save` returns.
    pub root: String,
    /// Every pack holding the list's objects or its chunks.
    pub packs: Vec<String>,
}

pub fn reps_dir(root: &Path) -> PathBuf {
    root.join(".transfs/reps")
}

fn dir(root: &Path, content: &str) -> PathBuf {
    reps_dir(root)
        .join(content.get(..2).unwrap_or(content))
        .join(content)
}

/// Writes a record durably, returning its id. Writing the same record twice
/// is harmless.
pub fn write(root: &Path, rep: &Rep) -> Result<String> {
    valid_hash(&rep.content)?;
    let bytes = serde_json::to_vec(rep)
        .map_err(|e| Error::Storage(format!("cannot encode representation: {e}")))?;
    let id = hex::encode(Sha256::digest(&bytes));
    let dir = dir(root, &rep.content);
    let path = dir.join(format!("{id}.json"));
    if path.exists() {
        return Ok(id);
    }
    fs::create_dir_all(&dir)?;
    sync_dir(dir.parent().expect("fan-out has a parent"))?;
    sync_dir(&reps_dir(root))?;
    let mut random = [0_u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let tmp = dir.join(format!(".{id}.{}.tmp", hex::encode(random)));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        sync_dir(&dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(id)
}

/// Every record for this content, each checked against its id and contents.
pub fn read(root: &Path, content: &str) -> Result<Vec<Rep>> {
    let entries = match fs::read_dir(dir(root, content)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "json")
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with('.'))
        })
        .collect();
    paths.sort();
    let mut reps = Vec::new();
    for path in paths {
        let bytes = fs::read(&path)?;
        let id = path.file_stem().unwrap_or_default().to_string_lossy();
        let damaged =
            |why: &str| Error::Storage(format!("damaged representation {}: {why}", path.display()));
        if hex::encode(Sha256::digest(&bytes)) != id {
            return Err(damaged("its bytes do not match its id"));
        }
        let rep: Rep = serde_json::from_slice(&bytes).map_err(|e| damaged(&e.to_string()))?;
        if rep.format != 1 {
            return Err(damaged("unsupported format"));
        }
        if rep.content != content {
            return Err(damaged("it describes other content"));
        }
        valid_hash(&rep.root).map_err(|_| damaged("invalid root"))?;
        for pack in &rep.packs {
            valid_hash(pack).map_err(|_| damaged("invalid pack name"))?;
        }
        reps.push(rep);
    }
    Ok(reps)
}

/// The content hashes that have records.
pub fn contents(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let top = match fs::read_dir(reps_dir(root)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for fanout in top {
        let fanout = fanout?;
        if !fanout.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(fanout.path())? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() && valid_hash(&name).is_ok() {
                out.push(name);
            }
        }
    }
    out.sort();
    Ok(out)
}
