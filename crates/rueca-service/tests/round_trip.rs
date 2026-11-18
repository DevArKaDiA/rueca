//! A store round-trips through in-memory object storage (RFC 0008 §10,
//! phase 0): a snapshot indexed into one local store, published, and
//! pulled into a second local store answers every query byte for byte as
//! the original, in local mode and in organization mode.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rueca_query::Engine;
use rueca_segment::org::{OrgMember, update_org};
use rueca_service::{
    PullWhich, RefKind, Retention, ServiceStore, publish_generation, publish_snapshot, pull_generation, pull_repo,
};
use serde_json::{Value, json};

fn queries() -> Vec<(&'static str, Value)> {
    vec![
        ("search_symbols", json!({"query": "validate graph"})),
        ("search_symbols", json!({"query": "SnapshotInfo", "limit": 5})),
        ("get_symbol", json!({"qualified_name": "ResolvedGraph.validate"})),
        ("trace", json!({"qualified_name": "ResolvedGraph.validate", "direction": "both", "depth": 2})),
        ("get_snippet", json!({"qualified_name": "git_blob_sha"})),
        ("search_code", json!({"pattern": "fn validate"})),
        ("get_file_outline", json!({"path": "src/lib.rs"})),
        ("list_repositories", json!({})),
    ]
}

/// Runs every query on both engines and asserts byte-equal JSON.
fn assert_same(a: &Engine, b: &Engine, mode: &str) {
    for (tool, args) in queries() {
        let ra = serde_json::to_string(&a.call(tool, args.clone()).unwrap()).unwrap();
        let rb = serde_json::to_string(&b.call(tool, args.clone()).unwrap()).unwrap();
        assert!(ra.len() > 40, "{mode} {tool}: suspiciously small response {ra}");
        assert_eq!(ra, rb, "{mode} {tool} {args}");
    }
}

#[test]
fn a_published_snapshot_answers_like_the_original() {
    let model: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-model").canonicalize().unwrap();
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let report = rueca_index::index_worktree(&rueca_index::IndexOptions {
        root: model.clone(),
        store: a.path().to_path_buf(),
        ..rueca_index::IndexOptions::default()
    })
    .unwrap();
    let repo = report.segment.snapshot.repo.clone();
    let url = "memory://round-trip";

    // Indexer: publish the snapshot, build the organization, publish it.
    let indexer = ServiceStore::open(url, &BTreeMap::new(), a.path(), "acme").unwrap();
    let published =
        publish_snapshot(&indexer, &repo, "main", RefKind::Default, &report.segment.id, &Retention::default()).unwrap();
    assert!(!published.duplicate && published.segment_uploaded && published.packs_uploaded > 0);
    update_org(&indexer.local, "acme", &[OrgMember { root: report.root.clone(), ..OrgMember::default() }]).unwrap();
    let generation = publish_generation(&indexer).unwrap();
    assert!(generation.published && generation.files_uploaded > 0);

    // Replica: pull both into an empty mirror.
    let replica = ServiceStore::open(url, &BTreeMap::new(), b.path(), "acme").unwrap();
    let pulled = pull_repo(&replica, &repo, PullWhich::Default).unwrap();
    assert_eq!(pulled.segments_downloaded, 1);
    assert_eq!(pulled.packs_downloaded, published.packs_uploaded);
    let pulled_gen = pull_generation(&replica, None).unwrap().unwrap();
    assert_eq!(pulled_gen.generation, 1);
    assert!(pull_generation(&replica, Some(&pulled_gen.version)).unwrap().is_none(), "unchanged pointer");

    // Local mode: the original root vs the mirror's only (synthetic) root.
    let original = Engine::new(a.path().to_path_buf(), Some(model)).unwrap();
    let mirror = Engine::new(replica.local.dir.clone(), None).unwrap();
    assert_same(&original, &mirror, "local");

    // Organization mode.
    let original = Engine::for_org(a.path().to_path_buf(), "acme").unwrap();
    let mirror = Engine::for_org(replica.local.dir.clone(), "acme").unwrap();
    assert_same(&original, &mirror, "org");

    // Pulling again downloads nothing and leaves the catalog alone.
    let again = pull_repo(&replica, &repo, PullWhich::All).unwrap();
    assert_eq!((again.segments_downloaded, again.packs_downloaded), (0, 0));
    assert!(again.refs.iter().all(|r| !r.registered));

    // The mirror can rebuild the organization from its synthetic roots and
    // reuses the pulled entry instead of copying it again.
    let members = replica.mirror_members().unwrap();
    assert_eq!(members.len(), 1);
    let up = update_org(&replica.local, "acme", &members).unwrap();
    assert!(!up.published && up.unchanged == 1, "{up:?}");
}
