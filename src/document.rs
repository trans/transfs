use crate::{
    causal::{CausalSet, FieldValue, TagValue, VersionValue},
    claim::Claim,
    log::Log,
    Error, Result,
};
use chrono::{DateTime, Utc};
use std::{collections::BTreeSet, path::Path};

pub type Version = VersionValue;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub id: String,
    /// Singleton name for callers that require one unambiguous label.
    pub name: Option<String>,
    pub names: Vec<FieldValue>,
    pub created_at: DateTime<Utc>,
    pub versions: Vec<Version>,
    pub heads: Vec<Version>,
    /// Deepest paths for facets and display; claim IDs stay in tag_assertions.
    pub tags: BTreeSet<String>,
    pub tag_assertions: Vec<TagValue>,
    pub tag_conflicts: BTreeSet<String>,
}

impl Document {
    pub fn head(&self) -> Option<&str> {
        (self.heads.len() == 1).then(|| self.heads[0].hash.as_str())
    }

    pub fn head_id(&self) -> Option<&str> {
        (self.heads.len() == 1).then(|| self.heads[0].id.as_str())
    }

    pub fn version_count(&self) -> usize {
        self.versions.len()
    }

    pub fn has_conflicts(&self) -> bool {
        self.names.len() > 1 || self.heads.len() > 1 || !self.tag_conflicts.is_empty()
    }

    pub fn fold(id: impl Into<String>, claims: &[Claim]) -> Result<Self> {
        let id = id.into();
        if !matches!(claims.first(), Some(Claim::Create { .. })) {
            return Err(Error::InvalidClaim("first claim must be create".into()));
        }
        let set = CausalSet::from_claims(claims.iter().cloned())?;
        let mut creates = Vec::new();
        for claim in set.claims().values() {
            if let Claim::Create { ts, .. } = claim {
                creates.push((claim.id()?, *ts));
            }
        }
        if creates.len() != 1 || creates[0].0 != id {
            return Err(Error::InvalidClaim(format!(
                "document {id} has no matching unique create claim"
            )));
        }
        let state = set.fold()?;
        let name = (state.names.len() == 1).then(|| state.names[0].value.clone());
        let tags = state.projected_tags();
        let tag_conflicts = state.tag_conflict_keys();
        Ok(Self {
            id,
            name,
            names: state.names,
            created_at: creates[0].1,
            versions: state.versions,
            heads: state.heads,
            tags,
            tag_assertions: state.tags,
            tag_conflicts,
        })
    }

    pub fn load(root: &Path, id: &str) -> Result<Self> {
        Self::fold(id, &Log::new(root, id).read()?.claims)
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
