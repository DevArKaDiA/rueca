//! Indexes small temporary projects end to end (walk → extract → resolve →
//! segment → catalog), plus an opt-in smoke run over real repositories.

use std::fs;
use std::path::Path;

use rueca_extract::Budget;
use rueca_index::{IndexOptions, IndexReport, index_worktree};
use rueca_model::EdgeKind;
use rueca_segment::{Segment, Store};

fn write(root: &Path, path: &str, content: &[u8]) {
    let p = root.join(path);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn options(root: &Path, store: &Path) -> IndexOptions {
    IndexOptions {
        root: root.to_path_buf(),
        store: store.to_path_buf(),
        repo: Some("local/demo".into()),
        budget: Budget::default(),
        threads: Some(2),
    }
}

fn open(store: &Path, root: &Path) -> Segment {
    let store = Store::open(store).unwrap();
    let entry = store.entry_for_path(root).unwrap().expect("root registered");
    store.open_segment(&entry.segment_id).unwrap()
}

/// The single node named `name` defined in `path`.
fn node(seg: &Segment, path: &str, name: &str) -> u32 {
    let file = seg.file_by_path(path).unwrap_or_else(|| panic!("no file {path}"));
    let hits: Vec<u32> = seg.nodes_by_name(name).into_iter().filter(|&n| seg.node(n).file == Some(file)).collect();
    assert_eq!(hits.len(), 1, "{name} in {path}");
    hits[0]
}

fn calls(seg: &Segment, src: u32, dst: u32) -> bool {
    seg.out_edges(src, Some(EdgeKind::Calls)).iter().any(|e| e.other == dst)
}

const CODE_FILES: u64 = 5;

fn demo_project(root: &Path) {
    write(root, ".gitignore", b"ignored/\n");
    write(root, "ignored/skip.py", b"def skipped():\n    pass\n");
    write(root, "pkg/__init__.py", b"");
    write(root, "pkg/util.py", b"def helper(x):\n    return x * 2\n");
    write(root, "app.py", b"from pkg.util import helper\n\n\ndef main():\n    return helper(21)\n");
    write(root, "web/math.ts", b"export function add(a: number, b: number): number {\n  return a + b;\n}\n");
    write(
        root,
        "web/app.ts",
        b"import { add } from './math';\n\nexport function run(): number {\n  return add(1, 2);\n}\n",
    );
    write(root, "data.bin", b"\x00\x01\x02binary\x00");
    write(root, "README.md", b"# Demo\n\nCall `main()`.\n");
    write(root, "node_modules/dep/index.js", b"module.exports = 1;\n");
}

#[test]
fn index_small_project_and_reindex_from_cache() {
    let project = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let root = &fs::canonicalize(project.path()).unwrap();
    demo_project(root);

    let first = index_worktree(&options(root, store.path())).unwrap();
    // .gitignore, pkg/__init__.py, pkg/util.py, app.py, web/math.ts, web/app.ts,
    // data.bin, README.md.
    assert_eq!(first.files_total, 8, "{first:?}");
    assert_eq!(first.files_extracted, CODE_FILES);
    assert_eq!(first.facts_cache_hits, 0);
    assert_eq!(first.files_unsupported, 2);
    assert_eq!(first.files_skipped, 1);
    assert_eq!(first.segment.snapshot.repo, "local/demo");
    assert_eq!(first.segment.snapshot.commit, "worktree");
    assert!(first.segment.snapshot.dirty);
    assert!(first.references_resolved >= 2);

    let seg = open(store.path(), root);
    assert!(seg.file_by_path("ignored/skip.py").is_none());
    assert!(seg.file_by_path("node_modules/dep/index.js").is_none());
    let bin = seg.file_by_path("data.bin").expect("binary files are listed");
    assert_eq!(seg.file(bin).status, "skipped:binary");
    let readme = seg.file_by_path("README.md").expect("non-code files are listed");
    assert_eq!(seg.file(readme).status, "skipped:unsupported");
    assert_eq!(seg.file(seg.file_by_path("app.py").unwrap()).status, "ok");

    // Cross-file calls through imports.
    let main = node(&seg, "app.py", "main");
    let helper = node(&seg, "pkg/util.py", "helper");
    assert!(calls(&seg, main, helper), "main -> helper");
    let run = node(&seg, "web/app.ts", "run");
    let add = node(&seg, "web/math.ts", "add");
    assert!(calls(&seg, run, add), "run -> add");
    let app_file = seg.file(seg.file_by_path("app.py").unwrap()).node;
    let util_file = seg.file(seg.file_by_path("pkg/util.py").unwrap()).node;
    assert!(seg.out_edges(app_file, Some(EdgeKind::Imports)).iter().any(|e| e.other == util_file));
    let node_count = seg.meta().node_count;
    let edge_count = seg.meta().edge_count;
    drop(seg);

    // Re-index: every code file comes from the facts cache; same graph size.
    let second = index_worktree(&options(root, store.path())).unwrap();
    assert_eq!(second.facts_cache_hits, CODE_FILES);
    assert_eq!(second.segment.node_count, node_count);
    assert_eq!(second.segment.edge_count, edge_count);
    assert_eq!(second.references_resolved, first.references_resolved);
    let seg = open(store.path(), root);
    assert_eq!(seg.meta().id, second.segment.id);
}

fn git(dir: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Snapshot identity comes from git (read with gix): origin remote, HEAD, and
/// a dirty flag that tracks the indexed content.
#[test]
fn git_snapshot_identity() {
    let project = tempfile::tempdir().unwrap();
    let store = tempfile::tempdir().unwrap();
    let root = &fs::canonicalize(project.path()).unwrap();
    demo_project(root);
    // The test fixture is created with the git CLI; skip where git is missing.
    if !git(root, &["init", "-q"]) {
        eprintln!("git not available; skipping");
        return;
    }
    assert!(git(root, &["add", "-A"]));
    assert!(git(root, &["commit", "-q", "-m", "init"]));
    assert!(git(root, &["remote", "add", "origin", "git@github.com:acme/demo.git"]));
    let head = std::process::Command::new("git").arg("-C").arg(root).args(["rev-parse", "HEAD"]).output().unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_string();

    let opts = IndexOptions { repo: None, ..options(root, store.path()) };
    let clean = index_worktree(&opts).unwrap();
    let snap = &clean.segment.snapshot;
    assert_eq!(snap.repo, "github.com/acme/demo");
    assert_eq!(snap.commit, head);
    assert!(!snap.dirty, "fresh commit must be clean");

    // A modification makes it dirty; so does a deletion of a tracked file.
    write(root, "app.py", b"def main():\n    return 1\n");
    assert!(index_worktree(&opts).unwrap().segment.snapshot.dirty);
    assert!(git(root, &["checkout", "--", "app.py"]));
    assert!(!index_worktree(&opts).unwrap().segment.snapshot.dirty);
    fs::remove_file(root.join("README.md")).unwrap();
    assert!(index_worktree(&opts).unwrap().segment.snapshot.dirty);
    assert!(git(root, &["checkout", "--", "README.md"]));
    // Untracked, non-ignored files are indexed, hence dirty; ignored ones are not.
    write(root, "ignored/more.py", b"x = 1\n");
    assert!(!index_worktree(&opts).unwrap().segment.snapshot.dirty);
    write(root, "new.py", b"x = 1\n");
    assert!(index_worktree(&opts).unwrap().segment.snapshot.dirty);
    fs::remove_file(root.join("new.py")).unwrap();

    // Indexing a subdirectory uses the enclosing repository.
    let sub = index_worktree(&IndexOptions { root: root.join("web"), ..opts.clone() }).unwrap();
    assert_eq!(sub.segment.snapshot.repo, "github.com/acme/demo");
    assert_eq!(sub.segment.snapshot.commit, head);
    assert!(!sub.segment.snapshot.dirty);
    assert_eq!(sub.files_total, 2);
}

/// Smoke run over real repositories, opt-in:
///
/// - `RUECA_SMOKE=1` shallow-clones `psf/requests` and `honojs/hono` into a
///   temporary directory (deleted afterwards);
/// - `RUECA_SMOKE_REPOS=/path/a,/path/b` indexes existing checkouts instead.
///
/// `RUECA_SMOKE=1 cargo test -p rueca-index --release --test index -- --ignored --nocapture smoke`
#[test]
#[ignore]
fn smoke_real_repositories() {
    let clones = tempfile::tempdir().unwrap();
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    if std::env::var_os("RUECA_SMOKE").is_some() {
        for url in ["https://github.com/psf/requests", "https://github.com/honojs/hono"] {
            let dest = clones.path().join(url.rsplit('/').next().unwrap());
            let ok = std::process::Command::new("git")
                .args(["clone", "-q", "--depth", "1", url])
                .arg(&dest)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "clone {url}");
            roots.push(dest);
        }
    }
    if let Ok(list) = std::env::var("RUECA_SMOKE_REPOS") {
        roots.extend(list.split(',').filter(|s| !s.is_empty()).map(Into::into));
    }
    if roots.is_empty() {
        eprintln!("set RUECA_SMOKE=1 or RUECA_SMOKE_REPOS; skipping");
        return;
    }
    for repo in roots {
        let store = tempfile::tempdir().unwrap();
        let opts = IndexOptions {
            root: repo,
            store: store.path().to_path_buf(),
            repo: None,
            budget: Budget::default(),
            threads: None,
        };
        let r = index_worktree(&opts).unwrap();
        print_report(&r);
        let again = index_worktree(&opts).unwrap();
        println!(
            "  re-index: {} ms, cache hits {}/{} extracted",
            again.duration_ms, again.facts_cache_hits, again.files_extracted
        );
        assert_eq!(again.facts_cache_hits, r.files_extracted);
        assert_eq!(again.segment.node_count, r.segment.node_count);
        assert_eq!(again.segment.edge_count, r.segment.edge_count);
    }
}

fn print_report(r: &IndexReport) {
    let pct = |n: u64| if r.references_total == 0 { 0.0 } else { 100.0 * n as f64 / r.references_total as f64 };
    let s = &r.segment.snapshot;
    println!("== {} @ {} (dirty {})", s.repo, s.commit, s.dirty);
    println!(
        "  files {} (extracted {}, partial {}, skipped {}, unsupported {})",
        r.files_total, r.files_extracted, r.files_partial, r.files_skipped, r.files_unsupported
    );
    println!(
        "  nodes {}, edges {}, segment {} bytes (+{} content)",
        r.segment.node_count, r.segment.edge_count, r.segment.bytes, r.segment.content_bytes_added
    );
    println!(
        "  references {} resolved {} ({:.1}%), builtin {} ({:.1}%), external {} ({:.1}%)",
        r.references_total,
        r.references_resolved,
        pct(r.references_resolved),
        r.references_builtin,
        pct(r.references_builtin),
        r.references_external,
        pct(r.references_external)
    );
    for (s, n) in &r.resolved_by_strategy {
        println!("    {s:<16} {n:>8} ({:.1}%)", pct(*n));
    }
    println!(
        "  total {} ms (walk {}, extract {}, resolve {}, write {})",
        r.duration_ms, r.timings.walk_ms, r.timings.extract_ms, r.timings.resolve_ms, r.timings.write_ms
    );
}
