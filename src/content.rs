//! A version's bytes, stored as a whole blob or as chunks
//! (docs/chunked-storage.md). A version claim names its content by the
//! SHA-256 of the complete file; this module finds and writes the bytes
//! behind that hash, whichever way they are kept.
use crate::{
    cas::Cas,
    chunker::{self, read_full},
    config::Chunking,
    filetype::{self, Layout, HEAD},
    objects::ObjectStore,
    rep::{self, Rep},
    Error, Result,
};
use merkle_champ::{
    pack::{self, Member, BLOB_DOMAIN},
    read_tagged, write_tagged, Decode, DecodeError, Identify, Identity, Loader, Objects, Sequence,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashSet},
    fs::{self, File},
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

/// One chunk in a version's chunk list: its blob identity and its length.
/// The identity is written as 32 contiguous bytes, which is how a pack finds
/// the reference and places the chunk right after the leaf that names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChunkRef {
    pub id: Identity,
    pub len: u64,
}

impl Identify for ChunkRef {
    // `Sink` is named by path: importing it beside `sha2::Digest` makes
    // `update` ambiguous.
    fn identify<S: merkle_champ::Sink + ?Sized>(&self, sink: &mut S) {
        let mut bytes = self.id.to_vec();
        bytes.extend_from_slice(&self.len.to_le_bytes());
        write_tagged(sink, b'C', &bytes);
    }
    const MEASURES: usize = 1;
    fn measure(&self, sums: &mut [u64]) {
        sums[0] += self.len;
    }
}

impl Decode for ChunkRef {
    fn decode(input: &mut &[u8], _: &mut Loader<'_>) -> std::result::Result<Self, DecodeError> {
        let bytes = read_tagged(input, b'C')?;
        if bytes.len() != 40 {
            return Err(DecodeError::Malformed(
                "a chunk reference of the wrong length",
            ));
        }
        Ok(ChunkRef {
            id: bytes[..32].try_into().expect("32 bytes"),
            len: u64::from_le_bytes(bytes[32..].try_into().expect("8 bytes")),
        })
    }
}

/// A chunk's identity: the merkle-champ blob identity of its bytes.
pub fn chunk_id(chunk: &[u8]) -> Identity {
    let mut hasher = Sha256::new();
    hasher.update(BLOB_DOMAIN);
    hasher.update(chunk);
    hasher.finalize().into()
}

/// How a version's content is kept, for `show`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stored {
    Blob,
    Chunks { chunks: usize, packs: usize },
}

/// A new chunk on its way into a pack: compressed, or raw when compression
/// didn't shrink it.
enum NewChunk {
    /// The blob's stored bytes: the prefix followed by the chunk.
    Raw(Vec<u8>),
    /// A zstd frame of the chunk alone, without the prefix. Only native
    /// builds compress.
    #[cfg_attr(not(feature = "native"), allow(dead_code))]
    Zstd(Vec<u8>),
}

/// How many chunk lists to keep loaded. Reading a file in pieces would
/// otherwise reload its list for every piece, and two files read at once
/// would reload each other's (measured: 1.65 times slower with one).
const LISTS: usize = 16;

#[derive(Debug)]
struct State {
    objects: ObjectStore,
    /// Recently loaded chunk lists by their own identity, oldest first. Keyed
    /// by list identity, not content hash: a list is content-addressed, so
    /// the same root is always the same list, while a record for a content
    /// hash may be replaced and must then be read afresh.
    lists: Vec<(String, Arc<Sequence<ChunkRef>>)>,
}

impl State {
    fn remember(&mut self, root: String, list: Arc<Sequence<ChunkRef>>) {
        self.lists.retain(|(known, _)| *known != root);
        if self.lists.len() == LISTS {
            self.lists.remove(0);
        }
        self.lists.push((root, list));
    }
}

#[derive(Clone, Debug)]
pub struct Content {
    root: PathBuf,
    cas: Cas,
    state: Arc<Mutex<State>>,
}

fn storage(why: impl std::fmt::Display) -> Error {
    Error::Storage(why.to_string())
}

impl Content {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            cas: Cas::new(&root),
            state: Arc::new(Mutex::new(State {
                objects: ObjectStore::new(&root),
                lists: Vec::new(),
            })),
            root,
        }
    }

    pub fn cas(&self) -> &Cas {
        &self.cas
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("content state lock poisoned")
    }

    fn rep(&self, hash: &str) -> Result<Option<Rep>> {
        Ok(rep::read(&self.root, hash)?.into_iter().next())
    }

    /// Whether this content is stored, as a blob or as chunks.
    pub fn exists(&self, hash: &str) -> Result<bool> {
        Ok(self.cas.exists(hash) || self.rep(hash)?.is_some())
    }

    pub fn describe(&self, hash: &str) -> Result<Option<Stored>> {
        if self.cas.exists(hash) {
            return Ok(Some(Stored::Blob));
        }
        let Some(rep) = self.rep(hash)? else {
            return Ok(None);
        };
        let mut state = self.lock();
        let list = self.list(&mut state, &rep)?;
        Ok(Some(Stored::Chunks {
            chunks: list.len(),
            packs: rep.packs.len(),
        }))
    }

    pub fn size(&self, hash: &str) -> Result<Option<u64>> {
        if let Ok(meta) = fs::metadata(self.cas.path_for(hash)) {
            return Ok(Some(meta.len()));
        }
        Ok(self.rep(hash)?.map(|rep| rep.length))
    }

    /// The whole content, checked against its hash.
    pub fn read(&self, hash: &str) -> Result<Option<Vec<u8>>> {
        if self.cas.exists(hash) {
            return self.cas.get(hash);
        }
        let Some(rep) = self.rep(hash)? else {
            return Ok(None);
        };
        let mut state = self.lock();
        let list = self.list(&mut state, &rep)?;
        let mut bytes = Vec::with_capacity(rep.length as usize);
        for chunk in list.iter() {
            bytes.extend_from_slice(&Self::chunk(&mut state, chunk)?);
        }
        if bytes.len() as u64 != rep.length || hex::encode(Sha256::digest(&bytes)) != hash {
            return Err(storage(format!("content {hash} does not match its hash")));
        }
        Ok(Some(bytes))
    }

    /// Up to `len` bytes starting at `offset`.
    pub fn read_at(&self, hash: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        let path = self.cas.path_for(hash);
        if path.exists() {
            let mut file = File::open(path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = Vec::new();
            file.take(len).read_to_end(&mut bytes)?;
            return Ok(bytes);
        }
        let rep = self
            .rep(hash)?
            .ok_or_else(|| storage(format!("missing content {hash}")))?;
        let end = rep.length.min(offset.saturating_add(len));
        if offset >= end {
            return Ok(Vec::new());
        }
        let mut state = self.lock();
        let list = self.list(&mut state, &rep)?;
        let first = list
            .locate(0, offset)
            .ok_or_else(|| storage(format!("content {hash} is shorter than its record")))?;
        let mut position = list.prefix(0, first);
        let mut bytes = Vec::with_capacity((end - offset) as usize);
        for chunk in list.iter_from(first) {
            if position >= end {
                break;
            }
            let data = Self::chunk(&mut state, chunk)?;
            let start = offset.saturating_sub(position) as usize;
            let stop = (end - position).min(data.len() as u64) as usize;
            if start < stop {
                bytes.extend_from_slice(&data[start..stop]);
            }
            position += chunk.len;
        }
        Ok(bytes)
    }

    /// Checks a chunked version: its record, that the packs it names exist,
    /// its chunk list, and that every chunk is stored. `deep` also rebuilds
    /// the content and compares it with its hash. Returns the record's packs.
    pub fn verify_chunked(&self, hash: &str, deep: bool) -> Result<Vec<String>> {
        let rep = self
            .rep(hash)?
            .ok_or_else(|| storage(format!("no representation record for {hash}")))?;
        let mut state = self.lock();
        let packs: HashSet<String> = state.objects.pack_names()?.into_iter().collect();
        if let Some(missing) = rep.packs.iter().find(|pack| !packs.contains(*pack)) {
            return Err(storage(format!("its record names missing pack {missing}")));
        }
        let list = self.list(&mut state, &rep)?;
        let most = match rep.chunking.strip_prefix("fixed/") {
            Some(page) => {
                let page: u64 = page.parse().map_err(|_| storage("invalid page size"))?;
                rep.length.div_ceil(page.max(1))
            }
            None => rep.length / u64::from(chunker::MIN) + 1,
        };
        if list.len() as u64 > most {
            return Err(storage(format!(
                "its chunk list has {} chunks, more than its length allows",
                list.len()
            )));
        }
        for chunk in list.iter() {
            if !state.objects.contains(&chunk.id)? {
                return Err(storage(format!("missing chunk {}", hex::encode(chunk.id))));
            }
        }
        drop(state);
        if deep {
            self.read(hash)?;
        }
        Ok(rep.packs)
    }

    /// The first `n` bytes, for recognizing a file's type.
    pub fn head(&self, hash: &str, n: usize) -> Result<Vec<u8>> {
        self.read_at(hash, 0, n as u64)
    }

    /// The chunk's bytes, without the blob prefix.
    fn chunk(state: &mut State, chunk: &ChunkRef) -> Result<Vec<u8>> {
        let object = state
            .objects
            .get(&chunk.id, true)?
            .ok_or_else(|| storage(format!("missing chunk {}", hex::encode(chunk.id))))?;
        let data = object
            .strip_prefix(BLOB_DOMAIN)
            .ok_or_else(|| storage("a chunk is not a blob"))?;
        if data.len() as u64 != chunk.len {
            return Err(storage(format!(
                "chunk {} has the wrong length",
                hex::encode(chunk.id)
            )));
        }
        Ok(data.to_vec())
    }

    /// A version's chunk list, from the packs its record names.
    fn list(&self, state: &mut State, rep: &Rep) -> Result<Arc<Sequence<ChunkRef>>> {
        if let Some((_, list)) = state.lists.iter().find(|(root, _)| *root == rep.root) {
            return Ok(list.clone());
        }
        let root: Identity = hex::decode(&rep.root)
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| storage("invalid chunk list root"))?;
        let objects = state.objects.non_blobs(&rep.packs)?;
        let list = Sequence::<ChunkRef>::load(&root, &objects)
            .map_err(|e| storage(format!("cannot load chunk list for {}: {e:?}", rep.content)))?;
        if list.measures()[0] != rep.length {
            return Err(storage(format!(
                "chunk list for {} does not add up to its length",
                rep.content
            )));
        }
        let list = Arc::new(list);
        state.remember(rep.root.clone(), list.clone());
        Ok(list)
    }

    /// Stores a file and returns its content hash: as a whole blob, or as
    /// chunks, as `chunking` and docs/file-types.md decide.
    pub fn put_file(&self, path: &Path, chunking: Chunking) -> Result<String> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        let mut head = [0_u8; HEAD];
        let n = read_full(&mut file, &mut head)?;
        let layout = match chunking {
            Chunking::Off => Layout::Whole,
            Chunking::Auto => filetype::layout(&head[..n], len),
        };
        if layout == Layout::Whole {
            return self.cas.put(&fs::read(path)?);
        }
        file.seek(SeekFrom::Start(0))?;
        let mut state = self.lock();
        state.objects.refresh()?;

        // Cut the file, hashing it as it goes, and compress each new chunk.
        let mut hasher = Sha256::new();
        let mut refs = Vec::new();
        let mut new_chunks: Vec<(Identity, NewChunk)> = Vec::new();
        let mut new_ids = HashSet::new();
        let mut compressor = Compressor::new()?;
        let mut total = 0_u64;
        chunker::chunks(BufReader::new(file), layout, |data| {
            hasher.update(data);
            total += data.len() as u64;
            let id = chunk_id(data);
            refs.push(ChunkRef {
                id,
                len: data.len() as u64,
            });
            if !new_ids.contains(&id) && !state.objects.contains(&id)? {
                new_ids.insert(id);
                new_chunks.push((id, compressor.chunk(data)?));
            }
            Ok(())
        })?;
        let hash = hex::encode(hasher.finalize());
        if self.cas.exists(&hash) || self.rep(&hash)?.is_some() {
            return Ok(hash);
        }

        // One pack of the new list nodes and new chunks, rooted at the list.
        let list: Sequence<ChunkRef> = refs.into_iter().collect();
        let mut objects = Objects::new();
        let list_id = list.save(&mut objects);
        let mut members: Vec<(Identity, Member<'_>)> = Vec::new();
        for (id, bytes) in objects.iter() {
            if !state.objects.contains(id)? {
                members.push((*id, Member::Raw(bytes)));
            }
        }
        for (id, chunk) in &new_chunks {
            members.push((
                *id,
                match chunk {
                    NewChunk::Raw(bytes) => Member::Raw(bytes),
                    NewChunk::Zstd(frame) => Member::Zstd {
                        frame,
                        dictionary: None,
                    },
                },
            ));
        }
        if !members.is_empty() {
            state.objects.write_pack(&[list_id], &members)?;
        }

        // Every pack this version's list and chunks live in.
        let mut packs = BTreeSet::new();
        let ids = std::iter::once(list_id)
            .chain(list.node_ids())
            .chain(list.iter().map(|chunk| chunk.id));
        for id in ids {
            let pack = state
                .objects
                .pack_of(&id)?
                .ok_or_else(|| storage(format!("object {} was not stored", hex::encode(id))))?;
            packs.insert(pack);
        }
        let rep = Rep {
            format: 1,
            content: hash.clone(),
            length: total,
            chunking: chunker::name(layout),
            root: hex::encode(list_id),
            packs: packs.into_iter().collect(),
        };
        rep::write(&self.root, &rep)?;
        state.remember(hex::encode(list_id), Arc::new(list));
        Ok(hash)
    }
}

/// Compresses chunks with zstd at level 3, writing each frame's content size
/// and checksum. Builds without the native feature store chunks raw.
struct Compressor {
    #[cfg(feature = "native")]
    zstd: zstd::bulk::Compressor<'static>,
}

impl Compressor {
    fn new() -> Result<Self> {
        #[cfg(feature = "native")]
        {
            use zstd::zstd_safe::CParameter;
            let mut zstd = zstd::bulk::Compressor::new(3)?;
            zstd.set_parameter(CParameter::ChecksumFlag(true))?;
            zstd.set_parameter(CParameter::ContentSizeFlag(true))?;
            Ok(Self { zstd })
        }
        #[cfg(not(feature = "native"))]
        Ok(Self {})
    }

    fn chunk(&mut self, data: &[u8]) -> Result<NewChunk> {
        #[cfg(feature = "native")]
        {
            let frame = self.zstd.compress(data)?;
            if frame.len() < data.len() {
                return Ok(NewChunk::Zstd(frame));
            }
        }
        Ok(NewChunk::Raw(pack::blob(data)))
    }
}
