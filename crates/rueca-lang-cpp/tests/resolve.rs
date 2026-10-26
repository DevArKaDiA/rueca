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
        branch: None,
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
    /// The node `qn` of `path` whose span starts on (1-based) `line`: a
    /// declaration and its body in one file are two nodes.
    fn at_line(&self, qn: &str, path: &str, line: u32) -> NodeId {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap_or_else(|| panic!("no file {path}"));
        let v: Vec<NodeId> = self
            .0
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.qualified_name == qn && n.file == Some(f as u32) && n.span.is_some_and(|s| s.start_line + 1 == line)
            })
            .map(|(i, _)| i as NodeId)
            .collect();
        assert_eq!(v.len(), 1, "expected one node {qn} in {path} at line {line}, got {}", v.len());
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
    assert_eq!(imports_of_main, 3, "shape.h, units.h, list.h");
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
    // A namespace macro defined in another header (`GEO_BEGIN_NAMESPACE`
    // opens `geo::v1`, an inline namespace): `geo::to_radians(a)` reaches
    // the body of `geo::v1::to_radians`.
    let to_radians = g.at("geo::v1::to_radians", "src/units.cpp");
    g.has(main, to_radians, EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    assert_eq!(
        out.graph.nodes[g.at("geo::v1::to_radians", "lib/inc/geo/units.h") as usize].doc.as_deref(),
        Some("Degrees to radians.")
    );
    // A free function declared in a header, defined in a nested namespace:
    // `detail::next_id()` is looked up from the enclosing namespace `geo`.
    let next_id = g.at("geo::detail::next_id", SHAPE_CPP);
    g.has(ctor_body, next_id, EdgeKind::Calls, Strategy::SameModule, 0.9);
}

#[test]
fn implicit_self_macros_and_anonymous_namespaces() {
    let out = build(fixture_files());
    let g = G(&out.graph);
    let name_body = g.at("geo::Shape::name", SHAPE_CPP);
    // A file-local helper in an anonymous namespace (same module).
    let describe = g.at("describe", SHAPE_CPP);
    g.has(name_body, describe, EdgeKind::Calls, Strategy::SameModule, 0.9);
    // Exported: `#include` is textual, so includers see it too.
    assert!(out.graph.nodes[describe as usize].exported);
    // `id_` in an out-of-line body: a member of the class named by the
    // body's qualified name.
    let id_field = g.at("geo::Shape::id_", SHAPE_H);
    g.has(name_body, id_field, EdgeKind::References, Strategy::TypeResolved, 0.9);
    let ctor_body = g.at("geo::Circle::Circle", SHAPE_CPP);
    g.has(ctor_body, id_field, EdgeKind::Writes, Strategy::TypeResolved, 0.85);
    // Macros through the include: function-like → Function, object-like →
    // Constant; an invocation is a use (`REFERENCES`), not a call.
    let area_body = g.at("geo::Circle::area", SHAPE_CPP);
    g.has(area_body, g.at("GEO_SQ", SHAPE_H), EdgeKind::References, Strategy::ImportResolved, 0.95);
    g.lacks(area_body, g.at("GEO_SQ", SHAPE_H), EdgeKind::Calls);
    g.has(area_body, g.at("GEO_PI", SHAPE_H), EdgeKind::References, Strategy::ImportResolved, 0.95);
    g.has(area_body, g.at("geo::Circle::r_", SHAPE_H), EdgeKind::References, Strategy::TypeResolved, 0.9);
    // A field declared in the header (`Point center_`) types a call in an
    // out-of-line body of the `.cpp` (`$member:center_`).
    let reach = g.at("geo::Circle::reach", SHAPE_CPP);
    g.has(reach, g.at("geo::Point::norm", SHAPE_CPP), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
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
    // `OVERRIDES` joins the bodies: `Circle::area` is defined in the `.cpp`,
    // the pure virtual `Shape::area` has only its declaration.
    g.has(
        g.at("geo::Circle::area", SHAPE_CPP),
        g.at("geo::Shape::area", SHAPE_H),
        EdgeKind::Overrides,
        Strategy::TypeResolved,
        0.9,
    );
    g.lacks(g.at("geo::Circle::area", SHAPE_H), g.at("geo::Shape::area", SHAPE_H), EdgeKind::Overrides);
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
    // The in-class declaration and the body in the same file are one node
    // with the body's span.
    let reset = g.at_line("CircleFixture::reset", "tests/shape_test.cpp", 10);
    g.has(radius, reset, EdgeKind::Calls, Strategy::TypeResolved, 0.9);
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

fn sources(files: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    files.iter().map(|(p, s)| (p.to_string(), s.as_bytes().to_vec())).collect()
}

/// Lua's style: API prototypes with parenthesized names, macros that
/// expand to calls (nested), and a debug-only assertion macro.
#[test]
fn macros_expand_to_the_calls_they_make() {
    let out = build(sources(&[
        (
            "lua.h",
            "#define LUA_API extern\n\
             LUA_API void (lua_settop) (lua_State *L, int idx);\n\
             LUA_API lua_State *(lua_newstate) (void *ud);\n\
             LUA_API long (lua_tointegerx) (lua_State *L, int idx, int *isnum);\n\
             #define lua_pop(L,n) lua_settop(L, -(n)-1)\n\
             #define lua_tointeger(L,i) lua_tointegerx(L,(i),NULL)\n\
             #define luaL_checkint(L,n) ((int)lua_tointeger(L, (n)))\n\
             #if defined(NDEBUG)\n\
             #define api_check(e) ignore(e)\n\
             #else\n\
             #define api_check(e) check_failed(e)\n\
             #endif\n\
             void ignore(int e);\n\
             void check_failed(int e);\n",
        ),
        (
            "lapi.c",
            "#include \"lua.h\"\n\
             void lua_settop(lua_State *L, int idx) { api_check(idx); }\n\
             long lua_tointegerx(lua_State *L, int idx, int *isnum) { return 0; }\n\
             lua_State *lua_newstate(void *ud) { return 0; }\n\
             void ignore(int e) {}\n\
             void check_failed(int e) {}\n",
        ),
        (
            "lbase.c",
            "#include \"lua.h\"\n\
             static int base(lua_State *L) {\n  lua_pop(L, 1);\n  return luaL_checkint(L, 2);\n}\n\
             lua_State *open(void) { return lua_newstate(0); }\n",
        ),
    ]));
    let g = G(&out.graph);
    let base = g.at("base", "lbase.c");
    // Prototypes with parenthesized names are functions linked to their bodies.
    assert_eq!(g.0.nodes[g.at("lua_settop", "lua.h") as usize].label, rueca_model::Label::Function);
    let settop = g.at("lua_settop", "lapi.c");
    assert!(g.edge(base, settop, EdgeKind::Calls).is_some(), "lua_pop → lua_settop");
    // Through two macros: luaL_checkint → lua_tointeger → lua_tointegerx.
    assert!(g.edge(base, g.at("lua_tointegerx", "lapi.c"), EdgeKind::Calls).is_some());
    assert!(g.edge(g.at("open", "lbase.c"), g.at("lua_newstate", "lapi.c"), EdgeKind::Calls).is_some());
    // The debug definition of a macro defined per `#if` branch.
    assert!(g.edge(settop, g.at("check_failed", "lapi.c"), EdgeKind::Calls).is_some());
    g.lacks(settop, g.at("ignore", "lapi.c"), EdgeKind::Calls);
}

/// C++: overloads by argument type, cv-qualified and SFINAE overloads,
/// friend functions, smart-pointer members and namespaces taken for types.
#[test]
fn cpp_overloads_members_and_namespaces() {
    let out = build(sources(&[
        (
            "emitter.h",
            "#include <memory>\n\
             namespace YAML {\n\
             class State { public: int Count() const; };\n\
             class Emitter {\n public:\n  Emitter& Write(bool b);\n  Emitter& Write(char c);\n  Emitter& Write(const std::string& s);\n  void Run();\n  const int& get() const { return n_; }\n  int& get() { return n_; }\n\
             friend bool operator==(const Emitter& a, const Emitter& b) { return a.n_ == b.n_; }\n\
              private:\n  std::unique_ptr<State> m_pState;\n  int n_;\n};\n\
             enum Op { Seq, Or };\n\
             class RegEx {\n public:\n  explicit RegEx(char ch);\n  RegEx(const std::string& s, Op op = Seq);\n  explicit RegEx(Op op);\n};\n\
             namespace Exp {\ninline int Break() { return 1; }\n}\n\
             template <typename T, typename std::enable_if<sizeof(T) == 1, int>::type = 0>\nint pick(T v) {\n  return 1;\n}\n\
             template <typename T, typename std::enable_if<sizeof(T) != 1, int>::type = 0>\nint pick(T v) {\n  return 2;\n}\n\
             }\n",
        ),
        (
            "emitter.cpp",
            "#include \"emitter.h\"\n\
             namespace YAML {\n\
             Emitter& Emitter::Write(bool b) { return *this; }\n\
             Emitter& Emitter::Write(char c) { return *this; }\n\
             Emitter& Emitter::Write(const std::string& s) { return *this; }\n\
             RegEx::RegEx(char ch) {}\n\
             RegEx::RegEx(const std::string& s, Op op) {}\n\
             RegEx::RegEx(Op op) {}\n\
             void Emitter::Run() {\n  RegEx('a');\n  RegEx(\"ab\");\n  Write('x');\n  bool flag = true;\n  Write(flag);\n  Write(std::string(\"a\"));\n  m_pState->Count();\n  Exp::Break();\n}\n\
             }\n",
        ),
    ]));
    let g = G(&out.graph);
    let run = g.at("YAML::Emitter::Run", "emitter.cpp");
    let w = |p: &str| g.at(&format!("YAML::Emitter::Write({p})"), "emitter.cpp");
    for p in ["char", "bool", "const std::string&"] {
        assert_eq!(
            g.edge(run, w(p), EdgeKind::Calls).map(|m| m.confidence),
            Some(0.9),
            "Write({p}) chosen by argument type (no overload fan-out)"
        );
    }
    // A string literal takes the string overload, whose default argument
    // is written on the declaration only.
    let ctor = |p: &str| g.at(&format!("YAML::RegEx::RegEx({p})"), "emitter.cpp");
    assert!(g.edge(run, ctor("char"), EdgeKind::Calls).is_some());
    assert!(g.edge(run, ctor("const std::string&, Op"), EdgeKind::Calls).is_some());
    g.lacks(run, ctor("Op"), EdgeKind::Calls);
    assert!(g.edge(run, g.at("YAML::State::Count", "emitter.h"), EdgeKind::Calls).is_some(), "unique_ptr member");
    assert!(g.edge(run, g.at("YAML::Exp::Break", "emitter.h"), EdgeKind::Calls).is_some(), "namespace qualifier");
    // Overloads that differ by qualifiers or only by their constraints are
    // distinct definitions.
    g.at("YAML::Emitter::get()", "emitter.h");
    g.at("YAML::Emitter::get(/*const*/)", "emitter.h");
    g.at("YAML::pick(T)", "emitter.h");
    g.at("YAML::pick(T/*#2*/)", "emitter.h");
    // A friend defined in its class is a function of the namespace.
    assert_eq!(g.0.nodes[g.at("YAML::operator==", "emitter.h") as usize].label, rueca_model::Label::Function);
}
