//! End-to-end Go resolution with the real `GoPack` over the multi-module
//! fixture in `rueca-lang-go/tests/fixtures/shop`: a `go.work` workspace
//! (`app` = `example.com/shop`, `lib` = `example.com/lib`, a local
//! `replace` of `example.com/tools`), a package split over several files,
//! methods with pointer and value receivers, struct and interface
//! embedding, implicit interfaces, generics, internal and external test
//! packages, an `internal/` package, dot / alias imports and a package
//! whose name differs from its directory.

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_go::GoPack;
use rueca_lang_python::PythonPack;
use rueca_model::{EdgeKind, Label, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-go/tests/fixtures/shop")
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

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(PythonPack));
    r.register(Box::new(GoPack::default()));
    r
}

fn build(files: Vec<(String, Vec<u8>)>) -> ResolveOutput {
    let registry = registry();
    let mut project = ProjectFiles { paths: files.iter().map(|(p, _)| p.clone()).collect(), ..Default::default() };
    for (p, src) in &files {
        if ProjectFiles::is_config_file(p) {
            project.configs.insert(p.clone(), String::from_utf8_lossy(src).into_owned());
        }
    }
    let inputs = files
        .iter()
        .map(|(path, src)| {
            let facts = registry
                .for_path(path)
                .map(|(lang, pack)| pack.extract(path, lang, src, &Budget::default()).expect("extract"));
            FileInput {
                path: path.clone(),
                blob_sha: git_blob_sha(src),
                language: Language::from_path(path).filter(|_| facts.is_some()),
                size: src.len() as u64,
                status: if facts.is_some() { "ok".into() } else { "skipped:unsupported".into() },
                facts,
            }
        })
        .collect();
    let snapshot = SnapshotInfo {
        repo: "local/shop".into(),
        commit: "worktree".into(),
        dirty: true,
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
    fn folder(&self, path: &str) -> NodeId {
        self.0
            .nodes
            .iter()
            .position(|n| n.label == Label::Folder && n.qualified_name == path)
            .unwrap_or_else(|| panic!("no folder {path}")) as NodeId
    }
    fn find(&self, src: NodeId, dst: NodeId, kind: EdgeKind) -> Option<(Strategy, f32)> {
        self.0
            .edges
            .iter()
            .find(|e| e.src == src && e.dst == dst && e.kind == kind)
            .map(|e| (e.meta.strategy, e.meta.confidence))
    }
    #[track_caller]
    fn has(&self, src: &str, dst: &str, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let (s, d) = (self.id(src), self.id(dst));
        let got = self.find(s, d, kind).unwrap_or_else(|| panic!("missing {kind} {src} -> {dst}"));
        assert_eq!(got.0, strategy, "{kind} {src} -> {dst}");
        assert!((got.1 - confidence).abs() < 1e-4, "{kind} {src} -> {dst}: {} != {confidence}", got.1);
    }
    #[track_caller]
    fn lacks(&self, src: &str, dst: &str, kind: EdgeKind) {
        let (s, d) = (self.id(src), self.id(dst));
        assert!(self.find(s, d, kind).is_none(), "unexpected {kind} {src} -> {dst}");
    }
    #[track_caller]
    fn imports_dir(&self, from: &str, dir: &str) {
        let (s, d) = (self.file(from), self.folder(dir));
        assert_eq!(self.find(s, d, EdgeKind::Imports), Some((Strategy::ImportResolved, 1.0)), "{from} -> {dir}/");
    }
    fn module_of(&self, path: &str) -> &str {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap();
        &self.0.modules[f]
    }
}

const SRV: &str = "example.com/shop/server";
const STORE: &str = "example.com/shop/internal/store";

fn q(module: &str, local: &str) -> String {
    format!("{module}.{local}")
}

#[test]
fn module_names_are_import_paths() {
    let out = fixture();
    let g = G(&out.graph);
    assert_eq!(g.module_of("app/main.go"), "example.com/shop");
    assert_eq!(g.module_of("app/server/server.go"), SRV);
    assert_eq!(g.module_of("app/server/server_test.go"), SRV);
    assert_eq!(g.module_of("app/server/server_ext_test.go"), "example.com/shop/server_test");
    assert_eq!(g.module_of("app/internal/store/store.go"), STORE);
    assert_eq!(g.module_of("lib/money/money.go"), "example.com/lib/money");
    // A replaced directory without its own go.mod keeps its path.
    assert_eq!(g.module_of("tools/gen/gen.go"), "tools/gen");
    assert_eq!(g.module_of("app/pkg/termx/term.go"), "example.com/shop/pkg/termx");
    g.id("example.com/lib/money.Amount.String");
    g.id(&q(SRV, "Server.Handle"));
    g.id("example.com/shop/server_test.ExampleServer");
}

#[test]
fn directory_imports_bind_packages() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    for dir in ["app/server", "app/internal/store", "app/util", "app/pkg/termx", "lib/money", "tools/gen"] {
        g.imports_dir("app/main.go", dir);
    }
    g.imports_dir("app/server/server_ext_test.go", "app/server");
    let main = "example.com/shop.main";
    // pkg.F() through the import binding (0.90), across modules and replace.
    g.has(main, &q(SRV, "NewServer"), Calls, ImportResolved, 0.9);
    g.has(main, &q(STORE, "New"), Calls, ImportResolved, 0.9);
    g.has(main, "example.com/lib/money.Parse", Calls, ImportResolved, 0.9);
    g.has(main, "tools/gen.Run", Calls, ImportResolved, 0.9);
    // Package `term` in directory `termx`: bound by the unused-import rule.
    g.has(main, "example.com/shop/pkg/termx.Width", Calls, ImportResolved, 0.9);
    // Dot import binds the package's exported names.
    g.has(main, "example.com/shop/util.Clamp", Calls, ImportResolved, 0.95);
    // Same package, other file (package scope).
    g.has(main, "example.com/shop.Version", References, SameModule, 0.9);
    // The external test package binds the package under test, not itself.
    g.has("example.com/shop/server_test.ExampleServer", &q(SRV, "NewServer"), Calls, ImportResolved, 0.9);
}

#[test]
fn receivers_resolve_through_package_scope() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let main = "example.com/shop.main";
    // `srv := server.NewServer(…)` (constructor convention), `var s
    // server.Server`, and a typed package-level var: methods declared in
    // other files of the package.
    g.has(main, &q(SRV, "Server.Start"), Calls, TypeResolved, 0.85);
    g.has(main, &q(SRV, "Server.Handle"), Calls, TypeResolved, 0.85);
    g.has("example.com/shop/server_test.TestExternal", &q(SRV, "Server.Handle"), Tests, TypeResolved, 0.85);
    // `$self`: a method in another file than its type.
    g.has(&q(SRV, "Server.Handle"), &q(SRV, "Server.Start"), Calls, TypeResolved, 0.9);
    // Promoted method of an embedded struct (value receiver).
    g.has(&q(SRV, "Server.Start"), &q(SRV, "Base.Log"), Calls, TypeResolved, 0.85);
    // Field type: `s.store.Get()` with `store *store.Store`.
    g.has(&q(SRV, "Server.Start"), &q(STORE, "Store.Get"), Calls, TypeResolved, 0.85);
    // Typed parameter, also inside a function literal.
    g.has(&q(SRV, "Server.Handle"), &q(SRV, "Context.Reset"), Calls, TypeResolved, 0.85);
    // Same-package function from another file; field reads and writes.
    g.has(&q(SRV, "Server.Handle"), &q(SRV, "helper"), Calls, SameModule, 0.9);
    g.has(&q(SRV, "Server.Start"), &q(SRV, "Server.store"), References, TypeResolved, 0.9);
    g.has(&q(SRV, "Server.Start"), &q(SRV, "Server.Hits"), Writes, TypeResolved, 0.9);
    g.has(&q(SRV, "Context.Reset"), &q(SRV, "Context.Path"), Writes, TypeResolved, 0.9);
    // Composite literal → New → CALLS the struct.
    g.has(&q(SRV, "NewServer"), &q(SRV, "Server"), Calls, SameModule, 0.9);
    // Internal test file: same package, typed by the constructor result.
    g.has(&q(SRV, "TestStart"), &q(SRV, "Server.Start"), Tests, TypeResolved, 0.85);
    g.has(&q(SRV, "TestStart"), &q(SRV, "newFixture"), Calls, SameModule, 0.9);
}

#[test]
fn embedding_and_implicit_interfaces() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // Embedding → INHERITS (struct and interface).
    g.has(&q(SRV, "Server"), &q(SRV, "Base"), Inherits, SameModule, 0.9);
    g.has(&q(SRV, "ReadLogger"), &q(SRV, "Logger"), Inherits, SameModule, 0.9);
    // Implicit interfaces: own methods, promoted methods, across packages.
    g.has(&q(STORE, "Store"), &q(STORE, "Repo"), Implements, Heuristic, 0.5);
    g.has(&q(STORE, "Store.Get"), &q(STORE, "Repo.Get"), Overrides, Heuristic, 0.5);
    g.has(&q(SRV, "Base"), &q(SRV, "Logger"), Implements, Heuristic, 0.5);
    g.has(&q(SRV, "Base"), &q(SRV, "ReadLogger"), Implements, Heuristic, 0.5);
    g.has(&q(SRV, "Server"), &q(SRV, "ReadLogger"), Implements, Heuristic, 0.5);
    g.has(&q(SRV, "Base.Read"), &q(SRV, "ReadLogger.Read"), Overrides, Heuristic, 0.5);
    g.has(&q(SRV, "Base.Log"), &q(SRV, "Logger.Log"), Overrides, Heuristic, 0.5);
    g.lacks(&q(SRV, "Context"), &q(SRV, "Logger"), Implements);
    // `String()` covers no project interface; constraints are not implemented.
    let number = g.id(&q(SRV, "Number"));
    assert!(!out.graph.edges.iter().any(|e| e.dst == number && e.kind == Implements));
}

#[test]
fn generics_tests_and_bridge() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // Generic receivers attach to the type; type parameters are not types.
    g.id(&q(STORE, "Cache.Put"));
    g.has(&q(STORE, "Cache.Put"), &q(STORE, "Cache.items"), References, TypeResolved, 0.9);
    for tp in ["K", "V", "T", "U"] {
        assert!(!out.graph.nodes.iter().any(|n| n.name == tp), "type parameter {tp} became a node");
    }
    // Tests: `Test*` / `Benchmark*` / `Example*` in `_test.go`, not helpers.
    let is_test = |qn: &str| out.graph.nodes[g.id(qn) as usize].is_test;
    assert!(is_test(&q(SRV, "TestStart")) && is_test(&q(SRV, "BenchmarkStart")));
    assert!(is_test("example.com/shop/server_test.ExampleServer"));
    assert!(!is_test(&q(SRV, "newFixture")) && !is_test(&q(SRV, "Testlowercase")));
    // Env var and viper config key reads (RFC 0003 §7.2).
    g.has("example.com/shop.main", "$env:DATABASE_URL", Reads, Contract, 0.95);
    let main = g.id("example.com/shop.main");
    let viper = out.graph.edges.iter().filter(|e| e.src == main && e.kind == Reads).count();
    assert_eq!(viper, 1, "the viper key has no config file, so only the env var is read");
}

#[test]
fn statistics_and_expected_misses() {
    let out = fixture();
    let s = &out.stats;
    // Builtins (`len`, `append`, `make`, `min`) and stdlib / third-party
    // packages (`fmt`, `os`, `testing`, viper) are expected misses.
    assert!(s.references_builtin >= 4, "{s:?}");
    assert!(s.references_external >= 8, "{s:?}");
    for strategy in ["type_resolved", "import_resolved", "same_module", "contract"] {
        assert!(s.resolved_by_strategy.contains_key(strategy), "{s:?}");
    }
    // Calls into stdlib are never fuzzy-matched to project methods:
    // `price.String()` (an untyped local) may only guess `Amount.String`.
    let g = G(&out.graph);
    let main = g.id("example.com/shop.main");
    let targets: std::collections::BTreeSet<&str> = out
        .graph
        .edges
        .iter()
        .filter(|e| e.src == main && e.kind == EdgeKind::Calls)
        .map(|e| out.graph.nodes[e.dst as usize].qualified_name.as_str())
        .collect();
    for t in &targets {
        assert!(t.starts_with("example.com/") || t.starts_with("tools/"), "{t}");
    }
}

#[test]
fn deterministic_and_order_independent() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    let a = build(files.clone());
    files.reverse();
    let b = build(files);
    assert_eq!(serde_json::to_string(&a.graph).unwrap(), serde_json::to_string(&b.graph).unwrap());
}

#[test]
fn go_and_python_do_not_cross() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.push(("tools/start.py".into(), b"class Repo:\n    def Get(self):\n        pass\n".to_vec()));
    let out = build(files);
    let g = G(&out.graph);
    // A Python class with a `Get` method is not a Go implementer.
    let repo = g.id(&q(STORE, "Repo"));
    let py = g.id("tools.start.Repo");
    assert!(!out.graph.edges.iter().any(|e| e.src == py && e.dst == repo));
    g.has(&q(STORE, "Store"), &q(STORE, "Repo"), EdgeKind::Implements, Strategy::Heuristic, 0.5);
}
