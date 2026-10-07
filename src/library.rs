use crate::{
    cas::Cas,
    causal::mint_nonce,
    claim::Claim,
    config::StoreConfig,
    content::Content,
    document::{normalize_tag, Document},
    log::Log,
    Error, Result,
};
use chrono::{DateTime, Utc};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
    pub cas: Cas,
    /// Versions' bytes, as whole blobs or chunks.
    pub content: Content,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            cas: Cas::new(&root),
            content: Content::new(&root),
            root,
        }
    }

    /// Stores a file's bytes as the store's settings say, returning its hash.
    fn put_file(&self, path: &Path) -> Result<String> {
        let chunking = StoreConfig::load(&self.root)?.chunking;
        self.content.put_file(path, chunking)
    }

    fn append_and_load(&self, id: &str, claims: &[Claim]) -> Result<Document> {
        Log::new(&self.root, id).append(claims)?;
        Document::load(&self.root, id)
    }

    pub fn add(&self, path: &Path, name: Option<&str>) -> Result<Document> {
        self.add_at(path, name, Utc::now())
    }

    pub fn add_at(&self, path: &Path, name: Option<&str>, ts: DateTime<Utc>) -> Result<Document> {
        let hash = self.put_file(path)?;
        let create = Claim::mint(ts);
        let id = create.doc_id().expect("mint returns create");
        let label = name.map(str::to_owned).unwrap_or_else(|| {
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        self.append_and_load(
            &id,
            &[
                create,
                Claim::Version {
                    doc: id.clone(),
                    nonce: mint_nonce(),
                    ts,
                    hash,
                    parents: vec![],
                },
                Claim::Name {
                    doc: id.clone(),
                    nonce: mint_nonce(),
                    ts,
                    name: label,
                    supersedes: vec![],
                },
            ],
        )
    }

    pub fn add_version(&self, doc: &Document, path: &Path) -> Result<Document> {
        self.add_version_at(doc, path, Utc::now())
    }

    pub fn add_version_at(
        &self,
        doc: &Document,
        path: &Path,
        ts: DateTime<Utc>,
    ) -> Result<Document> {
        self.add_version_at_with_id(doc, path, ts)
            .map(|(document, _)| document)
    }

    pub fn add_version_with_id(&self, doc: &Document, path: &Path) -> Result<(Document, String)> {
        self.add_version_at_with_id(doc, path, Utc::now())
    }

    pub fn add_version_at_with_id(
        &self,
        doc: &Document,
        path: &Path,
        ts: DateTime<Utc>,
    ) -> Result<(Document, String)> {
        let base = doc.head_id().ok_or_else(|| Error::AmbiguousHead {
            id: doc.id.clone(),
            heads: doc.heads.iter().map(|head| head.id.clone()).collect(),
        })?;
        self.add_version_from_at_with_id(doc, &[base.to_owned()], path, ts)
    }

    /// Explicit parents allow a conflict resolution version to join all heads.
    /// Passing a stale snapshot is safe: its old parent creates a visible fork.
    pub fn add_version_from_at(
        &self,
        doc: &Document,
        parents: &[String],
        path: &Path,
        ts: DateTime<Utc>,
    ) -> Result<Document> {
        self.add_version_from_at_with_id(doc, parents, path, ts)
            .map(|(document, _)| document)
    }

    pub fn add_version_from_at_with_id(
        &self,
        doc: &Document,
        parents: &[String],
        path: &Path,
        ts: DateTime<Utc>,
    ) -> Result<(Document, String)> {
        if parents.is_empty() {
            return Err(Error::InvalidClaim(
                "version needs an observed parent".into(),
            ));
        }
        let mut resolved = Vec::new();
        for prefix in parents {
            let matches: Vec<_> = doc
                .versions
                .iter()
                .filter(|version| version.id.starts_with(prefix))
                .collect();
            if matches.len() != 1 {
                return Err(Error::InvalidClaim(format!(
                    "parent prefix {prefix} matches {} observed versions",
                    matches.len()
                )));
            }
            resolved.push(matches[0].id.clone());
        }
        let hash = self.put_file(path)?;
        let claim = Claim::Version {
            doc: doc.id.clone(),
            nonce: mint_nonce(),
            ts,
            hash,
            parents: resolved,
        };
        let id = claim.id()?;
        let document = self.append_and_load(&doc.id, &[claim])?;
        Ok((document, id))
    }

    pub fn rename(&self, doc: &Document, name: &str) -> Result<Document> {
        self.rename_at(doc, name, Utc::now())
    }

    pub fn rename_at(&self, doc: &Document, name: &str, ts: DateTime<Utc>) -> Result<Document> {
        self.append_and_load(
            &doc.id,
            &[Claim::Name {
                doc: doc.id.clone(),
                nonce: mint_nonce(),
                ts,
                name: name.into(),
                supersedes: doc.names.iter().map(|value| value.id.clone()).collect(),
            }],
        )
    }

    pub fn tag(&self, doc: &Document, add: &[String], del: &[String]) -> Result<Document> {
        self.tag_at(doc, add, del, Utc::now())
    }

    pub fn tag_at(
        &self,
        doc: &Document,
        add: &[String],
        del: &[String],
        ts: DateTime<Utc>,
    ) -> Result<Document> {
        let mut claims = Vec::new();
        for path in normalized_paths(del) {
            let removes: Vec<_> = doc
                .tag_assertions
                .iter()
                .filter(|tag| tag.value == path)
                .map(|tag| tag.id.clone())
                .collect();
            if !removes.is_empty() {
                claims.push(Claim::TagRemove {
                    doc: doc.id.clone(),
                    nonce: mint_nonce(),
                    ts,
                    tag: path,
                    removes,
                });
            }
        }
        for path in normalized_paths(add) {
            let supersedes: Vec<_> = doc
                .tag_assertions
                .iter()
                .filter(|tag| tag.value == path || is_descendant(&tag.value, &path))
                .map(|tag| tag.id.clone())
                .collect();
            claims.push(Claim::TagAdd {
                doc: doc.id.clone(),
                nonce: mint_nonce(),
                ts,
                tag: path,
                scope: None,
                supersedes,
            });
        }
        if claims.is_empty() {
            return Ok(doc.clone());
        }
        self.append_and_load(&doc.id, &claims)
    }

    pub fn set_tag(&self, doc: &Document, key: &str, value: &str) -> Result<Document> {
        self.set_tag_at(doc, key, value, Utc::now())
    }

    pub fn set_tag_at(
        &self,
        doc: &Document,
        key: &str,
        value: &str,
        ts: DateTime<Utc>,
    ) -> Result<Document> {
        let key = normalize_tag(key);
        let value = normalize_tag(value);
        if key.is_empty() || value.is_empty() {
            return Err(Error::InvalidClaim(
                "set needs a nonempty key and value".into(),
            ));
        }
        let tag = format!("{key}/{value}");
        let supersedes = doc
            .tag_assertions
            .iter()
            .filter(|assertion| assertion.value == key || is_descendant(&key, &assertion.value))
            .map(|assertion| assertion.id.clone())
            .collect();
        self.append_and_load(
            &doc.id,
            &[Claim::TagAdd {
                doc: doc.id.clone(),
                nonce: mint_nonce(),
                ts,
                tag,
                scope: Some(key),
                supersedes,
            }],
        )
    }

    pub fn read(&self, doc: &Document) -> Result<Option<Vec<u8>>> {
        match doc.head() {
            Some(hash) => self.content.read(hash),
            None if doc.heads.is_empty() => Ok(None),
            None => Err(Error::AmbiguousHead {
                id: doc.id.clone(),
                heads: doc.heads.iter().map(|head| head.id.clone()).collect(),
            }),
        }
    }

    pub fn read_version(&self, doc: &Document, version_id: &str) -> Result<Option<Vec<u8>>> {
        let matches: Vec<_> = doc
            .versions
            .iter()
            .filter(|version| version.id.starts_with(version_id))
            .collect();
        if matches.len() != 1 {
            return Err(Error::InvalidClaim(format!(
                "version prefix {version_id} matches {} versions",
                matches.len()
            )));
        }
        self.content.read(&matches[0].hash)
    }

    pub fn documents(&self) -> Result<Vec<Document>> {
        Document::all(&self.root)
    }

    pub fn document(&self, prefix: &str) -> Result<Option<Document>> {
        if Log::new(&self.root, prefix).exists() {
            return Document::load(&self.root, prefix).map(Some);
        }
        let ids: Vec<_> = Log::all_ids(&self.root)?
            .into_iter()
            .filter(|id| id.starts_with(prefix))
            .collect();
        match ids.len() {
            0 => Ok(None),
            1 => Document::load(&self.root, &ids[0]).map(Some),
            n => Err(Error::AmbiguousId {
                prefix: prefix.into(),
                matches: n,
            }),
        }
    }
}

fn normalized_paths(paths: &[String]) -> BTreeSet<String> {
    paths
        .iter()
        .map(|path| normalize_tag(path))
        .filter(|path| !path.is_empty())
        .collect()
}

fn is_descendant(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|suffix| suffix.starts_with('/'))
}
