use crate::{claim::Claim, Error, Result};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

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
        let path = self.path();
        fs::create_dir_all(path.parent().expect("log path has parent"))?;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        for claim in claims {
            writeln!(file, "{}", claim.to_json_line())?;
        }
        file.flush()?;
        file.sync_all()?;
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
        let mut lines: Vec<_> = bytes.split(|b| *b == b'\n').collect();
        if bytes.last() == Some(&b'\n') {
            lines.pop();
        }
        let mut claims = vec![];
        let mut torn_tail = None;
        for (index, line) in lines.iter().enumerate() {
            let parsed = std::str::from_utf8(line)
                .map_err(|e| Error::InvalidClaim(e.to_string()))
                .and_then(Claim::parse);
            match parsed {
                Ok(Some(claim)) => claims.push(claim),
                Ok(None) => {}
                Err(e) if index + 1 == lines.len() => {
                    torn_tail = Some(TornTail {
                        path: path.clone(),
                        line: index + 1,
                        reason: e.to_string(),
                    })
                }
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
