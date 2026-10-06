//! Checkpoint a private working store to a passive directory remote and recover
//! a new working store from all published writer refs.
use crate::{
    cas::{sync_dir, Cas},
    claim::Claim,
    document::Document,
    library::Library,
    log::{Log, StoreLock},
    pack,
    remote::{ensure_dir, hash_bytes, valid_hash, valid_writer, RemoteStore},
    writer::{PendingRef, WriterState},
    Error, Result,
};
use merkle_champ::{ChampMap, Identity, Objects};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

type Ledger = ChampMap<String, ChampMap<String, Vec<u8>>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WriterRef {
    pub format: u8,
    pub writer: String,
    pub sequence: u64,
    pub previous: Option<String>,
    pub root: String,
    pub packs: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishReport {
    pub writer: String,
    pub sequence: u64,
    pub root: String,
    pub changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoverReport {
    pub writers: usize,
    pub documents: usize,
    pub blobs: usize,
}

struct PublishedRef {
    value: WriterRef,
    hash: String,
}

/// Give a copied working store its own writer chain while retaining its claims.
pub fn fork_writer(root: &Path, label: Option<&str>) -> Result<(String, String)> {
    let _lock = StoreLock::acquire(root)?;
    let mut state = WriterState::load_or_create(root)?;
    let previous = state.id.clone();
    state.fork()?;
    if let Some(label) = label {
        state.label = Some(label.to_owned());
    }
    state.save(root)?;
    Ok((previous, state.id))
}

/// Publish a self-contained checkpoint after all referenced bytes are durable.
/// The optional label is for display; the store's durable ID owns the ref chain.
pub fn publish(
    library: &Library,
    remote: &dyn RemoteStore,
    label: Option<&str>,
) -> Result<PublishReport> {
    let _lock = StoreLock::acquire(&library.root)?;
    let mut state = WriterState::load_or_create(&library.root)?;
    if let Some(label) = label {
        state.label = Some(label.to_owned());
    }
    let writer = state.id.clone();
    valid_writer(&writer)?;
    let mut previous = latest_ref(remote, &writer)?;
    reconcile_pending(&mut state, remote, &library.root, &mut previous)?;
    if previous.as_ref().map(|published| published.hash.as_str()) != state.last_ref.as_deref() {
        return Err(Error::Storage(format!(
            "another device is publishing as writer {writer}; remote tip differs from this store's last published ref"
        )));
    }
    let (ledger, blobs) = ledger_from_logs(&library.root)?;
    let mut objects = if let Some(ref previous) = previous {
        let objects = load_objects(remote, &previous.value.packs)?;
        let prior_root = decode_identity(&previous.value.root)?;
        let prior: Ledger = ChampMap::load(&prior_root, &objects)
            .map_err(|e| Error::Storage(format!("cannot load prior writer root: {e:?}")))?;
        ensure_prior_claims(&prior, &ledger)?;
        objects
    } else {
        Objects::new()
    };
    let prior_objects: BTreeSet<Identity> = objects.iter().map(|(id, _)| *id).collect();

    for hash in &blobs {
        let bytes = library
            .cas
            .get(hash)?
            .ok_or_else(|| Error::Storage(format!("missing local blob {hash}")))?;
        remote.put_blob(hash, &bytes)?;
    }

    let root = ledger.save(&mut objects);
    let root_hex = hex::encode(root);
    if let Some(ref previous) = previous {
        if previous.value.root == root_hex {
            state.save(&library.root)?;
            return Ok(PublishReport {
                writer: writer.clone(),
                sequence: previous.value.sequence,
                root: root_hex,
                changed: false,
            });
        }
    }
    // The pack holds only nodes the remote lacks. Each is reachable from the
    // new root through other new nodes: a node already published has only
    // published children, so every new node's parent is new as well.
    let mut new_objects = Objects::new();
    for (id, bytes) in objects.iter() {
        if !prior_objects.contains(id) {
            new_objects.insert(bytes);
        }
    }
    if new_objects.is_empty() {
        return Err(Error::Storage("new root has no new CHAMP objects".into()));
    }
    let pack_id = remote.put_pack(&pack::encode(&[root], &new_objects)?)?;
    let sequence = previous
        .as_ref()
        .map_or(Some(1), |old| old.value.sequence.checked_add(1))
        .ok_or_else(|| Error::Storage("writer ref sequence overflow".into()))?;
    let mut packs = previous
        .as_ref()
        .map_or_else(Vec::new, |old| old.value.packs.clone());
    packs.push(pack_id);
    let reference = WriterRef {
        format: 1,
        writer: writer.clone(),
        sequence,
        previous: previous.map(|old| old.hash),
        root: root_hex.clone(),
        packs,
    };
    let bytes = serde_json::to_vec(&reference)
        .map_err(|e| Error::Storage(format!("cannot encode writer ref: {e}")))?;
    let hash = hash_bytes(&bytes);
    state.pending = Some(PendingRef {
        sequence,
        hash: hash.clone(),
        json: String::from_utf8(bytes.clone()).expect("JSON is UTF-8"),
    });
    state.save(&library.root)?;
    if !remote.publish_ref(&writer, sequence, &bytes)? {
        return Err(Error::Storage(format!(
            "writer ref {writer}/{sequence} is occupied; another device may own this writer, or an interrupted ref needs repair"
        )));
    }
    state.last_ref = Some(hash);
    state.pending = None;
    state.save(&library.root)?;
    Ok(PublishReport {
        writer,
        sequence,
        root: root_hex,
        changed: true,
    })
}

fn reconcile_pending(
    state: &mut WriterState,
    remote: &dyn RemoteStore,
    root: &Path,
    previous: &mut Option<PublishedRef>,
) -> Result<()> {
    let Some(pending) = state.pending.clone() else {
        return Ok(());
    };
    let reference: WriterRef = serde_json::from_str(&pending.json)
        .map_err(|e| Error::Storage(format!("invalid pending writer ref: {e}")))?;
    if reference.format != 1
        || reference.writer != state.id
        || reference.sequence != pending.sequence
        || reference.previous != state.last_ref
    {
        return Err(Error::Storage(
            "pending writer ref does not match local state".into(),
        ));
    }
    let tip = previous.as_ref().map(|published| published.hash.as_str());
    if tip == Some(pending.hash.as_str()) {
        state.last_ref = Some(pending.hash);
        state.pending = None;
        state.save(root)?;
        return Ok(());
    }
    if tip != state.last_ref.as_deref() {
        return Err(Error::Storage(format!(
            "another device is publishing as writer {}; remote tip differs from the pending ref",
            state.id
        )));
    }
    if !remote.publish_ref(&state.id, pending.sequence, pending.json.as_bytes())? {
        return Err(Error::Storage(format!(
            "pending writer ref {}/{} is occupied by different bytes or an unmarked reservation",
            state.id, pending.sequence
        )));
    }
    state.last_ref = Some(pending.hash.clone());
    state.pending = None;
    state.save(root)?;
    *previous = latest_ref(remote, &state.id)?;
    Ok(())
}

/// Recover all writer roots into a new, absent working-store directory.
pub fn recover(remote: &dyn RemoteStore, target: &Path) -> Result<RecoverReport> {
    if fs::symlink_metadata(target).is_ok() {
        return Err(Error::Storage(format!(
            "recovery target already exists: {}",
            target.display()
        )));
    }
    let mut refs = Vec::new();
    let mut pack_ids = BTreeSet::new();
    for writer in remote.writers()? {
        if let Some(reference) = latest_ref(remote, &writer)? {
            pack_ids.extend(reference.value.packs.iter().cloned());
            refs.push(reference.value);
        }
    }
    if refs.is_empty() {
        return Err(Error::Storage("remote has no published writer refs".into()));
    }
    let objects = load_objects(remote, &pack_ids.into_iter().collect::<Vec<_>>())?;
    let mut claims: BTreeMap<String, BTreeMap<String, Claim>> = BTreeMap::new();
    for reference in &refs {
        let root = decode_identity(&reference.root)?;
        let ledger: Ledger = ChampMap::load(&root, &objects)
            .map_err(|e| Error::Storage(format!("cannot load writer root: {e:?}")))?;
        for (doc_id, doc_claims) in ledger.iter() {
            valid_hash(doc_id)?;
            let entry = claims.entry(doc_id.clone()).or_default();
            for (id, bytes) in doc_claims.iter() {
                valid_hash(id)?;
                let line = std::str::from_utf8(bytes)
                    .map_err(|e| Error::Storage(format!("claim is not UTF-8: {e}")))?;
                let claim = Claim::from_json_line(line)?;
                if claim.id()? != *id || claim.bound_doc().is_some_and(|bound| bound != doc_id) {
                    return Err(Error::Storage(format!(
                        "claim {id} does not belong under document {doc_id}"
                    )));
                }
                if let Some(existing) = entry.insert(id.clone(), claim.clone()) {
                    if existing != claim {
                        return Err(Error::Storage(format!("claim ID collision: {id}")));
                    }
                }
            }
        }
    }

    let mut documents = Vec::new();
    let mut blobs = BTreeSet::new();
    for (doc_id, claims) in claims {
        let create = claims
            .get(&doc_id)
            .filter(|claim| matches!(claim, Claim::Create { .. }))
            .ok_or_else(|| Error::Storage(format!("missing create claim for {doc_id}")))?
            .clone();
        let mut ordered = vec![create];
        ordered.extend(
            claims
                .into_iter()
                .filter(|(id, _)| id != &doc_id)
                .map(|(_, claim)| claim),
        );
        Document::fold(&doc_id, &ordered)?;
        for claim in &ordered {
            if let Claim::Version { hash, .. } = claim {
                blobs.insert(hash.clone());
            }
        }
        documents.push((doc_id, ordered));
    }

    let mut stage = Stage::new(target)?;
    let cas = Cas::new(&stage.path);
    for hash in &blobs {
        let bytes = remote.get_blob(hash)?;
        if cas.put(&bytes)? != *hash {
            return Err(Error::Storage(format!("recovered blob mismatch: {hash}")));
        }
    }
    for (doc_id, claims) in &documents {
        Log::new(&stage.path, doc_id).append(claims)?;
    }
    stage.publish(target)?;
    Ok(RecoverReport {
        writers: refs.len(),
        documents: documents.len(),
        blobs: blobs.len(),
    })
}

fn ledger_from_logs(root: &Path) -> Result<(Ledger, BTreeSet<String>)> {
    let mut ledger = Ledger::new();
    let mut blobs = BTreeSet::new();
    for doc_id in Log::all_ids(root)? {
        valid_hash(&doc_id)?;
        let read = Log::new(root, &doc_id).read()?;
        if read.torn_tail.is_some() {
            return Err(Error::Storage(format!(
                "cannot publish document {doc_id} with a torn log tail"
            )));
        }
        Document::fold(&doc_id, &read.claims)?;
        let mut inner = ChampMap::new();
        for claim in read.claims {
            let id = claim.id()?;
            let bytes = claim.to_json_line()?.into_bytes();
            if let Some(existing) = inner.insert(id.clone(), bytes.clone()) {
                if existing != bytes {
                    return Err(Error::Storage(format!("claim ID collision: {id}")));
                }
            }
            if let Claim::Version { hash, .. } = claim {
                blobs.insert(hash);
            }
        }
        ledger.insert(doc_id, inner);
    }
    Ok((ledger, blobs))
}

fn ensure_prior_claims(prior: &Ledger, current: &Ledger) -> Result<()> {
    for (doc_id, old_claims) in prior.iter() {
        let new_claims = current.get(doc_id).ok_or_else(|| {
            Error::Storage(format!(
                "local store lacks previously published document {doc_id}"
            ))
        })?;
        for (id, bytes) in old_claims.iter() {
            if new_claims.get(id) != Some(bytes) {
                return Err(Error::Storage(format!(
                    "local store lacks previously published claim {id}"
                )));
            }
        }
    }
    Ok(())
}

fn latest_ref(remote: &dyn RemoteStore, writer: &str) -> Result<Option<PublishedRef>> {
    let mut previous: Option<PublishedRef> = None;
    for (sequence, bytes) in remote.refs_for(writer)? {
        let reference: WriterRef = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Storage(format!("invalid writer ref {writer}/{sequence}: {e}")))?;
        if reference.format != 1
            || reference.writer != writer
            || reference.sequence != sequence
            || Some(reference.sequence)
                != previous
                    .as_ref()
                    .map_or(Some(1), |old| old.value.sequence.checked_add(1))
            || reference.previous != previous.as_ref().map(|old| old.hash.clone())
            || reference.packs.is_empty()
        {
            return Err(Error::Storage(format!(
                "invalid writer ref chain: {writer}/{sequence}"
            )));
        }
        valid_hash(&reference.root)?;
        for pack in &reference.packs {
            valid_hash(pack)?;
        }
        if let Some(ref old) = previous {
            if !reference.packs.starts_with(&old.value.packs) {
                return Err(Error::Storage(format!(
                    "writer ref dropped reachable packs: {writer}/{sequence}"
                )));
            }
        }
        previous = Some(PublishedRef {
            value: reference,
            hash: hash_bytes(&bytes),
        });
    }
    Ok(previous)
}

fn load_objects(remote: &dyn RemoteStore, packs: &[String]) -> Result<Objects> {
    let mut objects = Objects::new();
    for pack_id in packs {
        for (&id, bytes) in pack::decode(&remote.get_pack(pack_id)?)?.objects.iter() {
            if let Some(existing) = objects.get(&id) {
                if existing != bytes {
                    return Err(Error::Storage(format!(
                        "CHAMP object identity collision: {}",
                        hex::encode(id)
                    )));
                }
            } else if objects.insert(bytes) != id {
                return Err(Error::Storage("CHAMP object identity mismatch".into()));
            }
        }
    }
    Ok(objects)
}

fn decode_identity(hash: &str) -> Result<Identity> {
    valid_hash(hash)?;
    Ok(hex::decode(hash)
        .expect("validated hex")
        .try_into()
        .expect("validated SHA-256 length"))
}

struct Stage {
    path: PathBuf,
    active: bool,
}

impl Stage {
    fn new(target: &Path) -> Result<Self> {
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        ensure_dir(parent)?;
        let mut nonce = [0_u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let path = parent.join(format!(".transfs-restore-{}", hex::encode(nonce)));
        fs::create_dir(&path)?;
        sync_dir(parent)?;
        Ok(Self { path, active: true })
    }

    fn publish(&mut self, target: &Path) -> Result<()> {
        if fs::symlink_metadata(target).is_ok() {
            return Err(Error::Storage(format!(
                "recovery target appeared during restore: {}",
                target.display()
            )));
        }
        sync_dir(&self.path)?;
        fs::rename(&self.path, target)?;
        self.active = false;
        let parent = target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        sync_dir(parent)?;
        Ok(())
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
