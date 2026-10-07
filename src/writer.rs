//! Durable identity and per-remote publish state for one private working store.
use crate::{
    cas::sync_dir,
    remote::{hash_bytes, valid_hash},
    Error, Result,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

/// A working store's writer identity and, for each remote it publishes to,
/// the last ref it published there and any ref it was about to publish.
/// Remotes are keyed by the id each remote keeps in its own `remote.json`, so
/// a remote keeps its identity wherever it is mounted.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterState {
    format: u8,
    pub id: String,
    pub label: Option<String>,
    #[serde(default)]
    pub remotes: BTreeMap<String, RemoteState>,
    /// Publish state from before it was kept per remote: assigned to the
    /// first remote whose tip proves it belongs there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    unassigned: Option<RemoteState>,
}

/// What one remote has from this writer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RemoteState {
    pub last_ref: Option<String>,
    #[serde(default)]
    pub pending: Option<PendingRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingRef {
    pub sequence: u64,
    pub hash: String,
    pub json: String,
}

/// writer.json before publish state was kept per remote.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Format1 {
    #[allow(dead_code)]
    format: u8,
    id: String,
    label: Option<String>,
    last_ref: Option<String>,
    #[serde(default)]
    pending: Option<PendingRef>,
}

impl RemoteState {
    fn validate(&self) -> Result<()> {
        if let Some(ref hash) = self.last_ref {
            valid_hash(hash)?;
        }
        if let Some(ref pending) = self.pending {
            valid_hash(&pending.hash)?;
            if pending.sequence == 0 || hash_bytes(pending.json.as_bytes()) != pending.hash {
                return Err(Error::Storage("invalid pending writer ref".into()));
            }
        }
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.last_ref.is_none() && self.pending.is_none()
    }
}

impl WriterState {
    pub fn fork(&mut self) -> Result<()> {
        self.id = mint_id()?;
        self.remotes.clear();
        self.unassigned = None;
        Ok(())
    }

    /// This writer's state on a remote: empty for a remote it has never
    /// published to. Old, unassigned state moves to the remote whose tip is
    /// its last or pending ref, which proves where it was published.
    pub fn remote(&mut self, remote_id: &str, tip: Option<&str>) -> RemoteState {
        if !self.remotes.contains_key(remote_id) {
            if let Some(old) = self.unassigned.clone() {
                let proven = tip.is_some()
                    && (tip == old.last_ref.as_deref()
                        || tip == old.pending.as_ref().map(|p| p.hash.as_str()));
                if proven {
                    self.unassigned = None;
                    self.remotes.insert(remote_id.to_owned(), old);
                }
            }
        }
        self.remotes.get(remote_id).cloned().unwrap_or_default()
    }

    pub fn set_remote(&mut self, remote_id: &str, state: RemoteState) {
        self.remotes.insert(remote_id.to_owned(), state);
    }

    pub fn load_or_create(root: &Path) -> Result<Self> {
        let path = root.join(".transfs/writer.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let invalid =
                    |e: serde_json::Error| Error::Storage(format!("invalid writer state: {e}"));
                let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(invalid)?;
                let state = match value.get("format").and_then(|f| f.as_u64()) {
                    Some(1) => {
                        let old: Format1 = serde_json::from_value(value).map_err(invalid)?;
                        let unassigned = RemoteState {
                            last_ref: old.last_ref,
                            pending: old.pending,
                        };
                        Self {
                            format: 2,
                            id: old.id,
                            label: old.label,
                            remotes: BTreeMap::new(),
                            unassigned: (!unassigned.is_empty()).then_some(unassigned),
                        }
                    }
                    Some(2) => serde_json::from_value(value).map_err(invalid)?,
                    _ => return Err(Error::Storage("unsupported writer state format".into())),
                };
                if !valid_ulid(&state.id) {
                    return Err(Error::Storage("invalid writer state identity".into()));
                }
                for (remote, remote_state) in &state.remotes {
                    if !valid_ulid(remote) {
                        return Err(Error::Storage("invalid remote id in writer state".into()));
                    }
                    remote_state.validate()?;
                }
                if let Some(ref old) = state.unassigned {
                    old.validate()?;
                }
                Ok(state)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let state = Self {
                    format: 2,
                    id: mint_id()?,
                    label: None,
                    remotes: BTreeMap::new(),
                    unassigned: None,
                };
                state.save(root)?;
                Ok(state)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let dir = root.join(".transfs");
        fs::create_dir_all(&dir)?;
        let mut random = [0_u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let tmp = dir.join(format!(".writer.{}.tmp", hex::encode(random)));
        let path = dir.join("writer.json");
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            file.write_all(
                &serde_json::to_vec(self)
                    .map_err(|e| Error::Storage(format!("cannot encode writer state: {e}")))?,
            )?;
            file.sync_all()?;
            fs::rename(&tmp, &path)?;
            sync_dir(&dir)?;
            sync_dir(root)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

pub(crate) fn valid_ulid(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'K' | b'M'..=b'N' | b'P'..=b'T' | b'V'..=b'Z'))
        && id.as_bytes()[0] <= b'7'
}

pub(crate) fn mint_id() -> Result<String> {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Storage(format!("clock predates Unix epoch: {e}")))?
        .as_millis();
    if ms >= (1_u128 << 48) {
        return Err(Error::Storage("writer timestamp overflow".into()));
    }
    let mut random = [0_u8; 10];
    rand::rngs::OsRng.fill_bytes(&mut random);
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&(ms as u64).to_be_bytes()[2..]);
    bytes[6..].copy_from_slice(&random);
    let mut value = u128::from_be_bytes(bytes);
    let mut id = [0_u8; 26];
    for digit in id.iter_mut().rev() {
        *digit = ALPHABET[(value & 31) as usize];
        value >>= 5;
    }
    Ok(String::from_utf8(id.to_vec()).expect("Crockford alphabet is ASCII"))
}
