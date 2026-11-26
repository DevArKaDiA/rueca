//! End-to-end shell resolution with the real `ShellPack` over the fixture
//! in `rueca-lang-shell/tests/fixtures/tools`: an extension-less entry
//! script that sources a library through `$(cd "$(dirname …)" && pwd)`,
//! a library sourcing another (transitive), two libraries defining one
//! name (ambiguous), a `trap` handler, a script run by path, an env read
//! of a variable a sourced file assigns, bats tests and a library calling
//! a function only another library defines (unique name).

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, LanguagePack, ProjectFiles, Registry};
use rueca_lang_shell::ShellPack;
use rueca_model::{EdgeKind, Label, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-shell/tests/fixtures/tools")
}

fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, root, out);
        } else {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

fn language(path: &str, src: &[u8]) -> Option<Language> {
    Language::from_path(path).or_else(|| Language::from_shebang(src))
}

fn build(files: Vec<(String, Vec<u8>)>) -> ResolveOutput {
    let mut registry = Registry::new();
    registry.register(Box::new(ShellPack));
    let project = ProjectFiles { paths: files.iter().map(|(p, _)| p.clone()).collect(), ..Default::default() };
    let inputs = files
        .iter()
        .map(|(path, src)| {
            let lang = language(path, src).filter(|l| *l == Language::Shell);
            let facts = lang.map(|l| ShellPack.extract(path, l, src, &Budget::default()).expect("extract"));
            FileInput {
                path: path.clone(),
                blob_sha: git_blob_sha(src),
                language: lang,
                size: src.len() as u64,
                status: if facts.is_some() { "ok".into() } else { "skipped:unsupported".into() },
                facts,
            }
        })
        .collect();
    let snapshot = SnapshotInfo {
        repo: "local/tools".into(),
        commit: "worktree".into(),
        dirty: true,
        branch: None,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    };
    let out = resolve(snapshot, inputs, &project, &registry);
    out.graph.validate().unwrap();
    out
}

fn fixture() -> ResolveOutput {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    build(files)
}

struct G<'g>(&'g ResolvedGraph);

impl G<'_> {
    #[track_caller]
    fn id(&self, qn: &str) -> NodeId {
        let hits: Vec<NodeId> = (0..self.0.nodes.len() as NodeId)
            .filter(|&i| {
                let n = &self.0.nodes[i as usize];
                n.qualified_name == qn && !matches!(n.label, Label::File | Label::Folder)
            })
            .collect();
        assert_eq!(hits.len(), 1, "expected one node {qn}, got {hits:?}");
        hits[0]
    }
    fn file(&self, path: &str) -> NodeId {
        self.0.files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no file {path}")).node
    }
    fn find(&self, src: NodeId, dst: NodeId, kind: EdgeKind) -> Option<(Strategy, f32)> {
        self.0
            .edges
            .iter()
            .find(|e| e.src == src && e.dst == dst && e.kind == kind)
            .map(|e| (e.meta.strategy, e.meta.confidence))
    }
    #[track_caller]
    fn has_ids(&self, s: NodeId, d: NodeId, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let got = self.find(s, d, kind).unwrap_or_else(|| panic!("missing {kind} {s} -> {d}"));
        assert_eq!(got.0, strategy, "{kind} {s} -> {d}");
        assert!((got.1 - confidence).abs() < 1e-4, "{kind} {s} -> {d}: {} != {confidence}", got.1);
    }
    #[track_caller]
    fn has(&self, src: &str, dst: &str, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        self.has_ids(self.id(src), self.id(dst), kind, strategy, confidence);
    }
    #[track_caller]
    fn callees(&self, src: &str) -> Vec<String> {
        let s = self.id(src);
        let mut v: Vec<String> = self
            .0
            .edges
            .iter()
            .filter(|e| e.src == s && e.kind == EdgeKind::Calls)
            .map(|e| self.0.nodes[e.dst as usize].qualified_name.clone())
            .collect();
        v.sort();
        v
    }
}

use EdgeKind::*;
use Strategy::*;

#[test]
fn sourced_functions_scripts_and_the_bridge() {
    let out = fixture();
    let g = G(&out.graph);
    let deploy = "bin/deploy:deploy";
    // Own file first, then the sourced libraries (transitively).
    // A trap handler is a reference, not a call.
    g.has(deploy, "bin/deploy:cleanup", References, SameModule, 0.9);
    g.has(deploy, "bin/deploy:version", Calls, SameModule, 0.9);
    g.has(deploy, "lib/common.sh:log", Calls, ImportResolved, 0.95);
    g.has(deploy, "lib/net.sh:fetch", Calls, ImportResolved, 0.95);
    // `pick` is defined by two sourced files; `command fetch` runs no
    // function; builtins and utilities are not symbols.
    assert_eq!(g.callees(deploy), ["bin/deploy:version", "lib/common.sh:log", "lib/net.sh:fetch", "scripts/build.sh"]);
    // A script run by path is a call of the script's File node.
    g.has_ids(g.id(deploy), g.file("scripts/build.sh"), Calls, ImportResolved, 0.9);
    g.has_ids(g.id("bin/release.sh:release"), g.file("bin/deploy"), Calls, ImportResolved, 0.9);
    // Top-level code belongs to the File.
    g.has_ids(g.file("bin/deploy"), g.id(deploy), Calls, SameModule, 0.9);
    g.has_ids(g.file("scripts/build.sh"), g.id("scripts/build.sh:compile"), Calls, SameModule, 0.9);
    // IMPORTS for `source` / `.` / bats `load`.
    for (from, to) in [
        ("bin/deploy", "lib/common.sh"),
        ("bin/deploy", "lib/pick_a.sh"),
        ("lib/common.sh", "lib/net.sh"),
        ("test/deploy.bats", "test/helpers.bash"),
        ("test/helpers.bash", "lib/common.sh"),
    ] {
        assert_eq!(g.find(g.file(from), g.file(to), Imports), Some((ImportResolved, 1.0)), "{from} -> {to}");
    }
    // A library calling a function only another library defines.
    g.has("lib/orphan.sh:uses_ambient", "lib/helpers.sh:only_here", Calls, UniqueName, 0.6);
    // Recursion-free retry calls fetch in its own file.
    g.has("lib/net.sh:retry", "lib/net.sh:fetch", Calls, SameModule, 0.9);
    // bats: the test calls `log` through `load helpers` → common.sh.
    g.has("test/deploy.bats:log writes to stderr", "lib/common.sh:log", Calls, ImportResolved, 0.95);
    g.has("test/deploy.bats:log writes to stderr", "lib/common.sh:log", Tests, ImportResolved, 0.95);
    // Env: reads of unassigned upper-case names, a provider, and a read of
    // a variable the sourced library assigns (a reference, not an env read).
    g.has(deploy, "$env:DEPLOY_URL", Reads, Contract, 0.95);
    g.has("lib/net.sh:fetch", "$env:API_TOKEN", Reads, Contract, 0.95);
    g.has("bin/deploy:deploy.DEPLOY_TARGET", "$env:DEPLOY_TARGET", Writes, Syntactic, 1.0);
    g.has("bin/deploy:version", "lib/common.sh:APP_VERSION", References, ImportResolved, 0.95);
    let reads_app_version = out
        .graph
        .edges
        .iter()
        .any(|e| e.kind == Reads && out.graph.nodes[e.dst as usize].qualified_name == "$env:APP_VERSION");
    assert!(!reads_app_version);
    // Definitions and module names.
    let f = out.graph.files.iter().position(|f| f.path == "bin/deploy").unwrap();
    assert_eq!(out.graph.modules[f], "bin/deploy");
    assert_eq!(out.graph.files[f].language, Some(Language::Shell));
    assert_eq!(out.graph.nodes[g.id("lib/common.sh:APP_VERSION") as usize].label, Label::Variable);
    assert!(out.graph.nodes[g.id("test/deploy.bats:log writes to stderr") as usize].is_test);
}

#[test]
fn deterministic_under_shuffled_inputs() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    let a = build(files.clone()).graph;
    files.reverse();
    let b = build(files).graph;
    assert_eq!(a, b);
}
