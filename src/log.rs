use crate::{cas::sync_dir, claim::Claim, Error, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// Serializes local claim appends and checkpoint capture across processes.
pub(crate) struct StoreLock {
    _file: fs::File,
}

impl StoreLock {
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        let dir = root.join(".transfs");
        fs::create_dir_all(&dir)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))?;
        file.lock()?;
        crate::writer::WriterState::load_or_create(root)?;
        Ok(Self { _file: file })
    }
}

#[derive(Clone, Debug)]
pub struct TornTail {
    pub path: PathBuf,
    pub line: usize,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct ReadResult {
    pub claims: Vec<Claim>,
    pub torn_tail: Option<TornTail>,
}

#[derive(Clone, Debug)]
pub struct Log {
    root: PathBuf,
    pub id: String,
}

impl Log {
    pub fn new(root: impl Into<PathBuf>, id: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            id: id.into(),
        }
    }
    pub fn docs_dir(root: &Path) -> PathBuf {
        root.join(".transfs/docs")
    }
    pub fn path(&self) -> PathBuf {
        Self::docs_dir(&self.root)
            .join(self.id.get(..2).unwrap_or(&self.id))
            .join(format!("{}.log", self.id))
    }
    pub fn exists(&self) -> bool {
        self.path().exists()
    }
    pub fn append(&self, claims: &[Claim]) -> Result<()> {
        let _lock = StoreLock::acquire(&self.root)?;
        let path = self.path();
        let mut combined = if path.exists() {
            let existing = self.read()?;
            if existing.torn_tail.is_some() {
                return Err(Error::InvalidClaim(
                    "repair torn log tail before appending".into(),
                ));
            }
            if claims
                .iter()
                .any(|claim| matches!(claim, Claim::Create { .. }))
            {
                return Err(Error::InvalidClaim("duplicate create claim".into()));
            }
            existing.claims
        } else if !matches!(claims.first(), Some(Claim::Create { .. })) {
            return Err(Error::InvalidClaim(
                "new log must start with a v2 create claim".into(),
            ));
        } else {
            Vec::new()
        };
        combined.extend_from_slice(claims);
        crate::document::Document::fold(&self.id, &combined)?;
        let mut bytes = Vec::new();
        for claim in claims {
            bytes.extend_from_slice(claim.to_json_line()?.as_bytes());
            bytes.push(b'\n');
        }
        let new_log = !path.exists();
        let dir = path.parent().expect("log path has parent");
        fs::create_dir_all(dir)?;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        if new_log {
            sync_dir(&self.root)?;
            sync_dir(&self.root.join(".transfs"))?;
            sync_dir(&Self::docs_dir(&self.root))?;
            sync_dir(dir)?;
        }
        Ok(())
    }
    pub fn read(&self) -> Result<ReadResult> {
        let path = self.path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReadResult {
                    claims: vec![],
                    torn_tail: None,
                })
            }
            Err(e) => return Err(e.into()),
        };
        if bytes.is_empty() {
            return Ok(ReadResult {
                claims: vec![],
                torn_tail: None,
            });
        }
        let unterminated = bytes.last() != Some(&b'\n');
        let mut lines: Vec<_> = bytes.split(|b| *b == b'\n').collect();
        if !unterminated {
            lines.pop();
        }
        let mut claims = vec![];
        let mut torn_tail = None;
        for (index, line) in lines.iter().enumerate() {
            let parsed = std::str::from_utf8(line)
                .map_err(|e| Error::InvalidClaim(e.to_string()))
                .and_then(|line| {
                    if line.trim().is_empty() {
                        Ok(None)
                    } else {
                        Claim::from_json_line(line).map(Some)
                    }
                });
            if unterminated && index + 1 == lines.len() {
                let reason = parsed
                    .err()
                    .map_or_else(|| "missing terminating newline".into(), |e| e.to_string());
                if index == 0 {
                    return Err(Error::CorruptLog {
                        path: path.clone(),
                        line: 1,
                        reason,
                    });
                }
                torn_tail = Some(TornTail {
                    path: path.clone(),
                    line: index + 1,
                    reason,
                });
                continue;
            }
            match parsed {
                Ok(Some(claim)) => claims.push(claim),
                Ok(None) => {}
                Err(e) => {
                    return Err(Error::CorruptLog {
                        path: path.clone(),
                        line: index + 1,
                        reason: e.to_string(),
                    })
                }
            }
        }
        Ok(ReadResult { claims, torn_tail })
    }
    pub fn all_ids(root: &Path) -> Result<Vec<String>> {
        let docs = Self::docs_dir(root);
        if !docs.exists() {
            return Ok(vec![]);
        }
        let mut ids = vec![];
        for fanout in fs::read_dir(docs)? {
            let fanout = fanout?;
            if !fanout.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(fanout.path())? {
                let entry = entry?;
                if entry.path().extension().is_some_and(|e| e == "log") {
                    if let Some(stem) = entry.path().file_stem() {
                        ids.push(stem.to_string_lossy().into_owned());
                    }
                }
            }
        }
        ids.sort();
        Ok(ids)
    }
}
