//! Unit tests with hand-built facts and tiny fake language packs.

use super::*;
use rueca_extract::{Budget, ExtractError};
use rueca_model::ImportedName;

// ---------------------------------------------------------------------------
// Fake packs
// ---------------------------------------------------------------------------

struct FakePy;

impl LanguagePack for FakePy {
    fn languages(&self) -> &'static [Language] {
        &[Language::Python]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(&self, _: &str, l: Language, _: &[u8], _: &Budget) -> Result<FileFacts, ExtractError> {
        Err(ExtractError::Unsupported(l))
    }
    fn module_name(&self, path: &str, _: &ProjectFiles) -> String {
        let m = path.trim_end_matches(".py").replace('/', ".");
        m.strip_suffix(".__init__").map(str::to_string).unwrap_or(m)
    }
    fn resolve_import(&self, from: &str, imp: &Import, project: &ProjectFiles) -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        if imp.relative_level > 0 {
            parts = from.split('/').collect();
            parts.pop();
            for _ in 1..imp.relative_level {
                parts.pop()?;
            }
        }
        parts.extend(imp.module.split('.').filter(|s| !s.is_empty()));
        let p = parts.join("/");
        [format!("{p}.py"), format!("{p}/__init__.py")].into_iter().find(|c| project.contains(c))
    }
    fn is_builtin(&self, name: &str) -> bool {
        matches!(name, "print" | "len" | "object" | "Exception" | "ValueError" | "str" | "super")
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["self", "cls"]
    }
}

struct FakeEs;

impl LanguagePack for FakeEs {
    fn languages(&self) -> &'static [Language] {
        &[Language::TypeScript, Language::Tsx, Language::JavaScript]
    }
    fn extractor_version(&self) -> u32 {
        1
    }
    fn extract(&self, _: &str, l: Language, _: &[u8], _: &Budget) -> Result<FileFacts, ExtractError> {
        Err(ExtractError::Unsupported(l))
    }
    fn module_name(&self, path: &str, _: &ProjectFiles) -> String {
        path.rsplit_once('.').map_or(path, |(s, _)| s).to_string()
    }
    fn resolve_import(&self, from: &str, imp: &Import, project: &ProjectFiles) -> Option<String> {
        if !imp.module.starts_with('.') {
            return None;
        }
        let mut parts: Vec<&str> = from.split('/').collect();
        parts.pop();
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
        [p.clone(), format!("{p}.ts"), format!("{p}.tsx"), format!("{p}.js"), format!("{p}/index.ts")]
            .into_iter()
            .find(|c| project.contains(c))
    }
    fn is_builtin(&self, name: &str) -> bool {
        matches!(name, "console" | "Math" | "JSON" | "Error" | "Array")
    }
    fn self_receivers(&self) -> &'static [&'static str] {
        &["this"]
    }
}

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register(Box::new(FakePy));
    r.register(Box::new(FakeEs));
    r
}

// ---------------------------------------------------------------------------
// Facts builder
// ---------------------------------------------------------------------------

fn sp(a: u32, b: u32) -> Span {
    Span { start_byte: a, end_byte: b, start_line: a, start_col: 0, end_line: b, end_col: 0 }
}

#[derive(Default)]
struct F {
    facts: FileFacts,
    python: bool,
}

impl F {
    fn py() -> Self {
        F { facts: FileFacts::default(), python: true }
    }
    fn es() -> Self {
        F { facts: FileFacts::default(), python: false }
    }
    /// Adds a definition spanning bytes `a..b`; the name occupies `a+1..a+2`.
    fn def(&mut self, label: Label, name: &str, parent: Option<u32>, a: u32, b: u32) -> u32 {
        let qn = match parent {
            Some(p) => format!("{}.{name}", self.facts.definitions[p as usize].qualified_name),
            None => name.to_string(),
        };
        let exported = self.python && parent.is_none() && !name.starts_with('_');
        self.facts.definitions.push(Definition {
            label,
            name: name.into(),
            qualified_name: qn,
            parent,
            span: sp(a, b),
            name_span: sp(a + 1, a + 2),
            signature: None,
            doc: None,
            decorators: vec![],
            bases: vec![],
            implements: vec![],
            exported,
            is_test: false,
        });
        (self.facts.definitions.len() - 1) as u32
    }
    fn d(&mut self, id: u32) -> &mut Definition {
        &mut self.facts.definitions[id as usize]
    }
    fn r(&mut self, kind: RefKind, name: &str, receiver: Option<&str>, enclosing: Option<u32>, a: u32) {
        self.facts.references.push(Reference {
            kind,
            name: name.into(),
            receiver: receiver.map(Into::into),
            enclosing,
            span: sp(a, a + 1),
        });
    }
    fn call(&mut self, name: &str, receiver: Option<&str>, enclosing: Option<u32>, a: u32) {
        self.r(RefKind::Call, name, receiver, enclosing, a);
    }
    fn imp(&mut self, module: &str, level: u32, names: &[(&str, Option<&str>)], alias: Option<&str>) -> &mut Import {
        self.facts.imports.push(Import {
            module: module.into(),
            relative_level: level,
            names: names.iter().map(|(n, a)| ImportedName { name: (*n).into(), alias: a.map(Into::into) }).collect(),
            module_alias: alias.map(Into::into),
            type_only: false,
            reexport: false,
            span: sp(0, 1),
        });
        self.facts.imports.last_mut().unwrap()
    }
    fn reexport(&mut self, module: &str, names: &[(&str, Option<&str>)], alias: Option<&str>) {
        self.imp(module, 0, names, alias).reexport = true;
    }
    fn file(self, path: &str) -> FileInput {
        FileInput {
            path: path.into(),
            blob_sha: format!("sha-{path}"),
            language: Language::from_path(path),
            size: 10,
            facts: Some(self.facts),
            status: "ok".into(),
        }
    }
}

fn plain(path: &str) -> FileInput {
    FileInput {
        path: path.into(),
        blob_sha: "x".into(),
        language: Language::from_path(path),
        size: 1,
        facts: None,
        status: "skipped:unsupported".into(),
    }
}

fn snapshot() -> SnapshotInfo {
    SnapshotInfo {
        repo: "github.com/acme/demo".into(),
        commit: "abc".into(),
        dirty: false,
        root: None,
        indexed_at: 0,
        rueca_version: "test".into(),
    }
}

fn run(files: Vec<FileInput>) -> ResolveOutput {
    let project = ProjectFiles { paths: files.iter().map(|f| f.path.clone()).collect(), configs: Default::default() };
    let out = resolve(snapshot(), files, &project, &registry());
    out.graph.validate().expect("graph must validate");
    out
}

struct G<'g>(&'g ResolvedGraph);

impl G<'_> {
    fn id(&self, qn: &str) -> NodeId {
        let v: Vec<_> = self.0.nodes.iter().enumerate().filter(|(_, n)| n.qualified_name == qn).collect();
        assert_eq!(v.len(), 1, "expected exactly one node {qn}, got {}", v.len());
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
    fn out_count(&self, src: &str, kind: EdgeKind) -> usize {
        let s = self.id(src);
        self.0.edges.iter().filter(|e| e.src == s && e.kind == kind).count()
    }
}

// ---------------------------------------------------------------------------
// Python-like project
// ---------------------------------------------------------------------------

fn python_project() -> Vec<FileInput> {
    use Label::*;
    let mut files = Vec::new();

    // pkg/__init__.py: from .models import User
    let mut f = F::py();
    f.imp("models", 1, &[("User", None)], None);
    files.push(f.file("pkg/__init__.py"));

    // pkg/models.py
    let mut f = F::py();
    let base = f.def(Class, "Base", None, 0, 100);
    let _save = f.def(Method, "save", Some(base), 10, 50);
    let user = f.def(Class, "User", None, 100, 400);
    f.d(user).bases = vec!["Base".into()];
    f.r(RefKind::Read, "Base", None, None, 102); // the base-list expression
    let _init = f.def(Method, "__init__", Some(user), 110, 150);
    let greet = f.def(Method, "greet", Some(user), 150, 250);
    let _name_of = f.def(Method, "name_of", Some(user), 250, 290);
    let create = f.def(Method, "create", Some(user), 290, 330);
    f.d(create).decorators = vec!["classmethod".into()];
    f.call("save", Some("self"), Some(greet), 160);
    f.call("name_of", Some("self"), Some(greet), 170);
    f.call("missing", Some("self"), Some(greet), 180);
    f.call("compute_total", Some("self.ledger"), Some(greet), 190);
    f.call("User", Some("cls"), Some(create), 300); // not a member: unresolved
    f.r(RefKind::Throw, "ValueError", None, Some(greet), 200);
    let ledger = f.def(Class, "Ledger", None, 400, 500);
    let _ct = f.def(Method, "compute_total", Some(ledger), 410, 450);
    files.push(f.file("pkg/models.py"));

    // pkg/utils.py
    let mut f = F::py();
    let _helper = f.def(Function, "helper", None, 0, 50);
    let outer = f.def(Function, "outer", None, 50, 200);
    let inner = f.def(Function, "inner", Some(outer), 60, 100);
    f.call("inner", None, Some(outer), 120);
    f.call("inner", None, Some(inner), 70);
    f.call("helper", None, Some(outer), 130);
    let _register = f.def(Function, "register", None, 200, 250);
    // Redefinition: the second `helper` is merged into the first.
    let _helper2 = f.def(Function, "helper", None, 250, 300);
    f.call("helper", None, Some(4), 260);
    files.push(f.file("pkg/utils.py"));

    // lib/other.py: a uniquely named function nobody imports.
    let mut f = F::py();
    f.def(Function, "standalone", None, 0, 10);
    f.def(Function, "get", None, 10, 20);
    files.push(f.file("lib/other.py"));

    // app.py
    let mut f = F::py();
    f.imp("pkg.utils", 0, &[], None); // import pkg.utils  → binds `pkg`
    f.imp("pkg", 0, &[("User", None)], None); // from pkg import User (via __init__)
    f.imp("pkg.models", 0, &[("Base", Some("B"))], None);
    f.imp("pkg.utils", 0, &[("register", None)], None);
    f.imp("os", 0, &[], None);
    f.imp("requests", 0, &[("get", None)], None);
    f.imp("pkg", 0, &[("utils", Some("u"))], None); // submodule import
    let main = f.def(Label::Function, "main", None, 100, 300);
    f.d(main).decorators = vec!["register".into()];
    f.r(RefKind::Decorator, "register", None, Some(main), 90);
    f.call("helper", Some("pkg.utils"), Some(main), 110);
    f.call("User", None, Some(main), 120);
    f.call("create", Some("User"), Some(main), 125);
    f.call("join", Some("os.path"), Some(main), 130);
    f.call("get", None, Some(main), 140);
    f.call("print", None, Some(main), 150);
    f.call("compute_total", Some("obj"), Some(main), 160);
    f.call("run", Some("obj"), Some(main), 170);
    f.call("standalone", None, Some(main), 180);
    f.call("helper", Some("u"), Some(main), 190);
    f.r(RefKind::Type, "B", None, Some(main), 200);
    f.call("main", None, None, 400);
    files.push(f.file("app.py"));

    // tests/test_app.py
    let mut f = F::py();
    f.imp("pkg.models", 0, &[("User", None)], None);
    let t = f.def(Label::Function, "test_user", None, 0, 100);
    f.d(t).is_test = true;
    f.call("User", None, Some(t), 10);
    files.push(f.file("tests/test_app.py"));

    files.push(plain("README.md"));
    files
}

#[test]
fn python_structure() {
    let out = run(python_project());
    let g = G(&out.graph);
    let n = &out.graph.nodes;
    assert_eq!(n[0].label, Label::Repository);
    assert_eq!(n[0].qualified_name, "github.com/acme/demo");
    g.has("github.com/acme/demo", "pkg", EdgeKind::Contains, Strategy::Syntactic, 1.0);
    g.has("github.com/acme/demo", "README.md", EdgeKind::Contains, Strategy::Syntactic, 1.0);
    g.has("pkg", "pkg/models.py", EdgeKind::Contains, Strategy::Syntactic, 1.0);
    g.has("pkg/models.py", "pkg.models.User", EdgeKind::Defines, Strategy::Syntactic, 1.0);
    g.has("pkg.models.User", "pkg.models.User.greet", EdgeKind::Defines, Strategy::Syntactic, 1.0);
    g.lacks("pkg/models.py", "pkg.models.User.greet", EdgeKind::Defines);
    // File node for an unsupported file.
    let readme = &out.graph.files.iter().find(|f| f.path == "README.md").unwrap();
    assert_eq!(n[readme.node as usize].label, Label::File);
    assert_eq!(readme.status, "skipped:unsupported");
    // __init__ module name.
    let user = &n[g.id("pkg.models.User") as usize];
    assert_eq!(user.name, "User");
    assert_eq!(user.language, Some(Language::Python));
    // Redefinition merged.
    assert_eq!(n.iter().filter(|x| x.qualified_name == "pkg.utils.helper").count(), 1);
    // Imports.
    g.has("pkg/__init__.py", "pkg/models.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has("app.py", "pkg/utils.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has("app.py", "pkg/__init__.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    assert_eq!(g.out_count("app.py", EdgeKind::Imports), 3);
}

#[test]
fn python_strategies() {
    let out = run(python_project());
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // 1. Local scope (incl. recursion through the parent chain).
    g.has("pkg.utils.outer", "pkg.utils.outer.inner", Calls, SameModule, 0.95);
    g.has("pkg.utils.outer.inner", "pkg.utils.outer.inner", Calls, SameModule, 0.95);
    // 2. Self receiver: own member and inherited.
    g.has("pkg.models.User.greet", "pkg.models.User.name_of", Calls, TypeResolved, 0.9);
    g.has("pkg.models.User.greet", "pkg.models.Base.save", Calls, TypeResolved, 0.85);
    // 3. Imported binding through a package __init__ re-export + constructor.
    g.has("app.main", "pkg.models.User", Calls, ImportResolved, 0.95);
    g.has("app.main", "pkg.models.User.__init__", Calls, ImportResolved, 0.95 * 0.95);
    // 3. Module receiver through `import pkg.utils` and a submodule alias.
    g.has("app.main", "pkg.utils.helper", Calls, ImportResolved, 0.9);
    // 3. Static member of an imported class.
    g.has("app.main", "pkg.models.User.create", Calls, ImportResolved, 0.85);
    // 3. Aliased import, type position.
    g.has("app.main", "pkg.models.Base", References, ImportResolved, 0.95);
    // 4. Same module (from a function and from module level).
    g.has("pkg.utils.outer", "pkg.utils.helper", Calls, SameModule, 0.9);
    g.has("app.py", "app.main", Calls, SameModule, 0.9);
    // 6. Unique name; heuristic method with an unknown receiver.
    g.has("app.main", "lib.other.standalone", Calls, UniqueName, 0.6);
    g.has("app.main", "pkg.models.Ledger.compute_total", Calls, Heuristic, 0.5);
    g.has("pkg.models.User.greet", "pkg.models.Ledger.compute_total", Calls, Heuristic, 0.5);
    // Decorator: decorator → decorated.
    g.has("pkg.utils.register", "app.main", Decorates, ImportResolved, 0.95);
    // Inheritance, from Definition.bases and from the base-list reference.
    g.has("pkg.models.User", "pkg.models.Base", Inherits, SameModule, 0.9);
    g.lacks("pkg/models.py", "pkg.models.Base", References);
    // Tests.
    g.has("tests.test_app.test_user", "pkg.models.User", Tests, ImportResolved, 0.95);
    g.has("tests.test_app.test_user", "pkg.models.User.__init__", Tests, ImportResolved, 0.95 * 0.95);

    // External names never fall back to unique-name matching.
    g.lacks("app.main", "lib.other.get", Calls);
    // Self receiver that misses does not fall back either; short heuristics are refused.
    assert_eq!(g.out_count("pkg.models.User.create", Calls), 0);

    let s = &out.stats;
    // join (os) + get (requests).
    assert_eq!(s.references_external, 2);
    // print + ValueError.
    assert_eq!(s.references_builtin, 2);
    let total: u64 = s.resolved_by_strategy.values().sum();
    assert_eq!(total, s.references_resolved);
    assert!(s.resolved_by_strategy["heuristic"] >= 2);
    // missing, run, cls.User, classmethod (synthesized? no: app.py has a decorator ref,
    // models.py does not → classmethod synthesized and unresolved).
    let unresolved = s.references_total - s.references_resolved;
    assert_eq!(unresolved, s.references_external + s.references_builtin + 4);
}

#[test]
fn python_star_and_cycles() {
    use Label::*;
    let mut files = Vec::new();
    let mut f = F::py();
    f.def(Function, "public", None, 0, 10);
    f.def(Function, "_private", None, 10, 20);
    files.push(f.file("lib.py"));
    // a.py and b.py import `x` from each other; nobody defines it.
    let mut f = F::py();
    f.imp("b", 0, &[("x", None)], None);
    files.push(f.file("a.py"));
    let mut f = F::py();
    f.imp("a", 0, &[("x", None)], None);
    files.push(f.file("b.py"));
    let mut f = F::py();
    f.imp("lib", 0, &[("*", None)], None);
    f.imp("a", 0, &[("x", None)], None);
    let m = f.def(Function, "m", None, 0, 100);
    f.call("public", None, Some(m), 10);
    f.call("_private", None, Some(m), 20);
    f.call("x", None, Some(m), 30);
    files.push(f.file("c.py"));
    let out = run(files);
    let g = G(&out.graph);
    g.has("c.m", "lib.public", EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    // `_private` is not star-imported; it is still unique in the project.
    g.has("c.m", "lib._private", EdgeKind::Calls, Strategy::UniqueName, 0.6);
    assert_eq!(g.out_count("c.m", EdgeKind::Calls), 2);
    g.has("a.py", "b.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has("b.py", "a.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
}

#[test]
fn inheritance_chain_and_cycles() {
    use Label::*;
    let mut f = F::py();
    let d = f.def(Class, "D", None, 0, 100);
    f.def(Method, "deep", Some(d), 10, 20);
    let c = f.def(Class, "C", None, 100, 200);
    f.d(c).bases = vec!["D".into()];
    let b = f.def(Class, "B", None, 200, 300);
    f.d(b).bases = vec!["C".into()];
    let a = f.def(Class, "A", None, 300, 500);
    f.d(a).bases = vec!["object".into(), "B".into(), "metaclass=Meta".into()];
    let run_m = f.def(Method, "run", Some(a), 310, 400);
    f.call("deep", Some("self"), Some(run_m), 320);
    f.call("deep", Some("super()"), Some(run_m), 330);
    // Base cycle X <-> Y.
    let x = f.def(Class, "X", None, 500, 600);
    f.d(x).bases = vec!["Y".into()];
    let y = f.def(Class, "Y", None, 600, 700);
    f.d(y).bases = vec!["X".into(), "Y".into()];
    let xm = f.def(Method, "go", Some(x), 510, 590);
    f.call("nothing_here", Some("self"), Some(xm), 520);
    let out = run(vec![f.file("m.py")]);
    let g = G(&out.graph);
    g.has("m.A.run", "m.D.deep", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has("m.A", "m.B", EdgeKind::Inherits, Strategy::SameModule, 0.9);
    g.has("m.X", "m.Y", EdgeKind::Inherits, Strategy::SameModule, 0.9);
    g.lacks("m.Y", "m.Y", EdgeKind::Inherits);
    assert_eq!(g.out_count("m.X.go", EdgeKind::Calls), 0);
}

// ---------------------------------------------------------------------------
// ECMAScript-like project
// ---------------------------------------------------------------------------

fn es_project() -> Vec<FileInput> {
    use Label::*;
    let mut files = Vec::new();

    let mut f = F::es();
    let add = f.def(Function, "add", None, 0, 50);
    f.d(add).exported = true;
    let calc = f.def(Class, "Calc", None, 50, 200);
    f.d(calc).exported = true;
    f.def(Method, "constructor", Some(calc), 60, 80);
    f.def(Method, "sum", Some(calc), 80, 120);
    files.push(f.file("src/util/math.ts"));

    // Barrel with a cycle back to itself through `cyc.ts`.
    let mut f = F::es();
    f.reexport("./math", &[], None);
    f.reexport("./math", &[("add", Some("plus"))], None);
    f.reexport("./cyc", &[], None);
    f.reexport("./math", &[], Some("M"));
    files.push(f.file("src/util/index.ts"));
    let mut f = F::es();
    f.reexport("./index", &[], None);
    files.push(f.file("src/util/cyc.ts"));

    let mut f = F::es();
    let logger = f.def(Class, "Logger", None, 0, 100);
    f.d(logger).exported = true;
    f.def(Method, "log", Some(logger), 10, 50);
    let helper = f.def(Function, "internalHelper", None, 100, 150);
    f.d(helper).exported = false;
    files.push(f.file("src/Logger.ts"));

    let mut f = F::es();
    let base = f.def(Class, "Base", None, 0, 100);
    f.d(base).exported = true;
    f.def(Method, "baseOnly", Some(base), 10, 50);
    files.push(f.file("src/base.ts"));

    let mut f = F::es();
    f.imp("./util", 0, &[("add", None), ("Calc", None), ("plus", None)], None);
    f.imp("./Logger", 0, &[("default", Some("Logger"))], None);
    f.imp("./util", 0, &[], Some("U"));
    f.imp("react", 0, &[("default", Some("React"))], None);
    f.imp("./base", 0, &[("Base", None)], None);
    f.imp("./util/math", 0, &[("missingName", None)], None);
    let ithing = f.def(Interface, "IThing", None, 0, 20);
    let app = f.def(Class, "App", None, 20, 400);
    f.d(app).exported = true;
    f.d(app).bases = vec!["Base".into()];
    f.d(app).implements = vec!["IThing".into()];
    f.r(RefKind::Type, "Base", None, Some(app), 25);
    f.r(RefKind::Type, "IThing", None, Some(app), 27);
    let run_m = f.def(Method, "run", Some(app), 100, 300);
    f.def(Method, "helper", Some(app), 300, 350);
    f.call("helper", Some("this"), Some(run_m), 110);
    f.call("baseOnly", Some("this"), Some(run_m), 115);
    f.call("add", None, Some(run_m), 120);
    f.call("plus", None, Some(run_m), 125);
    f.r(RefKind::New, "Calc", None, Some(run_m), 130);
    f.call("add", Some("U"), Some(run_m), 135);
    f.call("add", Some("U.M"), Some(run_m), 136);
    f.r(RefKind::New, "Logger", None, Some(run_m), 140);
    f.call("useState", Some("React"), Some(run_m), 145);
    f.call("log", Some("console"), Some(run_m), 150);
    f.call("missingName", None, Some(run_m), 155);
    f.r(RefKind::Type, "IThing", None, Some(run_m), 160);
    f.r(RefKind::Throw, "Error", None, Some(run_m), 165);
    let _ = ithing;
    files.push(f.file("src/app.ts"));

    let mut f = F::es();
    f.imp("./app", 0, &[("App", None)], None);
    let t = f.def(Function, "it renders", None, 0, 100);
    f.d(t).is_test = true;
    f.d(t).label = Test;
    f.r(RefKind::New, "App", None, Some(t), 10);
    f.call("run", Some("app"), Some(t), 20);
    files.push(f.file("src/app.test.ts"));
    files
}

#[test]
fn ecmascript_strategies() {
    let out = run(es_project());
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // Qualified names use `:` after the module path.
    let calc = &out.graph.nodes[g.id("src/util/math:Calc") as usize];
    assert_eq!(calc.qualified_name, "src/util/math:Calc");
    g.id("src/util/math:Calc.sum");
    // Barrel: star re-export, aliased re-export, namespace re-export.
    g.has("src/app:App.run", "src/util/math:add", Calls, ImportResolved, 0.95);
    g.has("src/app:App.run", "src/util/math:Calc", Calls, ImportResolved, 0.95);
    g.has("src/app:App.run", "src/util/math:Calc.constructor", Calls, ImportResolved, 0.95 * 0.95);
    // Default import, matched by file name.
    g.has("src/app:App.run", "src/Logger:Logger", Calls, ImportResolved, 0.95);
    // Self receiver: own and inherited (through an imported base class).
    g.has("src/app:App.run", "src/app:App.helper", Calls, TypeResolved, 0.9);
    g.has("src/app:App.run", "src/base:Base.baseOnly", Calls, TypeResolved, 0.85);
    // Inherits / implements.
    g.has("src/app:App", "src/base:Base", Inherits, ImportResolved, 0.95);
    g.has("src/app:App", "src/app:IThing", Implements, SameModule, 0.9);
    g.lacks("src/app:App", "src/app:IThing", References);
    g.has("src/app:App.run", "src/app:IThing", References, SameModule, 0.9);
    // Imports.
    g.has("src/app.ts", "src/util/index.ts", Imports, ImportResolved, 1.0);
    g.has("src/util/index.ts", "src/util/math.ts", Imports, ImportResolved, 1.0);
    g.has("src/util/cyc.ts", "src/util/index.ts", Imports, ImportResolved, 1.0);
    // Tests + heuristic method (`run` is too short → no edge).
    g.has("src/app.test:it renders", "src/app:App", Tests, ImportResolved, 0.95);
    assert_eq!(g.out_count("src/app.test:it renders", Calls), 1);

    let s = &out.stats;
    // React.useState, console.log (builtin receiver).
    assert_eq!(s.references_external, 2);
    // Error.
    assert_eq!(s.references_builtin, 1);
    assert_eq!(s.resolved_by_strategy["import_resolved"], 7 + 1);
}

#[test]
fn ecmascript_namespace_receivers() {
    let out = run(es_project());
    let g = G(&out.graph);
    // `U.add()` and `U.M.add()` (namespace import of a barrel, namespace re-export).
    let run_m = g.id("src/app:App.run");
    let add = g.id("src/util/math:add");
    let e = out.graph.edges.iter().find(|e| e.src == run_m && e.dst == add && e.kind == EdgeKind::Calls).unwrap();
    // Merged with the direct import (0.95 wins); evidence is the earliest span.
    assert!((e.meta.confidence - 0.95).abs() < 1e-6);
    assert_eq!(e.meta.evidence.unwrap().1.start_byte, 120);
    // Remove the direct calls and check the namespace path alone.
    let mut files = es_project();
    let app = files.iter_mut().find(|f| f.path == "src/app.ts").unwrap();
    app.facts.as_mut().unwrap().references.retain(|r| r.receiver.as_deref().is_some_and(|x| x.starts_with('U')));
    let out = run(files);
    let g = G(&out.graph);
    g.has("src/app:App.run", "src/util/math:add", EdgeKind::Calls, Strategy::ImportResolved, 0.9);
    // U.add, U.M.add, plus `new App()` in the test file.
    assert_eq!(out.stats.references_resolved, 3);
}

#[test]
fn decorators_synthesized_from_definitions() {
    use Label::*;
    let mut f = F::py();
    f.def(Function, "deco", None, 0, 10);
    let k = f.def(Class, "K", None, 20, 100);
    let m = f.def(Method, "m", Some(k), 30, 90);
    f.d(m).decorators = vec!["deco(1, 2)".into()];
    let out = run(vec![f.file("x.py")]);
    let g = G(&out.graph);
    g.has("x.deco", "x.K.m", EdgeKind::Decorates, Strategy::SameModule, 0.9);
}

#[test]
fn deterministic_across_input_order() {
    let mut a = python_project();
    a.extend(es_project());
    let mut b = a.clone();
    b.reverse();
    b.rotate_left(3);
    let ga = serde_json::to_string(&run(a).graph).unwrap();
    let gb = serde_json::to_string(&run(b).graph).unwrap();
    assert_eq!(ga, gb);
}

#[test]
fn malformed_facts_do_not_panic() {
    use Label::*;
    let mut f = F::py();
    let a = f.def(Function, "a", None, 0, 10);
    let b = f.def(Function, "b", Some(a), 2, 8);
    f.d(a).parent = Some(b); // parent cycle
    f.d(b).bases = vec!["".into(), "foo(".into()];
    f.r(RefKind::Call, "a", None, Some(99), 1); // enclosing out of range
    f.r(RefKind::Call, "", Some("x..y"), Some(a), 1);
    f.imp("", 3, &[("z", None)], None);
    let mut dup = f.file("dup.py");
    dup.blob_sha = "1".into();
    let mut dup2 = dup.clone();
    dup2.blob_sha = "2".into();
    let out = run(vec![dup2, dup]);
    assert_eq!(out.graph.files.len(), 1);
    assert_eq!(out.graph.files[0].blob_sha, "1");
}

// ---------------------------------------------------------------------------
// Edge kinds, receivers, limits
// ---------------------------------------------------------------------------

#[test]
fn writes_reads_and_throws() {
    use Label::*;
    let mut f = F::py();
    let err = f.def(Class, "MyError", None, 0, 10);
    f.d(err).bases = vec!["Exception".into()];
    f.def(Variable, "LIMIT", None, 10, 20);
    let counter = f.def(Class, "Counter", None, 20, 200);
    f.def(Field, "count", Some(counter), 30, 40);
    let inc = f.def(Method, "inc", Some(counter), 50, 150);
    f.r(RefKind::Write, "count", Some("self"), Some(inc), 60);
    f.r(RefKind::Read, "count", Some("self"), Some(inc), 70);
    f.r(RefKind::Read, "LIMIT", None, Some(inc), 80);
    f.r(RefKind::Throw, "MyError", None, Some(inc), 90);
    // Unknown receiver: reads never use the method heuristic.
    f.r(RefKind::Read, "count", Some("other"), Some(inc), 100);
    f.r(RefKind::Write, "LIMIT", None, Some(inc), 110);
    let out = run(vec![f.file("c.py")]);
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    g.has("c.Counter.inc", "c.Counter.count", Writes, TypeResolved, 0.9);
    g.has("c.Counter.inc", "c.Counter.count", References, TypeResolved, 0.9);
    g.has("c.Counter.inc", "c.LIMIT", References, SameModule, 0.9);
    g.has("c.Counter.inc", "c.LIMIT", Writes, SameModule, 0.9);
    g.has("c.Counter.inc", "c.MyError", Throws, SameModule, 0.9);
    assert_eq!(out.stats.references_resolved, 5);
    assert_eq!(out.stats.references_total, 6);
}

#[test]
fn same_module_class_receiver_and_super() {
    use Label::*;
    let mut f = F::es();
    let base = f.def(Class, "Base", None, 0, 100);
    f.def(Method, "save", Some(base), 10, 50);
    let util = f.def(Class, "Util", None, 100, 200);
    f.def(Method, "build", Some(util), 110, 150);
    let child = f.def(Class, "Child", None, 200, 400);
    f.d(child).bases = vec!["Base".into()];
    let save2 = f.def(Method, "save", Some(child), 210, 300);
    f.call("save", Some("super"), Some(save2), 220);
    f.call("build", Some("Util"), Some(save2), 230);
    f.r(RefKind::New, "Util", None, Some(save2), 240);
    let out = run(vec![f.file("src/m.ts")]);
    let g = G(&out.graph);
    g.has("src/m:Child.save", "src/m:Base.save", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has("src/m:Child.save", "src/m:Util.build", EdgeKind::Calls, Strategy::SameModule, 0.85);
    g.has("src/m:Child.save", "src/m:Util", EdgeKind::Calls, Strategy::SameModule, 0.9);
    g.has("src/m:Child", "src/m:Base", EdgeKind::Inherits, Strategy::SameModule, 0.9);
}

#[test]
fn python_module_alias_and_submodule_receivers() {
    use Label::*;
    let mut files = Vec::new();
    files.push(F::py().file("pkg/__init__.py"));
    let mut f = F::py();
    f.def(Function, "helper", None, 0, 10);
    files.push(f.file("pkg/utils.py"));
    let mut f = F::py();
    let user = f.def(Class, "User", None, 0, 100);
    f.def(Method, "create", Some(user), 10, 20);
    files.push(f.file("pkg/models.py"));
    let mut f = F::py();
    f.imp("pkg.utils", 0, &[], Some("pu"));
    f.imp("pkg", 0, &[("models", None)], None);
    f.imp("numpy", 0, &[], Some("np"));
    let main = f.def(Function, "main", None, 0, 100);
    f.call("helper", Some("pu"), Some(main), 10);
    f.call("create", Some("models.User"), Some(main), 20);
    f.call("array", Some("np"), Some(main), 30);
    f.call("helper", Some("np.linalg"), Some(main), 40);
    files.push(f.file("app.py"));
    // A project method named like the external call must not be matched.
    let mut f = F::py();
    let k = f.def(Class, "K", None, 0, 100);
    f.def(Method, "array", Some(k), 10, 20);
    files.push(f.file("k.py"));
    let out = run(files);
    let g = G(&out.graph);
    g.has("app.main", "pkg.utils.helper", EdgeKind::Calls, Strategy::ImportResolved, 0.9);
    g.has("app.main", "pkg.models.User.create", EdgeKind::Calls, Strategy::ImportResolved, 0.85);
    g.lacks("app.main", "k.K.array", EdgeKind::Calls);
    assert_eq!(out.stats.references_external, 2);
    g.has("app.py", "pkg/utils.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has("app.py", "pkg/models.py", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
}

#[test]
fn unique_name_requires_one_compatible_candidate() {
    use Label::*;
    let mut a = F::py();
    a.def(Function, "process", None, 0, 10);
    let k = a.def(Class, "Job", None, 10, 100);
    a.def(Method, "only_method", Some(k), 20, 30);
    a.def(Method, "shared", Some(k), 30, 40);
    let outer = a.def(Function, "outer", None, 100, 200);
    a.def(Function, "nested_only", Some(outer), 110, 150);
    let mut b = F::py();
    b.def(Function, "process", None, 0, 10);
    let k2 = b.def(Class, "Task", None, 10, 100);
    b.def(Method, "shared", Some(k2), 20, 30);
    let mut c = F::py();
    let m = c.def(Function, "m", None, 0, 100);
    c.call("process", None, Some(m), 10); // two candidates
    c.call("only_method", None, Some(m), 20); // bare call never hits a method
    c.call("shared", Some("x"), Some(m), 30); // two methods
    c.call("only_method", Some("x"), Some(m), 40); // unique method
    c.call("nested_only", None, Some(m), 50); // function-local elsewhere
    // A test helper is never a unique-name target, and TESTS skips test callees.
    let t = c.def(Function, "test_it", None, 100, 200);
    c.d(t).is_test = true;
    let h = c.def(Function, "test_helper", None, 200, 300);
    c.d(h).is_test = true;
    c.call("test_helper", None, Some(t), 110);
    c.call("m", None, Some(t), 120);
    let out = run(vec![a.file("a.py"), b.file("b.py"), c.file("c.py")]);
    let g = G(&out.graph);
    assert_eq!(g.out_count("c.m", EdgeKind::Calls), 1);
    g.has("c.m", "a.Job.only_method", EdgeKind::Calls, Strategy::Heuristic, 0.5);
    g.has("c.test_it", "c.test_helper", EdgeKind::Calls, Strategy::SameModule, 0.9);
    g.lacks("c.test_it", "c.test_helper", EdgeKind::Tests);
    g.has("c.test_it", "c.m", EdgeKind::Tests, Strategy::SameModule, 0.9);
}

/// `b0 → b1 → … → bN` barrels of `export *`; `deep` is defined in the last one.
fn barrel_chain(n: usize) -> Vec<FileInput> {
    let mut files = Vec::new();
    for i in 0..n {
        let mut f = F::es();
        f.reexport(&format!("./b{}", i + 1), &[], None);
        files.push(f.file(&format!("src/b{i}.ts")));
    }
    let mut f = F::es();
    let d = f.def(Label::Function, "deep", None, 0, 10);
    f.d(d).exported = true;
    // A second definition elsewhere makes the unique-name fallback ambiguous.
    files.push(f.file(&format!("src/b{n}.ts")));
    let mut f = F::es();
    let d = f.def(Label::Function, "deep", None, 0, 10);
    f.d(d).exported = true;
    files.push(f.file("src/other.ts"));
    let mut f = F::es();
    f.imp("./b0", 0, &[("deep", None)], None);
    let m = f.def(Label::Function, "main", None, 0, 100);
    f.call("deep", None, Some(m), 10);
    files.push(f.file("src/main.ts"));
    files
}

#[test]
fn barrels_are_followed_up_to_the_depth_limit() {
    let out = run(barrel_chain(8));
    let g = G(&out.graph);
    g.has("src/main:main", "src/b8:deep", EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    // Too deep: not resolved through imports, and no unique-name guess.
    let out = run(barrel_chain(14));
    let g = G(&out.graph);
    assert_eq!(g.out_count("src/main:main", EdgeKind::Calls), 0);
    assert_eq!(out.stats.references_resolved, 0);
}

#[test]
fn ecmascript_namespace_new_and_type_only() {
    use Label::*;
    let mut f = F::es();
    let c = f.def(Class, "Calc", None, 0, 100);
    f.d(c).exported = true;
    f.def(Method, "constructor", Some(c), 10, 20);
    let t = f.def(Interface, "Shape", None, 100, 150);
    f.d(t).exported = true;
    let lib = f;
    let mut f = F::es();
    f.imp("./lib", 0, &[], Some("L"));
    f.imp("./lib", 0, &[("Shape", None)], None).type_only = true;
    let m = f.def(Label::Function, "main", None, 0, 100);
    f.r(RefKind::New, "Calc", Some("L"), Some(m), 10);
    f.r(RefKind::Type, "Shape", None, Some(m), 20);
    f.r(RefKind::Type, "Shape", Some("L"), Some(m), 30);
    let out = run(vec![lib.file("src/lib.ts"), f.file("src/main.ts")]);
    let g = G(&out.graph);
    g.has("src/main:main", "src/lib:Calc", EdgeKind::Calls, Strategy::ImportResolved, 0.9);
    g.has("src/main:main", "src/lib:Calc.constructor", EdgeKind::Calls, Strategy::ImportResolved, 0.9 * 0.95);
    g.has("src/main:main", "src/lib:Shape", EdgeKind::References, Strategy::ImportResolved, 0.95);
    g.has("src/main.ts", "src/lib.ts", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
}

/// Deterministic Fisher–Yates shuffle (no `rand` dependency).
fn shuffle<T>(v: &mut [T], mut seed: u64) {
    for i in (1..v.len()).rev() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let j = (seed >> 33) as usize % (i + 1);
        v.swap(i, j);
    }
}

#[test]
fn shuffle_invariance() {
    let mut base = python_project();
    base.extend(es_project());
    base.extend(barrel_chain(3).into_iter().map(|mut f| {
        f.path = format!("chain/{}", f.path);
        f
    }));
    let expected = serde_json::to_string(&run(base.clone()).graph).unwrap();
    let stats = format!("{:?}", run(base.clone()).stats);
    for seed in 1..=8 {
        let mut files = base.clone();
        shuffle(&mut files, seed);
        let out = run(files);
        assert_eq!(serde_json::to_string(&out.graph).unwrap(), expected, "seed {seed}");
        assert_eq!(format!("{:?}", out.stats), stats, "seed {seed}");
    }
}

#[test]
fn ecmascript_export_of_imported_name() {
    use Label::*;
    let mut files = Vec::new();
    let mut f = F::es();
    let h = f.def(Class, "Hono", None, 0, 10);
    f.d(h).exported = true;
    files.push(f.file("src/hono.ts"));
    // index.ts: `import { Hono } from './hono'; export { Hono }`.
    let mut f = F::es();
    f.imp("./hono", 0, &[("Hono", None)], None);
    files.push(f.file("src/index.ts"));
    // A star barrel over a module that only *imports* `Other` must not leak
    // it; the second star branch exports the real one.
    let mut f = F::es();
    f.imp("./hono", 0, &[("Hono", Some("Other"))], None);
    files.push(f.file("src/a.ts"));
    let mut f = F::es();
    let o = f.def(Class, "Other", None, 0, 10);
    f.d(o).exported = true;
    files.push(f.file("src/b.ts"));
    let mut f = F::es();
    f.reexport("./a", &[("*", None)], None);
    f.reexport("./b", &[("*", None)], None);
    files.push(f.file("src/barrel.ts"));
    let mut f = F::es();
    f.imp("./index", 0, &[("Hono", None)], None);
    f.imp("./barrel", 0, &[("Other", None)], None);
    let m = f.def(Function, "main", None, 0, 100);
    f.r(RefKind::New, "Hono", None, Some(m), 10);
    f.r(RefKind::New, "Other", None, Some(m), 20);
    files.push(f.file("src/main.ts"));
    let out = run(files);
    let g = G(&out.graph);
    g.has("src/main:main", "src/hono:Hono", EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.has("src/main:main", "src/b:Other", EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    assert_eq!(g.out_count("src/main:main", EdgeKind::Calls), 2);
}
