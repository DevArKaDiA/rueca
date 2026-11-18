//! Garbage collection never deletes a referenced object (RFC 0008 §5.8,
//! §10): a seeded random interleaving of publishes, ref expiry,
//! organization generations and collections with grace 0 and a large
//! grace, then collections running concurrently with a publisher.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use common::{commit, write};
use rueca_segment::org::{current_generation, update_org};
use rueca_segment::{SegmentMeta, Store};
use rueca_service::{
    PullWhich, RefKind, RepoCatalog, Retention, ServiceStore, collect_garbage, publish_generation, publish_snapshot,
    pull_generation, pull_repo, repo_dir,
};
use rueca_storage::{CasStep, ObjectMeta, ObjectVersion, PutError, Storage, Versioned, cas_update};

const REPO: &str = "github.com/acme/core";
const REFS: [(&str, RefKind); 5] = [
    ("main", RefKind::Default),
    ("release/1", RefKind::Branch),
    ("v1", RefKind::Tag),
    ("pr/1", RefKind::Pr),
    ("pr/2", RefKind::Pr),
];
const HOUR: Duration = Duration::from_secs(3600);

/// Snapshot `k`: a file every snapshot shares, one of its own and, with
/// `rotate`, one of three rotating files, so snapshots share packs and a
/// pack can lose every reference and gain one again.
fn pool(dir: &Path, range: Range<usize>, rotate: bool) -> Vec<SegmentMeta> {
    range
        .map(|k| {
            let (rot, own) = (format!("core/r{}.py", k % 3), format!("core/s{k}.py"));
            let (rot_src, own_src) =
                (format!("def rot_{}():\n    return 0\n", k % 3), format!("def s{k}(x):\n    return x * {k}\n"));
            let mut files = vec![("core/common.py", "def common():\n    return 1\n"), (own.as_str(), own_src.as_str())];
            if rotate {
                files.push((rot.as_str(), rot_src.as_str()));
            }
            write(dir, REPO, &commit(k), &files)
        })
        .collect()
}

fn listing(svc: &ServiceStore, prefix: &str) -> BTreeSet<String> {
    svc.storage.list(prefix).unwrap().into_iter().map(|m| m.path).collect()
}

fn catalog(svc: &ServiceStore) -> Option<RepoCatalog> {
    let v = svc.storage.get(&format!("repos/{}/catalog.json", repo_dir(REPO, None))).unwrap()?;
    Some(serde_json::from_slice(&v.value).unwrap())
}

/// Objects under `repos/` a reader may need: every catalog snapshot and
/// every segment of the current and previous generation, with their
/// packs (read from the indexer's local copy of the segment).
fn live_repo_objects(svc: &ServiceStore, local: &Store) -> BTreeSet<String> {
    let rd = repo_dir(REPO, None);
    let mut segments: BTreeSet<String> = BTreeSet::new();
    if let Some(c) = catalog(svc) {
        segments.extend(c.snapshots().map(|s| s.segment.clone()));
    }
    for g in kept_generations(svc) {
        segments.extend(g.repos.iter().flat_map(|r| r.entries()).map(|e| e.segment_id.clone()));
    }
    let mut live = BTreeSet::new();
    for s in segments {
        for p in local.open_segment(&s).unwrap().meta().packs.clone() {
            live.insert(format!("repos/{rd}/packs/{p}.rcpack"));
        }
        live.insert(format!("repos/{rd}/segments/{s}.rcseg"));
    }
    live
}

/// The current and previous generation in the store.
fn kept_generations(svc: &ServiceStore) -> Vec<rueca_segment::org::OrgGeneration> {
    let Some(v) = svc.storage.get("orgs/acme/org.json").unwrap() else { return Vec::new() };
    let n = serde_json::from_slice::<serde_json::Value>(&v.value).unwrap()["generation"].as_u64().unwrap();
    (n.saturating_sub(1).max(1)..=n)
        .map(|k| {
            let g = svc.storage.get(&format!("orgs/acme/generations/{k:010}.json")).unwrap();
            serde_json::from_slice(&g.unwrap_or_else(|| panic!("generation {k} deleted")).value).unwrap()
        })
        .collect()
}

/// Every live object is in the store, and a fresh replica can pull every
/// ref and the current generation.
fn assert_live(svc: &ServiceStore, local: &Store, step: usize) {
    let present = listing(svc, "repos/");
    for o in live_repo_objects(svc, local) {
        assert!(present.contains(&o), "step {step}: live object {o} was deleted");
    }
    let replica_dir = tempfile::tempdir().unwrap();
    let replica = ServiceStore::with_storage(svc.storage.clone(), replica_dir.path(), "acme").unwrap();
    if catalog(svc).is_some() {
        pull_repo(&replica, REPO, PullWhich::All).unwrap();
    }
    pull_generation(&replica, None).unwrap();
}

/// After a collection with grace 0 (nothing in flight), the store holds
/// exactly the live objects.
fn assert_exact(svc: &ServiceStore, local: &Store, step: usize) {
    let present: BTreeSet<String> =
        listing(svc, "repos/").into_iter().filter(|p| p.contains("/segments/") || p.contains("/packs/")).collect();
    assert_eq!(present, live_repo_objects(svc, local), "step {step}");
    let kept = kept_generations(svc);
    let gens: BTreeSet<String> = listing(svc, "orgs/acme/generations/");
    let want: BTreeSet<String> =
        kept.iter().map(|g| format!("orgs/acme/generations/{:010}.json", g.generation)).collect();
    assert_eq!(gens, want, "step {step}");
    let entries = || kept.iter().flat_map(|g| g.repos.iter().flat_map(|r| r.entries()));
    let uuids: HashSet<String> = entries().filter_map(|e| e.tantivy_segment.clone()).collect();
    for f in listing(svc, "orgs/acme/index/") {
        let uuid = f.trim_start_matches("orgs/acme/index/").split('.').next().unwrap().to_string();
        assert!(uuids.contains(&uuid), "step {step}: unreferenced index file {f} survived");
    }
    let profiles: BTreeSet<String> =
        entries().filter_map(|e| e.profile.clone()).map(|p| format!("orgs/acme/profiles/{p}")).collect();
    assert_eq!(listing(svc, "orgs/acme/profiles/"), profiles, "step {step}");
}

/// Marks a pull request closed eight days ago (the refs lifecycle of
/// phase 2 does this from webhooks).
fn close(svc: &ServiceStore, name: &str) {
    if catalog(svc).is_none() {
        return;
    }
    let path = format!("repos/{}/catalog.json", repo_dir(REPO, None));
    let at = rueca_unix_now() - 8 * 24 * 3600;
    cas_update(svc.storage.as_ref(), &path, 8, |c: Option<RepoCatalog>| {
        let mut c = c.unwrap();
        match c.refs.get_mut(name) {
            Some(r) if r.closed_at.is_none() => {
                r.closed_at = Some(at);
                Ok(CasStep::Write(c))
            }
            _ => Ok(CasStep::Unchanged(c)),
        }
    })
    .unwrap();
}

fn rueca_unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
}

/// The updater: pull every ref, rebuild the organization, publish it.
fn new_generation(updater: &ServiceStore) {
    if catalog(updater).is_none_or(|c| c.default_ref.is_none()) {
        return;
    }
    pull_repo(updater, REPO, PullWhich::All).unwrap();
    update_org(&updater.local, "acme", &updater.mirror_members().unwrap()).unwrap();
    publish_generation(updater).unwrap();
}

#[test]
fn randomized_publishes_and_collections_never_delete_live_objects() {
    let url = "memory://gc-randomized";
    let indexer_dir = tempfile::tempdir().unwrap();
    let updater_dir = tempfile::tempdir().unwrap();
    let indexer = common::open(url, indexer_dir.path());
    let updater = common::open(url, updater_dir.path());
    let metas = pool(indexer_dir.path(), 0..9, true);
    let retention = Retention::default();
    let large = Retention { grace: HOUR, ..Retention::default() };
    let zero = Retention { grace: Duration::ZERO, ..Retention::default() };
    let mut rng = fastrand::Rng::with_seed(0x005e_ed6c);
    let (mut deleted_zero, mut expired, mut generations, mut collections) = (0usize, 0usize, 0usize, 0usize);
    const STEPS: usize = 120;
    for step in 0..STEPS {
        match rng.u32(0..12) {
            0..=5 => {
                let (name, kind) = REFS[rng.usize(..REFS.len())];
                let meta = &metas[rng.usize(..metas.len())];
                publish_snapshot(&indexer, REPO, name, kind, &meta.id, &retention).unwrap();
            }
            6 => close(&indexer, REFS[rng.usize(3..5)].0),
            7 | 8 => {
                new_generation(&updater);
                generations += 1;
            }
            9 => {
                let r = collect_garbage(&indexer, &large, SystemTime::now()).unwrap();
                assert!(r.deleted.is_empty(), "step {step}: grace 1 h deleted young objects: {:?}", r.deleted);
                expired += r.refs_expired.len();
                collections += 1;
                assert_live(&indexer, &indexer.local, step);
            }
            _ => {
                let r = collect_garbage(&indexer, &zero, SystemTime::now() + Duration::from_secs(1)).unwrap();
                deleted_zero += r.deleted.len();
                expired += r.refs_expired.len();
                collections += 1;
                assert_live(&indexer, &indexer.local, step);
                assert_exact(&indexer, &indexer.local, step);
            }
        }
    }
    eprintln!(
        "gc randomized: {STEPS} steps, {collections} collections, {generations} generation builds, {expired} refs \
         expired, {deleted_zero} unreferenced objects deleted with grace 0, live objects deleted: 0"
    );
    assert!(deleted_zero > 0 && expired > 0 && collections > 10, "the interleaving exercised too little");
    let g = current_generation(&updater.local.dir, "acme").unwrap();
    assert!(g.is_some_and(|g| g.generation > 1));
}

/// A backend whose listing reports the objects of `old` as written two
/// hours earlier (the in-memory backend stamps writes with the real clock).
struct Aged {
    inner: Arc<dyn Storage>,
    old: Mutex<HashSet<String>>,
}

impl Aged {
    fn age_all(&self) {
        let all = self.inner.list("").unwrap();
        self.old.lock().unwrap().extend(all.into_iter().map(|m| m.path));
    }
}

impl Storage for Aged {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.inner.get(path)
    }
    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.inner.get_if_changed(path, known)
    }
    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        self.inner.get_range(path, range)
    }
    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        self.inner.get_to_file(path, dest)
    }
    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        self.inner.put_new(path, bytes)
    }
    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        self.inner.put_new_file(path, src)
    }
    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        self.inner.put_if(path, bytes, expected)
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        let old = self.old.lock().unwrap();
        let mut out = self.inner.list(prefix)?;
        for m in &mut out {
            if old.contains(&m.path) {
                m.last_modified = m.last_modified.map(|t| t - 2 * HOUR);
            }
        }
        Ok(out)
    }
    fn delete(&self, path: &str) -> anyhow::Result<()> {
        self.inner.delete(path)
    }
    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.inner.local_path(path)
    }
}

#[test]
fn collections_concurrent_with_a_publisher_delete_only_old_garbage() {
    let dir = tempfile::tempdir().unwrap();
    // No rotating files: a pack that lost every reference is never
    // referenced again (the race the crate docs leave to phase 2).
    let metas = pool(dir.path(), 0..40, false);
    let aged = Arc::new(Aged {
        inner: rueca_storage::open("memory://gc-concurrent", &BTreeMap::new()).unwrap(),
        old: Mutex::new(HashSet::new()),
    });
    let svc = ServiceStore::with_storage(aged.clone(), dir.path(), "acme").unwrap();
    let retention = Retention { grace: HOUR, ..Retention::default() };
    let rd = repo_dir(REPO, None);
    // Old state: ten publishes of `main` (eight trimmed: garbage) and a
    // stray segment, all two hours old.
    for m in &metas[..10] {
        publish_snapshot(&svc, REPO, "main", RefKind::Default, &m.id, &retention).unwrap();
    }
    let stray = format!("repos/{rd}/segments/stray.rcseg");
    svc.storage.put_new(&stray, Bytes::from_static(b"left by a crashed publisher")).unwrap();
    aged.age_all();
    let old: HashSet<String> = aged.old.lock().unwrap().clone();

    let done = AtomicBool::new(false);
    let (deleted, iterations) = std::thread::scope(|s| {
        let gc = s.spawn(|| {
            let (mut deleted, mut iterations) = (Vec::new(), 0usize);
            while !done.load(Ordering::Acquire) || iterations < 3 {
                let r = collect_garbage(&svc, &retention, SystemTime::now()).unwrap();
                deleted.extend(r.deleted);
                iterations += 1;
            }
            (deleted, iterations)
        });
        let mut rng = fastrand::Rng::with_seed(0xc0c0);
        for m in &metas[10..] {
            let (name, kind) = REFS[rng.usize(..4)];
            publish_snapshot(&svc, REPO, name, kind, &m.id, &retention).unwrap();
        }
        done.store(true, Ordering::Release);
        gc.join().unwrap()
    });
    // Two-round deletion: the collections during the publishes only
    // recorded the old garbage as candidates; one a grace later deletes it.
    assert!(deleted.is_empty(), "deleted in the recording rounds: {deleted:?}");
    let last = collect_garbage(&svc, &retention, SystemTime::now() + 2 * HOUR).unwrap();
    let deleted = last.deleted;
    let live = live_repo_objects(&svc, &svc.local);
    let live_deleted: Vec<&String> = deleted.iter().filter(|d| live.contains(*d) || !old.contains(*d)).collect();
    eprintln!(
        "gc concurrent: {iterations} collections during 30 publishes (candidates only), then {} old unreferenced \
         objects deleted a grace later; live or young objects deleted: {}",
        deleted.len(),
        live_deleted.len()
    );
    assert!(live_deleted.is_empty(), "deleted live or young objects: {live_deleted:?}");
    assert_live(&svc, &svc.local, 0);
    // The old garbage is gone: the stray object and the trimmed snapshots.
    let present = listing(&svc, "repos/");
    assert!(!present.contains(&stray));
    for m in &metas[..8] {
        assert!(!present.contains(&format!("repos/{rd}/segments/{}.rcseg", m.id)), "{} survived", m.id);
    }
    assert!(deleted.len() >= 9);
}

fn candidates(svc: &ServiceStore) -> BTreeMap<String, u64> {
    let Some(v) = svc.storage.get("gc/candidates.json").unwrap() else { return BTreeMap::new() };
    let c: serde_json::Value = serde_json::from_slice(&v.value).unwrap();
    serde_json::from_value(c["candidates"].clone()).unwrap()
}

#[test]
fn deletion_takes_two_rounds_and_a_reference_cancels_it() {
    let dir = tempfile::tempdir().unwrap();
    let metas = pool(dir.path(), 0..2, false);
    let aged = Arc::new(Aged {
        inner: rueca_storage::open("memory://gc-two-rounds", &BTreeMap::new()).unwrap(),
        old: Mutex::new(HashSet::new()),
    });
    let svc = ServiceStore::with_storage(aged.clone(), dir.path(), "acme").unwrap();
    let retention = Retention { grace: HOUR, ..Retention::default() };
    let rd = repo_dir(REPO, None);
    // Snapshot 1 is published, then trimmed by snapshot 0: its segment and
    // its own pack are unreferenced (snapshot 0 keeps the shared pack).
    publish_snapshot(&svc, REPO, "release", RefKind::Branch, &metas[1].id, &retention).unwrap();
    publish_snapshot(&svc, REPO, "release", RefKind::Branch, &metas[0].id, &retention).unwrap();
    let own_pack = metas[1].packs.iter().find(|p| !metas[0].packs.contains(p)).unwrap();
    let pack = format!("repos/{rd}/packs/{own_pack}.rcpack");
    let segment = format!("repos/{rd}/segments/{}.rcseg", metas[1].id);
    let stray = format!("repos/{rd}/segments/stray.rcseg");
    svc.storage.put_new(&stray, Bytes::from_static(b"never referenced")).unwrap();
    aged.age_all();
    let t0 = SystemTime::now();

    // Round 1 only records the three as candidates.
    let r1 = collect_garbage(&svc, &retention, t0).unwrap();
    assert!(r1.deleted.is_empty() && r1.candidates == 3, "{r1:?}");
    let c1 = candidates(&svc);
    assert!([&pack, &segment, &stray].iter().all(|p| c1.contains_key(*p)), "{c1:?}");

    // A publish references snapshot 1 again: it takes its objects back.
    publish_snapshot(&svc, REPO, "v1", RefKind::Tag, &metas[1].id, &retention).unwrap();
    let c = candidates(&svc);
    assert!(!c.contains_key(&pack) && !c.contains_key(&segment) && c.contains_key(&stray), "{c:?}");
    // Even listed again with its old time (a rescue that lost a race), a
    // referenced pack is kept and leaves the list.
    let t_pack = c1[&pack];
    cas_update(svc.storage.as_ref(), "gc/candidates.json", 8, |v: Option<serde_json::Value>| {
        let mut v = v.unwrap();
        v["candidates"][&pack] = t_pack.into();
        Ok(CasStep::Write(v))
    })
    .unwrap();

    // Within the grace of the first sighting nothing is deleted.
    let early = collect_garbage(&svc, &retention, t0 + HOUR / 2).unwrap();
    assert!(early.deleted.is_empty(), "{early:?}");
    // A grace later: the never-referenced object goes, the referenced ones
    // stay and leave the candidate list.
    let r2 = collect_garbage(&svc, &retention, t0 + 2 * HOUR).unwrap();
    assert_eq!(r2.deleted, vec![stray.clone()]);
    let present = listing(&svc, "repos/");
    assert!(present.contains(&pack) && present.contains(&segment) && !present.contains(&stray));
    let c2 = candidates(&svc);
    assert!(!c2.contains_key(&pack) && !c2.contains_key(&segment) && !c2.contains_key(&stray), "{c2:?}");
    assert_live(&svc, &svc.local, 2);
}
