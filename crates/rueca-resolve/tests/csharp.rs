//! End-to-end C# resolution with the real `CSharpPack` over the solution
//! fixture in `rueca-lang-csharp/tests/fixtures/shop`: a library project
//! (`Shop.Core`: file-scoped and block namespaces, a `global using`, a
//! partial class split over two files, a record, an abstract base with a
//! virtual method, an interface, `using static`, an extension method,
//! configuration and environment reads), an application project
//! (`Shop.App`: a using alias, fully qualified names, an interface
//! implementation) and an xUnit test project.

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_csharp::CSharpPack;
use rueca_model::{EdgeKind, Label, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-csharp/tests/fixtures/shop")
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

fn build(files: Vec<(String, Vec<u8>)>) -> ResolveOutput {
    let mut registry = Registry::new();
    registry.register(Box::new(CSharpPack));
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
    /// The node `qn` with `label` (partial parts: the one in `path`).
    #[track_caller]
    fn id_in(&self, qn: &str, path: Option<&str>) -> NodeId {
        let hits: Vec<NodeId> = (0..self.0.nodes.len() as NodeId)
            .filter(|&i| {
                let n = &self.0.nodes[i as usize];
                n.qualified_name == qn
                    && !matches!(n.label, Label::File | Label::Folder)
                    && path.is_none_or(|p| n.file.is_some_and(|f| self.0.files[f as usize].path == p))
            })
            .collect();
        let callable: Vec<NodeId> =
            hits.iter().copied().filter(|&i| self.0.nodes[i as usize].label.is_callable()).collect();
        if hits.len() > 1 && callable.len() == 1 {
            return callable[0];
        }
        assert_eq!(hits.len(), 1, "expected one node {qn}, got {hits:?}");
        hits[0]
    }
    #[track_caller]
    fn id(&self, qn: &str) -> NodeId {
        self.id_in(qn, None)
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
    fn dump(&self, kind: EdgeKind) -> String {
        self.0
            .edges
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| {
                let (s, d) = (&self.0.nodes[e.src as usize], &self.0.nodes[e.dst as usize]);
                format!("{} -> {} [{:?} {}]\n", s.qualified_name, d.qualified_name, e.meta.strategy, e.meta.confidence)
            })
            .collect()
    }
    #[track_caller]
    fn has_ids(&self, s: NodeId, d: NodeId, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let (src, dst) = (&self.0.nodes[s as usize].qualified_name, &self.0.nodes[d as usize].qualified_name);
        let got = self.find(s, d, kind).unwrap_or_else(|| panic!("missing {kind} {src} -> {dst}\n{}", self.dump(kind)));
        assert_eq!(got.0, strategy, "{kind} {src} -> {dst}");
        assert!((got.1 - confidence).abs() < 1e-4, "{kind} {src} -> {dst}: {} != {confidence}", got.1);
    }
    #[track_caller]
    fn has(&self, src: &str, dst: &str, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        self.has_ids(self.id(src), self.id(dst), kind, strategy, confidence);
    }
    /// An edge to one of the parts of partial type `dst`.
    #[track_caller]
    fn has_part(&self, src: &str, dst: &str, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let s = self.id(src);
        let parts: Vec<NodeId> =
            (0..self.0.nodes.len() as NodeId).filter(|&i| self.0.nodes[i as usize].qualified_name == dst).collect();
        let d = parts.iter().copied().find(|&d| self.find(s, d, kind).is_some());
        let d = d.unwrap_or_else(|| panic!("missing {kind} {src} -> {dst}\n{}", self.dump(kind)));
        self.has_ids(s, d, kind, strategy, confidence);
    }
    #[track_caller]
    fn lacks(&self, src: &str, dst: &str, kind: EdgeKind) {
        let (s, d) = (self.id(src), self.id(dst));
        assert!(self.find(s, d, kind).is_none(), "unexpected {kind} {src} -> {dst}");
    }
    fn module_of(&self, path: &str) -> &str {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap();
        &self.0.modules[f]
    }
}

const MODELS: &str = "Acme.Shop.Models";
const SERVICES: &str = "Acme.Shop.Services";
const APP: &str = "Acme.Shop.App";

fn q(module: &str, local: &str) -> String {
    format!("{module}.{local}")
}

#[test]
fn module_names_are_namespaces() {
    let out = fixture();
    let g = G(&out.graph);
    assert_eq!(g.module_of("src/Shop.Core/Models/Order.cs"), MODELS);
    assert_eq!(g.module_of("src/Shop.Core/Models/Order.Totals.cs"), MODELS);
    assert_eq!(g.module_of("src/Shop.Core/Services/OrderService.cs"), SERVICES);
    assert_eq!(g.module_of("src/Shop.App/Program.cs"), APP);
    assert_eq!(g.module_of("src/Shop.Core/GlobalUsings.cs"), "");
    g.id(&q(MODELS, "Line.Sku"));
    g.id(&q(MODELS, "Order.Add(Line)"));
    g.id(&q(MODELS, "Order.Add(string,int)"));
    g.id_in(&q(MODELS, "Order"), Some("src/Shop.Core/Models/Order.Totals.cs"));
}

#[test]
fn usings_import_namespace_directories() {
    let out = fixture();
    let g = G(&out.graph);
    let program = g.file("src/Shop.App/Program.cs");
    let services = out.graph.nodes.iter().position(|n| n.qualified_name == "src/Shop.Core/Services").unwrap();
    assert_eq!(g.find(program, services as NodeId, EdgeKind::Imports), Some((Strategy::ImportResolved, 1.0)));
}

#[test]
fn calls_resolve_through_namespaces_and_types() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let place = q(SERVICES, "OrderService.Place");
    // A type of another namespace through a `global using` of the project;
    // its constructor; an overload chosen by arity and argument type.
    g.has_part(&place, &q(MODELS, "Order"), Calls, ImportResolved, 0.95);
    g.has(&place, &q(MODELS, "Order.Order"), Calls, ImportResolved, 0.95 * 0.95);
    g.has(&place, &q(MODELS, "Order.Add(string,int)"), Calls, TypeResolved, 0.85);
    g.lacks(&place, &q(MODELS, "Order.Add(Line)"), Calls);
    // Inherited member through a typed local; interface method through a field.
    g.has(&place, &q(MODELS, "Entity.Validate"), Calls, TypeResolved, 0.85);
    g.has(&place, &q(SERVICES, "IOrderStore.Save"), Calls, TypeResolved, 0.85);
    // `using static`, and an extension method on a typed receiver.
    g.has(&place, &q(SERVICES, "Pricing.Discount"), Calls, ImportResolved, 0.95);
    g.has(&place, &q(SERVICES, "OrderExtensions.Audit"), Calls, TypeResolved, 0.85);
    // A partial part calls a member inherited through the other part.
    let total = g.id_in(&q(MODELS, "Order.Total"), Some("src/Shop.Core/Models/Order.Totals.cs"));
    g.has_ids(total, g.id(&q(MODELS, "Entity.Validate")), Calls, TypeResolved, 0.85);
    // `: base(id)` calls the base constructor.
    g.has(&q(MODELS, "Order.Order"), &q(MODELS, "Entity.Entity"), Calls, TypeResolved, 0.85);
    // An overload delegating to its sibling, by argument type.
    g.has(&q(MODELS, "Order.Add(string,int)"), &q(MODELS, "Order.Add(Line)"), Calls, TypeResolved, 0.9);
    g.has(&q(MODELS, "Order.Add(string,int)"), &q(MODELS, "Line"), Calls, SameModule, 0.9);
}

#[test]
fn qualified_names_aliases_and_implementations() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let main = q(APP, "Program.Main");
    g.has(&main, &q(SERVICES, "OrderService"), Calls, ImportResolved, 0.95);
    g.has(&main, &q(SERVICES, "OrderService.Place"), Calls, TypeResolved, 0.85);
    g.has(&main, &q(APP, "MemoryStore"), Calls, SameModule, 0.9);
    g.has(&main, &q(APP, "Helpers.Log"), Calls, TypeResolved, 0.85);
    // `using Store = …IOrderStore;` as a base, and the interface method it
    // implements.
    g.has(&q(APP, "MemoryStore"), &q(SERVICES, "IOrderStore"), Implements, ImportResolved, 0.9);
    g.has(&q(APP, "MemoryStore.Save"), &q(SERVICES, "IOrderStore.Save"), Overrides, TypeResolved, 0.9);
    // A fully qualified type in a signature.
    g.has_part(&q(APP, "MemoryStore.Save"), &q(MODELS, "Order"), References, ImportResolved, 0.9);
    // Partial parts: one type, both parts inherit from the base.
    g.has(&q(MODELS, "Order.Total"), &q(MODELS, "Order.Lines"), References, TypeResolved, 0.9);
}

#[test]
fn tests_and_bridge() {
    let out = fixture();
    let g = G(&out.graph);
    let t = &out.graph.nodes[g.id("Acme.Shop.Tests.OrderTests.AddsLines") as usize];
    assert!(t.is_test);
    g.has(
        "Acme.Shop.Tests.OrderTests.AddsLines",
        &q(MODELS, "Order.Add(string,int)"),
        EdgeKind::Tests,
        Strategy::TypeResolved,
        0.85,
    );
    let env = g.id("$env:SHOP_DB");
    g.has_ids(g.id(&q(SERVICES, "OrderService.Place")), env, EdgeKind::Reads, Strategy::Contract, 0.95);
}
