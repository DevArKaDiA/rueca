//! `status` of a service store (RFC 0008 §10: `rueca status` works against
//! a bucket).

mod common;

use common::{commit, open, write};
use rueca_segment::org::update_org;
use rueca_service::{PullWhich, RefKind, Retention, publish_generation, publish_snapshot, pull_repo, repo_dir, status};

#[test]
fn status_reports_repos_refs_generation_and_bytes() {
    let url = "memory://status";
    let dir = tempfile::tempdir().unwrap();
    let svc = open(url, dir.path());
    let empty = status(&svc).unwrap();
    assert_eq!(empty.store.as_ref().map(|s| s.org.as_str()), Some("acme"));
    assert!(empty.repos.is_empty() && empty.generation.is_none());
    assert_eq!(empty.objects, 1, "store.json only");

    let repo = "github.com/acme/billing";
    let r = Retention::default();
    let m0 = write(dir.path(), repo, &commit(0), &[("a.py", "def a():\n    return 0\n")]);
    let m1 = write(dir.path(), repo, &commit(1), &[("a.py", "def a():\n    return 1\n")]);
    let m2 = write(dir.path(), repo, &commit(2), &[("b.py", "def b():\n    return 2\n")]);
    publish_snapshot(&svc, repo, "main", RefKind::Default, &m0.id, &r).unwrap();
    publish_snapshot(&svc, repo, "main", RefKind::Default, &m1.id, &r).unwrap();
    publish_snapshot(&svc, repo, "pr/7", RefKind::Pr, &m2.id, &r).unwrap();
    let updater_dir = tempfile::tempdir().unwrap();
    let updater = open(url, updater_dir.path());
    pull_repo(&updater, repo, PullWhich::All).unwrap();
    update_org(&updater.local, "acme", &updater.mirror_members().unwrap()).unwrap();
    publish_generation(&updater).unwrap();

    let st = status(&svc).unwrap();
    assert_eq!(st.generation, Some(1));
    assert_eq!(st.repos.len(), 1);
    let rs = &st.repos[0];
    assert_eq!((rs.repo.as_str(), rs.repo_dir.clone()), (repo, repo_dir(repo, None)));
    assert_eq!(rs.default_ref.as_deref(), Some("main"));
    let refs: Vec<(&str, RefKind, usize, Option<&str>)> =
        rs.refs.iter().map(|r| (r.ref_name.as_str(), r.kind, r.snapshots, r.segment.as_deref())).collect();
    assert_eq!(
        refs,
        [("main", RefKind::Default, 2, Some(m1.id.as_str())), ("pr/7", RefKind::Pr, 1, Some(m2.id.as_str()))]
    );
    assert_classes(&st);
    let json = serde_json::to_value(&st).unwrap();
    assert_eq!(json["repos"][0]["refs"][1]["ref"], "pr/7");
    assert_eq!(json["repos"][0]["refs"][1]["kind"], "pr");
}

/// Every class of object is counted, with the expected counts.
fn assert_classes(st: &rueca_service::ServiceStatus) {
    for class in ["catalogs", "segments", "packs", "pointers", "generations", "org_index", "profiles"] {
        let c = st.classes.get(class).unwrap_or_else(|| panic!("no {class}: {:?}", st.classes));
        assert!(c.objects > 0 && c.bytes > 0, "{class}: {c:?}");
    }
    assert_eq!(st.classes["segments"].objects, 3);
    assert_eq!(st.classes["catalogs"].objects, 1);
    assert_eq!(st.objects, st.classes.values().map(|c| c.objects).sum::<u64>());
}
