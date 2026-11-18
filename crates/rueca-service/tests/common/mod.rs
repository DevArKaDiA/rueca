//! Shared test helpers: small graphs written straight into a local store,
//! and a storage wrapper that widens the compare-and-swap race window.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rueca_model::{FileEntry, Label, Language, Node, ResolvedGraph, SnapshotInfo, Span, git_blob_sha};
use rueca_segment::{ContentSource, SegmentMeta, write_segment};
use rueca_service::ServiceStore;
use rueca_storage::{ObjectMeta, ObjectVersion, PutError, Storage, Versioned};

pub struct Mem(HashMap<String, Vec<u8>>);

impl ContentSource for Mem {
    fn content(&self, file: &FileEntry) -> std::io::Result<Vec<u8>> {
        self.0.get(&file.path).cloned().ok_or_else(|| std::io::Error::other(file.path.clone()))
    }
}

/// A graph of Python files: one `File` node per file and one `Function`
/// per `def name(` line.
pub fn graph(repo: &str, commit: &str, files: &[(&str, &str)]) -> (ResolvedGraph, Mem) {
    let mut g = ResolvedGraph {
        snapshot: SnapshotInfo {
            repo: repo.into(),
            commit: commit.into(),
            dirty: false,
            branch: Some("main".into()),
            root: Some(format!("/src/{repo}")),
            indexed_at: 1_700_000_000,
            rueca_version: "test".into(),
        },
        nodes: vec![],
        edges: vec![],
        files: vec![],
        modules: vec![],
        contracts: vec![],
    };
    let mut mem = HashMap::new();
    for (path, src) in files {
        let fid = g.files.len() as u32;
        let fnode = g.nodes.len() as u32;
        g.nodes.push(node(Label::File, path, path, fid, None));
        g.files.push(FileEntry {
            path: (*path).into(),
            blob_sha: git_blob_sha(src.as_bytes()),
            language: Some(Language::Python),
            size: src.len() as u64,
            line_count: src.lines().count() as u32,
            node: fnode,
            status: "ok".into(),
        });
        mem.insert(path.to_string(), src.as_bytes().to_vec());
        for (line, l) in src.lines().enumerate() {
            if let Some(name) = l.strip_prefix("def ").and_then(|r| r.split('(').next()) {
                let span = Span {
                    start_byte: 0,
                    end_byte: src.len() as u32,
                    start_line: line as u32,
                    start_col: 0,
                    end_line: line as u32,
                    end_col: 0,
                };
                let qn = format!("{}.{name}", path.trim_end_matches(".py").replace('/', "."));
                g.nodes.push(node(Label::Function, name, &qn, fid, Some(span)));
            }
        }
    }
    g.validate().unwrap();
    (g, Mem(mem))
}

fn node(label: Label, name: &str, qn: &str, file: u32, span: Option<Span>) -> Node {
    Node {
        label,
        name: name.rsplit('/').next().unwrap().into(),
        qualified_name: qn.into(),
        file: Some(file),
        span,
        language: Some(Language::Python),
        signature: None,
        doc: None,
        exported: true,
        is_test: false,
    }
}

/// Writes a snapshot of `repo` at `commit` with `files` into the local
/// store at `store`.
pub fn write(store: &Path, repo: &str, commit: &str, files: &[(&str, &str)]) -> SegmentMeta {
    let (g, mem) = graph(repo, commit, files);
    write_segment(&g, &mem, store).unwrap()
}

/// A 40-digit commit from a number.
pub fn commit(n: usize) -> String {
    format!("{n:040x}")
}

pub fn open(url: &str, dir: &Path) -> ServiceStore {
    ServiceStore::open(url, &BTreeMap::new(), dir, "acme").unwrap()
}

/// Delegates to a backend; reading a catalog or a job object sleeps
/// first, so concurrent compare-and-swaps of one object overlap.
pub struct SlowCatalogs(pub Arc<dyn Storage>, pub Duration);

impl Storage for SlowCatalogs {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        let v = self.0.get(path);
        if path.ends_with("catalog.json") || path.starts_with("jobs/") {
            std::thread::sleep(self.1);
        }
        v
    }
    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.0.get_if_changed(path, known)
    }
    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        self.0.get_range(path, range)
    }
    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        self.0.get_to_file(path, dest)
    }
    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        self.0.put_new(path, bytes)
    }
    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        self.0.put_new_file(path, src)
    }
    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        self.0.put_if(path, bytes, expected)
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        self.0.list(prefix)
    }
    fn delete(&self, path: &str) -> anyhow::Result<()> {
        self.0.delete(path)
    }
    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.0.local_path(path)
    }
}

/// Delegates to a backend; the first write of a catalog first deletes
/// `victim`, as a collection that marked before the publisher's CAS would.
pub struct DeleteOnCatalogWrite(pub Arc<dyn Storage>, pub std::sync::Mutex<Option<String>>);

impl Storage for DeleteOnCatalogWrite {
    fn get(&self, path: &str) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.0.get(path)
    }
    fn get_if_changed(&self, path: &str, known: &ObjectVersion) -> anyhow::Result<Option<Versioned<Bytes>>> {
        self.0.get_if_changed(path, known)
    }
    fn get_range(&self, path: &str, range: Range<u64>) -> anyhow::Result<Bytes> {
        self.0.get_range(path, range)
    }
    fn get_to_file(&self, path: &str, dest: &Path) -> anyhow::Result<ObjectVersion> {
        self.0.get_to_file(path, dest)
    }
    fn put_new(&self, path: &str, bytes: Bytes) -> Result<ObjectVersion, PutError> {
        self.0.put_new(path, bytes)
    }
    fn put_new_file(&self, path: &str, src: &Path) -> Result<ObjectVersion, PutError> {
        self.0.put_new_file(path, src)
    }
    fn put_if(&self, path: &str, bytes: Bytes, expected: Option<&ObjectVersion>) -> Result<ObjectVersion, PutError> {
        if path.ends_with("catalog.json")
            && let Some(victim) = self.1.lock().unwrap().take()
        {
            self.0.delete(&victim)?;
        }
        self.0.put_if(path, bytes, expected)
    }
    fn list(&self, prefix: &str) -> anyhow::Result<Vec<ObjectMeta>> {
        self.0.list(prefix)
    }
    fn delete(&self, path: &str) -> anyhow::Result<()> {
        self.0.delete(path)
    }
    fn local_path(&self, path: &str) -> Option<PathBuf> {
        self.0.local_path(path)
    }
}

/// A local "code host": a working repository `src` and its bare clone
/// `<name>.git`, served as a `file://` URL.
pub struct GitHost {
    pub dir: tempfile::TempDir,
    pub src: PathBuf,
    pub bare: PathBuf,
}

/// Runs `git` in `dir` with a fixed identity; stdout, trimmed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl GitHost {
    /// A repository `name` with `files` committed on `main`.
    pub fn new(name: &str, files: &[(&str, &str)]) -> GitHost {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        git(&src, &["init", "--quiet", "-b", "main"]);
        let bare = dir.path().join(format!("{name}.git"));
        let host = GitHost { src, bare, dir };
        host.write(files);
        git(&host.src, &["commit", "--quiet", "-m", "first"]);
        git(host.dir.path(), &["clone", "--quiet", "--bare", "src", &format!("{name}.git")]);
        host
    }

    pub fn url(&self) -> String {
        format!("file://{}", self.bare.display())
    }

    fn write(&self, files: &[(&str, &str)]) {
        for (path, text) in files {
            let p = self.src.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, text).unwrap();
        }
        git(&self.src, &["add", "-A"]);
    }

    /// Commits `files` on `main` and pushes; the new commit.
    pub fn push(&self, files: &[(&str, &str)]) -> String {
        self.write(files);
        git(&self.src, &["commit", "--quiet", "-m", "change"]);
        let bare = self.bare.to_string_lossy().into_owned();
        git(&self.src, &["push", "--quiet", &bare, "main"]);
        self.commit("main")
    }

    /// Pushes `main`'s commit as branch or tag `refname` (`refs/...`).
    pub fn push_ref(&self, refname: &str) {
        let bare = self.bare.to_string_lossy().into_owned();
        git(&self.src, &["push", "--quiet", &bare, &format!("main:{refname}")]);
    }

    /// Deletes `refname` on the host.
    pub fn delete_ref(&self, refname: &str) {
        git(&self.bare, &["update-ref", "-d", refname]);
    }

    /// The commit of `rev` on the host.
    pub fn commit(&self, rev: &str) -> String {
        git(&self.bare, &["rev-parse", rev])
    }
}
