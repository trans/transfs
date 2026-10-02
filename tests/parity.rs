use chrono::{TimeZone, Utc};
use std::{fs, io::Write};
use tempfile::TempDir;
use transfs::{
    cas::Cas, check::check, claim::Claim, document::Document, library::Library, log::Log, query,
};

fn source(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn crystal_claim_codec_and_identity_fixture() {
    let ts = Utc.with_ymd_and_hms(2024, 1, 2, 3, 4, 5).unwrap();
    let create = Claim::Create {
        nonce: "00112233445566778899aabbccddeeff".into(),
        ts,
    };
    assert_eq!(
        create.doc_id().unwrap(),
        "354c12bba3c21451e9add55af57f9eaba785aceb64a017c444900db4123ccfc7"
    );
    assert_eq!(
        create.to_json_line(),
        r#"{"op":"create","nonce":"00112233445566778899aabbccddeeff","ts":"2024-01-02T03:04:05.000000000Z"}"#
    );
    let version = Claim::Version {
        hash: "a".repeat(64),
        parent: None,
        ts,
    };
    assert_eq!(version.to_json_line(), format!(
        "{{\"op\":\"version\",\"hash\":\"{}\",\"parent\":null,\"ts\":\"2024-01-02T03:04:05.000000000Z\"}}",
        "a".repeat(64)));
    let tag = Claim::Tag {
        add: vec!["year/2024".into()],
        del: vec!["year/2023".into()],
        ts,
    };
    assert_eq!(
        tag.to_json_line(),
        r#"{"op":"tag","add":["year/2024"],"del":["year/2023"],"ts":"2024-01-02T03:04:05.000000000Z"}"#
    );
    for claim in [create, version, tag] {
        assert_eq!(Claim::parse(&claim.to_json_line()).unwrap(), Some(claim));
    }
}

#[test]
fn crystal_log_fixture_replays_in_rust() {
    let dir = TempDir::new().unwrap();
    let id = "354c12bba3c21451e9add55af57f9eaba785aceb64a017c444900db4123ccfc7";
    let log = Log::new(dir.path(), id);
    fs::create_dir_all(log.path().parent().unwrap()).unwrap();
    let lines = [
        r#"{"op":"create","nonce":"00112233445566778899aabbccddeeff","ts":"2024-01-02T03:04:05.000000000Z"}"#,
        r#"{"op":"version","hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","parent":null,"ts":"2024-01-02T03:04:05.000000000Z"}"#,
        r#"{"op":"name","name":"report.pdf","ts":"2024-01-02T03:04:05.000000000Z"}"#,
        r#"{"op":"tag","add":["year/2024","finance"],"ts":"2024-01-02T03:04:05.000000000Z"}"#,
    ];
    fs::write(log.path(), format!("{}\n", lines.join("\n"))).unwrap();
    let doc = Document::load(dir.path(), id).unwrap();
    assert_eq!(doc.name.as_deref(), Some("report.pdf"));
    assert_eq!(
        doc.head(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(doc.tags.contains("year/2024"));
    assert!(doc.tags.contains("finance"));
}

#[test]
fn add_deduplicates_content_but_not_documents() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let a = source(&dir, "a", b"dup\n");
    let b = source(&dir, "b", b"dup\n");
    let first = lib.add(&a, None).unwrap();
    let second = lib.add(&b, None).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(first.head(), second.head());
    assert_eq!(lib.read(&first).unwrap(), Some(b"dup\n".to_vec()));
    assert!(Cas::new(&lib.root).path_for(first.head().unwrap()).exists());
}

#[test]
fn tag_leaf_and_set_semantics_match_crystal() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let mut doc = lib.add(&file, None).unwrap();
    doc = lib
        .tag(
            &doc,
            &["date/1920".into(), "genre/jazz".into(), "genre/rock".into()],
            &[],
        )
        .unwrap();
    doc = lib.tag(&doc, &["date/1920/08/10".into()], &[]).unwrap();
    assert!(!doc.tags.contains("date/1920"));
    let count = Log::new(&lib.root, &doc.id).read().unwrap().claims.len();
    doc = lib.tag(&doc, &["date/1920".into()], &[]).unwrap();
    assert_eq!(
        count,
        Log::new(&lib.root, &doc.id).read().unwrap().claims.len()
    );
    doc = lib.set_tag(&doc, "genre", "jazz").unwrap();
    assert!(doc.tags.contains("genre/jazz"));
    assert!(!doc.tags.contains("genre/rock"));
    doc = lib.set_tag(&doc, "date", "1921").unwrap();
    assert!(doc.tags.contains("date/1921"));
    assert!(!doc.tags.contains("date/1920/08/10"));
}

#[test]
fn versions_rename_and_fresh_fold() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let v1 = source(&dir, "v1", b"one");
    let v2 = source(&dir, "v2", b"two");
    let mut doc = lib.add(&v1, Some("first")).unwrap();
    let old = doc.head().unwrap().to_owned();
    doc = lib.add_version(&doc, &v2).unwrap();
    doc = lib.rename(&doc, "second").unwrap();
    assert_eq!(doc.versions[1].parent.as_deref(), Some(old.as_str()));
    assert_eq!(doc.version_count(), 2);
    assert_eq!(Document::load(&lib.root, &doc.id).unwrap(), doc);
    assert_eq!(lib.document(&doc.id[..12]).unwrap(), Some(doc));
}

#[test]
fn torn_tail_and_blank_lines_preserve_line_numbers() {
    let dir = TempDir::new().unwrap();
    let log = Log::new(dir.path(), "a".repeat(64));
    fs::create_dir_all(log.path().parent().unwrap()).unwrap();
    fs::write(
        log.path(),
        "\n{\"op\":\"future\",\"ts\":\"2024-01-02T03:04:05Z\"}\n{bad",
    )
    .unwrap();
    let read = log.read().unwrap();
    assert_eq!(read.torn_tail.unwrap().line, 3);
    fs::write(
        log.path(),
        "\n{bad\n{\"op\":\"future\",\"ts\":\"2024-01-02T03:04:05Z\"}\n",
    )
    .unwrap();
    assert!(matches!(
        log.read(),
        Err(transfs::Error::CorruptLog { line: 2, .. })
    ));
}

#[test]
fn check_reports_missing_blob_and_torn_tail() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let doc = lib.add(&file, Some("Doc")).unwrap();
    fs::remove_file(lib.cas.path_for(doc.head().unwrap())).unwrap();
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(Log::new(&lib.root, &doc.id).path())
        .unwrap();
    write!(log, "{{broken").unwrap();
    let result = check(&lib.root).unwrap();
    assert!(result
        .errors
        .iter()
        .any(|e| e.message.contains("version references missing blob")));
    assert!(result
        .warnings
        .iter()
        .any(|e| e.message.contains("ignored torn trailing record")));
}

#[test]
fn query_parser_matches_toggle_rules() {
    let parsed = query::parse("/year=1920/=/report.pdf");
    assert_eq!(parsed.components, ["year", "1920"]);
    assert!(parsed.doc_view);
    assert_eq!(parsed.doc_name.as_deref(), Some("report.pdf"));
    assert!(!query::parse("/=/=/").doc_view);
}

#[cfg(feature = "native")]
#[test]
fn native_index_rebuild_and_facets() {
    use transfs::index::Index;
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let pdf = source(
        &dir,
        "a.bin",
        b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj\n<<>>\nendobj\n",
    );
    let text = source(&dir, "b.txt", b"notes");
    let mut a = lib.add(&pdf, Some("a.pdf")).unwrap();
    a = lib
        .tag(&a, &["vacation".into(), "year=1920".into()], &[])
        .unwrap();
    let mut b = lib.add(&text, Some("b.txt")).unwrap();
    b = lib
        .tag(&b, &["year=2020".into(), "stars=4".into()], &[])
        .unwrap();
    let idx = Index::open(&lib.root).unwrap();
    assert_eq!(idx.all().unwrap().len(), 2);
    assert_eq!(idx.by_tag("vacation").unwrap()[0].id, a.id);
    assert_eq!(idx.by_type("application/pdf").unwrap()[0].id, a.id);
    assert_eq!(idx.by_name("b.").unwrap()[0].id, b.id);
    let walk = idx.walk(&["year".into(), "1920".into()]).unwrap();
    assert_eq!(idx.docs(&walk, None).unwrap()[0].id, a.id);
    assert_eq!(
        idx.facets(&idx.walk(&[]).unwrap()).unwrap(),
        vec!["stars", "tag", "type", "year"]
    );
    fs::remove_file(Index::db_path(&lib.root)).unwrap();
    assert_eq!(Index::open(&lib.root).unwrap().all().unwrap().len(), 2);
}

#[cfg(feature = "native")]
#[test]
fn mount_view_resolves_facets_disambiguated_leaves_and_blob_offsets() {
    use transfs::mount::{MountView, Node};
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let one = source(&dir, "one", b"one\n");
    let two = source(&dir, "two", b"two\n");
    let a = lib.add(&one, Some("report.txt")).unwrap();
    let b = lib.add(&two, Some("report.txt")).unwrap();
    lib.tag(&a, &["finance".into(), "year=2024".into()], &[])
        .unwrap();
    lib.tag(&b, &["finance".into()], &[]).unwrap();
    let view = MountView::new(&lib.root).unwrap();
    let root = view.list("/").unwrap().unwrap();
    assert!(root.iter().any(|(name, _)| name == "year"));
    assert_eq!(view.resolve("/tag/finance").unwrap(), Some(Node::Directory));
    assert_eq!(view.resolve("/tag/no-such-tag").unwrap(), None);
    let entries = view.list("/tag/finance/=").unwrap().unwrap();
    assert_eq!(entries.len(), 2);
    let names: Vec<_> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert!(names.contains(&format!("report~{}.txt", &a.id[..4]).as_str()));
    assert!(names.contains(&format!("report~{}.txt", &b.id[..4]).as_str()));
    for (name, node) in entries {
        let resolved = view.resolve(&format!("/tag/finance/=/{name}")).unwrap();
        assert_eq!(resolved, Some(node.clone()));
        let Node::File { hash, .. } = node else {
            panic!("expected file")
        };
        assert_eq!(
            view.read(&hash, 1, 2).unwrap(),
            if hash == a.head().unwrap() {
                b"ne"
            } else {
                b"wo"
            }
        );
        assert!(view
            .list(&format!("/tag/finance/=/{name}"))
            .unwrap()
            .is_none());
    }
}

#[cfg(feature = "native")]
#[test]
fn mount_leaves_tolerate_short_ids_in_a_damaged_index() {
    use transfs::{index::Row, mount::leaves};
    let row = |id: &str, name: Option<&str>| Row {
        id: id.into(),
        name: name.map(str::to_owned),
        mime_type: None,
        size: Some(1),
        version_count: 1,
        date_added: String::new(),
        head_hash: Some("a".repeat(64)),
        tags: vec![],
    };
    let names: Vec<_> = leaves(vec![
        row("a", None),
        row("b", None),
        row("c", Some("x.txt")),
        row("d", Some("x.txt")),
    ])
    .into_iter()
    .map(|(name, _)| name)
    .collect();
    assert_eq!(
        names,
        ["untitled-a~a", "untitled-b~b", "x~c.txt", "x~d.txt"]
    );
}
