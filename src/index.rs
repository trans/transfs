//! Disposable native SQLite index. The claim log and CAS remain the source of truth.
use crate::{
    claim::format_ts,
    content::Content,
    document::Document,
    log::{Log, TornTail},
    Error, Result,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, params_from_iter, Connection, Params};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{c_char, c_int, c_void, CStr, CString},
    fs,
    path::{Path, PathBuf},
};

#[link(name = "magic")]
unsafe extern "C" {
    fn magic_open(flags: c_int) -> *mut c_void;
    fn magic_load(cookie: *mut c_void, filename: *const c_char) -> c_int;
    fn magic_file(cookie: *mut c_void, filename: *const c_char) -> *const c_char;
    fn magic_buffer(cookie: *mut c_void, buffer: *const c_void, length: usize) -> *const c_char;
    fn magic_error(cookie: *mut c_void) -> *const c_char;
    fn magic_close(cookie: *mut c_void);
}

struct Magic(*mut c_void);
// Index keeps the handle behind a Mutex in the mount. libmagic is never called
// concurrently through this handle, and the handle is closed after the Index.
unsafe impl Send for Magic {}
impl Magic {
    fn open() -> Result<Self> {
        let ptr = unsafe { magic_open(0x10) }; // MAGIC_MIME_TYPE
        if ptr.is_null() {
            return Err(Error::Native("magic_open failed".into()));
        }
        let this = Self(ptr);
        if unsafe { magic_load(ptr, std::ptr::null()) } != 0 {
            return Err(Error::Native(this.error()));
        }
        Ok(this)
    }
    fn error(&self) -> String {
        let ptr = unsafe { magic_error(self.0) };
        if ptr.is_null() {
            "libmagic error".into()
        } else {
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned()
        }
    }
    fn file(&self, path: &Path) -> Option<String> {
        let name = CString::new(path.to_string_lossy().as_bytes()).ok()?;
        Self::answer(unsafe { magic_file(self.0, name.as_ptr()) })
    }
    fn buffer(&self, bytes: &[u8]) -> Option<String> {
        Self::answer(unsafe { magic_buffer(self.0, bytes.as_ptr().cast(), bytes.len()) })
    }
    fn answer(ptr: *const c_char) -> Option<String> {
        if ptr.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }
}
impl Drop for Magic {
    fn drop(&mut self) {
        unsafe { magic_close(self.0) };
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub name: Option<String>,
    pub names: Vec<String>,
    pub mime_type: Option<String>,
    pub size: Option<i64>,
    pub version_count: i64,
    pub date_added: String,
    pub head_hash: Option<String>,
    pub heads: Vec<HeadRow>,
    pub tags: Vec<String>,
    pub tag_conflicts: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadRow {
    pub id: String,
    pub hash: String,
    pub size: Option<i64>,
    pub ts: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walk {
    pub constraints: Vec<String>,
    pub partial: String,
    pub valid: bool,
}

pub struct Index {
    root: PathBuf,
    content: Content,
    conn: Connection,
    magic: Magic,
    pub rebuild_errors: Vec<String>,
    pub rebuild_warnings: Vec<TornTail>,
}

const FIELDS: &str =
    "d.id,d.name,d.type,d.size,d.version_count,d.date_added,d.head_hash,d.tag_conflicts";

impl Index {
    pub fn db_path(root: &Path) -> PathBuf {
        root.join(".transfs/index.db")
    }
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let db_path = Self::db_path(&root);
        let existed = db_path.exists();
        fs::create_dir_all(db_path.parent().expect("index path has parent"))?;
        let conn = Connection::open(db_path)?;
        let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let mut this = Self {
            content: Content::new(&root),
            root,
            conn,
            magic: Magic::open()?,
            rebuild_errors: vec![],
            rebuild_warnings: vec![],
        };
        this.ensure_schema()?;
        if !existed || schema_version < 4 {
            this.rebuild()?;
            if !this.rebuild_errors.is_empty() {
                return Err(Error::InvalidClaim(format!(
                    "index rebuild rejected claim logs: {}",
                    this.rebuild_errors.join("; ")
                )));
            }
            this.conn.execute_batch("PRAGMA user_version=4")?;
        }
        Ok(this)
    }
    fn ensure_schema(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 4 {
            self.conn.execute_batch(
                "DROP TABLE IF EXISTS documents;
                DROP TABLE IF EXISTS versions; DROP TABLE IF EXISTS doc_names;
                DROP TABLE IF EXISTS doc_heads; DROP TABLE IF EXISTS doc_tags;
                DROP TABLE IF EXISTS membership; DROP TABLE IF EXISTS blob_refs;",
            )?;
        }
        self.conn.execute_batch(
            "\
            CREATE TABLE IF NOT EXISTS documents (
                id TEXT PRIMARY KEY, head_hash TEXT, name TEXT, type TEXT, size INTEGER,
                is_collection INTEGER NOT NULL DEFAULT 0, owner TEXT NOT NULL DEFAULT 'local',
                source TEXT, date_added TEXT NOT NULL, date_content TEXT,
                version_count INTEGER NOT NULL DEFAULT 0,
                tag_conflicts TEXT NOT NULL DEFAULT '[]');
            CREATE TABLE IF NOT EXISTS versions (
                doc_id TEXT NOT NULL, id TEXT NOT NULL, hash TEXT NOT NULL,
                parents TEXT NOT NULL, ts TEXT NOT NULL, size INTEGER, type TEXT,
                PRIMARY KEY(doc_id,id));
            CREATE TABLE IF NOT EXISTS doc_names (
                doc_id TEXT NOT NULL, id TEXT NOT NULL, name TEXT NOT NULL,
                PRIMARY KEY(doc_id,id));
            CREATE TABLE IF NOT EXISTS doc_heads (
                doc_id TEXT NOT NULL, id TEXT NOT NULL, hash TEXT NOT NULL,
                size INTEGER, type TEXT, ts TEXT NOT NULL, PRIMARY KEY(doc_id,id));
            CREATE TABLE IF NOT EXISTS doc_tags (
                doc_id TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY(doc_id,path));
            CREATE TABLE IF NOT EXISTS membership (
                coll_id TEXT NOT NULL, member_ref TEXT NOT NULL, name TEXT, kind TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS blob_refs (
                blob_hash TEXT NOT NULL, referrer TEXT NOT NULL, PRIMARY KEY(blob_hash,referrer));
            CREATE INDEX IF NOT EXISTS idx_documents_name_type ON documents(name,type);
            CREATE INDEX IF NOT EXISTS idx_doc_tags_path ON doc_tags(path);
            CREATE INDEX IF NOT EXISTS idx_versions_hash ON versions(hash);
            CREATE INDEX IF NOT EXISTS idx_doc_names_name ON doc_names(name);
            CREATE INDEX IF NOT EXISTS idx_doc_heads_type ON doc_heads(type);
        ",
        )?;
        Ok(())
    }
    pub fn rebuild(&mut self) -> Result<()> {
        self.rebuild_errors.clear();
        self.rebuild_warnings.clear();
        let tx = self.conn.transaction()?;
        for table in [
            "documents",
            "versions",
            "doc_names",
            "doc_heads",
            "doc_tags",
            "membership",
            "blob_refs",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])?;
        }
        for id in Log::all_ids(&self.root)? {
            match Log::new(&self.root, &id).read() {
                Ok(read) => {
                    if let Some(tail) = read.torn_tail {
                        self.rebuild_warnings.push(tail);
                    }
                    match Document::fold(&id, &read.claims) {
                        Ok(doc) => upsert(&tx, &self.content, &self.magic, &doc)?,
                        Err(e) => self.rebuild_errors.push(format!("{id}: {e}")),
                    }
                }
                Err(Error::CorruptLog { path, line, reason }) => {
                    self.rebuild_errors
                        .push(format!("{}:{line}: {reason}", path.display()));
                }
                Err(e) => return Err(e),
            }
        }
        if !self.rebuild_errors.is_empty() {
            tx.rollback()?;
            return Ok(());
        }
        tx.commit()?;
        Ok(())
    }
    pub fn index_document(&mut self, doc: &Document) -> Result<()> {
        let tx = self.conn.transaction()?;
        upsert(&tx, &self.content, &self.magic, doc)?;
        tx.commit()?;
        Ok(())
    }
    fn query_rows<P: Params>(&self, sql: &str, params: P) -> Result<Vec<Row>> {
        let mut stmt = self.conn.prepare(sql)?;
        let iter = stmt.query_map(params, |r| {
            let conflicts: String = r.get(7)?;
            Ok(Row {
                id: r.get(0)?,
                name: r.get(1)?,
                names: vec![],
                mime_type: r.get(2)?,
                size: r.get(3)?,
                version_count: r.get(4)?,
                date_added: r.get(5)?,
                head_hash: r.get(6)?,
                heads: vec![],
                tags: vec![],
                tag_conflicts: serde_json::from_str(&conflicts).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        7,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            })
        })?;
        let mut rows: Vec<Row> = iter.collect::<rusqlite::Result<_>>()?;
        for row in &mut rows {
            row.tags = self.tags_for(&row.id)?;
            row.names = self.names_for(&row.id)?;
            row.heads = self.heads_for(&row.id)?;
        }
        Ok(rows)
    }
    fn tags_for(&self, id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM doc_tags WHERE doc_id=? ORDER BY path")?;
        let rows = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }
    fn names_for(&self, id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name FROM doc_names WHERE doc_id=? ORDER BY id")?;
        let names = stmt
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(names)
    }
    fn heads_for(&self, id: &str) -> Result<Vec<HeadRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id,hash,size,ts FROM doc_heads WHERE doc_id=? ORDER BY id")?;
        let heads = stmt
            .query_map([id], |r| {
                let ts: String = r.get(3)?;
                let ts = DateTime::parse_from_rfc3339(&ts)
                    .map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?
                    .with_timezone(&Utc);
                Ok(HeadRow {
                    id: r.get(0)?,
                    hash: r.get(1)?,
                    size: r.get(2)?,
                    ts,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(heads)
    }
    pub fn all(&self) -> Result<Vec<Row>> {
        self.query_rows(
            &format!("SELECT {FIELDS} FROM documents d ORDER BY d.date_added"),
            [],
        )
    }
    pub fn by_tag(&self, key: &str) -> Result<Vec<Row>> {
        let key = key.replace('=', "/");
        let k = escape_like(&key);
        self.query_rows(
            &format!(
                "SELECT {FIELDS} FROM documents d WHERE EXISTS \
            (SELECT 1 FROM doc_tags t WHERE t.doc_id=d.id AND \
            (t.path=?1 OR t.path LIKE ?2 ESCAPE '\\' OR t.path LIKE ?3 ESCAPE '\\' OR \
            t.path LIKE ?4 ESCAPE '\\')) ORDER BY d.date_added"
            ),
            params![key, format!("{k}/%"), format!("%/{k}"), format!("%/{k}/%")],
        )
    }
    pub fn by_type(&self, prefix: &str) -> Result<Vec<Row>> {
        self.query_rows(
            &format!("SELECT {FIELDS} FROM documents d WHERE EXISTS (SELECT 1 FROM doc_heads h WHERE h.doc_id=d.id AND h.type LIKE ?) ORDER BY d.date_added"),
            [format!("{prefix}%")],
        )
    }
    pub fn by_name(&self, substr: &str) -> Result<Vec<Row>> {
        self.query_rows(
            &format!("SELECT {FIELDS} FROM documents d WHERE EXISTS (SELECT 1 FROM doc_names n WHERE n.doc_id=d.id AND n.name LIKE ?) ORDER BY d.date_added"),
            [format!("%{substr}%")],
        )
    }
    pub fn neighborhood(&self, name: &str, mime_type: Option<&str>) -> Result<Vec<Row>> {
        if let Some(mime_type) = mime_type {
            self.query_rows(
                &format!("SELECT {FIELDS} FROM documents d WHERE EXISTS (SELECT 1 FROM doc_names n WHERE n.doc_id=d.id AND n.name=?) AND EXISTS (SELECT 1 FROM doc_heads h WHERE h.doc_id=d.id AND h.type=?)"),
                params![name, mime_type],
            )
        } else {
            self.query_rows(
                &format!("SELECT {FIELDS} FROM documents d WHERE EXISTS (SELECT 1 FROM doc_names n WHERE n.doc_id=d.id AND n.name=?)"),
                [name],
            )
        }
    }
    fn tag_prefix_exists(&self, prefix: &str) -> Result<bool> {
        if prefix.is_empty() {
            return Ok(true);
        }
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM doc_tags WHERE path=? OR path LIKE ? ESCAPE '\\')",
            params![prefix, format!("{}/%", escape_like(prefix))],
            |r| r.get::<_, i64>(0),
        )? != 0)
    }
    fn tag_complete(&self, path: &str) -> Result<bool> {
        if path.is_empty() {
            return Ok(false);
        }
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM doc_tags WHERE path=?)",
            [path],
            |r| r.get::<_, i64>(0),
        )? != 0)
    }
    pub fn walk(&self, components: &[String]) -> Result<Walk> {
        let mut walk = Walk {
            constraints: vec![],
            partial: String::new(),
            valid: true,
        };
        for component in components {
            let candidate = if walk.partial.is_empty() {
                component.clone()
            } else {
                format!("{}/{component}", walk.partial)
            };
            if self.tag_prefix_exists(&candidate)? {
                walk.partial = candidate;
            } else {
                if !walk.partial.is_empty() {
                    walk.constraints.push(std::mem::take(&mut walk.partial));
                }
                walk.partial = component.clone();
                if !self.tag_prefix_exists(component)? {
                    walk.valid = false;
                }
            }
            if self.tag_complete(&walk.partial)? {
                walk.constraints.push(std::mem::take(&mut walk.partial));
            }
        }
        Ok(walk)
    }
    fn matching_rows(&self, walk: &Walk) -> Result<Vec<Row>> {
        let mut sql = format!("SELECT {FIELDS} FROM documents d");
        let mut values = walk.constraints.clone();
        if !walk.partial.is_empty() {
            values.push(walk.partial.clone());
        }
        if !values.is_empty() {
            sql.push_str(" WHERE ");
            let clauses: Vec<_> = values.iter().map(|_| "EXISTS (SELECT 1 FROM doc_tags t WHERE t.doc_id=d.id AND (t.path=? OR t.path LIKE ? ESCAPE '\\'))").collect();
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY d.date_added DESC");
        let binds: Vec<String> = values
            .iter()
            .flat_map(|pre| [pre.clone(), format!("{}/%", escape_like(pre))])
            .collect();
        self.query_rows(&sql, params_from_iter(binds.iter()))
    }
    pub fn docs(&self, walk: &Walk, limit: Option<usize>) -> Result<Vec<Row>> {
        let mut rows = self.matching_rows(walk)?;
        if let Some(limit) = limit {
            rows.truncate(limit);
        }
        Ok(rows)
    }
    pub fn facets(&self, walk: &Walk) -> Result<Vec<String>> {
        let rows = self.matching_rows(walk)?;
        if rows.is_empty() {
            return Ok(vec![]);
        }
        let total = rows.len();
        let depth = if walk.partial.is_empty() {
            0
        } else {
            walk.partial.split('/').count()
        };
        let mut docs_by_comp: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut deeper_by_comp: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for row in &rows {
            for path in &row.tags {
                if !walk.partial.is_empty()
                    && path != &walk.partial
                    && !path.starts_with(&format!("{}/", walk.partial))
                {
                    continue;
                }
                let components: Vec<_> = path.split('/').collect();
                if components.len() <= depth {
                    continue;
                }
                let component = components[depth].to_owned();
                docs_by_comp
                    .entry(component.clone())
                    .or_default()
                    .insert(row.id.clone());
                if let Some(next) = components.get(depth + 1) {
                    deeper_by_comp
                        .entry(component)
                        .or_default()
                        .insert((*next).into());
                }
            }
        }
        let splitting: Vec<_> = docs_by_comp
            .iter()
            .filter(|(component, docs)| {
                docs.len() < total
                    || deeper_by_comp
                        .get(*component)
                        .is_some_and(|values| values.len() > 1)
            })
            .map(|(component, _)| component.clone())
            .collect();
        if splitting.is_empty() {
            Ok(docs_by_comp.into_keys().collect())
        } else {
            Ok(splitting)
        }
    }
}

fn upsert(conn: &Connection, content: &Content, magic: &Magic, doc: &Document) -> Result<()> {
    for (table, col) in [
        ("documents", "id"),
        ("versions", "doc_id"),
        ("doc_names", "doc_id"),
        ("doc_heads", "doc_id"),
        ("doc_tags", "doc_id"),
        ("membership", "coll_id"),
        ("blob_refs", "referrer"),
    ] {
        conn.execute(&format!("DELETE FROM {table} WHERE {col}=?"), [&doc.id])?;
    }
    let head = doc.head();
    let head_size = head.and_then(|hash| blob_size(content, hash));
    let head_type = head.and_then(|hash| type_for(content, magic, hash));
    let added = format_ts(doc.created_at);
    let content_date = doc
        .versions
        .iter()
        .map(|version| version.ts)
        .max()
        .map(format_ts);
    conn.execute("INSERT INTO documents (id,head_hash,name,type,size,is_collection,owner,source,date_added,date_content,version_count,tag_conflicts) \
        VALUES (?1,?2,?3,?4,?5,0,'local',NULL,?6,?7,?8,?9)",
        params![doc.id, head, doc.name, head_type, head_size, added, content_date, doc.version_count() as i64,
            serde_json::to_string(&doc.tag_conflicts).expect("tag conflicts serialize")])?;
    for name in &doc.names {
        conn.execute(
            "INSERT INTO doc_names (doc_id,id,name) VALUES (?,?,?)",
            params![doc.id, name.id, name.value],
        )?;
    }
    for head in &doc.heads {
        conn.execute(
            "INSERT INTO doc_heads (doc_id,id,hash,size,type,ts) VALUES (?,?,?,?,?,?)",
            params![
                doc.id,
                head.id,
                head.hash,
                blob_size(content, &head.hash),
                type_for(content, magic, &head.hash),
                format_ts(head.ts)
            ],
        )?;
    }
    for version in &doc.versions {
        conn.execute(
            "INSERT INTO versions (doc_id,id,hash,parents,ts,size,type) VALUES (?,?,?,?,?,?,?)",
            params![
                doc.id,
                version.id,
                version.hash,
                serde_json::to_string(&version.parents).expect("parents serialize"),
                format_ts(version.ts),
                blob_size(content, &version.hash),
                type_for(content, magic, &version.hash)
            ],
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO blob_refs (blob_hash,referrer) VALUES (?,?)",
            params![version.hash, doc.id],
        )?;
    }
    let mut paths = BTreeSet::new();
    for head in &doc.heads {
        if let Some(mime) = type_for(content, magic, &head.hash) {
            paths.insert(format!("type/{mime}"));
        }
    }
    paths.insert("owner/local".into());
    for tag in &doc.tags {
        let normalized = tag.replace('=', "/");
        paths.insert(if normalized.contains('/') {
            normalized
        } else {
            format!("tag/{normalized}")
        });
    }
    for path in paths {
        conn.execute(
            "INSERT OR IGNORE INTO doc_tags (doc_id,path) VALUES (?,?)",
            params![doc.id, path],
        )?;
    }
    Ok(())
}

fn blob_size(content: &Content, hash: &str) -> Option<i64> {
    content.size(hash).ok().flatten().map(|size| size as i64)
}
/// A whole blob is recognized from its file; chunked content from its first
/// 64 KiB, which is all libmagic needs.
fn type_for(content: &Content, magic: &Magic, hash: &str) -> Option<String> {
    let path = content.cas().path_for(hash);
    if path.exists() {
        return magic.file(&path);
    }
    if !content.exists(hash).ok()? {
        return None;
    }
    magic.buffer(&content.head(hash, 64 << 10).ok()?)
}
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}
