use crate::{claim::Claim, log::Log, Result};
use chrono::{DateTime, Utc};
use std::{collections::BTreeSet, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub hash: String,
    pub parent: Option<String>,
    pub ts: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    pub name: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub versions: Vec<Version>,
    pub tags: BTreeSet<String>,
}

impl Document {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: None,
            created_at: None,
            versions: vec![],
            tags: BTreeSet::new(),
        }
    }
    pub fn head(&self) -> Option<&str> {
        self.versions.last().map(|v| v.hash.as_str())
    }
    pub fn version_count(&self) -> usize {
        self.versions.len()
    }
    pub fn fold(id: impl Into<String>, claims: &[Claim]) -> Self {
        let mut doc = Self::new(id);
        let mut ordered: Vec<_> = claims.iter().enumerate().collect();
        ordered.sort_by_key(|(index, claim)| (claim.ts(), *index));
        for (_, claim) in ordered {
            match claim {
                Claim::Create { ts, .. } => doc.created_at = Some(*ts),
                Claim::Version { hash, parent, ts } => doc.versions.push(Version {
                    hash: hash.clone(),
                    parent: parent.clone(),
                    ts: *ts,
                }),
                Claim::Name { name, .. } => doc.name = Some(name.clone()),
                Claim::Tag { add, del, .. } => {
                    for tag in del {
                        doc.tags.remove(&normalize_tag(tag));
                    }
                    for tag in add {
                        doc.tags.insert(normalize_tag(tag));
                    }
                }
            }
        }
        doc
    }
    pub fn load(root: &Path, id: &str) -> Result<Self> {
        Ok(Self::fold(id, &Log::new(root, id).read()?.claims))
    }
    pub fn all(root: &Path) -> Result<Vec<Self>> {
        Log::all_ids(root)?
            .into_iter()
            .map(|id| Self::load(root, &id))
            .collect()
    }
}

pub fn normalize_tag(tag: &str) -> String {
    tag.replace('=', "/")
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}
