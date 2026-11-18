//! Bare mirrors (RFC 0008 §7.6–§7.7) against a local bare "host" served
//! over `file://` (`uploadpack.allowFilter` on, as hosted git servers
//! have it): a commit fetched by id at depth 1 with large blobs filtered
//! out, the export from the bare mirror listing them as coverage gaps and
//! the index recording them as `skipped:size`, a moved ref fetched again,
//! the fallback through the ref name when the host refuses a commit want,
//! and expiry. A `git daemon` host is not exercised (it needs a free port
//! and a daemon process per test); `file://` runs the same upload-pack.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use rueca_index::mirror::{MIRROR_MARKER, Mirror, RefMoved, expire_mirrors};
use rueca_index::refs::export_with;
use rueca_index::{IndexOptions, index_worktree_with};
use rueca_segment::Store;

const MAX_BYTES: u64 = 1000;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(["-c", "init.defaultBranch=main"])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// A working repository `src` pushing to the bare `host.git`.
struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    fn new() -> Host {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        git(&src, &["init", "-q"]);
        git(dir.path(), &["init", "-q", "--bare", "host.git"]);
        git(&dir.path().join("host.git"), &["config", "uploadpack.allowFilter", "true"]);
        Host { dir }
    }

    fn src(&self) -> PathBuf {
        self.dir.path().join("src")
    }

    fn bare(&self) -> PathBuf {
        self.dir.path().join("host.git")
    }

    fn url(&self) -> String {
        format!("file://{}", self.bare().display())
    }

    /// Writes `files`, commits and pushes `main`; the new commit.
    fn push(&self, files: &[(&str, String)]) -> String {
        for (path, text) in files {
            let p = self.src().join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        git(&self.src(), &["add", "-A"]);
        git(&self.src(), &["commit", "-q", "-m", "change"]);
        let bare = self.bare().to_string_lossy().into_owned();
        git(&self.src(), &["push", "-q", &bare, "HEAD:refs/heads/main"]);
        git(&self.src(), &["rev-parse", "HEAD"])
    }
}

fn mirror(root: &Path, host: &Host) -> Mirror {
    Mirror::open(&root.join("billing.git"), &host.url())
        .unwrap()
        .with_git(vec!["protocol.file.allow=always".into()], Vec::new())
}

fn big() -> String {
    "x".repeat(MAX_BYTES as usize + 500)
}

/// A mirror with `commit` fetched by id: bare, shallow, fetched once.
fn fetched_mirror(root: &Path, host: &Host, commit: &str) -> Mirror {
    let m = mirror(root, host);
    let fetched = m.fetch_commit(commit, MAX_BYTES, &[]).unwrap();
    assert_eq!((fetched.commit.as_str(), fetched.via.as_str()), (commit, "commit"));
    assert_eq!(m.fetch_commit(commit, MAX_BYTES, &[]).unwrap().via, "present", "no second fetch");
    assert_eq!(git(m.dir(), &["rev-parse", "--is-bare-repository"]), "true");
    assert_eq!(git(m.dir(), &["rev-parse", "--is-shallow-repository"]), "true");
    m
}

#[test]
fn a_commit_is_fetched_filtered_and_exported_with_its_gaps() {
    let host = Host::new();
    let small = "def issue_invoice(order):\n    return order\n".to_string();
    let commit = host.push(&[("billing/invoices.py", small.clone()), ("data/big.txt", big())]);
    let root = tempfile::tempdir().unwrap();
    let m = fetched_mirror(root.path(), &host, &commit);

    // The export reads the bare mirror; the filtered blob is a gap.
    let store = root.path().join("store");
    let dest = store.join("refs/billing/main");
    let report = export_with(m.dir(), &commit, &dest, Some(MAX_BYTES + 1)).unwrap();
    assert_eq!(report.files, 1);
    assert_eq!(report.absent.len(), 1, "{report:?}");
    assert_eq!(report.absent[0].path, "data/big.txt");
    assert_eq!(report.absent[0].blob_sha, git(&host.src(), &["rev-parse", "HEAD:data/big.txt"]));
    assert_eq!(fs::read_to_string(dest.join("billing/invoices.py")).unwrap(), small);
    assert!(!dest.join("data/big.txt").exists());
    // Without a gap size a missing blob is an error, as in local mode.
    assert!(export_with(m.dir(), &commit, &root.path().join("strict"), None).is_err());

    index_with_gap(&dest, &store, &commit, &report.absent);
}

/// The index lists the gap as `skipped:size`, with its blob id.
fn index_with_gap(dest: &Path, store: &Path, commit: &str, absent: &[rueca_index::AbsentFile]) {
    let opts = IndexOptions {
        root: dest.to_path_buf(),
        store: store.to_path_buf(),
        repo: Some("local/billing".into()),
        ref_name: Some("main".into()),
        commit: Some(commit.to_string()),
        threads: Some(2),
        ..IndexOptions::default()
    };
    let index = index_worktree_with(&opts, absent).unwrap();
    assert_eq!((index.files_total, index.files_skipped), (2, 1));
    let seg = Store::open(store).unwrap().open_segment(&index.segment.id).unwrap();
    let gap = seg.files().iter().find(|f| f.path == "data/big.txt").expect("the gap is in the file table");
    assert_eq!(gap.status, "skipped:size");
    assert_eq!(gap.blob_sha, absent[0].blob_sha);
    assert_eq!(gap.size, MAX_BYTES + 1, "the size is the filter's lower bound");
}

#[test]
fn a_moved_ref_is_fetched_again() {
    let host = Host::new();
    let one = host.push(&[("a.py", "def one():\n    return 1\n".into())]);
    let root = tempfile::tempdir().unwrap();
    let m = mirror(root.path(), &host);
    m.fetch_commit(&one, MAX_BYTES, &[]).unwrap();
    let two = host.push(&[("b.py", "def two():\n    return 2\n".into())]);
    assert_eq!(m.fetch_commit(&two, MAX_BYTES, &[]).unwrap().via, "commit");
    assert!(m.has_commit(&one) && m.has_commit(&two));
    let report = export_with(m.dir(), &two, &root.path().join("two"), Some(MAX_BYTES + 1)).unwrap();
    assert_eq!((report.files, report.absent.len()), (2, 0));
}

#[test]
fn a_refused_commit_want_falls_back_to_the_ref() {
    let host = Host::new();
    let old = host.push(&[("a.py", "def one():\n    return 1\n".into())]);
    // Protocol v0 with unadvertised wants refused, as a strict host.
    git(&host.bare(), &["config", "uploadpack.allowReachableSHA1InWant", "false"]);
    let root = tempfile::tempdir().unwrap();
    let v0 = |name: &str| {
        Mirror::open(&root.path().join(name), &host.url())
            .unwrap()
            .with_git(vec!["protocol.file.allow=always".into(), "protocol.version=0".into()], Vec::new())
    };
    let main = vec!["refs/heads/main".to_string()];
    let m = v0("tip.git");
    // The tip is advertised: wanted by id directly.
    assert_eq!(m.fetch_commit(&old, MAX_BYTES, &main).unwrap().via, "commit");

    let new = host.push(&[("b.py", "def two():\n    return 2\n".into())]);
    let m = v0("moved.git");
    let err = m.fetch_commit(&old, MAX_BYTES, &main).unwrap_err();
    let moved = err.downcast_ref::<RefMoved>().expect("the ref moved");
    assert_eq!((moved.tip.as_str(), moved.wanted.as_str()), (new.as_str(), old.as_str()));
    let err = m.fetch_commit(&old, MAX_BYTES, &[]).unwrap_err();
    assert!(format!("{err:#}").contains("unadvertised"), "{err:#}");

    // The commit of an annotated tag on no branch is not advertised (the
    // tag object is): refused by id, recorded through the tag's peeled tip.
    git(&host.src(), &["checkout", "-q", "--detach"]);
    fs::write(host.src().join("c.py"), "def three():\n    return 3\n").unwrap();
    git(&host.src(), &["add", "-A"]);
    git(&host.src(), &["commit", "-q", "-m", "tagged"]);
    git(&host.src(), &["tag", "-a", "v2", "-m", "v2"]);
    let tagged = git(&host.src(), &["rev-parse", "HEAD"]);
    git(&host.src(), &["push", "-q", &host.bare().to_string_lossy(), "refs/tags/v2"]);
    let m = v0("fallback.git");
    let f = m.fetch_commit(&tagged, MAX_BYTES, &["refs/tags/v2".to_string()]).unwrap();
    assert_eq!((f.commit.as_str(), f.via.as_str()), (tagged.as_str(), "refs/tags/v2"));
    assert!(m.has_commit(&tagged));
    assert!(m.fetch_commit(&tagged, MAX_BYTES, &[]).is_ok(), "present now");
}

#[test]
fn mirrors_expire_by_age() {
    let host = Host::new();
    host.push(&[("a.py", "x = 1\n".into())]);
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("billing.git");
    let day = 24 * 3600;
    let week = Duration::from_secs(7 * day);
    Mirror::open_with(&dir, &host.url(), Some(week), 1_000 * day).unwrap();
    fs::write(dir.join("objects/keep-me"), "").unwrap();
    // Within the age: kept; past it: re-created (empty again).
    Mirror::open_with(&dir, &host.url(), Some(week), 1_006 * day).unwrap();
    assert!(dir.join("objects/keep-me").exists());
    Mirror::open_with(&dir, &host.url(), Some(week), 1_008 * day).unwrap();
    assert!(!dir.join("objects/keep-me").exists() && dir.join(MIRROR_MARKER).exists());
    // Another URL re-creates it too.
    fs::write(dir.join("objects/keep-me"), "").unwrap();
    Mirror::open_with(&dir, "file:///elsewhere.git", Some(week), 1_008 * day).unwrap();
    assert!(!dir.join("objects/keep-me").exists());

    // The sweep removes mirrors unused for longer than the age, and
    // nothing without a marker.
    fs::create_dir_all(root.path().join("store")).unwrap();
    assert!(expire_mirrors(root.path(), week, 1_010 * day).is_empty());
    assert_eq!(expire_mirrors(root.path(), week, 1_016 * day), vec![dir.clone()]);
    assert!(!dir.exists() && root.path().join("store").exists());
}
