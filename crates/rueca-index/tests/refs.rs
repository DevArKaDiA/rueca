//! Ref snapshots end to end (RFC 0006 §9.3): resolve a branch, a pull
//! request, a tag or a commit; export it under the store; index it next to
//! the working tree with the facts cache; re-index it as a no-op; remove it.
//! Fixtures are built with the git CLI; every test skips where git is
//! missing.

use std::fs;
use std::path::{Path, PathBuf};

use rueca_index::refs::{self, RefIndexOptions, RefKind};
use rueca_index::{IndexOptions, index_worktree};
use rueca_segment::{CatalogEntry, Segment, Store};

fn write(root: &Path, path: &str, content: &str) {
    let p = root.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

/// Runs git in `dir` with a fixed identity; `false` when it fails or git is
/// missing.
fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(["-c", "tag.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn rev(dir: &Path, rev: &str) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(["rev-parse", rev]).output().unwrap();
    assert!(out.status.success(), "rev-parse {rev}");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit_all(dir: &Path, msg: &str) {
    assert!(git(dir, &["add", "-A"]));
    assert!(git(dir, &["commit", "-q", "-m", msg]));
}

const UTIL_PY: &str = "def helper(x):\n    return x * 2\n";
const UTIL_PY_BRANCH: &str = "def helper(x):\n    return x * 3\n\n\ndef feature_only():\n    return helper(1)\n";

/// A repository on `main` (two commits) with a branch `feature/x` that
/// changes `pkg/util.py` and adds `pkg/extra.py`; `main` checked out. `None`
/// without git.
fn fixture(root: &Path) -> Option<()> {
    if !git(root, &["init", "-q"]) {
        eprintln!("git not available; skipping");
        return None;
    }
    assert!(git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]));
    write(root, "pkg/__init__.py", "");
    write(root, "pkg/util.py", UTIL_PY);
    write(root, "app.py", "from pkg.util import helper\n\n\ndef main():\n    return helper(21)\n");
    commit_all(root, "one");
    write(root, "README.md", "# demo\n\nSee `app.py`.\n");
    write(root, "web/math.ts", "export function add(a: number, b: number): number {\n  return a + b;\n}\n");
    commit_all(root, "two");
    assert!(git(root, &["checkout", "-q", "-b", "feature/x"]));
    write(root, "pkg/util.py", UTIL_PY_BRANCH);
    write(root, "pkg/extra.py", "from pkg.util import feature_only\n\n\ndef extra():\n    return feature_only()\n");
    commit_all(root, "feature");
    assert!(git(root, &["checkout", "-q", "main"]));
    Some(())
}

fn ref_options(root: &Path, store: &Path, name: &str) -> RefIndexOptions {
    RefIndexOptions {
        root: root.to_path_buf(),
        name: name.to_string(),
        store: store.to_path_buf(),
        threads: Some(2),
        ..RefIndexOptions::default()
    }
}

fn entries(store: &Path) -> Vec<CatalogEntry> {
    Store::open(store).unwrap().entries().unwrap()
}

fn open(store: &Path, e: &CatalogEntry) -> Segment {
    Store::open(store).unwrap().open_segment(&e.segment_id).unwrap()
}

fn has_name(seg: &Segment, name: &str) -> bool {
    !seg.nodes_by_name(name).is_empty()
}

#[test]
fn branch_snapshot_next_to_the_working_tree() {
    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let root = &fs::canonicalize(project.path()).unwrap();
    let store = store_dir.path();
    if fixture(root).is_none() {
        return;
    }
    let main = index_worktree(&IndexOptions {
        root: root.clone(),
        store: store.to_path_buf(),
        threads: Some(2),
        ..IndexOptions::default()
    })
    .unwrap();

    let r = refs::index_ref(&ref_options(root, store, "feature/x")).unwrap();
    assert!(!r.unchanged);
    assert_eq!((r.ref_name.as_str(), r.kind, r.fetched), ("feature/x", RefKind::Branch, false));
    assert_eq!(r.source, "refs/heads/feature/x");
    assert_eq!(r.commit, rev(root, "feature/x"));
    assert_eq!(r.repo, main.segment.snapshot.repo);
    assert_eq!(r.export.files, 6, "every blob of the commit is exported");
    // The facts cache is shared: only the changed and the new file are
    // extracted again.
    assert!(r.index.facts_cache_hits >= r.index.files_extracted - 2, "{:?}", r.index);
    assert!(r.index.facts_cache_hits >= 4);

    // Two catalog entries of one repository; the ref one is marked.
    let all = entries(store);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|e| e.repo == main.segment.snapshot.repo));
    let wt = all.iter().find(|e| !e.is_ref_export()).unwrap();
    let rf = all.iter().find(|e| e.is_ref_export()).unwrap();
    assert_eq!(wt.root, root.to_string_lossy());
    assert_eq!(rf.ref_name.as_deref(), Some("feature/x"));
    assert_eq!(rf.root, r.export_dir);
    let store_canon = fs::canonicalize(store).unwrap();
    assert!(Path::new(&rf.root).starts_with(store_canon.join("refs")), "{}", rf.root);
    assert_eq!(PathBuf::from(&rf.root), refs::export_dir(&store_canon, &r.repo, "", "feature/x"));

    // The ref snapshot is pinned to the ref; the working tree is untouched.
    let seg = open(store, rf);
    let snap = seg.snapshot();
    assert_eq!(snap.branch.as_deref(), Some("feature/x"));
    assert_eq!(snap.commit, r.commit);
    assert!(!snap.dirty);
    assert!(has_name(&seg, "feature_only") && has_name(&seg, "extra"));
    assert!(!has_name(&open(store, wt), "feature_only"));
    assert_eq!(fs::read_to_string(root.join("pkg/util.py")).unwrap(), UTIL_PY);
    assert_eq!(rev(root, "HEAD"), rev(root, "main"), "nothing checked out");

    // Same commit again: nothing to do.
    let again = refs::index_ref(&ref_options(root, store, "feature/x")).unwrap();
    assert!(again.unchanged);
    assert_eq!(again.index.segment.id, r.index.segment.id);
    assert_eq!((again.index.files_extracted, again.export.files), (0, 0));

    // A new commit on the branch: re-exported and re-indexed, the old
    // segment collected.
    assert!(git(root, &["checkout", "-q", "feature/x"]));
    write(root, "pkg/extra.py", "def extra():\n    return 2\n\n\ndef later():\n    return 3\n");
    commit_all(root, "later");
    assert!(git(root, &["checkout", "-q", "main"]));
    let moved = refs::index_ref(&ref_options(root, store, "feature/x")).unwrap();
    assert!(!moved.unchanged);
    assert_eq!(moved.commit, rev(root, "feature/x"));
    assert_ne!(moved.index.segment.id, r.index.segment.id);
    assert!(!Store::open(store).unwrap().segment_path(&r.index.segment.id).exists());
    assert!(has_name(&open(store, &entries(store).into_iter().find(|e| e.is_ref_export()).unwrap()), "later"));

    // Removal drops the entry, its segment and the export; the working tree stays.
    let gone = refs::remove_ref(store, &r.repo, "", "feature/x").unwrap();
    assert!(gone.export_removed);
    assert_eq!(gone.segment_id.as_deref(), Some(moved.index.segment.id.as_str()));
    assert!(!Path::new(&r.export_dir).exists());
    assert!(!Store::open(store).unwrap().segment_path(&moved.index.segment.id).exists());
    let left = entries(store);
    assert_eq!(left.len(), 1);
    assert!(!left[0].is_ref_export());
    let err = refs::remove_ref(store, &r.repo, "", "feature/x").unwrap_err().to_string();
    assert!(err.contains("is not indexed"), "{err}");
}

#[test]
fn tags_commits_subdirectories_and_errors() {
    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let root = &fs::canonicalize(project.path()).unwrap();
    let store = store_dir.path();
    if fixture(root).is_none() {
        return;
    }
    let first = rev(root, "main~1");
    assert!(git(root, &["tag", "-a", "v1", "-m", "release", "main~1"]));

    let tag = refs::resolve(root, "v1").unwrap();
    assert_eq!((tag.kind, tag.commit.as_str(), tag.source.as_str()), (RefKind::Tag, first.as_str(), "refs/tags/v1"));
    let c = refs::resolve(root, &first[..8]).unwrap();
    assert_eq!((c.kind, c.name.as_str(), c.commit.as_str()), (RefKind::Commit, &first[..12], first.as_str()));
    let head = refs::resolve(root, "HEAD").unwrap();
    assert_eq!((head.kind, head.commit), (RefKind::Commit, rev(root, "main")));
    let err = refs::resolve(root, "nope").unwrap_err().to_string();
    assert!(err.contains("ref `nope` not found") && err.contains("refs/heads/nope"), "{err}");
    assert!(err.contains("no `origin` remote"), "{err}");
    assert!(refs::resolve(root, "a..b").is_err());

    // A commit is recorded under its 12-digit form, and removed by any prefix.
    let r = refs::index_ref(&ref_options(root, store, &first[..7])).unwrap();
    assert_eq!(r.ref_name, &first[..12]);
    let seg = open(store, &entries(store).into_iter().find(|e| e.is_ref_export()).unwrap());
    assert_eq!(seg.snapshot().branch.as_deref(), Some(&first[..12]));
    assert!(seg.file_by_path("README.md").is_none(), "README.md came with the second commit");
    assert!(refs::remove_ref(store, &r.repo, "", &first[..9]).unwrap().export_removed);

    // A subdirectory root exports (and indexes) only its subtree.
    // (Both roots carry the repository's id, as with an `origin` remote.)
    let mono = |dir: &str, name: &str| RefIndexOptions {
        repo: Some("github.com/acme/mono".into()),
        ..ref_options(&root.join(dir), store, name)
    };
    let sub = refs::index_ref(&mono("pkg", "feature/x")).unwrap();
    assert_eq!(sub.export.files, 3);
    let seg = open(store, &entries(store).into_iter().find(|e| e.is_ref_export()).unwrap());
    assert!(seg.file_by_path("util.py").is_some() && seg.file_by_path("extra.py").is_some());
    assert!(seg.file_by_path("app.py").is_none());

    // Another subdirectory root of the same repository, same ref: an export
    // of its own, not `pkg`'s.
    let web = refs::index_ref(&mono("web", "feature/x")).unwrap();
    assert!(!web.unchanged, "not taken for pkg's snapshot");
    assert_eq!((web.repo.as_str(), web.export.files), (sub.repo.as_str(), 1));
    assert_ne!(web.export_dir, sub.export_dir);
    assert!(Path::new(&sub.export_dir).join("util.py").exists() && Path::new(&web.export_dir).join("math.ts").exists());
    // Removing pkg's ref leaves web's.
    let gone = refs::remove_ref(store, &sub.repo, "pkg", "feature/x").unwrap();
    assert_eq!(gone.export_dir, sub.export_dir);
    assert!(!Path::new(&sub.export_dir).exists() && Path::new(&web.export_dir).join("math.ts").exists());
    let left: Vec<String> = entries(store).into_iter().filter(|e| e.is_ref_export()).map(|e| e.root).collect();
    assert_eq!(left, std::slice::from_ref(&web.export_dir));

    // A ref where the root's directory does not exist is an error, and
    // leaves nothing behind.
    let err = refs::index_ref(&mono("web", &first[..12])).unwrap_err().to_string();
    assert!(err.contains("has no files under `web`") && err.contains(&first[..12]), "{err}");
    assert_eq!(entries(store).into_iter().filter(|e| e.is_ref_export()).count(), 1);
}

#[test]
fn colliding_repository_slugs_keep_their_exports() {
    let project = tempfile::tempdir().unwrap();
    let store_dir = tempfile::tempdir().unwrap();
    let root = &fs::canonicalize(project.path()).unwrap();
    let store = store_dir.path();
    if fixture(root).is_none() {
        return;
    }
    // `acme/my.lib` and `acme/my-lib` have one slug.
    let dot = RefIndexOptions { repo: Some("acme/my.lib".into()), ..ref_options(root, store, "main") };
    let dash = RefIndexOptions { repo: Some("acme/my-lib".into()), ..ref_options(root, store, "main") };
    let a = refs::index_ref(&dot).unwrap();
    let b = refs::index_ref(&dash).unwrap();
    assert!(!b.unchanged, "the second repository is indexed, not refused or taken for the first");
    assert_ne!(a.export_dir, b.export_dir);
    // Removing one never deletes the other's export.
    refs::remove_ref(store, "acme/my-lib", "", "main").unwrap();
    assert!(Path::new(&a.export_dir).join("app.py").exists() && !Path::new(&b.export_dir).exists());
    let err = refs::remove_ref(store, "acme/my-lib", "", "main").unwrap_err().to_string();
    assert!(err.contains("is not indexed"), "{err}");
    assert!(Path::new(&a.export_dir).join("app.py").exists());
    // An export without a catalog entry is removed only by its owner.
    let store_canon = fs::canonicalize(store).unwrap();
    let orphan = refs::export_dir(&store_canon, "acme/other", "", "main");
    fs::create_dir_all(&orphan).unwrap();
    assert!(refs::remove_ref(store, "acme/other", "", "main").is_err());
    assert!(orphan.exists(), "no owner marker: not deleted");
    let e = entries(store).into_iter().find(|e| e.is_ref_export()).unwrap();
    Store::open(store).unwrap().unregister(&e.root).unwrap();
    let gone = refs::remove_ref(store, "acme/my.lib", "", "main").unwrap();
    assert!(gone.export_removed && !Path::new(&a.export_dir).exists());
    assert!(!Path::new(&a.export_dir).parent().unwrap().exists(), "the repository's directory goes with its last ref");
}

#[test]
fn pull_requests_and_branches_fetched_from_origin() {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let origin = base.join("origin");
    fs::create_dir_all(&origin).unwrap();
    if fixture(&origin).is_none() {
        return;
    }
    let pr_head = rev(&origin, "feature/x");
    assert!(git(&origin, &["update-ref", "refs/pull/7/head", &pr_head]));
    assert!(git(&origin, &["update-ref", "refs/merge-requests/8/head", "main~1"]));
    let url = format!("file://{}", origin.display());
    assert!(git(&base, &["clone", "-q", &url, "work"]));
    let work = base.join("work");
    assert!(git(&origin, &["branch", "late", "main~1"]), "a branch the clone has not seen");

    let pr = refs::resolve(&work, "pr/7").unwrap();
    assert_eq!((pr.kind, pr.commit.as_str(), pr.fetched), (RefKind::Pr, pr_head.as_str(), true));
    assert_eq!(pr.source, "refs/pull/7/head");
    let mr = refs::resolve(&work, "pr/8").unwrap();
    assert_eq!((mr.source.as_str(), mr.commit), ("refs/merge-requests/8/head", rev(&origin, "main~1")));
    let late = refs::resolve(&work, "late").unwrap();
    assert_eq!((late.kind, late.fetched, late.source.as_str()), (RefKind::Branch, true, "refs/remotes/origin/late"));
    let remote = refs::resolve(&work, "origin/feature/x").unwrap();
    assert_eq!((remote.commit.as_str(), remote.fetched), (pr_head.as_str(), false));
    let err = refs::resolve(&work, "pr/9").unwrap_err().to_string();
    assert!(err.contains("ref `pr/9` not found") && err.contains("refs/merge-requests/9/head"), "{err}");

    // Fetches write objects and refs/rueca/…, never a branch.
    let out = std::process::Command::new("git").arg("-C").arg(&work).args(["branch", "--list"]).output().unwrap();
    assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "* main");
    assert_eq!(rev(&work, "refs/rueca/pr/7"), pr_head);

    // Indexed as a snapshot of the clone's repository.
    let store = base.join("store");
    let r = refs::index_ref(&ref_options(&work, &store, "pr/7")).unwrap();
    assert_eq!((r.ref_name.as_str(), r.repo.as_str()), ("pr/7", "local/work"));
    let e = entries(&store).into_iter().find(|e| e.ref_name.as_deref() == Some("pr/7")).unwrap();
    assert!(has_name(&open(&store, &e), "feature_only"));
}
