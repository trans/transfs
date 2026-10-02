use crate::{cas::Cas, claim::Claim, document::Document, log::Log, Error, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Issue {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct CheckResult {
    pub documents: usize,
    pub blobs: usize,
    pub warnings: Vec<Issue>,
    pub errors: Vec<Issue>,
}
impl CheckResult {
    pub fn clean(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn check(root: &Path) -> Result<CheckResult> {
    let cas = Cas::new(root);
    let mut result = CheckResult {
        documents: 0,
        blobs: 0,
        warnings: vec![],
        errors: vec![],
    };
    let mut referenced = HashSet::new();
    let mut unreadable_logs = false;
    for id in Log::all_ids(root)? {
        let log = Log::new(root, &id);
        let path = log.path();
        match log.read() {
            Ok(read) => {
                result.documents += 1;
                if let Some(tail) = read.torn_tail {
                    result.warnings.push(Issue {
                        path: tail.path,
                        message: format!(
                            "line {}: ignored torn trailing record ({})",
                            tail.line, tail.reason
                        ),
                    });
                }
                let creates: HashSet<_> = read.claims.iter().filter_map(Claim::doc_id).collect();
                if creates.len() != 1 {
                    result.errors.push(Issue {
                        path: path.clone(),
                        message: format!(
                            "expected exactly one create claim, found {}",
                            creates.len()
                        ),
                    });
                } else if !creates.contains(&id) {
                    result.errors.push(Issue {
                        path: path.clone(),
                        message: format!(
                            "document id mismatch: create hashes to {}",
                            creates.iter().next().expect("one create ID")
                        ),
                    });
                }
                match Document::fold(&id, &read.claims) {
                    Ok(doc) if doc.has_conflicts() => {
                        let mut unresolved = Vec::new();
                        if doc.names.len() > 1 {
                            unresolved.push(format!("{} names", doc.names.len()));
                        }
                        if doc.heads.len() > 1 {
                            unresolved.push(format!("{} content heads", doc.heads.len()));
                        }
                        if !doc.tag_conflicts.is_empty() {
                            unresolved.push(format!(
                                "competing set keys {}",
                                doc.tag_conflicts.into_iter().collect::<Vec<_>>().join(", ")
                            ));
                        }
                        result.warnings.push(Issue {
                            path: path.clone(),
                            message: format!("unresolved claims: {}", unresolved.join(", ")),
                        });
                    }
                    Ok(_) => {}
                    Err(e) => result.errors.push(Issue {
                        path: path.clone(),
                        message: e.to_string(),
                    }),
                }
                for claim in &read.claims {
                    if let Claim::Version { hash, .. } = claim {
                        referenced.insert(hash.clone());
                        if !valid_hash(hash) {
                            result.errors.push(Issue {
                                path: path.clone(),
                                message: format!("version references invalid blob hash {hash:?}"),
                            });
                        } else if !cas.exists(hash) {
                            result.errors.push(Issue {
                                path: path.clone(),
                                message: format!("version references missing blob {hash}"),
                            });
                        }
                    }
                }
            }
            Err(Error::CorruptLog { line, reason, .. }) => {
                unreadable_logs = true;
                result.errors.push(Issue {
                    path,
                    message: format!("line {line}: {reason}"),
                });
            }
            Err(e) => return Err(e),
        }
    }
    let mut blobs = vec![];
    let blobs_dir = cas.blobs_dir();
    if blobs_dir.exists() {
        for fanout in fs::read_dir(blobs_dir)? {
            let fanout = fanout?;
            if !fanout.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(fanout.path())? {
                let entry = entry?;
                if entry.file_type()?.is_file() {
                    blobs.push(entry.path());
                }
            }
        }
    }
    blobs.sort();
    result.blobs = blobs.len();
    for path in blobs {
        let hash = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if hex::encode(Sha256::digest(fs::read(&path)?)) != hash {
            result.errors.push(Issue {
                path: path.clone(),
                message: "blob hash mismatch".into(),
            });
        }
        if !unreadable_logs && !referenced.contains(&hash) {
            result.warnings.push(Issue {
                path,
                message: "orphan blob is not referenced by any version claim".into(),
            });
        }
    }
    Ok(result)
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
