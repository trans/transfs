//! Chunked storage (docs/chunked-storage.md): files stored as chunks read back
//! exactly, versions share chunks, and simple stores keep whole blobs.
use merkle_champ::pack::Index;
use std::fs;
use tempfile::TempDir;
use transfs::{
    check::check_with,
    config::{Chunking, StoreConfig},
    content::Stored,
    library::Library,
    objects::ObjectStore,
    rep,
};

fn source(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

/// About `n` bytes of text from a small vocabulary: compressible, like most
/// files worth chunking.
fn text(n: usize, mut seed: u64) -> Vec<u8> {
    let words = [
        "tent",
        "ring",
        "clown",
        "juggle",
        "popcorn",
        "ticket",
        "acrobat",
        "lion",
        "drum",
        "rope",
        "trapeze",
        "spotlight",
    ];
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        out.extend_from_slice(words[(seed % words.len() as u64) as usize].as_bytes());
        out.push(if seed.is_multiple_of(9) { b'\n' } else { b' ' });
    }
    out
}

fn packs(lib: &Library) -> Vec<String> {
    ObjectStore::new(&lib.root).pack_names().unwrap()
}

fn pack_index(lib: &Library, name: &str) -> Index {
    let bytes = fs::read(ObjectStore::new(&lib.root).pack_path(name)).unwrap();
    Index::parse(&bytes).unwrap()
}

#[test]
fn a_large_file_is_stored_as_chunks_and_reads_back() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let bytes = text(1 << 20, 1);
    let doc = lib.add(&source(&dir, "big.txt", &bytes), None).unwrap();
    let hash = doc.head().unwrap().to_owned();

    assert!(!lib.cas.exists(&hash), "chunked content has no whole blob");
    let reps = rep::read(&lib.root, &hash).unwrap();
    assert_eq!(reps.len(), 1);
    assert_eq!(reps[0].length, bytes.len() as u64);
    assert_eq!(reps[0].chunking, "fastcdc-2020/2048/8192/32768");
    assert_eq!(lib.read(&doc).unwrap().unwrap(), bytes);
    assert!(matches!(
        lib.content.describe(&hash).unwrap(),
        Some(Stored::Chunks { packs: 1, .. })
    ));
    let names = packs(&lib);
    assert_eq!(names.len(), 1);
    // Text compresses, so a native build's pack holds zstd members. Builds
    // without the native feature store chunks raw.
    #[cfg(feature = "native")]
    {
        assert!(pack_index(&lib, &names[0]).is_encoded());
        let size = fs::metadata(ObjectStore::new(&lib.root).pack_path(&names[0]))
            .unwrap()
            .len();
        assert!(size < bytes.len() as u64 / 2);
    }
}

#[test]
fn a_small_edit_adds_a_few_objects() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let original = text(1 << 20, 2);
    let mut edited = original.clone();
    edited.splice(500_000..500_000, *b"a small insertion in the middle ");
    let doc = lib.add(&source(&dir, "v1.txt", &original), None).unwrap();
    let first = packs(&lib);
    let doc = lib
        .add_version(&doc, &source(&dir, "v2.txt", &edited))
        .unwrap();

    let second: Vec<_> = packs(&lib)
        .into_iter()
        .filter(|p| !first.contains(p))
        .collect();
    assert_eq!(second.len(), 1);
    let new_objects = pack_index(&lib, &second[0]).len();
    assert!(
        new_objects < 20,
        "{new_objects} new objects for one insertion"
    );
    assert_eq!(lib.read(&doc).unwrap().unwrap(), edited);
    let v1 = doc.versions.iter().find(|v| v.parents.is_empty()).unwrap();
    assert_eq!(lib.read_version(&doc, &v1.id).unwrap().unwrap(), original);
    // The new version's record names both packs: it reuses the first's chunks.
    let head = rep::read(&lib.root, doc.head().unwrap()).unwrap();
    assert_eq!(head[0].packs.len(), 2);
}

#[cfg(feature = "native")]
#[test]
fn sqlite_databases_are_cut_into_pages() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let db_path = dir.path().join("app.sqlite");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT);")
        .unwrap();
    let body = String::from_utf8(text(300, 3)).unwrap();
    for _ in 0..2000 {
        db.execute("INSERT INTO notes (body) VALUES (?)", [&body])
            .unwrap();
    }
    drop(db);
    let original = fs::read(&db_path).unwrap();
    let doc = lib.add(&db_path, None).unwrap();
    let rec = &rep::read(&lib.root, doc.head().unwrap()).unwrap()[0];
    assert_eq!(rec.chunking, "fixed/4096");

    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute("UPDATE notes SET body = 'changed' WHERE id = 1000", [])
        .unwrap();
    drop(db);
    let edited = fs::read(&db_path).unwrap();
    let before = packs(&lib);
    let doc = lib.add_version(&doc, &db_path).unwrap();
    let new: Vec<_> = packs(&lib)
        .into_iter()
        .filter(|p| !before.contains(p))
        .collect();
    // One row changed: a few pages, plus the list nodes above them.
    assert!(pack_index(&lib, &new[0]).len() < 12);
    assert_eq!(lib.read(&doc).unwrap().unwrap(), edited);
    let v1 = doc.versions.iter().find(|v| v.parents.is_empty()).unwrap();
    assert_eq!(lib.read_version(&doc, &v1.id).unwrap().unwrap(), original);
}

#[test]
fn chunking_off_keeps_whole_blobs() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let mut config = StoreConfig::default();
    config.chunking = Chunking::Off;
    config.save(&lib.root).unwrap();
    assert_eq!(
        StoreConfig::load(&lib.root).unwrap().chunking,
        Chunking::Off
    );

    let bytes = text(1 << 20, 4);
    let doc = lib.add(&source(&dir, "big.txt", &bytes), None).unwrap();
    assert!(lib.cas.exists(doc.head().unwrap()));
    assert!(packs(&lib).is_empty());
    assert!(!rep::reps_dir(&lib.root).exists());
    assert_eq!(lib.read(&doc).unwrap().unwrap(), bytes);
}

#[test]
fn small_and_already_compressed_files_stay_whole() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let small = lib
        .add(&source(&dir, "small.txt", &text(1000, 5)), None)
        .unwrap();
    let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
    webp.extend(text(200_000, 6));
    let image = lib.add(&source(&dir, "image.webp", &webp), None).unwrap();
    assert!(lib.cas.exists(small.head().unwrap()));
    assert!(lib.cas.exists(image.head().unwrap()));
    assert!(packs(&lib).is_empty());
}

#[test]
fn identical_content_is_stored_once() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let bytes = text(300_000, 7);
    let a = lib.add(&source(&dir, "a.txt", &bytes), None).unwrap();
    let b = lib.add(&source(&dir, "b.txt", &bytes), None).unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(a.head(), b.head());
    assert_eq!(packs(&lib).len(), 1);
    assert_eq!(rep::read(&lib.root, a.head().unwrap()).unwrap().len(), 1);
}

#[test]
fn check_finds_damaged_packs_and_missing_records() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let doc = lib
        .add(&source(&dir, "big.txt", &text(1 << 20, 8)), None)
        .unwrap();
    let result = check_with(&lib.root, true).unwrap();
    assert!(result.clean(), "{:?}", result.errors);
    assert_eq!((result.chunked, result.packs), (1, 1));

    // Flip one byte in the pack's data.
    let pack = ObjectStore::new(&lib.root).pack_path(&packs(&lib)[0]);
    let mut bytes = fs::read(&pack).unwrap();
    let last = bytes.len() - 100;
    bytes[last] ^= 0xFF;
    fs::write(&pack, &bytes).unwrap();
    let result = check_with(&lib.root, true).unwrap();
    assert!(result
        .errors
        .iter()
        .any(|e| e.message.contains("pack hash mismatch")));

    // Without its record, the version's content is missing.
    let hash = doc.head().unwrap();
    fs::remove_dir_all(rep::reps_dir(&lib.root).join(&hash[..2]).join(hash)).unwrap();
    let result = check_with(&lib.root, false).unwrap();
    assert!(result
        .errors
        .iter()
        .any(|e| e.message.contains("missing blob or chunks")));
}

#[test]
fn only_a_deep_check_catches_a_record_for_the_wrong_bytes() {
    // A record whose list loads, whose chunks all exist and whose length is
    // right, but which describes other content. Only rebuilding the content
    // and comparing its hash can tell.
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let n = 1 << 20;
    let a = lib
        .add(&source(&dir, "a.txt", &text(n, 13)[..n]), None)
        .unwrap();
    let b = lib
        .add(&source(&dir, "b.txt", &text(n, 14)[..n]), None)
        .unwrap();
    let (a_hash, b_hash) = (a.head().unwrap(), b.head().unwrap());
    let b_rep = rep::read(&lib.root, b_hash).unwrap().remove(0);
    fs::remove_dir_all(rep::reps_dir(&lib.root).join(&a_hash[..2]).join(a_hash)).unwrap();
    rep::write(
        &lib.root,
        &rep::Rep {
            content: a_hash.to_owned(),
            ..b_rep
        },
    )
    .unwrap();

    let shallow = check_with(&lib.root, false).unwrap();
    assert!(shallow.clean(), "{:?}", shallow.errors);
    let deep = check_with(&lib.root, true).unwrap();
    assert!(deep
        .errors
        .iter()
        .any(|e| e.message.contains("does not match its hash")));
    assert!(lib.read(&a).is_err(), "reading checks the whole hash too");
}

#[cfg(feature = "native")]
#[test]
fn the_index_reports_size_and_type_for_chunked_content() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let bytes = text(1 << 20, 10);
    lib.add(&source(&dir, "big.txt", &bytes), None).unwrap();
    let rows = transfs::index::Index::open(&lib.root)
        .unwrap()
        .all()
        .unwrap();
    assert_eq!(rows[0].size, Some(bytes.len() as i64));
    assert_eq!(rows[0].mime_type.as_deref(), Some("text/plain"));
}

#[cfg(feature = "native")]
#[test]
fn the_mount_reads_chunked_content_across_chunk_boundaries() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let bytes = text(1 << 20, 11);
    let doc = lib.add(&source(&dir, "big.txt", &bytes), None).unwrap();
    let hash = doc.head().unwrap();
    let view = transfs::mount::MountView::new(&lib.root).unwrap();
    for (offset, size) in [
        (0, 4096),
        (8000, 50_000),
        (131_072, 131_072),
        ((1 << 20) - 10, 4096),
    ] {
        let got = view.read(hash, offset, size).unwrap();
        let end = (offset as usize + size as usize).min(bytes.len());
        assert_eq!(got, bytes[offset as usize..end], "offset {offset}");
    }
    assert!(view.read(hash, 1 << 21, 10).unwrap().is_empty());
}

#[test]
fn publish_uploads_chunked_versions_as_whole_blobs() {
    use transfs::{remote::DirectoryRemote, replica};
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let bytes = text(1 << 20, 12);
    let doc = lib.add(&source(&dir, "big.txt", &bytes), None).unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    replica::publish(&lib, &remote, None).unwrap();
    let restored = dir.path().join("restored");
    replica::recover(&remote, &restored).unwrap();
    let restored = Library::new(&restored);
    let copy = restored.document(&doc.id).unwrap().unwrap();
    assert!(restored.cas.exists(copy.head().unwrap()));
    assert_eq!(restored.read(&copy).unwrap().unwrap(), bytes);
}
