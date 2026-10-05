//! Durable identity and last published ref for one private working store.
use crate::{
    cas::sync_dir,
    remote::{hash_bytes, valid_hash},
    Error, Result,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriterState {
    format: u8,
    pub id: String,
    pub label: Option<String>,
    pub last_ref: Option<String>,
    #[serde(default)]
    pub pending: Option<PendingRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingRef {
    pub sequence: u64,
    pub hash: String,
    pub json: String,
}

impl WriterState {
    pub fn fork(&mut self) -> Result<()> {
        self.id = mint_id()?;
        self.last_ref = None;
        self.pending = None;
        Ok(())
    }

    pub fn load_or_create(root: &Path) -> Result<Self> {
        let path = root.join(".transfs/writer.json");
        match fs::read(&path) {
            Ok(bytes) => {
                let state: Self = serde_json::from_slice(&bytes)
                    .map_err(|e| Error::Storage(format!("invalid writer state: {e}")))?;
                if state.format != 1 || !valid_ulid(&state.id) {
                    return Err(Error::Storage("invalid writer state identity".into()));
                }
                if let Some(ref hash) = state.last_ref {
                    valid_hash(hash)?;
                }
                if let Some(ref pending) = state.pending {
                    valid_hash(&pending.hash)?;
                    if pending.sequence == 0 || hash_bytes(pending.json.as_bytes()) != pending.hash
                    {
                        return Err(Error::Storage("invalid pending writer ref".into()));
                    }
                }
                Ok(state)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let state = Self {
                    format: 1,
                    id: mint_id()?,
                    label: None,
                    last_ref: None,
                    pending: None,
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

fn valid_ulid(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'A'..=b'H' | b'J'..=b'K' | b'M'..=b'N' | b'P'..=b'T' | b'V'..=b'Z'))
        && id.as_bytes()[0] <= b'7'
}

fn mint_id() -> Result<String> {
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
