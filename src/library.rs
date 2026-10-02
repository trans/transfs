use crate::{
    cas::Cas,
    claim::Claim,
    document::{normalize_tag, Document},
    log::Log,
    Error, Result,
};
use chrono::{DateTime, Utc};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Library {
    pub root: PathBuf,
    pub cas: Cas,
}

impl Library {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            cas: Cas::new(&root),
            root,
        }
    }
    fn append_and_load(&self, id: &str, claims: &[Claim]) -> Result<Document> {
        Log::new(&self.root, id).append(claims)?;
        Document::load(&self.root, id)
    }
    pub fn add(&self, path: &Path, name: Option<&str>) -> Result<Document> {
        self.add_at(path, name, Utc::now())
    }
    pub fn add_at(&self, path: &Path, name: Option<&str>, ts: DateTime<Utc>) -> Result<Document> {
        let hash = self.cas.put(&fs::read(path)?)?;
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
                    hash,
                    parent: None,
                    ts,
                },
                Claim::Name { name: label, ts },
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
        let hash = self.cas.put(&fs::read(path)?)?;
        self.append_and_load(
            &doc.id,
            &[Claim::Version {
                hash,
                parent: doc.head().map(str::to_owned),
                ts,
            }],
        )
    }
    pub fn rename(&self, doc: &Document, name: &str) -> Result<Document> {
        self.rename_at(doc, name, Utc::now())
    }
    pub fn rename_at(&self, doc: &Document, name: &str, ts: DateTime<Utc>) -> Result<Document> {
        self.append_and_load(
            &doc.id,
            &[Claim::Name {
                name: name.into(),
                ts,
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
        let normalize = |tags: &[String]| -> Vec<String> {
            let mut values = Vec::new();
            for tag in tags {
                let path = normalize_tag(tag);
                if !path.is_empty() && !values.contains(&path) {
                    values.push(path);
                }
            }
            values
        };
        let add_paths = normalize(add);
        let del_paths = normalize(del);
        let mut current = doc.tags.clone();
        let mut effective_add = Vec::new();
        let mut effective_del = del_paths.clone();
        for tag in &del_paths {
            current.remove(tag);
        }
        for tag in add_paths {
            if current
                .iter()
                .any(|existing| existing == &tag || tag_prefix(&tag, existing))
            {
                continue;
            }
            for existing in &current {
                if tag_prefix(existing, &tag) {
                    effective_del.push(existing.clone());
                }
            }
            effective_add.push(tag.clone());
            current.insert(tag);
        }
        stable_dedup(&mut effective_del);
        stable_dedup(&mut effective_add);
        if effective_add.is_empty() && effective_del.is_empty() {
            return Ok(doc.clone());
        }
        self.append_and_load(
            &doc.id,
            &[Claim::Tag {
                add: effective_add,
                del: effective_del,
                ts,
            }],
        )
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
        let tag = format!("{key}/{value}");
        let del: Vec<_> = doc
            .tags
            .iter()
            .filter(|existing| *existing == &key || tag_prefix(&key, existing))
            .cloned()
            .collect();
        if del == [tag.clone()] {
            return Ok(doc.clone());
        }
        self.append_and_load(
            &doc.id,
            &[Claim::Tag {
                add: vec![tag],
                del,
                ts,
            }],
        )
    }
    pub fn read(&self, doc: &Document) -> Result<Option<Vec<u8>>> {
        match doc.head() {
            Some(hash) => self.cas.get(hash),
            None => Ok(None),
        }
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

fn tag_prefix(prefix: &str, tag: &str) -> bool {
    tag.starts_with(prefix) && tag.as_bytes().get(prefix.len()) == Some(&b'/')
}

fn stable_dedup(values: &mut Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
}
