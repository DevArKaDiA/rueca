//! Concurrent publishers of one repository never lose a snapshot (RFC 0008
//! §5.2, §5.3): the catalog is a compare-and-swap object.

mod common;

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::{SlowCatalogs, commit, write};
use rueca_segment::SegmentMeta;
use rueca_service::{RefKind, RepoCatalog, Retention, ServiceStore, publish_snapshot, repo_dir};
use rueca_storage::Storage;

const REPO: &str = "github.com/acme/billing";
const THREADS: usize = 4;
const PER_THREAD: usize = 20;

#[test]
fn four_publishers_keep_every_snapshot() {
    let local = tempfile::tempdir().unwrap();
    let metas: Vec<_> = (0..PER_THREAD)
        .map(|k| {
            let own = format!("billing/f{k}.py");
            let src = format!("def f{k}(x):\n    return x + {k}\n");
            write(
                local.path(),
                REPO,
                &commit(k),
                &[("billing/common.py", "def common():\n    return 1\n"), (&own, &src)],
            )
        })
        .collect();
    let memory = rueca_storage::open("memory://concurrent-publish", &BTreeMap::new()).unwrap();
    // Reading the catalog takes 2 ms, so the four CAS loops overlap.
    let storage: Arc<dyn Storage> = Arc::new(SlowCatalogs(memory, Duration::from_millis(2)));
    let svc = ServiceStore::with_storage(storage, local.path(), "acme").unwrap();
    let retention = Retention::default();
    let conflicts = AtomicU32::new(0);
    let duplicates = AtomicU32::new(0);
    std::thread::scope(|s| {
        for t in 0..THREADS {
            let (svc, metas, retention, conflicts, duplicates) = (&svc, &metas, &retention, &conflicts, &duplicates);
            s.spawn(move || {
                for (k, meta) in metas.iter().enumerate() {
                    let r =
                        publish_snapshot(svc, REPO, &format!("feature/t{t}-{k}"), RefKind::Branch, &meta.id, retention)
                            .unwrap();
                    conflicts.fetch_add(r.conflicts, Ordering::Relaxed);
                    duplicates.fetch_add(u32::from(r.duplicate), Ordering::Relaxed);
                }
            });
        }
    });

    let rd = repo_dir(REPO, None);
    let catalog = read_catalog(&svc, &rd);
    let kept = distinct_pairs(&catalog);
    let conflicts = conflicts.into_inner();
    eprintln!(
        "concurrent publishes: {kept} snapshots kept of {}, {} duplicates, {conflicts} CAS conflicts",
        THREADS * PER_THREAD,
        duplicates.load(Ordering::Relaxed),
    );
    assert_eq!(kept, THREADS * PER_THREAD);
    assert_heads(&catalog, &metas);
    assert_eq!(duplicates.into_inner(), 0);
    assert!(conflicts > 0, "the publishers never raced");
    assert_objects_present(&svc, &rd, &catalog);

    // The same (ref, commit) again is a duplicate and changes nothing.
    let again = publish_snapshot(&svc, REPO, "feature/t0-0", RefKind::Branch, &metas[0].id, &retention).unwrap();
    assert!(again.duplicate && again.trimmed.is_empty());
    assert!(!again.segment_uploaded && again.packs_uploaded == 0);
    assert_eq!(read_catalog(&svc, &rd), catalog);
    // A new commit on the ref replaces the head (branches keep one).
    let next = publish_snapshot(&svc, REPO, "feature/t0-0", RefKind::Branch, &metas[1].id, &retention).unwrap();
    assert!(!next.duplicate);
    assert_eq!(next.trimmed, vec![metas[0].id.clone()]);
}

/// Number of (ref, commit) pairs, asserting none repeats.
fn distinct_pairs(catalog: &RepoCatalog) -> usize {
    let mut pairs = HashSet::new();
    for (name, snap) in catalog.refs.iter().flat_map(|(n, e)| e.snapshots.iter().map(move |s| (n, s))) {
        assert!(pairs.insert((name.clone(), snap.commit.clone())), "duplicate ({name}, {})", snap.commit);
    }
    pairs.len()
}

/// Every ref `feature/t<t>-<k>` holds exactly snapshot `k`.
fn assert_heads(catalog: &RepoCatalog, metas: &[SegmentMeta]) {
    for (t, (k, meta)) in (0..THREADS).flat_map(|t| metas.iter().enumerate().map(move |x| (t, x))) {
        let entry = &catalog.refs[&format!("feature/t{t}-{k}")];
        assert_eq!(entry.snapshots.len(), 1);
        assert_eq!((&entry.snapshots[0].segment, &entry.snapshots[0].packs), (&meta.id, &meta.packs));
    }
}

/// Every object the catalog names is in the store.
fn assert_objects_present(svc: &ServiceStore, rd: &str, catalog: &RepoCatalog) {
    let listed: HashSet<String> = svc.storage.list("repos/").unwrap().into_iter().map(|m| m.path).collect();
    for snap in catalog.snapshots() {
        assert!(listed.contains(&format!("repos/{rd}/segments/{}.rcseg", snap.segment)));
        assert!(snap.packs.iter().all(|p| listed.contains(&format!("repos/{rd}/packs/{p}.rcpack"))));
    }
}

fn read_catalog(svc: &ServiceStore, rd: &str) -> RepoCatalog {
    let v = svc.storage.get(&format!("repos/{rd}/catalog.json")).unwrap().unwrap();
    serde_json::from_slice(&v.value).unwrap()
}
