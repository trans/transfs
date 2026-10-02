use chrono::{TimeZone, Utc};
use merkle_champ::Identify;
use sha2::{Digest, Sha256};
use transfs::causal::{CanonicalClaim, CausalClaim as C, CausalSet};

fn ts(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, hour, 4, 5).unwrap()
}

fn nonce(n: u128) -> String {
    format!("{n:032x}")
}
fn doc() -> String {
    "d".repeat(64)
}
fn hash(byte: char) -> String {
    byte.to_string().repeat(64)
}

fn name(n: u128, value: &str, supersedes: Vec<String>) -> C {
    C::Name {
        doc: doc(),
        nonce: nonce(n),
        ts: ts(3),
        name: value.into(),
        supersedes,
    }
}

fn version(n: u128, byte: char, parents: Vec<String>) -> C {
    C::Version {
        doc: doc(),
        nonce: nonce(n),
        ts: ts(3),
        hash: hash(byte),
        parents,
    }
}

fn tag(n: u128, path: &str, scope: Option<&str>, supersedes: Vec<String>) -> C {
    C::TagAdd {
        doc: doc(),
        nonce: nonce(n),
        ts: ts(3),
        tag: path.into(),
        scope: scope.map(str::to_owned),
        supersedes,
    }
}

#[test]
fn identity_is_stable_and_user_delimiters_do_not_alias() {
    let base = name(1, "a\x1fb,c☃", vec![]);
    let other = name(2, "a\x1fb,c☃", vec![]);
    assert_ne!(base.id().unwrap(), other.id().unwrap());
    assert_eq!(
        base.id().unwrap(),
        "8419519b28dae7570198b33c375e66fcd63d5e4339c105f45a9d0a361dd02825"
    );
    assert_eq!(
        C::from_json_line(&base.to_json_line().unwrap()).unwrap(),
        base
    );
    let mut another_doc = base.clone();
    if let C::Name { doc, .. } = &mut another_doc {
        *doc = "e".repeat(64);
    }
    assert_ne!(base.id().unwrap(), another_doc.id().unwrap());
    assert!(CausalSet::from_claims([base.clone(), another_doc])
        .unwrap()
        .fold()
        .is_err());
    let mut champ_hasher = Sha256::new();
    CanonicalClaim::new(base.clone())
        .unwrap()
        .identify(&mut champ_hasher);
    assert_eq!(hex::encode(champ_hasher.finalize()), base.id().unwrap());
    let create = C::Create {
        format: 2,
        nonce: nonce(42),
        ts: ts(3),
    };
    assert_eq!(
        create.id().unwrap(),
        "0782bcfaf44d3aebd6930307859512c4340c4acfa5be7b26ec3ff7028eb8068b"
    );
    assert_eq!(
        version(7, 'a', vec![]).id().unwrap(),
        "ea8baa5fd1da6aa57f2a4cd58290f3bd6a907bd3f5eb6da7e044f96c673a72c9"
    );
    assert_eq!(create.doc_id(), Some(create.id().unwrap()));
    assert_eq!(
        C::from_json_line(&create.to_json_line().unwrap()).unwrap(),
        create
    );
    assert!(C::from_json_line(r#"{"op":"create","format":1,"nonce":"0000000000000000000000000000002a","ts":"2026-01-02T03:04:05Z"}"#).is_err());
    assert!(C::from_json_line(r#"{"op":"v2_name","nonce":"00000000000000000000000000000001","ts":"2026-01-02T03:04:05Z","name":"x","supersedes":[]}"#).is_err());

    let t1 = tag(3, "genre=jazz", None, vec![]);
    let t2 = tag(3, "genre/jazz", None, vec![]);
    assert_eq!(t1.id().unwrap(), t2.id().unwrap());
    assert_eq!(t1.to_json_line().unwrap(), t2.to_json_line().unwrap());
    assert!(name(4, "bad/name", vec![]).id().is_err());
}

#[test]
fn concurrent_names_survive_and_later_rename_resolves_both() {
    let old = name(1, "report", vec![]);
    let old_id = old.id().unwrap();
    let left = name(2, "final", vec![old_id.clone()]);
    let right = name(3, "draft", vec![old_id]);
    let a = CausalSet::from_claims([old.clone(), left.clone()]).unwrap();
    let b = CausalSet::from_claims([old, right.clone()]).unwrap();
    let ab = a.union(&b).unwrap();
    assert_eq!(ab, b.union(&a).unwrap());
    let state = ab.fold().unwrap();
    assert_eq!(state.names.len(), 2);
    assert!(state.names.iter().any(|v| v.value == "final"));
    assert!(state.names.iter().any(|v| v.value == "draft"));
    let tagged = ab
        .union(&CausalSet::from_claims([tag(10, "status/urgent", None, vec![])]).unwrap())
        .unwrap();
    assert_eq!(tagged.fold().unwrap().names, state.names);
    assert_eq!(tagged.fold().unwrap().tags[0].value, "status/urgent");
    let resolved = name(4, "approved", vec![right.id().unwrap(), left.id().unwrap()]);
    let reordered = name(4, "approved", vec![left.id().unwrap(), right.id().unwrap()]);
    assert_eq!(resolved.id().unwrap(), reordered.id().unwrap());
    let resolved = ab
        .union(&CausalSet::from_claims([resolved]).unwrap())
        .unwrap();
    assert_eq!(resolved.fold().unwrap().names[0].value, "approved");
}

#[test]
fn identical_bytes_are_distinct_versions_and_stale_edits_fork() {
    let a = version(1, 'a', vec![]);
    let b = version(2, 'b', vec![a.id().unwrap()]);
    let back_to_a = version(3, 'a', vec![b.id().unwrap()]);
    let stale = version(4, 'c', vec![a.id().unwrap()]);
    let set = CausalSet::from_claims([back_to_a.clone(), stale.clone(), b, a]).unwrap();
    let state = set.fold().unwrap();
    assert_eq!(state.versions.len(), 4);
    assert_eq!(state.heads.len(), 2);
    assert!(state.heads.iter().any(|v| v.id == back_to_a.id().unwrap()));
    assert!(state.heads.iter().any(|v| v.id == stale.id().unwrap()));
    assert_eq!(
        back_to_a.id().unwrap(),
        C::from_json_line(&back_to_a.to_json_line().unwrap())
            .unwrap()
            .id()
            .unwrap()
    );
}

#[test]
fn tag_remove_and_concurrent_set_use_observed_ids() {
    let rock = tag(1, "genre/rock", None, vec![]);
    let jazz = tag(2, "genre/jazz", None, vec![]);
    let remove_rock = C::TagRemove {
        doc: doc(),
        nonce: nonce(3),
        ts: ts(3),
        tag: "genre/rock".into(),
        removes: vec![rock.id().unwrap()],
    };
    let set_blues = tag(
        4,
        "genre/blues",
        Some("genre"),
        vec![rock.id().unwrap(), jazz.id().unwrap()],
    );
    let set_funk = tag(
        5,
        "genre/funk",
        Some("genre"),
        vec![rock.id().unwrap(), jazz.id().unwrap()],
    );
    let concurrent_add = tag(6, "genre/soul", None, vec![]);
    let base = CausalSet::from_claims([rock, jazz]).unwrap();
    let left = base
        .union(&CausalSet::from_claims([remove_rock, set_blues]).unwrap())
        .unwrap();
    let right = base
        .union(&CausalSet::from_claims([set_funk, concurrent_add]).unwrap())
        .unwrap();
    let merged = left.union(&right).unwrap();
    assert_eq!(merged, right.union(&left).unwrap());
    let state = merged.fold().unwrap();
    assert_eq!(
        state.tag_conflict_keys().into_iter().collect::<Vec<_>>(),
        ["genre"]
    );
    let values: Vec<_> = state.tags.into_iter().map(|t| t.value).collect();
    assert_eq!(values.len(), 3);
    for expected in ["genre/blues", "genre/funk", "genre/soul"] {
        assert!(values.contains(&expected.into()));
    }
}

#[test]
fn concurrent_add_survives_remove_of_only_the_observed_assertion() {
    let old = tag(1, "genre/jazz", None, vec![]);
    let replacement = tag(2, "genre/jazz", None, vec![old.id().unwrap()]);
    let removal = C::TagRemove {
        doc: doc(),
        nonce: nonce(3),
        ts: ts(3),
        tag: "genre/jazz".into(),
        removes: vec![old.id().unwrap()],
    };
    let state = CausalSet::from_claims([old, replacement.clone(), removal])
        .unwrap()
        .fold()
        .unwrap();
    assert_eq!(state.tags.len(), 1);
    assert_eq!(state.tags[0].id, replacement.id().unwrap());
}

#[test]
fn concurrent_ancestor_and_descendant_tags_project_to_the_leaf() {
    let state = CausalSet::from_claims([
        tag(1, "date/1920", None, vec![]),
        tag(2, "date/1920/10/10", None, vec![]),
        tag(3, "genre/jazz", None, vec![]),
    ])
    .unwrap()
    .fold()
    .unwrap();
    assert_eq!(state.tags.len(), 3);
    assert!(state.tag_conflict_keys().is_empty());
    assert_eq!(
        state.projected_tags().into_iter().collect::<Vec<_>>(),
        ["date/1920/10/10", "genre/jazz"]
    );
}

#[test]
fn duplicate_delivery_and_clock_skew_do_not_change_frontiers() {
    let initial = name(1, "before", vec![]);
    let mut later = name(2, "after", vec![initial.id().unwrap()]);
    if let C::Name { ts: timestamp, .. } = &mut later {
        *timestamp = ts(1);
    }
    let set = CausalSet::from_claims([later.clone(), initial.clone(), later, initial]).unwrap();
    assert_eq!(set.claims().len(), 2);
    assert_eq!(set.fold().unwrap().names[0].value, "after");
}

#[test]
fn missing_and_cross_field_references_are_errors() {
    let missing = name(1, "x", vec![hash('a')]);
    assert!(CausalSet::from_claims([missing]).unwrap().fold().is_err());
    let v = version(1, 'a', vec![]);
    let bad_name = name(2, "x", vec![v.id().unwrap()]);
    assert!(CausalSet::from_claims([v, bad_name])
        .unwrap()
        .fold()
        .is_err());
    let rock = tag(3, "genre/rock", None, vec![]);
    let bad_set = tag(4, "date/2026", Some("date"), vec![rock.id().unwrap()]);
    assert!(CausalSet::from_claims([rock, bad_set])
        .unwrap()
        .fold()
        .is_err());
    assert!(tag(5, "genre/rock", Some("date"), vec![]).id().is_err());
}
