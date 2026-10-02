use chrono::Utc;
use std::{fs, io::Write};
use tempfile::TempDir;
use transfs::{cas::Cas, check::check, document::Document, library::Library, log::Log, query};

fn source(dir: &TempDir, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn add_deduplicates_bytes_but_keeps_distinct_documents() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let a = source(&dir, "a", b"dup\n");
    let b = source(&dir, "b", b"dup\n");
    let first = lib.add(&a, None).unwrap();
    let second = lib.add(&b, None).unwrap();
    assert_ne!(first.id, second.id);
    assert_ne!(first.head_id(), second.head_id());
    assert_eq!(first.head(), second.head());
    assert_eq!(lib.read(&first).unwrap(), Some(b"dup\n".to_vec()));
    assert!(Cas::new(&lib.root).path_for(first.head().unwrap()).exists());
    assert_eq!(Document::load(&lib.root, &first.id).unwrap(), first);
}

#[test]
fn document_fold_deduplicates_replayed_create_claim() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let doc = lib.add(&file, Some("Doc")).unwrap();
    let claims = Log::new(&lib.root, &doc.id).read().unwrap().claims;
    let mut replayed = claims.clone();
    replayed.extend(claims);
    assert_eq!(Document::fold(&doc.id, &replayed).unwrap(), doc);
}

#[test]
fn edit_claim_cannot_be_replayed_into_another_document() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let first = lib.add(&file, Some("First")).unwrap();
    let second = lib.add(&file, Some("Second")).unwrap();
    let stolen = Log::new(&lib.root, &first.id).read().unwrap().claims[1].clone();
    assert!(Log::new(&lib.root, &second.id).append(&[stolen]).is_err());
}

#[test]
fn stale_renames_remain_visible_until_explicit_resolution() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let base = lib.add(&file, Some("first")).unwrap();
    let left = lib.rename(&base, "left").unwrap();
    assert_eq!(left.name.as_deref(), Some("left"));
    let fork = lib.rename(&base, "right").unwrap();
    assert!(fork.name.is_none());
    assert_eq!(fork.names.len(), 2);
    assert!(fork.names.iter().any(|name| name.value == "left"));
    assert!(fork.names.iter().any(|name| name.value == "right"));
    let resolved = lib.rename(&fork, "final").unwrap();
    assert_eq!(resolved.name.as_deref(), Some("final"));
    assert_eq!(resolved.names.len(), 1);
}

#[test]
fn stale_content_forks_and_explicit_parent_join_restores_one_head() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let a = source(&dir, "a", b"A");
    let b = source(&dir, "b", b"B");
    let c = source(&dir, "c", b"C");
    let base = lib.add(&a, Some("file")).unwrap();
    let left = lib.add_version(&base, &b).unwrap();
    assert_eq!(left.versions.len(), 2);
    assert_eq!(left.heads[0].parents, vec![base.head_id().unwrap()]);
    let fork = lib.add_version(&base, &c).unwrap();
    assert_eq!(fork.heads.len(), 2);
    assert!(lib.read(&fork).is_err());
    for head in &fork.heads {
        assert_eq!(
            lib.read_version(&fork, &head.id).unwrap().unwrap(),
            if head.hash == left.head().unwrap() {
                b"B"
            } else {
                b"C"
            }
        );
    }
    let parents = fork
        .heads
        .iter()
        .map(|head| head.id.clone())
        .collect::<Vec<_>>();
    let resolved = lib
        .add_version_from_at(&fork, &parents, &a, Utc::now())
        .unwrap();
    assert_eq!(resolved.heads.len(), 1);
    assert_eq!(resolved.heads[0].parents, parents);
    assert_eq!(lib.read(&resolved).unwrap(), Some(b"A".to_vec()));
}

#[test]
fn tags_keep_causal_assertions_but_project_leaves() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let base = lib.add(&file, None).unwrap();
    let parent = lib.tag(&base, &["date/1920".into()], &[]).unwrap();
    let child = lib.tag(&base, &["date/1920/10/10".into()], &[]).unwrap();
    assert_eq!(child.tag_assertions.len(), 2);
    assert!(!child.tags.contains("date/1920"));
    assert!(child.tags.contains("date/1920/10/10"));
    assert!(child.tag_conflicts.is_empty());
    let first = lib.set_tag(&parent, "genre", "jazz").unwrap();
    let second = lib.set_tag(&parent, "genre", "rock").unwrap();
    assert!(first.tags.contains("genre/jazz"));
    assert!(second.tags.contains("genre/jazz"));
    assert!(second.tags.contains("genre/rock"));
    assert!(second.tag_conflicts.contains("genre"));
    #[cfg(feature = "native")]
    assert_eq!(
        transfs::index::Index::open(&lib.root)
            .unwrap()
            .all()
            .unwrap()[0]
            .tag_conflicts,
        vec!["genre"]
    );
    let resolved = lib.set_tag(&second, "genre", "blues").unwrap();
    assert_eq!(resolved.tag_conflicts.len(), 0);
    assert!(resolved.tags.contains("genre/blues"));
}

#[test]
fn a_sequential_tag_after_set_is_multiple_values_without_a_conflict() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let base = lib.add(&file, Some("Doc")).unwrap();
    let set = lib.set_tag(&base, "stars", "5").unwrap();
    let later = lib.tag(&set, &["stars/4".into()], &[]).unwrap();
    assert!(later.tag_conflicts.is_empty());
    assert!(later.set_multi_value_keys.contains("stars"));
    assert!(check(&lib.root).unwrap().warnings.is_empty());
}

#[test]
fn torn_tail_and_unsupported_v1_log_are_detected() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let doc = lib.add(&file, Some("Doc")).unwrap();
    fs::remove_file(lib.cas.path_for(doc.head().unwrap())).unwrap();
    let log = Log::new(&lib.root, &doc.id);
    let mut writer = fs::OpenOptions::new()
        .append(true)
        .open(log.path())
        .unwrap();
    write!(writer, "{{broken").unwrap();
    let read = log.read().unwrap();
    assert!(read.torn_tail.is_some());
    assert!(log.append(&[]).is_err());
    let result = check(&lib.root).unwrap();
    assert!(result
        .errors
        .iter()
        .any(|e| e.message.contains("missing blob")));
    assert!(result
        .warnings
        .iter()
        .any(|e| e.message.contains("torn trailing record")));

    let old_id = "a".repeat(64);
    let old = Log::new(&lib.root, &old_id);
    fs::create_dir_all(old.path().parent().unwrap()).unwrap();
    fs::write(
        old.path(),
        r#"{"op":"create","nonce":"00112233445566778899aabbccddeeff","ts":"2024-01-02T03:04:05Z"}"#,
    )
    .unwrap();
    assert!(old.read().is_err());
}

#[test]
fn a_valid_unterminated_record_is_not_appended_to() {
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let doc = lib.add(&file, Some("Doc")).unwrap();
    let log = Log::new(&lib.root, &doc.id);
    let mut bytes = fs::read(log.path()).unwrap();
    assert_eq!(bytes.pop(), Some(b'\n'));
    fs::write(log.path(), bytes).unwrap();

    let read = log.read().unwrap();
    assert_eq!(read.claims.len(), 2);
    assert_eq!(
        read.torn_tail.unwrap().reason,
        "missing terminating newline"
    );
    assert!(lib.rename(&doc, "Renamed").is_err());
    assert_eq!(fs::read(log.path()).unwrap().last(), Some(&b'}'));
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
fn index_and_mount_view_expose_every_name_and_head() {
    use transfs::{
        index::Index,
        mount::{leaves, MountView, Node},
    };
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let a = source(&dir, "a", b"A\n");
    let b = source(&dir, "b", b"B\n");
    let c = source(&dir, "c", b"C\n");
    let base = lib.add(&a, Some("report.txt")).unwrap();
    lib.tag(&base, &["finance".into()], &[]).unwrap();
    lib.rename(&base, "draft.txt").unwrap();
    lib.rename(&base, "final.txt").unwrap();
    lib.add_version(&base, &b).unwrap();
    lib.add_version(&base, &c).unwrap();
    let index = Index::open(&lib.root).unwrap();
    let row = &index.all().unwrap()[0];
    assert_eq!(row.names.len(), 2);
    assert_eq!(row.heads.len(), 2);
    assert_eq!(index.by_name("draft").unwrap().len(), 1);
    assert_eq!(index.by_name("final").unwrap().len(), 1);
    assert_eq!(index.by_tag("finance").unwrap().len(), 1);
    let leaves = leaves(vec![row.clone()]);
    assert_eq!(leaves.len(), 4);
    assert!(leaves
        .iter()
        .any(|(name, _)| name.starts_with("draft~") && name.ends_with(".txt")));
    assert!(leaves
        .iter()
        .any(|(name, _)| name.starts_with("final~") && name.ends_with(".txt")));
    let view = MountView::new(&lib.root).unwrap();
    let entries = view.list("/=").unwrap().unwrap();
    assert_eq!(entries.len(), 4);
    for (name, node) in entries {
        assert_eq!(
            view.resolve(&format!("/=/{}", name)).unwrap(),
            Some(node.clone())
        );
        let Node::File { hash, modified, .. } = node else {
            panic!("expected file")
        };
        assert!(row
            .heads
            .iter()
            .any(|head| std::time::SystemTime::from(head.ts) == modified));
        assert!([b"B\n".to_vec(), b"C\n".to_vec()].contains(&view.read(&hash, 0, 2).unwrap()));
    }
    fs::remove_file(Index::db_path(&lib.root)).unwrap();
    assert_eq!(
        Index::open(&lib.root).unwrap().all().unwrap()[0]
            .heads
            .len(),
        2
    );
    let conn = rusqlite::Connection::open(Index::db_path(&lib.root)).unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    assert_eq!(
        Index::open(&lib.root).unwrap().all().unwrap()[0]
            .heads
            .len(),
        2
    );
}

#[cfg(feature = "native")]
#[test]
fn index_facets_and_mime_survive_rebuild() {
    use transfs::index::Index;
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let pdf = source(&dir, "a.pdf", b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\n");
    let text = source(&dir, "b.txt", b"notes");
    let a = lib.add(&pdf, Some("a.pdf")).unwrap();
    let b = lib.add(&text, Some("b.txt")).unwrap();
    lib.tag(&a, &["vacation".into(), "year=1920".into()], &[])
        .unwrap();
    lib.tag(&b, &["year=2020".into(), "stars=4".into()], &[])
        .unwrap();
    let idx = Index::open(&lib.root).unwrap();
    assert_eq!(idx.all().unwrap().len(), 2);
    assert_eq!(idx.by_tag("vacation").unwrap()[0].id, a.id);
    assert_eq!(idx.by_name("b.").unwrap()[0].id, b.id);
    let walk = idx.walk(&["year".into(), "1920".into()]).unwrap();
    assert_eq!(idx.docs(&walk, None).unwrap()[0].id, a.id);
    assert_eq!(
        idx.facets(&idx.walk(&[]).unwrap()).unwrap(),
        vec!["stars", "tag", "type", "year"]
    );
}

#[cfg(feature = "native")]
#[test]
fn failed_rebuild_keeps_the_previous_index() {
    use transfs::index::Index;
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let file = source(&dir, "body", b"body");
    let doc = lib.add(&file, Some("Doc")).unwrap();
    let mut index = Index::open(&lib.root).unwrap();
    assert_eq!(index.all().unwrap()[0].id, doc.id);

    let invalid = Log::new(&lib.root, "a".repeat(64));
    fs::create_dir_all(invalid.path().parent().unwrap()).unwrap();
    fs::write(invalid.path(), b"{invalid}\n").unwrap();
    index.rebuild().unwrap();
    assert_eq!(index.rebuild_errors.len(), 1);
    assert_eq!(index.all().unwrap()[0].id, doc.id);
}

#[cfg(feature = "native")]
#[test]
fn mount_leaf_suffixes_grow_when_short_ids_collide() {
    use transfs::{
        index::{HeadRow, Row},
        mount::leaves,
    };
    let row = |id: &str, heads: &[&str]| Row {
        id: id.into(),
        name: Some("notes.md".into()),
        names: vec!["notes.md".into()],
        mime_type: Some("text/plain".into()),
        size: Some(1),
        version_count: heads.len() as i64,
        date_added: String::new(),
        head_hash: None,
        heads: heads
            .iter()
            .map(|head| HeadRow {
                id: (*head).into(),
                hash: "a".repeat(64),
                size: Some(1),
                ts: Utc::now(),
            })
            .collect(),
        tags: vec![],
        tag_conflicts: vec![],
    };
    let names: Vec<_> = leaves(vec![
        row("aaaa1111", &["bbbbbbbb1111", "bbbbbbbb2222"]),
        row("aaaa2222", &["cccccccc1111"]),
    ])
    .into_iter()
    .map(|(name, _)| name)
    .collect();
    assert_eq!(names.len(), 3);
    assert_eq!(
        names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert!(names.iter().any(|name| name.contains("bbbbbbbb1111")));
    assert!(names.iter().any(|name| name.contains("bbbbbbbb2222")));
}

#[cfg(feature = "native")]
#[test]
fn cli_requires_a_version_for_forked_content() {
    use std::process::Command;
    let dir = TempDir::new().unwrap();
    let lib = Library::new(dir.path().join("store"));
    let a = source(&dir, "a", b"A");
    let b = source(&dir, "b", b"B");
    let c = source(&dir, "c", b"C");
    let base = lib.add(&a, Some("notes.txt")).unwrap();
    lib.add_version(&base, &b).unwrap();
    let fork = lib.add_version(&base, &c).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_transfs"))
            .arg("--store")
            .arg(&lib.root)
            .args(args)
            .output()
            .unwrap()
    };
    let error = run(&["cat", &fork.id]);
    assert!(!error.status.success());
    let message = String::from_utf8(error.stderr).unwrap();
    for head in &fork.heads {
        assert!(message.contains(&head.id));
    }
    let selected = run(&["cat", &fork.id, &fork.heads[0].id[..12]]);
    assert!(selected.status.success());
    assert_eq!(
        selected.stdout,
        lib.read_version(&fork, &fork.heads[0].id).unwrap().unwrap()
    );
    let versions = String::from_utf8(run(&["versions", &fork.id]).stdout).unwrap();
    assert_eq!(
        versions
            .lines()
            .filter(|line| line.starts_with("* "))
            .count(),
        2
    );
    let listing = String::from_utf8(run(&["list"]).stdout).unwrap();
    assert!(listing.contains("heads=2"));
    let warnings = check(&lib.root).unwrap().warnings;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("2 content heads"));
    assert!(!warnings[0].message.contains("names"));
    let added = run(&[
        "addversion",
        &fork.id,
        b.to_str().unwrap(),
        "--parents",
        &base.head_id().unwrap()[..12],
    ]);
    assert!(added.status.success());
    let new_doc = Document::load(&lib.root, &fork.id).unwrap();
    let new_head = new_doc
        .heads
        .iter()
        .find(|head| !fork.heads.iter().any(|old| old.id == head.id))
        .unwrap();
    assert!(String::from_utf8(added.stdout)
        .unwrap()
        .contains(&format!("added version {}", &new_head.id[..12])));
}
