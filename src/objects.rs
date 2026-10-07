//! The store's packs of chunks and chunk-list nodes, in `.transfs/packs/`
//! (docs/chunked-storage.md). Where each object lives is rebuilt from the
//! packs' own indexes, which sit at the start of each pack; nothing else
//! records it.
use crate::{cas::sync_dir, Error, Result};
use merkle_champ::{
    pack::{self, Encoding, Entry, Index, Member, ReadOptions, BLOB_DOMAIN},
    Identity, Objects,
};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

/// The largest object a local pack may hold once decoded. Chunks are at most
/// a 64 KiB SQLite page plus the 20-byte blob prefix; list nodes are a few KB.
pub const MAX_OBJECT: u64 = 1 << 20;

#[derive(Clone, Copy, Debug)]
struct Located {
    pack: usize,
    entry: Entry,
}

#[derive(Debug)]
pub struct ObjectStore {
    dir: PathBuf,
    loaded: bool,
    /// Pack names (hex SHA-256 of their bytes) and their index entries.
    packs: Vec<(String, Vec<Entry>)>,
    by_name: HashMap<String, usize>,
    objects: HashMap<Identity, Located>,
}

fn storage(e: impl std::fmt::Display) -> Error {
    Error::Storage(e.to_string())
}

fn is_pack_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ObjectStore {
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join(".transfs/packs"),
            loaded: false,
            packs: Vec::new(),
            by_name: HashMap::new(),
            objects: HashMap::new(),
        }
    }

    pub fn pack_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Every pack in the store, by name.
    pub fn pack_names(&mut self) -> Result<Vec<String>> {
        self.refresh()?;
        let mut names: Vec<_> = self.packs.iter().map(|(name, _)| name.clone()).collect();
        names.sort();
        Ok(names)
    }

    /// Reads the index of every pack not yet known, including packs another
    /// process wrote since this one looked.
    pub fn refresh(&mut self) -> Result<()> {
        self.loaded = true;
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if is_pack_name(&name) && !self.by_name.contains_key(&name) {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            self.add_index(name)?;
        }
        Ok(())
    }

    fn ensure_loaded(&mut self) -> Result<()> {
        if !self.loaded {
            self.refresh()?;
        }
        Ok(())
    }

    fn add_index(&mut self, name: String) -> Result<()> {
        let mut file = File::open(self.pack_path(&name))?;
        let mut head = [0_u8; 24];
        file.read_exact(&mut head)
            .map_err(|e| Error::Storage(format!("pack {name} is truncated: {e}")))?;
        let needed = Index::needed(&head).map_err(|e| storage(format!("pack {name}: {e}")))?;
        let mut prefix = vec![0; needed];
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut prefix)
            .map_err(|e| Error::Storage(format!("pack {name} is truncated: {e}")))?;
        let index = Index::parse(&prefix).map_err(|e| storage(format!("pack {name}: {e}")))?;
        let position = self.packs.len();
        for entry in index.entries() {
            self.objects.entry(entry.id).or_insert(Located {
                pack: position,
                entry: *entry,
            });
        }
        self.packs.push((name.clone(), index.entries().to_vec()));
        self.by_name.insert(name, position);
        Ok(())
    }

    fn locate(&mut self, id: &Identity) -> Result<Option<Located>> {
        self.ensure_loaded()?;
        if let Some(found) = self.objects.get(id) {
            return Ok(Some(*found));
        }
        self.refresh()?;
        Ok(self.objects.get(id).copied())
    }

    pub fn contains(&mut self, id: &Identity) -> Result<bool> {
        Ok(self.locate(id)?.is_some())
    }

    /// The name of a pack holding this object.
    pub fn pack_of(&mut self, id: &Identity) -> Result<Option<String>> {
        Ok(self
            .locate(id)?
            .map(|found| self.packs[found.pack].0.clone()))
    }

    fn stored(&self, pack: usize, entry: &Entry, length: u64) -> Result<Vec<u8>> {
        let mut file = File::open(self.pack_path(&self.packs[pack].0))?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut bytes = vec![0; length as usize];
        file.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn read(&self, pack: usize, entry: &Entry, trusted: bool) -> Result<Vec<u8>> {
        let options = ReadOptions {
            max_decoded: MAX_OBJECT,
            trust_checksums: trusted,
        };
        if entry.length > MAX_OBJECT {
            return Err(Error::Storage("pack member larger than allowed".into()));
        }
        let stored = self.stored(pack, entry, entry.length)?;
        let dictionary = match entry.encoding {
            Encoding::ZstdDictionary(position) => {
                let dictionary = self.packs[pack]
                    .1
                    .get(position as usize)
                    .copied()
                    .ok_or_else(|| Error::Storage("pack names a missing dictionary".into()))?;
                Some(self.read(pack, &dictionary, trusted)?)
            }
            _ => None,
        };
        pack::read_member(entry, &stored, dictionary.as_deref(), &options).map_err(storage)
    }

    /// An object's bytes, decoded and checked. `trusted` relies on a zstd
    /// frame's checksum instead of SHA-256, for packs this store wrote to its
    /// own disk: the checksum catches corruption, not substitution.
    pub fn get(&mut self, id: &Identity, trusted: bool) -> Result<Option<Vec<u8>>> {
        let Some(found) = self.locate(id)? else {
            return Ok(None);
        };
        self.read(found.pack, &found.entry, trusted).map(Some)
    }

    /// The objects in these packs that are not blobs: a chunk list's header
    /// and nodes. Compressed members are always chunks and are skipped
    /// unread; raw members are told apart by their first 20 bytes, since every
    /// chunk starts with the blob prefix and no list node does.
    pub fn non_blobs(&mut self, packs: &[String]) -> Result<Objects> {
        self.ensure_loaded()?;
        let mut objects = Objects::new();
        for name in packs {
            let position = match self.by_name.get(name) {
                Some(&position) => position,
                None => {
                    self.refresh()?;
                    *self
                        .by_name
                        .get(name)
                        .ok_or_else(|| Error::Storage(format!("missing pack {name}")))?
                }
            };
            for entry in self.packs[position].1.clone() {
                if entry.encoding != Encoding::Raw {
                    continue;
                }
                if entry.length >= BLOB_DOMAIN.len() as u64
                    && self.stored(position, &entry, BLOB_DOMAIN.len() as u64)? == BLOB_DOMAIN
                {
                    continue;
                }
                let bytes = self.read(position, &entry, true)?;
                objects.insert(bytes);
            }
        }
        Ok(objects)
    }

    /// Writes a pack, durably, and returns its name.
    pub fn write_pack(
        &mut self,
        roots: &[Identity],
        members: &[(Identity, Member<'_>)],
    ) -> Result<String> {
        self.ensure_loaded()?;
        let bytes = pack::encode_members(roots, members).map_err(storage)?;
        let name = hex::encode(Sha256::digest(&bytes));
        if self.by_name.contains_key(&name) {
            return Ok(name);
        }
        fs::create_dir_all(&self.dir)?;
        sync_dir(self.dir.parent().expect("packs dir has a parent"))?;
        let mut random = [0_u8; 8];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let tmp = self
            .dir
            .join(format!(".{name}.{}.tmp", hex::encode(random)));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&tmp, self.pack_path(&name))?;
            sync_dir(&self.dir)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        self.add_index(name.clone())?;
        Ok(name)
    }
}
