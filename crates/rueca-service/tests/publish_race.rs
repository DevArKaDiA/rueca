//! A pack deleted between a publish's upload step and its catalog CAS (a
//! collection that marked before the CAS) is restored by the publish:
//! a referenced pack is present (crate docs, *Sequences*).

mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use common::{DeleteOnCatalogWrite, commit, write};
use rueca_service::{PullWhich, RefKind, Retention, ServiceStore, publish_snapshot, pull_repo, repo_dir};
use rueca_storage::Storage;

const REPO: &str = "github.com/acme/billing";

#[test]
fn a_pack_deleted_before_the_catalog_write_is_restored() {
    let local = tempfile::tempdir().unwrap();
    let meta = write(local.path(), REPO, &commit(1), &[("billing/a.py", "def a():\n    return 1\n")]);
    assert_eq!(meta.packs.len(), 1);
    let rd = repo_dir(REPO, None);
    let pack = format!("repos/{rd}/packs/{}.rcpack", meta.packs[0]);
    let memory = rueca_storage::open("memory://publish-race", &BTreeMap::new()).unwrap();
    let storage: Arc<dyn Storage> = Arc::new(DeleteOnCatalogWrite(memory.clone(), Mutex::new(Some(pack.clone()))));
    let svc = ServiceStore::with_storage(storage, local.path(), "acme").unwrap();

    let report = publish_snapshot(&svc, REPO, "main", RefKind::Default, &meta.id, &Retention::default()).unwrap();
    assert!(!report.duplicate && report.packs_uploaded == 1);
    assert_eq!(report.repaired, 1, "{report:?}");
    assert!(memory.get(&pack).unwrap().is_some(), "the referenced pack is missing");

    // A fresh replica opens the snapshot, content included.
    let mirror = tempfile::tempdir().unwrap();
    let replica = ServiceStore::with_storage(memory, mirror.path(), "acme").unwrap();
    let pulled = pull_repo(&replica, REPO, PullWhich::Default).unwrap();
    assert_eq!((pulled.segments_downloaded, pulled.packs_downloaded), (1, 1));
    let seg = replica.local.open_segment(&meta.id).unwrap();
    let file = seg.file_by_path("billing/a.py").unwrap();
    assert_eq!(seg.content(file).unwrap(), b"def a():\n    return 1\n");
}
