//! The RFC 0003 contract hooks, exercised with small fake packs and
//! hand-built facts. Each test is a working example for a pack author:
//!
//! - `go_like_package_scope` — `module_name_for`, `ModuleScope::Package`,
//!   directory imports (`IMPORTS` File → Folder), `$self`, `$type:T`.
//! - `java_like_implicit_self_overloads_overrides` — `implicit_self`,
//!   `overloads` (arity), `overrides`, on-demand and static imports,
//!   constructors named like the class.
//! - `java_like_member_slots` — a field and a method with one name, type
//!   lookups that skip constructors, `OVERRIDES` paired by signature.
//! - `cpp_like_declarations_and_includes` — `Definition::is_declaration`,
//!   includes as transitive star imports, out-of-line bodies,
//!   `$type:ns::Foo`.
//! - `bridge_env_vars_and_config_keys` — `RefKind::EnvVar` / `ConfigKey`,
//!   canonical `$env:NAME` nodes, providers, config scopes.
//! - `data_pointers_and_directory_scope` — `RefKind::DataRef`, data bases,
//!   `ModuleScope::Directory`, folder imports without a binding.

use rueca_extract::{Budget, ExtractError, LanguagePack, ModuleScope, ProjectFiles, Registry, type_receiver};
use rueca_model::{
    Definition, EdgeKind, EdgeMeta, FileFacts, Import, ImportedName, Label, Language, NodeId, RefKind, Reference,
    ResolvedGraph, SnapshotInfo, Span, Strategy,
};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

// ---------------------------------------------------------------------------
// Facts builder
// ---------------------------------------------------------------------------

fn sp(a: u32) -> Span {
    Span { start_byte: a, end_byte: a + 1, start_line: a, start_col: 0, end_line: a, end_col: 1 }
}

#[derive(Default)]
struct F(FileFacts);

impl F {
    /// Adds a definition; `qn` is the local qualified name.
    fn def(&mut self, label: Label, name: &str, qn: &str, parent: Option<u32>) -> u32 {
        let at = self.0.definitions.len() as u32 * 10;
        self.0.definitions.push(Definition {
            label,
            name: name.into(),
            qualified_name: qn.into(),
            parent,
            span: Span { end_byte: at + 9, end_line: at + 9, ..sp(at) },
            name_span: sp(at),
            signature: None,
            doc: None,
            decorators: vec![],
            bases: vec![],
            implements: vec![],
            exported: true,
            is_test: false,
            is_declaration: false,
        });
        self.0.definitions.len() as u32 - 1
    }
    fn d(&mut self, id: u32) -> &mut Definition {
        &mut self.0.definitions[id as usize]
    }
    fn r(&mut self, kind: RefKind, name: &str, receiver: Option<&str>, enclosing: Option<u32>, arity: Option<u32>) {
        let at = 1000 + self.0.references.len() as u32;
        self.0.references.push(Reference {
            kind,
            name: name.into(),
            receiver: receiver.map(Into::into),
            enclosing,
            span: sp(at),
            arity,
        });
    }
    fn import(&mut self, module: &str, names: &[&str], alias: Option<&str>) {
        self.0.imports.push(Import {
            module: module.into(),
            relative_level: 0,
            names: names.iter().map(|n| ImportedName { name: (*n).into(), alias: None }).collect(),
            module_alias: alias.map(Into::into),
            type_only: false,
            reexport: false,
            span: sp(0),
        });
    }
    fn file(self, path: &str, language: Language) -> FileInput {
        FileInput {
            path: path.into(),
            blob_sha: format!("sha-{path}"),
            language: Some(language),
            size: 1,
            facts: Some(self.0),
            status: "ok".into(),
        }
    }
}

fn run(registry: Registry, files: Vec<FileInput>) -> ResolveOutput {
    let project = ProjectFiles { paths: files.iter().map(|f| f.path.clone()).collect(), configs: Default::default() };
    let snapshot = SnapshotInfo {
        repo: "local/hooks".into(),
        commit: "c".into(),
        dirty: false,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    };
    let out = resolve(snapshot, files, &project, &registry);
    out.graph.validate().expect("valid graph");
    out
}

struct G<'g>(&'g ResolvedGraph);

impl G<'_> {
    #[track_caller]
    fn id(&self, qn: &str) -> NodeId {
        let v: Vec<_> = self.0.nodes.iter().enumerate().filter(|(_, n)| n.qualified_name == qn).collect();
        assert_eq!(v.len(), 1, "expected one node {qn}, got {}", v.len());
        v[0].0 as NodeId
    }
    fn edge(&self, src: &str, dst: &str, kind: EdgeKind) -> Option<EdgeMeta> {
        let (s, d) = (self.id(src), self.id(dst));
        self.0.edges.iter().find(|e| e.src == s && e.dst == d && e.kind == kind).map(|e| e.meta)
    }
    #[track_caller]
    fn has(&self, src: &str, dst: &str, kind: EdgeKind, strategy: Strategy, confidence: f32) {
        let m = self.edge(src, dst, kind).unwrap_or_else(|| panic!("missing {kind} {src} -> {dst}"));
        assert_eq!(m.strategy, strategy, "{kind} {src} -> {dst}");
        assert!((m.confidence - confidence).abs() < 1e-4, "{kind} {src} -> {dst}: {} != {confidence}", m.confidence);
    }
    #[track_caller]
    fn lacks(&self, src: &str, dst: &str, kind: EdgeKind) {
        assert!(self.edge(src, dst, kind).is_none(), "unexpected {kind} {src} -> {dst}");
    }
    fn module_of(&self, path: &str) -> &str {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap();
        &self.0.modules[f]
    }
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Boilerplate shared by the fake packs.
macro_rules! no_extract {
    () => {
        fn extractor_version(&self) -> u32 {
            1
        }
        fn extract(&self, _: &str, l: Language, _: &[u8], _: &Budget) -> Result<FileFacts, ExtractError> {
            Err(ExtractError::Unsupported(l))
        }
    };
}

// ---------------------------------------------------------------------------
// Go-like: package scope, module from facts, directory imports, $self, $type
// ---------------------------------------------------------------------------

/// Package = directory; import path `example.com/m/<dir>`; a file whose facts
/// declare package `x_test` (a `Package` definition) is the external test
/// package `<import path>_test`.
struct FakeGo;

impl LanguagePack for FakeGo {
    no_extract!();
    fn languages(&self) -> &'static [Language] {
        &[Language::Go]
    }
    fn module_name(&self, path: &str, _: &ProjectFiles) -> String {
        format!("example.com/m/{}", parent_dir(path))
    }
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        let base = self.module_name(path, project);
        let test_pkg =
            facts.is_some_and(|f| f.definitions.iter().any(|d| d.label == Label::Package && d.name.ends_with("_test")));
        if test_pkg { format!("{base}_test") } else { base }
    }
    fn resolve_import(&self, _: &str, imp: &Import, project: &ProjectFiles) -> Option<String> {
        let dir = imp.module.strip_prefix("example.com/m/")?;
        project.is_dir(dir).then(|| dir.to_string())
    }
    fn is_builtin(&self, name: &str) -> bool {
        matches!(name, "len" | "string" | "error")
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["$self"]
    }
    fn module_scope(&self, _: Language) -> ModuleScope {
        ModuleScope::Package
    }
}

#[test]
fn go_like_package_scope() {
    use Label::*;
    let mut files = Vec::new();

    // server/server.go: type Server struct; func (s *Server) Start(); func NewServer()
    let mut f = F::default();
    let server = f.def(Struct, "Server", "Server", None);
    f.def(Method, "Start", "Server.Start", Some(server));
    f.def(Function, "NewServer", "NewServer", None);
    files.push(f.file("server/server.go", Language::Go));

    // server/handle.go: func (s *Server) Handle() { helper(); s.Start() }
    let mut f = F::default();
    let handle = f.def(Method, "Handle", "Server.Handle", None);
    f.r(RefKind::Call, "helper", None, Some(handle), Some(0));
    f.r(RefKind::Call, "Start", Some("$self"), Some(handle), Some(0));
    files.push(f.file("server/handle.go", Language::Go));

    // server/util.go: func helper()
    let mut f = F::default();
    f.def(Function, "helper", "helper", None);
    files.push(f.file("server/util.go", Language::Go));

    // server/a_test.go: package server_test (sorts before server.go)
    let mut f = F::default();
    f.def(Package, "server_test", "server_test", None);
    f.def(Function, "TestX", "TestX", None);
    files.push(f.file("server/a_test.go", Language::Go));

    // cmd/main.go: import "example.com/m/server"; func main() {
    //   s := server.NewServer(); var t server.Server; t.Handle() }
    let mut f = F::default();
    f.import("example.com/m/server", &[], Some("server"));
    f.import("net/http", &[], Some("http"));
    let main = f.def(Function, "main", "main", None);
    f.r(RefKind::Call, "NewServer", Some("server"), Some(main), Some(0));
    f.r(RefKind::Call, "Handle", Some(&type_receiver("server.Server")), Some(main), Some(0));
    f.r(RefKind::Call, "Missing", Some(&type_receiver("server.Server")), Some(main), Some(0));
    f.r(RefKind::Call, "Close", Some(&type_receiver("*http.Client")), Some(main), Some(0));
    files.push(f.file("cmd/main.go", Language::Go));

    let mut registry = Registry::new();
    registry.register(Box::new(FakeGo));
    let out = run(registry, files);
    let g = G(&out.graph);

    // module_name_for: the test package is its own module.
    assert_eq!(g.module_of("server/a_test.go"), "example.com/m/server_test");
    assert_eq!(g.module_of("server/util.go"), "example.com/m/server");
    g.id("example.com/m/server_test.TestX");

    let m = "example.com/m/server";
    // Package scope: helper() is declared in another file of the package.
    g.has(&format!("{m}.Server.Handle"), &format!("{m}.helper"), EdgeKind::Calls, Strategy::SameModule, 0.9);
    // $self in a method declared outside its type → receiver type by name.
    g.has(&format!("{m}.Server.Handle"), &format!("{m}.Server.Start"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    // Directory import: IMPORTS File → Folder, binding the package (not the _test one).
    g.has("cmd/main.go", "server", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has("example.com/m/cmd.main", &format!("{m}.NewServer"), EdgeKind::Calls, Strategy::ImportResolved, 0.9);
    // $type:server.Server → member Handle, found through the package
    // (Handle lives in another file than the type).
    g.has("example.com/m/cmd.main", &format!("{m}.Server.Handle"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // A missing member of a known closed type stops; an external type too.
    assert_eq!(out.stats.references_external, 1, "http.Client is external");
    assert_eq!(out.stats.references_resolved, 4);
}

// ---------------------------------------------------------------------------
// Java-like: implicit self, overloads, overrides, on-demand / static imports
// ---------------------------------------------------------------------------

/// Module = the `package` clause (a `Package` definition in the facts);
/// `import a.b.*` / `import a.b.C` resolve to the package directory under
/// `src/`.
struct FakeJava;

impl LanguagePack for FakeJava {
    no_extract!();
    fn languages(&self) -> &'static [Language] {
        &[Language::Java]
    }
    fn module_name(&self, path: &str, _: &ProjectFiles) -> String {
        parent_dir(path).trim_start_matches("src/").replace('/', ".")
    }
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        let declared = facts.and_then(|f| f.definitions.iter().find(|d| d.label == Label::Package));
        declared.map_or_else(|| self.module_name(path, project), |d| d.name.clone())
    }
    fn resolve_import(&self, _: &str, imp: &Import, project: &ProjectFiles) -> Option<String> {
        let dir = format!("src/{}", imp.module.replace('.', "/"));
        project.is_dir(&dir).then_some(dir)
    }
    fn is_builtin(&self, name: &str) -> bool {
        matches!(name, "String" | "Object" | "System")
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
    fn module_scope(&self, _: Language) -> ModuleScope {
        ModuleScope::Package
    }
    fn implicit_self(&self) -> bool {
        true
    }
    fn overloads(&self) -> bool {
        true
    }
    fn overrides(&self) -> bool {
        true
    }
}

#[test]
fn java_like_implicit_self_overloads_overrides() {
    use Label::*;
    let mut files = Vec::new();

    // src/com/acme/Base.java
    let mut f = F::default();
    f.def(Package, "com.acme", "package", None);
    let base = f.def(Class, "Base", "Base", None);
    f.def(Method, "run", "Base.run", Some(base));
    f.def(Method, "close", "Base.close", Some(base));
    files.push(f.file("src/com/acme/Base.java", Language::Java));

    // src/com/acme/Parser.java: class Parser extends Base { Parser(String s);
    //   parse(String); parse(String, int); run() { parse(s, 1); close(); } }
    let mut f = F::default();
    f.def(Package, "com.acme", "package", None);
    let parser = f.def(Class, "Parser", "Parser", None);
    f.d(parser).bases = vec!["Base".into()];
    f.def(Method, "Parser", "Parser.Parser", Some(parser));
    f.def(Method, "parse", "Parser.parse(String)", Some(parser));
    f.def(Method, "parse", "Parser.parse(String, int)", Some(parser));
    let run_m = f.def(Method, "run", "Parser.run", Some(parser));
    f.r(RefKind::Call, "parse", None, Some(run_m), Some(2));
    f.r(RefKind::Call, "close", None, Some(run_m), Some(0));
    files.push(f.file("src/com/acme/Parser.java", Language::Java));

    // src/com/acme/Util.java: class Util { static int max(int, int) }
    let mut f = F::default();
    f.def(Package, "com.acme", "package", None);
    let util = f.def(Class, "Util", "Util", None);
    f.def(Method, "max", "Util.max", Some(util));
    files.push(f.file("src/com/acme/Util.java", Language::Java));

    // src/com/acme/app/App.java: import com.acme.*; import static com.acme.Util.max;
    let mut f = F::default();
    f.def(Package, "com.acme.app", "package", None);
    f.import("com.acme", &["*"], None);
    f.import("com.acme", &["Util.max"], None);
    let app = f.def(Class, "App", "App", None);
    let main = f.def(Method, "main", "App.main", Some(app));
    f.r(RefKind::New, "Parser", None, Some(main), Some(1));
    f.r(RefKind::Call, "parse", Some(&type_receiver("Parser")), Some(main), Some(1));
    f.r(RefKind::Call, "parse", Some(&type_receiver("com.acme.Parser")), Some(main), None);
    f.r(RefKind::Call, "max", None, Some(main), Some(2));
    files.push(f.file("src/com/acme/app/App.java", Language::Java));

    let mut registry = Registry::new();
    registry.register(Box::new(FakeJava));
    let out = run(registry, files);
    let g = G(&out.graph);

    let (p1, p2) = ("com.acme.Parser.parse(String)", "com.acme.Parser.parse(String, int)");
    // implicit self: parse(s, 1) inside Parser → the 2-argument overload.
    g.has("com.acme.Parser.run", p2, EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    g.lacks("com.acme.Parser.run", p1, EdgeKind::Calls);
    // implicit self through the base class.
    g.has("com.acme.Parser.run", "com.acme.Base.close", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // OVERRIDES from the subtype's method to the base method.
    g.has("com.acme.Parser.run", "com.acme.Base.run", EdgeKind::Overrides, Strategy::TypeResolved, 0.9);
    // On-demand import binds the package: new Parser(x) → class + constructor.
    g.has("com.acme.app.App.main", "com.acme.Parser", EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.has("com.acme.app.App.main", "com.acme.Parser.Parser", EdgeKind::Calls, Strategy::ImportResolved, 0.95 * 0.95);
    g.has("src/com/acme/app/App.java", "src/com/acme", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    // $type:Parser with one argument → parse(String); unknown arity → both × 0.8.
    g.has("com.acme.app.App.main", p1, EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    let both = g.edge("com.acme.app.App.main", p2, EdgeKind::Calls).expect("fan-out");
    assert!((both.confidence - 0.85 * 0.8).abs() < 1e-4);
    // import static com.acme.Util.max → the member.
    g.has("com.acme.app.App.main", "com.acme.Util.max", EdgeKind::Calls, Strategy::ImportResolved, 0.95);
}

#[test]
fn java_like_member_slots() {
    use Label::*;
    let mut files = Vec::new();

    // src/com/acme/Shape.java: class Shape { Shape(); int code; int code();
    //   void draw(String); void draw(String, int); }
    let mut f = F::default();
    f.def(Package, "com.acme", "package", None);
    let shape = f.def(Class, "Shape", "Shape", None);
    f.def(Method, "Shape", "Shape.Shape", Some(shape));
    f.def(Field, "code", "Shape.code", Some(shape));
    f.def(Method, "code", "Shape.code", Some(shape));
    f.def(Method, "draw", "Shape.draw(String)", Some(shape));
    f.def(Method, "draw", "Shape.draw(String,int)", Some(shape));
    files.push(f.file("src/com/acme/Shape.java", Language::Java));

    // src/com/acme/Circle.java: class Circle extends Shape { Circle();
    //   void draw(String s) {}  // overrides draw(String) only
    //   void use(Shape other) { code(); code; other.code(); other.code;
    //     Circle c; Shape s; new Circle(); c.use(); } }
    let mut f = F::default();
    f.def(Package, "com.acme", "package", None);
    let circle = f.def(Class, "Circle", "Circle", None);
    f.d(circle).bases = vec!["Shape".into()];
    f.def(Method, "Circle", "Circle.Circle", Some(circle));
    let draw = f.def(Method, "draw", "Circle.draw", Some(circle));
    f.d(draw).signature = Some("void draw(final String s)".into());
    let use_m = f.def(Method, "use", "Circle.use", Some(circle));
    f.r(RefKind::Call, "code", None, Some(use_m), Some(0));
    f.r(RefKind::Read, "code", None, Some(use_m), None);
    f.r(RefKind::Call, "code", Some(&type_receiver("Shape")), Some(use_m), Some(0));
    f.r(RefKind::Write, "code", Some(&type_receiver("Shape")), Some(use_m), None);
    f.r(RefKind::Type, "Circle", None, Some(use_m), None);
    f.r(RefKind::Type, "Shape", None, Some(use_m), None);
    f.r(RefKind::New, "Circle", None, Some(use_m), Some(0));
    f.r(RefKind::Call, "use", Some(&type_receiver("Circle")), Some(use_m), Some(1));
    files.push(f.file("src/com/acme/Circle.java", Language::Java));

    let mut registry = Registry::new();
    registry.register(Box::new(FakeJava));
    let out = run(registry, files);
    let g = G(&out.graph);
    let (field, method) = {
        let v: Vec<NodeId> = (0..out.graph.nodes.len() as NodeId)
            .filter(|&n| out.graph.nodes[n as usize].qualified_name == "com.acme.Shape.code")
            .collect();
        let field = *v.iter().find(|&&n| out.graph.nodes[n as usize].label == Field).unwrap();
        let method = *v.iter().find(|&&n| out.graph.nodes[n as usize].label == Method).unwrap();
        (field, method)
    };
    let use_id = g.id("com.acme.Circle.use");
    let edge = |dst: NodeId, kind: EdgeKind| {
        out.graph.edges.iter().find(|e| e.src == use_id && e.dst == dst && e.kind == kind)
    };
    // A field and a method with one name are separate members: calls find
    // the method, reads and writes the field (implicit `this` and typed).
    let call = edge(method, EdgeKind::Calls).expect("call to the method");
    assert_eq!(call.meta.strategy, Strategy::TypeResolved);
    assert!(edge(field, EdgeKind::Calls).is_none());
    assert!(edge(field, EdgeKind::References).is_some(), "read of the field");
    assert!(edge(field, EdgeKind::Writes).is_some(), "write of the field");
    assert!(edge(method, EdgeKind::References).is_none());
    // A type named like an enclosing class or its superclass is the type,
    // not its constructor (constructors are members named like the class).
    g.has("com.acme.Circle.use", "com.acme.Circle", EdgeKind::References, Strategy::SameModule, 0.9);
    g.has("com.acme.Circle.use", "com.acme.Shape", EdgeKind::References, Strategy::SameModule, 0.9);
    g.lacks("com.acme.Circle.use", "com.acme.Shape.Shape", EdgeKind::References);
    // `$type:Circle` inside Circle: the class, then its member.
    g.has("com.acme.Circle.use", "com.acme.Circle.use", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `new Circle()` inside Circle still reaches the constructor.
    let ctor = g.edge("com.acme.Circle.use", "com.acme.Circle.Circle", EdgeKind::Calls).expect("constructor");
    assert_eq!(ctor.strategy, Strategy::TypeResolved);
    // OVERRIDES paired by signature: draw(String s) → draw(String) only.
    g.has("com.acme.Circle.draw", "com.acme.Shape.draw(String)", EdgeKind::Overrides, Strategy::TypeResolved, 0.9);
    g.lacks("com.acme.Circle.draw", "com.acme.Shape.draw(String,int)", EdgeKind::Overrides);
    // Constructors override nothing.
    g.lacks("com.acme.Circle.Circle", "com.acme.Shape.Shape", EdgeKind::Overrides);
}

// ---------------------------------------------------------------------------
// C++-like: declarations vs bodies, transitive includes
// ---------------------------------------------------------------------------

/// No modules (names carry their namespace); `#include "x"` resolves
/// relative to the including file and is a star import.
struct FakeCpp;

impl LanguagePack for FakeCpp {
    no_extract!();
    fn languages(&self) -> &'static [Language] {
        &[Language::Cpp, Language::C]
    }
    fn module_name(&self, _: &str, _: &ProjectFiles) -> String {
        String::new()
    }
    fn resolve_import(&self, from: &str, imp: &Import, project: &ProjectFiles) -> Option<String> {
        let mut parts: Vec<&str> = parent_dir(from).split('/').filter(|s| !s.is_empty()).collect();
        for seg in imp.module.split('/') {
            match seg {
                ".." => {
                    parts.pop()?;
                }
                "." => {}
                s => parts.push(s),
            }
        }
        let p = parts.join("/");
        project.contains(&p).then_some(p)
    }
    fn is_builtin(&self, name: &str) -> bool {
        matches!(name, "printf" | "size_t")
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
    fn implicit_self(&self) -> bool {
        true
    }
}

#[test]
fn cpp_like_declarations_and_includes() {
    use Label::*;
    let mut files = Vec::new();

    // include/foo.h: namespace ns { class Foo { void bar(); void baz(); }; void util(); }
    let mut f = F::default();
    let foo = f.def(Class, "Foo", "ns::Foo", None);
    let bar = f.def(Method, "bar", "ns::Foo::bar", Some(foo));
    let baz = f.def(Method, "baz", "ns::Foo::baz", Some(foo));
    let util = f.def(Function, "util", "ns::util", None);
    for d in [bar, baz, util] {
        f.d(d).is_declaration = true;
    }
    files.push(f.file("include/foo.h", Language::Cpp));

    // include/all.h: #include "foo.h"
    let mut f = F::default();
    f.import("foo.h", &["*"], None);
    files.push(f.file("include/all.h", Language::Cpp));

    // src/foo.cpp: #include "../include/all.h"
    //   void ns::Foo::bar() { baz(); util(); }  void ns::Foo::baz() {}  void ns::util() {}
    let mut f = F::default();
    f.import("../include/all.h", &["*"], None);
    let bar_body = f.def(Method, "bar", "ns::Foo::bar", None);
    f.def(Method, "baz", "ns::Foo::baz", None);
    f.def(Function, "util", "ns::util", None);
    f.r(RefKind::Call, "baz", None, Some(bar_body), Some(0));
    f.r(RefKind::Call, "util", None, Some(bar_body), Some(0));
    files.push(f.file("src/foo.cpp", Language::Cpp));

    // src/main.cpp: #include "../include/all.h"  int main() { ns::Foo f; f.bar(); }
    let mut f = F::default();
    f.import("../include/all.h", &["*"], None);
    let main = f.def(Function, "main", "main", None);
    f.r(RefKind::Call, "bar", Some(&type_receiver("ns::Foo")), Some(main), Some(0));
    f.r(RefKind::Call, "util", None, Some(main), Some(0));
    files.push(f.file("src/main.cpp", Language::Cpp));

    let mut registry = Registry::new();
    registry.register(Box::new(FakeCpp));
    let out = run(registry, files);
    let g = &out.graph;
    let node = |qn: &str, path: &str| -> NodeId {
        let f = g.files.iter().position(|f| f.path == path).unwrap() as u32;
        g.nodes.iter().position(|n| n.qualified_name == qn && n.file == Some(f)).unwrap() as NodeId
    };
    let calls = |src: NodeId, dst: NodeId| {
        g.edges.iter().find(|e| e.src == src && e.dst == dst && e.kind == EdgeKind::Calls).map(|e| e.meta)
    };
    let body = |qn: &str| node(qn, "src/foo.cpp");
    let (main, bar_body) = (node("main", "src/main.cpp"), body("ns::Foo::bar"));
    // Out-of-line body: implicit self through the qualified-name prefix,
    // and the declaration found in the class is replaced by its body.
    let m = calls(bar_body, body("ns::Foo::baz")).expect("bar → baz body");
    assert_eq!((m.strategy, m.confidence), (Strategy::TypeResolved, 0.9));
    assert!(calls(bar_body, node("ns::Foo::baz", "include/foo.h")).is_none(), "never the declaration");
    // Transitive include (foo.cpp → all.h → foo.h) binds `util`; body preferred.
    let m = calls(bar_body, body("ns::util")).expect("bar → util body");
    assert_eq!(m.strategy, Strategy::ImportResolved);
    let m = calls(main, body("ns::util")).expect("main → util body");
    assert_eq!(m.strategy, Strategy::ImportResolved);
    // $type:ns::Foo → exact qualified type, member bar → its body.
    let m = calls(main, bar_body).expect("main → Foo::bar body");
    assert_eq!((m.strategy, m.confidence), (Strategy::TypeResolved, 0.85));
}

// ---------------------------------------------------------------------------
// Bridge: env vars and config keys
// ---------------------------------------------------------------------------

/// Data formats: module = path with extension; Terraform (`Hcl`) files of a
/// directory share one module; imports resolve relative to the file and may
/// name a directory.
struct FakeData;

impl LanguagePack for FakeData {
    no_extract!();
    fn languages(&self) -> &'static [Language] {
        &[Language::Json, Language::Yaml, Language::DotEnv, Language::Properties, Language::Hcl]
    }
    fn module_name(&self, path: &str, _: &ProjectFiles) -> String {
        path.to_string()
    }
    fn resolve_import(&self, from: &str, imp: &Import, project: &ProjectFiles) -> Option<String> {
        let mut parts: Vec<&str> = parent_dir(from).split('/').filter(|s| !s.is_empty()).collect();
        for seg in imp.module.split('/') {
            match seg {
                "." | "" => {}
                ".." => {
                    parts.pop()?;
                }
                s => parts.push(s),
            }
        }
        let p = parts.join("/");
        (project.contains(&p) || project.is_dir(&p)).then_some(p)
    }
    fn is_builtin(&self, _: &str) -> bool {
        false
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &[]
    }
    fn module_scope(&self, language: Language) -> ModuleScope {
        if language == Language::Hcl { ModuleScope::Directory } else { ModuleScope::File }
    }
}

/// Python-like code pack: module = dotted path.
struct FakeCode;

impl LanguagePack for FakeCode {
    no_extract!();
    fn languages(&self) -> &'static [Language] {
        &[Language::Python]
    }
    fn module_name(&self, path: &str, _: &ProjectFiles) -> String {
        path.trim_end_matches(".py").replace('/', ".")
    }
    fn resolve_import(&self, _: &str, _: &Import, _: &ProjectFiles) -> Option<String> {
        None
    }
    fn is_builtin(&self, _: &str) -> bool {
        false
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["self"]
    }
}

fn data_registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(FakeData));
    r.register(Box::new(FakeCode));
    r
}

#[test]
fn bridge_env_vars_and_config_keys() {
    use Label::*;
    let mut files = Vec::new();

    // .env.example: DATABASE_URL=… (a provider)
    let mut f = F::default();
    f.def(EnvVar, "DATABASE_URL", "DATABASE_URL", None);
    files.push(f.file(".env.example", Language::DotEnv));

    // k8s/web.yaml: containers["web"].env["DATABASE_URL"] (a provider)
    let mut f = F::default();
    let dep = f.def(Resource, "Deployment/web", "Deployment/web", None);
    f.def(EnvVar, "DATABASE_URL", "Deployment/web.env[\"DATABASE_URL\"]", Some(dep));
    files.push(f.file("k8s/web.yaml", Language::Yaml));

    // src/main/resources/application.yml: server.port, spring.datasource.url: ${DB_URL}
    let mut f = F::default();
    let server = f.def(Key, "server", "server", None);
    f.def(Key, "port", "server.port", Some(server));
    let url = f.def(Key, "url", "spring.datasource.url", None);
    f.r(RefKind::EnvVar, "DB_URL", None, Some(url), None);
    files.push(f.file("src/main/resources/application.yml", Language::Yaml));

    // src/main/resources/application-prod.properties: server.port (a profile)
    let mut f = F::default();
    f.def(Key, "port", "server.port", None);
    files.push(f.file("src/main/resources/application-prod.properties", Language::Properties));

    // other/settings.json: cache.ttl (unique); a/x.yaml, b/y.yaml: log.level (ambiguous)
    let mut f = F::default();
    let cache = f.def(Key, "cache", "cache", None);
    f.def(Key, "ttl", "cache.ttl", Some(cache));
    files.push(f.file("other/settings.json", Language::Json));
    for p in ["a/x.yaml", "b/y.yaml"] {
        let mut f = F::default();
        f.def(Key, "level", "log.level", None);
        files.push(f.file(p, Language::Yaml));
    }

    // app.py: def main(): os.getenv("DATABASE_URL"); os.environ["UNSET"];
    //   @Value-like spring reads; unscoped and ambiguous reads.
    let mut f = F::default();
    let main = f.def(Function, "main", "main", None);
    f.r(RefKind::EnvVar, "DATABASE_URL", None, Some(main), None);
    f.r(RefKind::EnvVar, "UNSET", None, Some(main), None);
    f.r(RefKind::ConfigKey, "server.port", Some("spring"), Some(main), None);
    f.r(RefKind::ConfigKey, "cache.ttl", None, Some(main), None);
    f.r(RefKind::ConfigKey, "log.level", None, Some(main), None);
    f.r(RefKind::ConfigKey, "server.port", Some("django"), Some(main), None);
    files.push(f.file("app.py", Language::Python));

    let out = run(data_registry(), files);
    let g = G(&out.graph);

    // Canonical env var nodes: file-less, one per name, created on demand.
    let db = g.id("$env:DATABASE_URL");
    let node = &out.graph.nodes[db as usize];
    assert_eq!((node.label, node.file, node.name.as_str()), (EnvVar, None, "DATABASE_URL"));
    g.id("$env:UNSET");
    g.id("$env:DB_URL");
    // Readers (contract 0.95) and providers (syntactic 1.0).
    g.has("app.main", "$env:DATABASE_URL", EdgeKind::Reads, Strategy::Contract, 0.95);
    g.has("app.main", "$env:UNSET", EdgeKind::Reads, Strategy::Contract, 0.95);
    g.has(".env.example:DATABASE_URL", "$env:DATABASE_URL", EdgeKind::Writes, Strategy::Syntactic, 1.0);
    g.has(
        "k8s/web.yaml:Deployment/web.env[\"DATABASE_URL\"]",
        "$env:DATABASE_URL",
        EdgeKind::Writes,
        Strategy::Syntactic,
        1.0,
    );
    // Config that reads env: Key → READS → $env (chains compose).
    g.has(
        "src/main/resources/application.yml:spring.datasource.url",
        "$env:DB_URL",
        EdgeKind::Reads,
        Strategy::Contract,
        0.95,
    );
    // Spring scope: every profile's key, contract 0.90.
    g.has("app.main", "src/main/resources/application.yml:server.port", EdgeKind::Reads, Strategy::Contract, 0.9);
    g.has(
        "app.main",
        "src/main/resources/application-prod.properties:server.port",
        EdgeKind::Reads,
        Strategy::Contract,
        0.9,
    );
    // No namespace: the unique key, 0.60; ambiguous: nothing.
    g.has("app.main", "other/settings.json:cache.ttl", EdgeKind::Reads, Strategy::UniqueName, 0.6);
    g.lacks("app.main", "a/x.yaml:log.level", EdgeKind::Reads);
    g.lacks("app.main", "b/y.yaml:log.level", EdgeKind::Reads);
    // Keys are never name-matched by code references.
    assert_eq!(out.stats.resolved_by_strategy.get("contract"), Some(&4));
    assert_eq!(out.stats.references_total, 7);
    assert_eq!(out.stats.references_resolved, 5);
}

#[test]
fn data_pointers_and_directory_scope() {
    use Label::*;
    let mut files = Vec::new();

    // openapi.yaml: $ref '#/components/schemas/User', $ref './common.yaml#/Error',
    // prod: {<<: *defaults}
    let mut f = F::default();
    f.import("./common.yaml", &[], None);
    let comps = f.def(Key, "components", "components", None);
    let schemas = f.def(Key, "schemas", "components.schemas", Some(comps));
    f.def(Key, "User", "components.schemas.User", Some(schemas));
    let get = f.def(Route, "GET /users", "paths./users.get", None);
    f.r(RefKind::DataRef, "components.schemas.User", None, Some(get), None);
    f.r(RefKind::DataRef, "Error", Some("./common.yaml"), Some(get), None);
    f.r(RefKind::DataRef, "app.main", Some("$qn"), Some(get), None);
    f.r(RefKind::DataRef, "ConfigMap/app.data.LEVEL", Some("$any"), Some(get), None);
    f.r(RefKind::DataRef, "nope", None, Some(get), None);
    f.def(Key, "defaults", "defaults", None);
    let prod = f.def(Key, "prod", "prod", None);
    f.d(prod).bases = vec!["defaults".into(), "./common.yaml#Error".into()];
    files.push(f.file("openapi.yaml", Language::Yaml));

    let mut f = F::default();
    f.def(Key, "Error", "Error", None);
    files.push(f.file("common.yaml", Language::Yaml));

    let mut f = F::default();
    let cm = f.def(Resource, "ConfigMap/app", "ConfigMap/app", None);
    f.def(Key, "LEVEL", "ConfigMap/app.data.LEVEL", Some(cm));
    files.push(f.file("k8s/cm.yaml", Language::Yaml));

    // infra/main.tf: module "net" { source = "./modules/net" }, var.region (declared in vars.tf)
    let mut f = F::default();
    f.import("./modules/net", &[], None);
    let bucket = f.def(Resource, "logs", "aws_s3_bucket.logs", None);
    f.r(RefKind::DataRef, "var.region", None, Some(bucket), None);
    files.push(f.file("infra/main.tf", Language::Hcl));
    let mut f = F::default();
    f.def(Variable, "region", "var.region", None);
    files.push(f.file("infra/vars.tf", Language::Hcl));
    let mut f = F::default();
    f.def(Variable, "cidr", "var.cidr", None);
    files.push(f.file("infra/modules/net/main.tf", Language::Hcl));

    let mut f = F::default();
    f.def(Function, "main", "main", None);
    files.push(f.file("app.py", Language::Python));

    let out = run(data_registry(), files);
    let g = G(&out.graph);
    let get = "openapi.yaml:paths./users.get";
    g.has(get, "openapi.yaml:components.schemas.User", EdgeKind::References, Strategy::Syntactic, 1.0);
    g.has(get, "common.yaml:Error", EdgeKind::References, Strategy::ImportResolved, 0.95);
    g.has(get, "app.main", EdgeKind::References, Strategy::ImportResolved, 0.95);
    g.has(get, "k8s/cm.yaml:ConfigMap/app.data.LEVEL", EdgeKind::References, Strategy::UniqueName, 0.6);
    g.has("openapi.yaml", "common.yaml", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    // Merge keys and cross-file bases → INHERITS.
    g.has("openapi.yaml:prod", "openapi.yaml:defaults", EdgeKind::Inherits, Strategy::Syntactic, 1.0);
    g.has("openapi.yaml:prod", "common.yaml:Error", EdgeKind::Inherits, Strategy::ImportResolved, 0.95);
    // Directory scope: var.region is declared in another .tf file of infra/.
    g.has(
        "infra/main.tf:aws_s3_bucket.logs",
        "infra/vars.tf:var.region",
        EdgeKind::References,
        Strategy::SameModule,
        0.95,
    );
    // Folder import (Terraform module source) without a package binding.
    g.has("infra/main.tf", "infra/modules/net", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    // Never fuzzy: an unknown key path stays unresolved.
    assert_eq!(out.stats.references_total, 6);
    assert_eq!(out.stats.references_resolved, 5);
}
