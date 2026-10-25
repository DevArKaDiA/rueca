//! End-to-end C / C++ resolution with the real `CppPack` over the fixture
//! project in `tests/fixtures/geo`: headers under a non-conventional
//! include directory found through `build/compile_commands.json`, a
//! namespace with nested and anonymous namespaces, a class hierarchy with
//! a pure virtual method and an override, out-of-line bodies (inside the
//! namespace and qualified), overloads, a template, macros, a C list with a
//! typedef'd struct and a function pointer, GoogleTest and Catch2 tests.

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_cpp::CppPack;
use rueca_model::{EdgeKind, EdgeMeta, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geo")
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

fn fixture_files() -> Vec<(String, Vec<u8>)> {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files
}

fn build(files: Vec<(String, Vec<u8>)>) -> ResolveOutput {
    let mut registry = Registry::new();
    registry.register(Box::new(CppPack::default()));
    let mut project = ProjectFiles { paths: files.iter().map(|(p, _)| p.clone()).collect(), ..Default::default() };
    for (p, src) in &files {
        if ProjectFiles::is_config_file(p) {
            project.configs.insert(p.clone(), String::from_utf8_lossy(src).into_owned());
        }
    }
    let has_cpp = files.iter().any(|(p, _)| Language::from_path(p) == Some(Language::Cpp));
    let inputs = files
        .iter()
        .map(|(path, src)| {
            let language = Language::header_language(path, has_cpp);
            let facts = language
                .and_then(|l| registry.for_language(l).map(|p| (l, p)))
                .map(|(l, pack)| pack.extract(path, l, src, &Budget::default()).expect("extract"));
            FileInput {
                path: path.clone(),
                blob_sha: git_blob_sha(src),
                language: language.filter(|_| facts.is_some()),
                size: src.len() as u64,
                status: if facts.is_some() { "ok".into() } else { "skipped:unsupported".into() },
                facts,
            }
        })
        .collect();
    let snapshot = SnapshotInfo {
        repo: "local/geo".into(),
        commit: "c".into(),
        dirty: false,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    };
    let out = resolve(snapshot, inputs, &project, &registry);
    out.graph.validate().expect("valid graph");
    out
}

struct G<'g>(&'g ResolvedGraph);

impl G<'_> {
    /// The node with qualified name `qn` in file `path`.
    #[track_caller]
    fn at(&self, qn: &str, path: &str) -> NodeId {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap_or_else(|| panic!("no file {path}"));
        let v: Vec<NodeId> = self
            .0
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.qualified_name == qn && n.file == Some(f as u32))
            .map(|(i, _)| i as NodeId)
            .collect();
        assert_eq!(v.len(), 1, "expected one node {qn} in {path}, got {}", v.len());
        v[0]
    }
    fn file(&self, path: &str) -> NodeId {
        self.0.files.iter().find(|f| f.path == path).unwrap().node
    }
    fn edge(&self, src: NodeId, dst: NodeId, kind: EdgeKind) -> Option<EdgeMeta> {
        self.0.edges.iter().find(|e| e.src == src && e.dst == dst && e.kind == kind).map(|e| e.meta)
    }
    #[track_caller]
    fn has(&self, src: NodeId, dst: NodeId, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let name = |n: NodeId| &self.0.nodes[n as usize].qualified_name;
        let m = self.edge(src, dst, kind).unwrap_or_else(|| panic!("missing {kind} {} -> {}", name(src), name(dst)));
        assert_eq!(m.strategy, strategy, "{kind} {} -> {}", name(src), name(dst));
        assert!((m.confidence - confidence).abs() < 1e-4, "{kind} {} -> {}: {}", name(src), name(dst), m.confidence);
    }
    #[track_caller]
    fn lacks(&self, src: NodeId, dst: NodeId, kind: EdgeKind) {
        let name = |n: NodeId| &self.0.nodes[n as usize].qualified_name;
        assert!(self.edge(src, dst, kind).is_none(), "unexpected {kind} {} -> {}", name(src), name(dst));
    }
}

const SHAPE_H: &str = "lib/inc/geo/shape.h";
const POINT_H: &str = "lib/inc/geo/point.h";
const SHAPE_CPP: &str = "src/shape.cpp";
const MAIN: &str = "src/main.cpp";

#[test]
fn includes_resolve_through_compile_commands() {
    let out = build(fixture_files());
    let g = G(&out.graph);
    use EdgeKind::Imports;
    g.has(g.file(MAIN), g.file(SHAPE_H), Imports, Strategy::ImportResolved, 1.0);
    g.has(g.file(SHAPE_CPP), g.file(SHAPE_H), Imports, Strategy::ImportResolved, 1.0);
    g.has(g.file(SHAPE_H), g.file(POINT_H), Imports, Strategy::ImportResolved, 1.0);
    g.has(g.file(MAIN), g.file("c/list.h"), Imports, Strategy::ImportResolved, 1.0);
    g.has(g.file("c/list.c"), g.file("c/list.h"), Imports, Strategy::ImportResolved, 1.0);
    g.has(g.file("tests/shape_test.cpp"), g.file(SHAPE_H), Imports, Strategy::ImportResolved, 1.0);
    // System headers are not project files.
    let imports_of_main = out.graph.edges.iter().filter(|e| e.src == g.file(MAIN) && e.kind == Imports).count();
    assert_eq!(imports_of_main, 2);
    assert_eq!(out.graph.modules[out.graph.files.iter().position(|f| f.path == MAIN).unwrap()], "");
}

#[test]
fn declarations_link_to_bodies() {
    let out = build(fixture_files());
    let g = G(&out.graph);
    let main = g.at("main", MAIN);
    let area_decl = g.at("geo::Circle::area", SHAPE_H);
    let area_body = g.at("geo::Circle::area", SHAPE_CPP);
    // `geo::Circle c(2.0); c.area()`: typed receiver, declaration → body.
    g.has(main, area_body, EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.lacks(main, area_decl, EdgeKind::Calls);
    // A qualified out-of-line body outside the namespace block.
    let radius_body = g.at("geo::Circle::radius", SHAPE_CPP);
    let test = g.at("CircleFixture::Radius", "tests/shape_test.cpp");
    g.has(test, radius_body, EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Constructor: the class and its constructor body (× 0.95).
    let circle = g.at("geo::Circle", SHAPE_H);
    let ctor_body = g.at("geo::Circle::Circle", SHAPE_CPP);
    let m = g.edge(main, circle, EdgeKind::Calls).expect("main → Circle");
    assert_eq!(m.strategy, Strategy::ImportResolved);
    let c = g.edge(main, ctor_body, EdgeKind::Calls).expect("main → Circle::Circle body");
    assert!((c.confidence - m.confidence * 0.95).abs() < 1e-4);
    // A pure virtual method has no body: the declaration stays the target.
    let shape_area = g.at("geo::Shape::area", SHAPE_H);
    let report = g.at("report", MAIN);
    g.has(report, shape_area, EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // A free function declared in a header, defined in a nested namespace.
    let next_id = g.at("geo::detail::next_id", SHAPE_CPP);
    g.has(ctor_body, next_id, EdgeKind::Calls, Strategy::ImportResolved, 0.95);
}

#[test]
fn implicit_self_macros_and_anonymous_namespaces() {
    let out = build(fixture_files());
    let g = G(&out.graph);
    let name_body = g.at("geo::Shape::name", SHAPE_CPP);
    // A file-local helper in an anonymous namespace (same module).
    let describe = g.at("describe", SHAPE_CPP);
    g.has(name_body, describe, EdgeKind::Calls, Strategy::SameModule, 0.9);
    assert!(!out.graph.nodes[describe as usize].exported);
    // `id_` in an out-of-line body: a member of the class named by the
    // body's qualified name.
    let id_field = g.at("geo::Shape::id_", SHAPE_H);
    g.has(name_body, id_field, EdgeKind::References, Strategy::TypeResolved, 0.9);
    let ctor_body = g.at("geo::Circle::Circle", SHAPE_CPP);
    g.has(ctor_body, id_field, EdgeKind::Writes, Strategy::TypeResolved, 0.85);
    // Macros through the include: function-like → Function, object-like → Constant.
    let area_body = g.at("geo::Circle::area", SHAPE_CPP);
    g.has(area_body, g.at("GEO_SQ", SHAPE_H), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.has(area_body, g.at("GEO_PI", SHAPE_H), EdgeKind::References, Strategy::ImportResolved, 0.95);
    g.has(area_body, g.at("geo::Circle::r_", SHAPE_H), EdgeKind::References, Strategy::TypeResolved, 0.9);
    // An inline method's implicit member read.
    let center = g.at("geo::Circle::center", SHAPE_H);
    g.has(center, g.at("geo::Circle::center_", SHAPE_H), EdgeKind::References, Strategy::TypeResolved, 0.9);
}

#[test]
fn overloads_templates_operators_inheritance() {
    let out = build(fixture_files());
    let g = G(&out.graph);
    let main = g.at("main", MAIN);
    // Overloads carry parameter suffixes; arity picks the body.
    let one = g.at("geo::scale(double)", SHAPE_CPP);
    let two = g.at("geo::scale(double, double)", SHAPE_CPP);
    g.has(main, one, EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.has(main, two, EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    let catch = g.at("scale doubles", "tests/catch_test.cpp");
    g.has(catch, two, EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.lacks(catch, one, EdgeKind::Calls);
    g.has(catch, two, EdgeKind::Tests, Strategy::ImportResolved, 0.95);
    // Templates by base name.
    g.has(main, g.at("geo::clamp_value", SHAPE_H), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    // `geo::Point{1, 2}`: a construction of the struct.
    g.has(main, g.at("geo::Point", POINT_H), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    // `geo::Vec v; v.norm()`: an alias's members through its aliased type.
    g.has(main, g.at("geo::Point::norm", SHAPE_CPP), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Operator overload declared in a header, defined qualified.
    let plus = g.at("geo::operator+", SHAPE_CPP);
    assert_eq!(out.graph.nodes[plus as usize].name, "operator+");
    // Inheritance and overrides.
    let circle = g.at("geo::Circle", SHAPE_H);
    let shape = g.at("geo::Shape", SHAPE_H);
    g.has(circle, shape, EdgeKind::Inherits, Strategy::SameModule, 0.9);
    g.has(
        g.at("geo::Circle::area", SHAPE_H),
        g.at("geo::Shape::area", SHAPE_H),
        EdgeKind::Overrides,
        Strategy::TypeResolved,
        0.9,
    );
}

#[test]
fn c_structs_function_pointers_and_tests() {
    let out = build(fixture_files());
    let g = G(&out.graph);
    let count = g.at("list_count", "c/list.c");
    let walk_body = g.at("list_walk", "c/list.c");
    g.has(count, walk_body, EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    // A function passed as a callback is a read of it.
    g.has(count, g.at("count_one", "c/list.c"), EdgeKind::References, Strategy::SameModule, 0.9);
    // `node_t *n; n->value`: the typedef's members through the struct.
    let value = g.at("node::value", "c/list.h");
    g.has(g.at("count_one", "c/list.c"), value, EdgeKind::References, Strategy::TypeResolved, 0.85);
    // The call through a function-pointer parameter is local: no edge.
    let fn_param_calls = out.graph.edges.iter().filter(|e| e.src == walk_body && e.kind == EdgeKind::Calls).count();
    assert_eq!(fn_param_calls, 0);
    // GoogleTest: TEST and TEST_F are tests; TEST_F bodies see the fixture.
    let area = g.at("CircleTest::Area", "tests/shape_test.cpp");
    assert!(out.graph.nodes[area as usize].is_test);
    g.has(area, g.at("geo::Circle::area", SHAPE_CPP), EdgeKind::Tests, Strategy::TypeResolved, 0.85);
    let radius = g.at("CircleFixture::Radius", "tests/shape_test.cpp");
    g.has(radius, g.at("CircleFixture::reset", "tests/shape_test.cpp"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    // The env var bridge.
    let home = out.graph.nodes.iter().position(|n| n.qualified_name == "$env:GEO_HOME").expect("$env:GEO_HOME");
    g.has(g.at("main", MAIN), home as NodeId, EdgeKind::Reads, Strategy::Contract, 0.95);
}

#[test]
fn deterministic_under_shuffled_inputs() {
    let files = fixture_files();
    let a = serde_json::to_string(&build(files.clone()).graph).unwrap();
    let mut reversed = files.clone();
    reversed.reverse();
    let b = serde_json::to_string(&build(reversed).graph).unwrap();
    let mut rotated = files;
    rotated.rotate_left(3);
    let c = serde_json::to_string(&build(rotated).graph).unwrap();
    assert_eq!(a, b);
    assert_eq!(a, c);
}
