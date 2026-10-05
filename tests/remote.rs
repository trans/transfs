use sha2::{Digest, Sha256};
use std::{
    fs,
    sync::{Arc, Barrier},
    thread,
};
use tempfile::TempDir;
use transfs::{
    check::check,
    document::Document,
    library::Library,
    pack,
    remote::{DirectoryRemote, RemoteStore},
    replica::{fork_writer, publish, recover, WriterRef},
};

fn file(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn two_writers_recover_forks_from_an_empty_store() {
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    let laptop = Library::new(dir.path().join("laptop"));
    let base_bytes = file(&dir, "base", b"base\n");
    let left_bytes = file(&dir, "left", b"left\n");
    let right_bytes = file(&dir, "right", b"right\n");
    let base = laptop.add(&base_bytes, Some("notes.txt")).unwrap();
    let first = publish(&laptop, &remote, Some("laptop")).unwrap();
    assert_eq!(first.sequence, 1);
    assert!(first.changed);
    assert_eq!(first.writer.len(), 26);
    assert!(!publish(&laptop, &remote, None).unwrap().changed);

    let desktop_path = dir.path().join("desktop");
    let restored = recover(&remote, &desktop_path).unwrap();
    assert_eq!(
        (restored.writers, restored.documents, restored.blobs),
        (1, 1, 1)
    );
    let desktop = Library::new(&desktop_path);
    assert_eq!(desktop.document(&base.id).unwrap().unwrap(), base);

    laptop.add_version(&base, &left_bytes).unwrap();
    laptop.rename(&base, "left.txt").unwrap();
    desktop.add_version(&base, &right_bytes).unwrap();
    desktop.rename(&base, "right.txt").unwrap();
    let second = publish(&laptop, &remote, None).unwrap();
    assert_eq!(second.sequence, 2);
    assert_eq!(first.writer, second.writer);
    let desktop_ref = publish(&desktop, &remote, Some("desktop")).unwrap();
    assert_ne!(desktop_ref.writer, first.writer);

    let recovered_path = dir.path().join("recovered");
    let restored = recover(&remote, &recovered_path).unwrap();
    assert_eq!(
        (restored.writers, restored.documents, restored.blobs),
        (2, 1, 3)
    );
    let recovered = Library::new(&recovered_path);
    let doc = recovered.document(&base.id).unwrap().unwrap();
    assert_eq!(doc.heads.len(), 2);
    assert_eq!(doc.names.len(), 2);
    let contents: std::collections::BTreeSet<_> = doc
        .heads
        .iter()
        .map(|head| recovered.read_version(&doc, &head.id).unwrap().unwrap())
        .collect();
    assert_eq!(contents, [b"left\n".to_vec(), b"right\n".to_vec()].into());
    assert!(check(&recovered_path).unwrap().clean());

    let laptop_refs = remote.refs_for(&first.writer).unwrap();
    let first_ref: WriterRef = serde_json::from_slice(&laptop_refs[0].1).unwrap();
    let second_ref: WriterRef = serde_json::from_slice(&laptop_refs[1].1).unwrap();
    assert_eq!(second_ref.packs.len(), 2);
    assert_eq!(second_ref.packs[0], first_ref.packs[0]);
    let first_pack = remote.get_pack(&first_ref.packs[0]).unwrap();
    let next_pack = remote.get_pack(&second_ref.packs[1]).unwrap();
    assert!(!pack::decode(&first_pack).unwrap().is_empty());
    assert!(!pack::decode(&next_pack).unwrap().is_empty());
}

#[test]
fn incomplete_or_corrupt_publication_cannot_create_a_recovered_store() {
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    remote.put_pack(b"orphan pack").unwrap();
    let absent = dir.path().join("absent");
    assert!(recover(&remote, &absent).is_err());
    assert!(!absent.exists());

    let lib = Library::new(dir.path().join("working"));
    let path = file(&dir, "body", b"body");
    lib.add(&path, Some("body.txt")).unwrap();
    let writer = publish(&lib, &remote, None).unwrap().writer;
    let published: WriterRef =
        serde_json::from_slice(&remote.refs_for(&writer).unwrap()[0].1).unwrap();
    let pack_path = remote.root().join("packs").join(&published.packs[0]);
    let original_pack = fs::read(&pack_path).unwrap();
    fs::write(pack_path, b"corrupt").unwrap();
    let target = dir.path().join("target");
    assert!(recover(&remote, &target).is_err());
    assert!(!target.exists());

    fs::write(
        remote.root().join("packs").join(&published.packs[0]),
        original_pack,
    )
    .unwrap();
    let doc = lib.documents().unwrap().remove(0);
    let hash = doc.head().unwrap();
    fs::remove_file(remote.root().join("blobs").join(&hash[..2]).join(hash)).unwrap();
    assert!(recover(&remote, &target).is_err());
    assert!(!target.exists());
}

#[test]
fn damaged_content_addressed_objects_are_replaced_from_verified_bytes() {
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    let body = b"recoverable bytes";
    let hash = hex::encode(Sha256::digest(body));
    remote.put_blob(&hash, body).unwrap();
    let blob_path = remote.root().join("blobs").join(&hash[..2]).join(&hash);
    fs::write(&blob_path, []).unwrap(); // abandoned reservation or damaged object
    assert!(remote.get_blob(&hash).is_err());
    remote.put_blob(&hash, body).unwrap();
    assert_eq!(remote.get_blob(&hash).unwrap(), body);

    let pack = b"pack bytes";
    let pack_id = remote.put_pack(pack).unwrap();
    fs::write(remote.root().join("packs").join(&pack_id), []).unwrap();
    assert!(remote.get_pack(&pack_id).is_err());
    assert_eq!(remote.put_pack(pack).unwrap(), pack_id);
    assert_eq!(remote.get_pack(&pack_id).unwrap(), pack);
}

#[test]
fn a_pack_index_cannot_point_outside_its_object_bytes() {
    let bytes = b"node".to_vec();
    let identity: [u8; 32] = Sha256::digest(&bytes).into();
    let mut packed = pack::encode(&[(identity, bytes.clone())]).unwrap();
    assert_eq!(pack::decode(&packed).unwrap(), vec![(identity, bytes)]);
    let index_offset = u64::from_be_bytes(packed[12..20].try_into().unwrap()) as usize;
    packed[index_offset + 32..index_offset + 40].copy_from_slice(&0_u64.to_be_bytes());
    assert!(pack::decode(&packed).is_err());
}

#[test]
fn competing_publishers_cannot_replace_a_ref() {
    let dir = TempDir::new().unwrap();
    let remote = Arc::new(DirectoryRemote::new(dir.path().join("remote")));
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|n| {
            let remote = remote.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                remote.publish_ref("shared", 1, format!("candidate {n}").as_bytes())
            })
        })
        .collect();
    let outcomes: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect();
    assert_eq!(outcomes.into_iter().filter(|won| *won).count(), 1);
    let refs = remote.refs_for("shared").unwrap();
    assert_eq!(refs.len(), 1);
    assert!(refs[0].1 == b"candidate 0" || refs[0].1 == b"candidate 1");
    assert!(remote.publish_ref("shared", 1, &refs[0].1).unwrap());
}

#[test]
fn copied_working_store_cannot_continue_a_writer_after_its_tip_moves() {
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    let first = Library::new(dir.path().join("first"));
    let body = file(&dir, "body", b"body");
    let doc = first.add(&body, Some("first.txt")).unwrap();
    let first_ref = publish(&first, &remote, None).unwrap();
    let second_path = dir.path().join("second");
    copy_tree(&first.root, &second_path);
    let second = Library::new(&second_path);
    second.rename(&doc, "second-renamed.txt").unwrap();
    assert!(publish(&second, &FailBeforeRef(&remote), None).is_err());
    first.rename(&doc, "first-renamed.txt").unwrap();
    publish(&first, &remote, None).unwrap();
    let error = publish(&second, &remote, None).unwrap_err().to_string();
    assert!(
        error.contains("another device is publishing as writer"),
        "{error}"
    );
    assert_eq!(remote.refs_for(&first_ref.writer).unwrap().len(), 2);
    let (_, new_writer) = fork_writer(&second.root, Some("copied device")).unwrap();
    assert_ne!(new_writer, first_ref.writer);
    let fork_ref = publish(&second, &remote, None).unwrap();
    assert_eq!(fork_ref.writer, new_writer);
    let restored = dir.path().join("restored");
    recover(&remote, &restored).unwrap();
    let names = Library::new(restored)
        .document(&doc.id)
        .unwrap()
        .unwrap()
        .names;
    assert_eq!(names.len(), 2);
}

#[test]
fn a_lost_local_ref_update_adopts_the_exact_pending_ref() {
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    let store = Library::new(dir.path().join("working"));
    let body = file(&dir, "body", b"body");
    let doc = store.add(&body, Some("first.txt")).unwrap();
    let first = publish(&store, &remote, None).unwrap();
    let state_path = store.root.join(".transfs/writer.json");
    let old_state = fs::read(&state_path).unwrap();
    store.rename(&doc, "second.txt").unwrap();
    let second = publish(&store, &remote, None).unwrap();
    let second_bytes = remote.refs_for(&first.writer).unwrap()[1].1.clone();
    let mut state: serde_json::Value = serde_json::from_slice(&old_state).unwrap();
    state["pending"] = serde_json::json!({
        "sequence": second.sequence,
        "hash": hex::encode(Sha256::digest(&second_bytes)),
        "json": String::from_utf8(second_bytes).unwrap()
    });
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert!(!publish(&store, &remote, None).unwrap().changed);
    let repaired: serde_json::Value =
        serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
    assert!(repaired["pending"].is_null());
    assert_eq!(repaired["last_ref"], state["pending"]["hash"]);
    assert_eq!(remote.refs_for(&first.writer).unwrap().len(), 2);
}

struct FailBeforeRef<'a>(&'a DirectoryRemote);

impl RemoteStore for FailBeforeRef<'_> {
    fn put_blob(&self, hash: &str, bytes: &[u8]) -> transfs::Result<()> {
        self.0.put_blob(hash, bytes)
    }
    fn get_blob(&self, hash: &str) -> transfs::Result<Vec<u8>> {
        self.0.get_blob(hash)
    }
    fn put_pack(&self, bytes: &[u8]) -> transfs::Result<String> {
        self.0.put_pack(bytes)
    }
    fn get_pack(&self, hash: &str) -> transfs::Result<Vec<u8>> {
        self.0.get_pack(hash)
    }
    fn publish_ref(&self, _: &str, _: u64, _: &[u8]) -> transfs::Result<bool> {
        Err(transfs::Error::Storage("simulated interruption".into()))
    }
    fn writers(&self) -> transfs::Result<Vec<String>> {
        self.0.writers()
    }
    fn refs_for(&self, writer: &str) -> transfs::Result<Vec<(u64, Vec<u8>)>> {
        self.0.refs_for(writer)
    }
}

#[test]
fn pending_ref_is_retried_after_interruption_before_remote_publication() {
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    let store = Library::new(dir.path().join("working"));
    let body = file(&dir, "body", b"body");
    store.add(&body, Some("body.txt")).unwrap();
    assert!(publish(&store, &FailBeforeRef(&remote), None).is_err());
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(store.root.join(".transfs/writer.json")).unwrap())
            .unwrap();
    assert!(state["pending"].is_object());
    let report = publish(&store, &remote, None).unwrap();
    assert!(!report.changed);
    assert_eq!(remote.refs_for(&report.writer).unwrap().len(), 1);
}

fn copy_tree(source: &std::path::Path, target: &std::path::Path) {
    fs::create_dir(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[test]
fn concurrent_local_claim_appends_keep_both_names() {
    let dir = TempDir::new().unwrap();
    let library = Library::new(dir.path().join("working"));
    let body = file(&dir, "body", b"body");
    let base = library.add(&body, Some("base.txt")).unwrap();
    let handles: Vec<_> = ["left.txt", "right.txt"]
        .into_iter()
        .map(|name| {
            let library = library.clone();
            let base = base.clone();
            thread::spawn(move || library.rename(&base, name))
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let doc = library.document(&base.id).unwrap().unwrap();
    assert_eq!(doc.names.len(), 2);
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    publish(&library, &remote, None).unwrap();
    let target = dir.path().join("restored");
    recover(&remote, &target).unwrap();
    assert_eq!(Document::load(&target, &base.id).unwrap().names.len(), 2);
}

#[cfg(feature = "native")]
#[test]
fn recovered_store_rebuilds_the_disposable_index() {
    use transfs::{index::Index, mount::MountView};
    let dir = TempDir::new().unwrap();
    let remote = DirectoryRemote::new(dir.path().join("remote"));
    let working = Library::new(dir.path().join("working"));
    let body = file(&dir, "body", b"hello\n");
    let doc = working.add(&body, Some("hello.txt")).unwrap();
    working.tag(&doc, &["notes".into()], &[]).unwrap();
    publish(&working, &remote, None).unwrap();
    let target = dir.path().join("target");
    recover(&remote, &target).unwrap();
    assert!(!target.join(".transfs/index.db").exists());
    let mut index = Index::open(&target).unwrap();
    index.rebuild().unwrap();
    assert!(index.rebuild_errors.is_empty());
    assert_eq!(index.by_tag("notes").unwrap()[0].id, doc.id);
    let view = MountView::new(&target).unwrap();
    assert!(view
        .list("/=")
        .unwrap()
        .unwrap()
        .iter()
        .any(|(name, _)| name == "hello.txt"));
    assert_eq!(
        transfs::document::Document::load(&target, &doc.id)
            .unwrap()
            .name
            .as_deref(),
        Some("hello.txt")
    );
}

#[cfg(feature = "native")]
#[test]
fn cli_publishes_and_recovers_a_checkable_store() {
    use std::process::Command;
    let dir = TempDir::new().unwrap();
    let executable = env!("CARGO_BIN_EXE_transfs");
    let source = file(&dir, "source", b"CLI recovery\n");
    let working = dir.path().join("working");
    let remote = dir.path().join("remote");
    let restored = dir.path().join("restored");
    let run = |store: &std::path::Path, command: &str, args: &[&std::ffi::OsStr]| {
        let output = Command::new(executable)
            .arg("--store")
            .arg(store)
            .arg(command)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}: {}",
            command,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    run(&working, "add", &[source.as_os_str(), "note.txt".as_ref()]);
    run(
        &working,
        "publish",
        &[remote.as_os_str(), "laptop".as_ref()],
    );
    run(&restored, "recover", &[remote.as_os_str()]);
    assert!(run(&restored, "check", &[]).contains("ok: 1 documents, 1 blobs"));
    assert!(run(&restored, "list", &[]).contains("note.txt"));
}
