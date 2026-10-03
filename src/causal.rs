//! Pure v2 claim identities, set union, and causal fold.
use chrono::{DateTime, Utc};
use merkle_champ::{Identify, Sink};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

use crate::{claim::format_ts, document::normalize_tag, Error, Result};

/// The create record carries the format version. JSON is the readable log
/// representation; `Identify` bytes, not JSON bytes, determine claim IDs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum CausalClaim {
    #[serde(rename = "create")]
    Create {
        format: u8,
        nonce: String,
        ts: DateTime<Utc>,
    },
    #[serde(rename = "v2_version")]
    Version {
        doc: String,
        nonce: String,
        ts: DateTime<Utc>,
        hash: String,
        parents: Vec<String>,
    },
    #[serde(rename = "v2_name")]
    Name {
        doc: String,
        nonce: String,
        ts: DateTime<Utc>,
        name: String,
        supersedes: Vec<String>,
    },
    #[serde(rename = "v2_tag_add")]
    TagAdd {
        doc: String,
        nonce: String,
        ts: DateTime<Utc>,
        tag: String,
        /// Some(key) means `set key value`; None is an ordinary tag assertion.
        scope: Option<String>,
        supersedes: Vec<String>,
    },
    #[serde(rename = "v2_tag_remove")]
    TagRemove {
        doc: String,
        nonce: String,
        ts: DateTime<Utc>,
        tag: String,
        removes: Vec<String>,
    },
}

/// Validated claim value. Its `Identify` bytes are the bytes hashed for a
/// standalone claim ID and the bytes used when stored in a CHAMP value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalClaim(CausalClaim);

impl CanonicalClaim {
    pub fn new(claim: CausalClaim) -> Result<Self> {
        Ok(Self(claim.normalized()?))
    }

    pub fn claim(&self) -> &CausalClaim {
        &self.0
    }

    pub fn id(&self) -> String {
        let mut hasher = Sha256::new();
        self.identify(&mut hasher);
        hex::encode(hasher.finalize())
    }
}

impl Identify for CanonicalClaim {
    fn identify<S: Sink + ?Sized>(&self, hasher: &mut S) {
        // 'C' is reserved for this claim value type; all fields after it use
        // merkle-champ FORMAT.md v1 Identify encodings.
        hasher.update(b"C");
        "transfs/claim/v2".identify(hasher);
        match &self.0 {
            CausalClaim::Create { format, nonce, ts } => {
                0_u64.identify(hasher);
                (*format as u64).identify(hasher);
                identify_hex(nonce, hasher);
                format_ts(*ts).identify(hasher);
            }
            CausalClaim::Version {
                doc,
                nonce,
                ts,
                hash,
                parents,
            } => {
                1_u64.identify(hasher);
                identify_hash(doc, hasher);
                identify_hex(nonce, hasher);
                format_ts(*ts).identify(hasher);
                identify_hash(hash, hasher);
                identify_refs(parents, hasher);
            }
            CausalClaim::Name {
                doc,
                nonce,
                ts,
                name,
                supersedes,
            } => {
                2_u64.identify(hasher);
                identify_hash(doc, hasher);
                identify_hex(nonce, hasher);
                format_ts(*ts).identify(hasher);
                name.identify(hasher);
                identify_refs(supersedes, hasher);
            }
            CausalClaim::TagAdd {
                doc,
                nonce,
                ts,
                tag,
                scope,
                supersedes,
            } => {
                3_u64.identify(hasher);
                identify_hash(doc, hasher);
                identify_hex(nonce, hasher);
                format_ts(*ts).identify(hasher);
                tag.identify(hasher);
                (scope.is_some() as u64).identify(hasher);
                if let Some(key) = scope {
                    key.identify(hasher);
                }
                identify_refs(supersedes, hasher);
            }
            CausalClaim::TagRemove {
                doc,
                nonce,
                ts,
                tag,
                removes,
            } => {
                4_u64.identify(hasher);
                identify_hash(doc, hasher);
                identify_hex(nonce, hasher);
                format_ts(*ts).identify(hasher);
                tag.identify(hasher);
                identify_refs(removes, hasher);
            }
        }
    }
}

fn identify_hex<S: Sink + ?Sized>(value: &str, hasher: &mut S) {
    hex::decode(value).expect("validated hex").identify(hasher);
}

fn identify_hash<S: Sink + ?Sized>(value: &str, hasher: &mut S) {
    let bytes: [u8; 32] = hex::decode(value)
        .expect("validated hash")
        .try_into()
        .expect("validated hash length");
    bytes.identify(hasher);
}

fn identify_refs<S: Sink + ?Sized>(refs: &[String], hasher: &mut S) {
    (refs.len() as u64).identify(hasher);
    for reference in refs {
        identify_hash(reference, hasher);
    }
}

pub fn mint_nonce() -> String {
    let mut bytes = [0_u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldValue {
    pub id: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagValue {
    pub id: String,
    pub value: String,
    pub scope: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionValue {
    pub id: String,
    pub hash: String,
    pub parents: Vec<String>,
    pub ts: DateTime<Utc>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CausalState {
    pub names: Vec<FieldValue>,
    pub tags: Vec<TagValue>,
    pub versions: Vec<VersionValue>,
    pub heads: Vec<VersionValue>,
}

impl CausalState {
    /// Facet/display paths keep the deepest value on each lineage; claim
    /// frontiers in `tags` still retain every concurrent assertion and ID.
    pub fn projected_tags(&self) -> BTreeSet<String> {
        let paths: BTreeSet<_> = self.tags.iter().map(|tag| tag.value.clone()).collect();
        paths
            .iter()
            .filter(|path| !paths.iter().any(|other| is_descendant(path, other)))
            .cloned()
            .collect()
    }

    /// Surviving `set` assertions for the same key are unresolved. A set on a
    /// nested key also conflicts with an ancestor set when their values
    /// diverge, but a more specific value on the same lineage is compatible.
    pub fn tag_conflict_keys(&self) -> BTreeSet<String> {
        let mut conflicts = BTreeSet::new();
        for (index, assertion) in self.tags.iter().enumerate() {
            let Some(key) = assertion.scope.as_ref() else {
                continue;
            };
            for other in self.tags.iter().skip(index + 1) {
                let Some(other_key) = other.scope.as_ref() else {
                    continue;
                };
                if key == other_key {
                    conflicts.insert(key.clone());
                } else {
                    let ancestor = if is_descendant(key, other_key) {
                        Some(key)
                    } else if is_descendant(other_key, key) {
                        Some(other_key)
                    } else {
                        None
                    };
                    if let Some(ancestor) = ancestor {
                        let values_diverge = assertion.value != other.value
                            && !is_descendant(&assertion.value, &other.value)
                            && !is_descendant(&other.value, &assertion.value);
                        if values_diverge {
                            conflicts.insert(ancestor.clone());
                        }
                    }
                }
            }
        }
        conflicts
    }

    /// A later ordinary `tag` can deliberately add a second value after a
    /// `set`; this reports cardinality without claiming concurrency.
    pub fn set_multi_value_keys(&self) -> BTreeSet<String> {
        let paths = self.projected_tags();
        self.tags
            .iter()
            .filter_map(|assertion| {
                let key = assertion.scope.as_ref()?;
                (paths
                    .iter()
                    .filter(|path| *path == key || is_descendant(key, path))
                    .count()
                    > 1)
                .then(|| key.clone())
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CausalSet {
    claims: BTreeMap<String, CausalClaim>,
}

impl CausalClaim {
    pub fn mint(ts: DateTime<Utc>) -> Self {
        Self::Create {
            format: 2,
            nonce: mint_nonce(),
            ts,
        }
    }

    pub fn doc_id(&self) -> Option<String> {
        matches!(self, Self::Create { .. })
            .then(|| self.id().ok())
            .flatten()
    }

    pub fn from_json_line(line: &str) -> Result<Self> {
        let claim: Self = serde_json::from_str(line)
            .map_err(|e| Error::InvalidClaim(format!("invalid v2 claim: {e}")))?;
        claim.normalized()
    }

    pub fn to_json_line(&self) -> Result<String> {
        serde_json::to_string(&self.clone().normalized()?)
            .map_err(|e| Error::InvalidClaim(e.to_string()))
    }

    pub fn id(&self) -> Result<String> {
        Ok(CanonicalClaim::new(self.clone())?.id())
    }

    fn normalized(mut self) -> Result<Self> {
        match &mut self {
            Self::Create { format, nonce, .. } => {
                if *format != 2 {
                    return Err(Error::InvalidClaim("unsupported claim format".into()));
                }
                require_hex(nonce, 32, "nonce")?;
            }
            Self::Version {
                doc,
                nonce,
                hash,
                parents,
                ..
            } => {
                require_hex(doc, 64, "document ID")?;
                require_hex(nonce, 32, "nonce")?;
                require_hex(hash, 64, "content hash")?;
                normalize_ids(parents)?;
            }
            Self::Name {
                doc,
                nonce,
                name,
                supersedes,
                ..
            } => {
                require_hex(doc, 64, "document ID")?;
                require_hex(nonce, 32, "nonce")?;
                if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
                    return Err(Error::InvalidClaim(
                        "name must be a flat, nonempty label".into(),
                    ));
                }
                normalize_ids(supersedes)?;
            }
            Self::TagAdd {
                doc,
                nonce,
                tag,
                scope,
                supersedes,
                ..
            } => {
                require_hex(doc, 64, "document ID")?;
                require_hex(nonce, 32, "nonce")?;
                *tag = normalized_tag(tag)?;
                if let Some(key) = scope {
                    *key = normalized_tag(key)?;
                    if !is_descendant(key, tag) {
                        return Err(Error::InvalidClaim("set tag is outside its key".into()));
                    }
                }
                normalize_ids(supersedes)?;
            }
            Self::TagRemove {
                doc,
                nonce,
                tag,
                removes,
                ..
            } => {
                require_hex(doc, 64, "document ID")?;
                require_hex(nonce, 32, "nonce")?;
                *tag = normalized_tag(tag)?;
                normalize_ids(removes)?;
                if removes.is_empty() {
                    return Err(Error::InvalidClaim(
                        "tag removal has no observed targets".into(),
                    ));
                }
            }
        }
        Ok(self)
    }

    fn refs(&self) -> &[String] {
        match self {
            Self::Create { .. } => &[],
            Self::Version { parents, .. } => parents,
            Self::Name { supersedes, .. } | Self::TagAdd { supersedes, .. } => supersedes,
            Self::TagRemove { removes, .. } => removes,
        }
    }

    pub fn bound_doc(&self) -> Option<&str> {
        match self {
            Self::Create { .. } => None,
            Self::Version { doc, .. }
            | Self::Name { doc, .. }
            | Self::TagAdd { doc, .. }
            | Self::TagRemove { doc, .. } => Some(doc),
        }
    }
}

impl CausalSet {
    pub fn from_claims(claims: impl IntoIterator<Item = CausalClaim>) -> Result<Self> {
        let mut set = Self::default();
        for claim in claims {
            set.insert(claim)?;
        }
        Ok(set)
    }

    pub fn insert(&mut self, claim: CausalClaim) -> Result<String> {
        let claim = claim.normalized()?;
        let id = claim.id()?;
        if let Some(existing) = self.claims.get(&id) {
            if existing != &claim {
                return Err(Error::InvalidClaim(format!("claim ID collision: {id}")));
            }
        } else {
            self.claims.insert(id.clone(), claim);
        }
        Ok(id)
    }

    pub fn union(&self, other: &Self) -> Result<Self> {
        let mut merged = self.clone();
        for claim in other.claims.values() {
            merged.insert(claim.clone())?;
        }
        Ok(merged)
    }

    pub fn claims(&self) -> &BTreeMap<String, CausalClaim> {
        &self.claims
    }

    pub fn fold(&self) -> Result<CausalState> {
        let mut document_ids = BTreeSet::new();
        for (id, claim) in &self.claims {
            document_ids.insert(claim.bound_doc().unwrap_or(id));
        }
        if document_ids.len() > 1 {
            return Err(Error::InvalidClaim("claim set mixes document IDs".into()));
        }
        let mut targets = BTreeSet::new();
        let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        let mut remaining: BTreeMap<&str, usize> = BTreeMap::new();
        for (id, claim) in &self.claims {
            remaining.insert(id, claim.refs().len());
            for reference in claim.refs() {
                let parent = self.claims.get(reference).ok_or_else(|| {
                    Error::InvalidClaim(format!("{id} references missing claim {reference}"))
                })?;
                validate_reference(claim, parent)?;
                targets.insert(reference.as_str());
                dependents.entry(reference).or_default().push(id);
            }
        }
        let mut ready: BTreeSet<&str> = remaining
            .iter()
            .filter_map(|(id, count)| (*count == 0).then_some(*id))
            .collect();
        let mut visited = 0;
        while let Some(id) = ready.pop_first() {
            visited += 1;
            if let Some(children) = dependents.get(id) {
                for child in children {
                    let count = remaining.get_mut(child).expect("child was indexed");
                    *count -= 1;
                    if *count == 0 {
                        ready.insert(child);
                    }
                }
            }
        }
        if visited != self.claims.len() {
            return Err(Error::InvalidClaim("causal claim cycle".into()));
        }

        let mut state = CausalState::default();
        for (id, claim) in &self.claims {
            match claim {
                CausalClaim::Create { .. } => {}
                CausalClaim::Name { name, .. } if !targets.contains(id.as_str()) => {
                    state.names.push(FieldValue {
                        id: id.clone(),
                        value: name.clone(),
                    })
                }
                CausalClaim::TagAdd { tag, scope, .. } if !targets.contains(id.as_str()) => {
                    state.tags.push(TagValue {
                        id: id.clone(),
                        value: tag.clone(),
                        scope: scope.clone(),
                    })
                }
                CausalClaim::Version {
                    hash, parents, ts, ..
                } => {
                    let version = VersionValue {
                        id: id.clone(),
                        hash: hash.clone(),
                        parents: parents.clone(),
                        ts: *ts,
                    };
                    if !targets.contains(id.as_str()) {
                        state.heads.push(version.clone());
                    }
                    state.versions.push(version);
                }
                _ => {}
            }
        }
        Ok(state)
    }
}

fn validate_reference(claim: &CausalClaim, parent: &CausalClaim) -> Result<()> {
    let valid = match (claim, parent) {
        (CausalClaim::Version { .. }, CausalClaim::Version { .. }) => true,
        (CausalClaim::Name { .. }, CausalClaim::Name { .. }) => true,
        (CausalClaim::TagAdd { tag, scope, .. }, CausalClaim::TagAdd { tag: prior, .. }) => {
            match scope {
                Some(key) => prior == key || is_descendant(key, prior),
                None => prior == tag || is_descendant(prior, tag),
            }
        }
        (CausalClaim::TagRemove { tag, .. }, CausalClaim::TagAdd { tag: prior, .. }) => {
            tag == prior
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(Error::InvalidClaim(
            "reference crosses fields or tag scope".into(),
        ))
    }
}

fn require_hex(value: &str, len: usize, field: &str) -> Result<()> {
    if value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(Error::InvalidClaim(format!(
            "invalid {field}: expected {len} lowercase hex digits"
        )))
    }
}

fn normalize_ids(ids: &mut Vec<String>) -> Result<()> {
    for id in ids.iter() {
        require_hex(id, 64, "claim ID")?;
    }
    ids.sort();
    ids.dedup();
    Ok(())
}

fn normalized_tag(tag: &str) -> Result<String> {
    let tag = normalize_tag(tag);
    if tag.is_empty()
        || tag.contains('\0')
        || tag.split('/').any(|part| part == "." || part == "..")
    {
        Err(Error::InvalidClaim("empty tag path".into()))
    } else {
        Ok(tag)
    }
}

fn is_descendant(prefix: &str, path: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|suffix| suffix.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str, supersedes: Vec<String>) -> CausalClaim {
        CausalClaim::Name {
            doc: "d".repeat(64),
            nonce: n.into(),
            ts: Utc::now(),
            name: "label".into(),
            supersedes,
        }
    }

    #[test]
    fn forged_id_collision_is_rejected() {
        let first = name(&"1".repeat(32), vec![]);
        let second = name(&"2".repeat(32), vec![]);
        let mut set = CausalSet::default();
        set.claims.insert(second.id().unwrap(), first);
        assert!(set.insert(second).is_err());
    }

    #[test]
    fn a_corrupt_causal_cycle_is_rejected() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let mut set = CausalSet::default();
        set.claims
            .insert(a.clone(), name(&"1".repeat(32), vec![b.clone()]));
        set.claims.insert(b, name(&"2".repeat(32), vec![a]));
        assert!(
            matches!(set.fold(), Err(Error::InvalidClaim(message)) if message.contains("cycle"))
        );
    }
}
