//! Read-only query-path FUSE mount. The view logic is testable without /dev/fuse.
use std::{
    collections::HashMap,
    ffi::{CString, OsStr},
    fs::File,
    os::unix::{ffi::OsStrExt, fs::FileExt},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
    LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen,
    ReplyStatfs, Request,
};

use crate::{
    cas::Cas,
    index::{Index, Row},
    query, Result,
};

const TTL: Duration = Duration::ZERO;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Directory,
    File { hash: String, size: u64 },
}

/// Keeps the query grammar, disambiguation, and blob resolution outside FUSE.
pub struct MountView {
    index: Mutex<Index>,
    cas: Cas,
}

impl MountView {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            index: Mutex::new(Index::open(root)?),
            cas: Cas::new(root),
        })
    }

    pub fn resolve(&self, path: &str) -> Result<Option<Node>> {
        let parsed = query::parse(path);
        let index = self.index.lock().expect("index lock poisoned");
        let walk = index.walk(&parsed.components)?;
        if !walk.valid {
            return Ok(None);
        }
        if let Some(name) = parsed.doc_name {
            let rows = index.docs(&walk, None)?;
            let Some((_, row)) = leaves(rows).into_iter().find(|(leaf, _)| leaf == &name) else {
                return Ok(None);
            };
            let hash = row.head_hash.expect("leaves only includes headed rows");
            let Ok(meta) = std::fs::metadata(self.cas.path_for(&hash)) else {
                return Ok(None);
            };
            Ok(Some(Node::File {
                hash,
                size: meta.len(),
            }))
        } else {
            Ok(Some(Node::Directory))
        }
    }

    pub fn list(&self, path: &str) -> Result<Option<Vec<(String, Node)>>> {
        let parsed = query::parse(path);
        let index = self.index.lock().expect("index lock poisoned");
        let walk = index.walk(&parsed.components)?;
        if !walk.valid || parsed.doc_name.is_some() {
            return Ok(None);
        }
        if parsed.doc_view {
            let entries = leaves(index.docs(&walk, None)?)
                .into_iter()
                .filter_map(|(name, row)| {
                    let hash = row.head_hash?;
                    let size = std::fs::metadata(self.cas.path_for(&hash)).ok()?.len();
                    Some((name, Node::File { hash, size }))
                })
                .collect();
            Ok(Some(entries))
        } else {
            Ok(Some(
                index
                    .facets(&walk)?
                    .into_iter()
                    .map(|name| (name, Node::Directory))
                    .collect(),
            ))
        }
    }

    pub fn read(&self, hash: &str, offset: u64, size: u32) -> std::io::Result<Vec<u8>> {
        let file = File::open(self.cas.path_for(hash))?;
        let mut bytes = vec![0; size as usize];
        let count = file.read_at(&mut bytes, offset)?;
        bytes.truncate(count);
        Ok(bytes)
    }

    pub fn document_count(&self) -> Result<u64> {
        Ok(self.index.lock().expect("index lock poisoned").all()?.len() as u64)
    }
}

/// Minimal temporary suffix used by the Crystal mount for same-name documents.
pub fn leaves(rows: Vec<Row>) -> Vec<(String, Row)> {
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| row.head_hash.is_some())
        .collect();
    let mut counts: HashMap<Option<String>, usize> = HashMap::new();
    for row in &rows {
        *counts.entry(row.name.clone()).or_default() += 1;
    }
    rows.into_iter()
        .map(|row| {
            let base = row
                .name
                .clone()
                .unwrap_or_else(|| format!("untitled-{}", row.id.get(..8).unwrap_or(&row.id)));
            let name = if counts.get(&row.name).copied().unwrap_or(0) > 1 {
                disambiguate(&base, &row.id)
            } else {
                base
            };
            (name, row)
        })
        .collect()
}

fn disambiguate(base: &str, id: &str) -> String {
    let suffix = format!("~{}", id.get(..4).unwrap_or(id));
    match base.rfind('.') {
        Some(dot) if dot > 0 => format!("{}{}{}", &base[..dot], suffix, &base[dot..]),
        _ => format!("{base}{suffix}"),
    }
}

struct Inodes {
    paths: HashMap<u64, String>,
    numbers: HashMap<String, u64>,
    next: u64,
}

impl Inodes {
    fn new() -> Self {
        Self {
            paths: HashMap::from([(1, "/".into())]),
            numbers: HashMap::from([("/".into(), 1)]),
            next: 2,
        }
    }
    fn path(&self, ino: INodeNo) -> Option<&str> {
        self.paths.get(&ino.0).map(String::as_str)
    }
    fn insert(&mut self, path: String) -> INodeNo {
        if let Some(ino) = self.numbers.get(&path) {
            return INodeNo(*ino);
        }
        let ino = self.next;
        self.next += 1;
        self.paths.insert(ino, path.clone());
        self.numbers.insert(path, ino);
        INodeNo(ino)
    }
    fn parent(&self, path: &str) -> INodeNo {
        let parent = path
            .rsplit_once('/')
            .map(|(head, _)| if head.is_empty() { "/" } else { head })
            .unwrap_or("/");
        INodeNo(*self.numbers.get(parent).unwrap_or(&1))
    }
}

pub struct QueryFs {
    root: PathBuf,
    view: MountView,
    inodes: Mutex<Inodes>,
    uid: u32,
    gid: u32,
}

impl QueryFs {
    pub fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.to_owned(),
            view: MountView::new(root)?,
            inodes: Mutex::new(Inodes::new()),
            uid: unsafe { libc::geteuid() },
            gid: unsafe { libc::getegid() },
        })
    }
    fn path(&self, ino: INodeNo) -> Option<String> {
        self.inodes
            .lock()
            .expect("inode lock poisoned")
            .path(ino)
            .map(str::to_owned)
    }
    fn attr(&self, ino: INodeNo, node: &Node) -> FileAttr {
        let (kind, size, perm, nlink) = match node {
            Node::Directory => (FileType::Directory, 0, 0o555, 2),
            Node::File { size, .. } => (FileType::RegularFile, *size, 0o444, 1),
        };
        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512),
            atime: SystemTime::UNIX_EPOCH,
            mtime: SystemTime::UNIX_EPOCH,
            ctime: SystemTime::UNIX_EPOCH,
            crtime: SystemTime::UNIX_EPOCH,
            kind,
            perm,
            nlink,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }
}

fn child_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

impl Filesystem for QueryFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let Some(parent_path) = self.path(parent) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(name) = name.to_str() else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.view.resolve(&parent_path) {
            Ok(Some(Node::Directory)) => {}
            Ok(Some(Node::File { .. })) => {
                reply.error(Errno::ENOTDIR);
                return;
            }
            Ok(None) => {
                reply.error(Errno::ENOENT);
                return;
            }
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        }
        if name == "." || name == ".." {
            let ino = if name == "." {
                parent
            } else {
                self.inodes
                    .lock()
                    .expect("inode lock poisoned")
                    .parent(&parent_path)
            };
            let path = self.path(ino).unwrap_or_else(|| "/".into());
            match self.view.resolve(&path) {
                Ok(Some(node)) => reply.entry(&TTL, &self.attr(ino, &node), Generation(0)),
                Ok(None) => reply.error(Errno::ENOENT),
                Err(_) => reply.error(Errno::EIO),
            }
            return;
        }
        let path = child_path(&parent_path, name);
        match self.view.resolve(&path) {
            Ok(Some(node)) => {
                let ino = self
                    .inodes
                    .lock()
                    .expect("inode lock poisoned")
                    .insert(path);
                reply.entry(&TTL, &self.attr(ino, &node), Generation(0));
            }
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        let Some(path) = self.path(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.view.resolve(&path) {
            Ok(Some(node)) => reply.attr(&TTL, &self.attr(ino, &node)),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn opendir(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let Some(path) = self.path(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.view.resolve(&path) {
            Ok(Some(Node::Directory)) => reply.opened(FileHandle(0), FopenFlags::empty()),
            Ok(Some(Node::File { .. })) => reply.error(Errno::ENOTDIR),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(path) = self.path(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let Some(entries) = (match self.view.list(&path) {
            Ok(entries) => entries,
            Err(_) => {
                reply.error(Errno::EIO);
                return;
            }
        }) else {
            reply.error(Errno::ENOENT);
            return;
        };
        let mut inodes = self.inodes.lock().expect("inode lock poisoned");
        let parent = inodes.parent(&path);
        let mut listing = vec![
            (".".to_owned(), ino, FileType::Directory),
            ("..".to_owned(), parent, FileType::Directory),
        ];
        for (name, node) in entries {
            let child = inodes.insert(child_path(&path, &name));
            let kind = match node {
                Node::Directory => FileType::Directory,
                Node::File { .. } => FileType::RegularFile,
            };
            listing.push((name, child, kind));
        }
        drop(inodes);
        for (index, (name, ino, kind)) in listing.into_iter().enumerate().skip(offset as usize) {
            if reply.add(ino, (index + 1) as u64, kind, name) {
                break;
            }
        }
        reply.ok();
    }
    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if flags.0 & libc::O_ACCMODE != libc::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        let Some(path) = self.path(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.view.resolve(&path) {
            Ok(Some(Node::File { .. })) => reply.opened(FileHandle(0), FopenFlags::empty()),
            Ok(Some(Node::Directory)) => reply.error(Errno::EISDIR),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let Some(path) = self.path(ino) else {
            reply.error(Errno::ENOENT);
            return;
        };
        match self.view.resolve(&path) {
            Ok(Some(Node::File { hash, .. })) => match self.view.read(&hash, offset, size) {
                Ok(bytes) => reply.data(&bytes),
                Err(_) => reply.error(Errno::EIO),
            },
            Ok(Some(Node::Directory)) => reply.error(Errno::EISDIR),
            Ok(None) => reply.error(Errno::ENOENT),
            Err(_) => reply.error(Errno::EIO),
        }
    }
    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let Ok(root) = CString::new(self.root.as_os_str().as_bytes()) else {
            reply.error(Errno::EIO);
            return;
        };
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        if unsafe { libc::statvfs(root.as_ptr(), stat.as_mut_ptr()) } != 0 {
            reply.error(Errno::EIO);
            return;
        }
        let stat = unsafe { stat.assume_init() };
        let Ok(documents) = self.view.document_count() else {
            reply.error(Errno::EIO);
            return;
        };
        reply.statfs(
            stat.f_blocks,
            stat.f_bfree,
            stat.f_bavail,
            documents,
            stat.f_ffree,
            stat.f_bsize as u32,
            stat.f_namemax as u32,
            stat.f_frsize as u32,
        );
    }
}

pub fn mount(root: &Path, mountpoint: &Path) -> Result<()> {
    let mut config = Config::default();
    config.mount_options = vec![MountOption::RO, MountOption::FSName("transfs".into())];
    config.n_threads = Some(1);
    fuser::mount(QueryFs::new(root)?, mountpoint, &config)?;
    Ok(())
}
