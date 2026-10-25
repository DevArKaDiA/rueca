//! Extraction tests, one group per row of the construct tables in the crate
//! docs. Module and import resolution live in `modules.rs`, scopes and type
//! inference in `locals.rs`; cross-file resolution is tested end to end in
//! `rueca-resolve/tests/go.rs`.

use super::*;
use rueca_model::{Definition, RefKind, Reference};

fn facts_at(path: &str, src: &str) -> FileFacts {
    GoPack::default().extract(path, Language::Go, src.as_bytes(), &Budget::default()).unwrap()
}

fn facts(src: &str) -> FileFacts {
    facts_at("x.go", src)
}

fn def<'a>(f: &'a FileFacts, qn: &str) -> &'a Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| {
        let all: Vec<&str> = f.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
        panic!("no definition {qn}; have {all:#?}")
    })
}

fn ref_text(r: &Reference) -> String {
    match &r.receiver {
        Some(recv) => format!("{recv} {}", r.name),
        None => r.name.clone(),
    }
}

fn enc_name(f: &FileFacts, r: &Reference) -> String {
    r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone()).unwrap_or_default()
}

/// References as `(kind, receiver + name, enclosing qualified name)`.
fn refs(f: &FileFacts) -> Vec<(RefKind, String, String)> {
    f.references.iter().map(|r| (r.kind, ref_text(r), enc_name(f, r))).collect()
}

#[track_caller]
fn assert_refs(f: &FileFacts, expected: &[(RefKind, &str, &str)]) {
    let got = refs(f);
    for (k, t, e) in expected {
        assert!(
            got.iter().any(|(gk, gt, ge)| gk == k && gt == t && ge == e),
            "missing {k:?} `{t}` in `{e}`; got {got:#?}"
        );
    }
}

#[track_caller]
fn assert_no_ref(f: &FileFacts, text: &str) {
    let got = refs(f);
    assert!(!got.iter().any(|(_, t, _)| t == text), "unexpected `{text}`; got {got:#?}");
}

fn summary(f: &FileFacts) -> Vec<(Label, &str, Option<&str>)> {
    f.definitions
        .iter()
        .map(|d| {
            (d.label, d.qualified_name.as_str(), d.parent.map(|p| f.definitions[p as usize].qualified_name.as_str()))
        })
        .collect()
}

macro_rules! fixture {
    ($path:literal) => {
        include_str!(concat!("../tests/fixtures/shop/", $path))
    };
}

const MAIN: &str = fixture!("app/main.go");
const SERVER: &str = fixture!("app/server/server.go");
const HANDLERS: &str = fixture!("app/server/handlers.go");
const LOGGER: &str = fixture!("app/server/logger.go");
const TESTS: &str = fixture!("app/server/server_test.go");
const EXT_TESTS: &str = fixture!("app/server/server_ext_test.go");
const STORE: &str = fixture!("app/internal/store/store.go");
const MONEY: &str = fixture!("lib/money/money.go");

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

#[test]
fn functions_methods_and_receivers() {
    let f = facts(SERVER);
    let new = def(&f, "NewServer");
    assert_eq!(new.label, Label::Function);
    assert_eq!(new.signature.as_deref(), Some("func NewServer(st *store.Store) *Server"));
    assert_eq!(new.doc.as_deref(), Some("NewServer returns a server over st."));
    assert!(new.exported && !new.is_test && new.parent.is_none());
    // Pointer receiver: a top-level method named `T.M`.
    let start = def(&f, "Server.Start");
    assert_eq!((start.label, start.name.as_str(), start.parent), (Label::Method, "Start", None));
    assert_eq!(start.signature.as_deref(), Some("func (s *Server) Start() error"));
    assert!(start.span.contains(&start.name_span));
    // Value receiver and generic receivers.
    let m = facts(MONEY);
    assert_eq!(def(&m, "Amount.String").label, Label::Method);
    assert_eq!(def(&m, "Amount").label, Label::TypeAlias);
    let s = facts(STORE);
    assert_eq!(def(&s, "Cache.Put").signature.as_deref(), Some("func (c *Cache[K, V]) Put(k K, v V)"));
    // Unnamed and unexported receivers.
    let f = facts("package p\n\ntype t struct{}\n\nfunc (*t) m() {}\nfunc (t) n() {}\n");
    assert!(!def(&f, "t.m").exported);
    def(&f, "t.n");
}

#[test]
fn structs_fields_and_embedding() {
    let f = facts(SERVER);
    let s = def(&f, "Server");
    assert_eq!(s.label, Label::Struct);
    assert_eq!(s.signature.as_deref(), Some("type Server struct"));
    assert_eq!(s.bases, vec!["Base".to_string()]);
    assert_eq!(
        summary(&f)[..5],
        [
            (Label::Struct, "Server", None),
            (Label::Field, "Server.Base", Some("Server")),
            (Label::Field, "Server.store", Some("Server")),
            (Label::Field, "Server.name", Some("Server")),
            (Label::Field, "Server.Hits", Some("Server")),
        ]
    );
    assert_eq!(def(&f, "Server.store").signature.as_deref(), Some("store *store.Store"));
    assert!(!def(&f, "Server.store").exported && def(&f, "Server.Hits").exported);
    // The trailing comment of `name` is not the doc of `Hits`; `Hits` has its own.
    assert_eq!(def(&f, "Server.name").doc, None);
    assert_eq!(def(&f, "Server.Hits").doc.as_deref(), Some("Hits counts handled requests."));
    // Embedded pointer, qualified and generic types; several names per field.
    let f = facts("package p\n\ntype S struct {\n\t*Base\n\tpkg.Other\n\tList[int]\n\ta, b int `json:\"a\"`\n}\n");
    assert_eq!(def(&f, "S").bases, vec!["Base", "pkg.Other", "List[int]"]);
    for qn in ["S.Base", "S.Other", "S.List", "S.a", "S.b"] {
        assert_eq!(def(&f, qn).label, Label::Field);
    }
    assert_eq!(def(&f, "S.a").signature.as_deref(), Some("a, b int"));
    // Embedded types are bases, not type references; generic arguments are.
    assert_no_ref(&f, "pkg Other");
    assert_refs(&f, &[]);
}

#[test]
fn interfaces_embedding_and_constraints() {
    let f = facts(LOGGER);
    assert_eq!(def(&f, "Logger").label, Label::Interface);
    let log = def(&f, "Logger.Log");
    assert_eq!((log.label, log.signature.as_deref()), (Label::Method, Some("Log(msg string)")));
    assert_eq!(log.parent, Some(0));
    assert_eq!(def(&f, "ReadLogger").bases, vec!["Logger".to_string()]);
    // Unions and `~T` are constraints: no base.
    assert!(def(&f, "Number").bases.is_empty());
    let f =
        facts("package p\n\ntype RW interface {\n\tio.Reader\n\tcomparable\n\tWrite(p []byte) (n int, err error)\n}\n");
    assert_eq!(def(&f, "RW").bases, vec!["io.Reader".to_string()]);
    assert_no_ref(&f, "byte");
}

#[test]
fn type_declarations_and_generics() {
    let f = facts(
        "package p\n\ntype (\n\t// HandlerFunc handles.\n\tHandlerFunc func(*Context)\n\tChain []HandlerFunc\n)\n\ntype Alias = Chain\n\ntype List[T any] struct {\n\titems []T\n\tnext  *List[T]\n}\n\nfunc Map[T, U any](xs []T, f func(T) U) []U { var u U; _ = u; return nil }\n",
    );
    assert_eq!(
        summary(&f),
        vec![
            (Label::TypeAlias, "HandlerFunc", None),
            (Label::TypeAlias, "Chain", None),
            (Label::TypeAlias, "Alias", None),
            (Label::Struct, "List", None),
            (Label::Field, "List.items", Some("List")),
            (Label::Field, "List.next", Some("List")),
            (Label::Function, "Map", None),
        ]
    );
    assert_eq!(def(&f, "HandlerFunc").doc.as_deref(), Some("HandlerFunc handles."));
    // An alias lists its target as a base (its members are the target's);
    // a defined type does not.
    assert_eq!(def(&f, "Alias").bases, vec!["Chain".to_string()]);
    assert!(def(&f, "Chain").bases.is_empty());
    assert_eq!(def(&f, "HandlerFunc").signature.as_deref(), Some("type HandlerFunc func(*Context)"));
    assert_eq!(def(&f, "List").signature.as_deref(), Some("type List[T any] struct"));
    assert_refs(
        &f,
        &[
            (RefKind::Type, "Context", "HandlerFunc"),
            (RefKind::Type, "HandlerFunc", "Chain"),
            (RefKind::Type, "Chain", "Alias"),
            (RefKind::Type, "List", "List.next"),
        ],
    );
    // Type parameters are never references.
    for tp in ["T", "U"] {
        assert_no_ref(&f, tp);
    }
}

#[test]
fn constants_and_variables() {
    let f = facts(
        "package p\n\n// Limits.\nconst (\n\t// Max is the maximum.\n\tMax = 10\n\tmin = iota\n)\n\nvar ErrX = errors.New(\"x\")\n\nvar (\n\ta, b = f(), g()\n\t_    = h()\n)\n",
    );
    assert_eq!(
        summary(&f),
        vec![
            (Label::Constant, "Max", None),
            (Label::Constant, "min", None),
            (Label::Variable, "ErrX", None),
            (Label::Variable, "a", None),
            (Label::Variable, "b", None),
        ]
    );
    assert_eq!(def(&f, "Max").doc.as_deref(), Some("Max is the maximum."));
    assert_eq!(def(&f, "Max").signature.as_deref(), Some("const Max = 10"));
    assert_eq!(def(&f, "ErrX").signature.as_deref(), Some("var ErrX = errors.New(\"x\")"));
    assert!(def(&f, "ErrX").exported && !def(&f, "min").exported);
    assert_refs(
        &f,
        &[
            (RefKind::Call, "errors New", "ErrX"),
            (RefKind::Call, "f", "a"),
            (RefKind::Call, "g", "a"),
            (RefKind::Call, "h", ""),
        ],
    );
}

#[test]
fn tests_and_external_test_packages() {
    let f = facts_at("server/server_test.go", TESTS);
    assert!(def(&f, "TestStart").is_test && def(&f, "BenchmarkStart").is_test);
    assert!(!def(&f, "newFixture").is_test && !def(&f, "Testlowercase").is_test);
    // The same names outside `_test.go` are not tests.
    let f = facts_at("server/server.go", TESTS);
    assert!(!def(&f, "TestStart").is_test);
    // External test package: a `Package` definition marks the module.
    let f = facts_at("server/server_ext_test.go", EXT_TESTS);
    let p = def(&f, "server_test");
    assert_eq!((p.label, p.is_test, p.exported), (Label::Package, true, false));
    assert!(def(&f, "ExampleServer").is_test && def(&f, "TestExternal").is_test);
    let project = ProjectFiles {
        paths: ["go.mod", "server/server_ext_test.go"].into_iter().map(String::from).collect(),
        configs: [("go.mod".to_string(), "module example.com/shop\n".to_string())].into(),
    };
    let pack = GoPack::default();
    assert_eq!(pack.module_name_for("server/server_ext_test.go", Some(&f), &project), "example.com/shop/server_test");
    assert_eq!(pack.module_name_for("server/server_test.go", Some(&facts(TESTS)), &project), "example.com/shop/server");
    for (name, test) in [
        ("Test", true),
        ("TestX", true),
        ("Test_x", true),
        ("TestMain", true),
        ("Testx", false),
        ("Example", true),
        ("ExampleT_M", true),
        ("Fuzz", true),
        ("FuzzParse", true),
        ("BenchmarkX", true),
        ("Tester", false),
    ] {
        assert_eq!(extract::is_test_name(name), test, "{name}");
    }
}

#[test]
fn doc_comments() {
    let f = facts(
        "// Package p does things.\npackage p\n\n// Run runs.\n//\n// It blocks.\n//go:noinline\nfunc Run() {}\n\n// Detached.\n\nfunc Detached() {}\n\n/*\n * Block docs.\n */\nfunc Block() {}\n",
    );
    assert_eq!(def(&f, "Run").doc.as_deref(), Some("Run runs.\n\nIt blocks."));
    assert_eq!(def(&f, "Detached").doc, None);
    assert_eq!(def(&f, "Block").doc.as_deref(), Some("Block docs."));
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[test]
fn calls_with_receivers_and_arity() {
    let f = facts(MAIN);
    let r = |name: &str| f.references.iter().find(|r| r.name == name).unwrap();
    assert_eq!(r("Clamp").arity, Some(3));
    assert_eq!(r("Start").arity, Some(0));
    assert_refs(
        &f,
        &[
            (RefKind::Call, "server NewServer", "defaultServer"),
            (RefKind::Call, "server NewServer", "main"),
            (RefKind::Call, "store New", "main"),
            (RefKind::Call, "$type:server.Server Start", "main"),
            (RefKind::Call, "$type:server.Server Handle", "main"),
            (RefKind::Call, "money Parse", "main"),
            (RefKind::Call, "Clamp", "main"),
            (RefKind::Call, "term Width", "main"),
            (RefKind::Call, "j Marshal", "main"),
            (RefKind::Type, "embed FS", "static"),
            (RefKind::Read, "Version", "main"),
            (RefKind::Read, "static", "main"),
        ],
    );
    // Locals are not references.
    for local in ["srv", "s", "price", "dsn", "port", "b"] {
        assert_no_ref(&f, local);
    }
}

#[test]
fn receiver_typing() {
    let f = facts(
        "package p\n\nimport \"net/url\"\n\nvar def = &Client{}\n\ntype Client struct {\n\tpool *Pool\n\tlog  log.Logger\n}\n\nfunc mk() *Maker { return nil }\n\nfunc (c *Client) Do(r *http.Request, opts ...Option) {\n\tc.pool.Get()\n\tc.log.Print()\n\tc.Close()\n\tr.Clone()\n\tdef.Close()\n\tx := mk()\n\tx.Make()\n\tvar w Writer\n\tw.Write()\n\ty := NewBuffer()\n\ty.Reset()\n\tz := pkg.NewThing()\n\tz.Run()\n\tq := new(Queue)\n\tq.Push()\n\tv := any(r).(*Valve)\n\tv.Open()\n\tswitch t := any(r).(type) {\n\tcase *Tap:\n\t\tt.Turn()\n\tcase *A, *B:\n\t\tt.Both()\n\t}\n\turl := r.URL\n\turl.Query()\n\tunknown := get()\n\tunknown.Foo()\n\tget().Bar()\n\tMaker{}.Make()\n\tfunc(k *Key) { k.Lock() }(nil)\n}\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::Call, "$type:Pool Get", "Client.Do"),
            (RefKind::Call, "$type:log.Logger Print", "Client.Do"),
            (RefKind::Call, "$self Close", "Client.Do"),
            (RefKind::Call, "$type:http.Request Clone", "Client.Do"),
            (RefKind::Call, "$type:Client Close", "Client.Do"),
            (RefKind::Call, "$type:Maker Make", "Client.Do"),
            (RefKind::Call, "$type:Writer Write", "Client.Do"),
            (RefKind::Call, "$type:Buffer Reset", "Client.Do"),
            (RefKind::Call, "$type:pkg.Thing Run", "Client.Do"),
            (RefKind::Call, "$type:Queue Push", "Client.Do"),
            (RefKind::Call, "$type:Valve Open", "Client.Do"),
            (RefKind::Call, "$type:Tap Turn", "Client.Do"),
            (RefKind::Call, "t Both", "Client.Do"),
            // An untyped local named like an import never binds to it.
            (RefKind::Call, "(url) Query", "Client.Do"),
            (RefKind::Call, "unknown Foo", "Client.Do"),
            (RefKind::Call, "get() Bar", "Client.Do"),
            (RefKind::Call, "$type:Maker Make", "Client.Do"),
            (RefKind::Call, "$type:Key Lock", "Client.Do"),
            (RefKind::Read, "$self pool", "Client.Do"),
            (RefKind::Type, "Option", "Client.Do"),
        ],
    );
}

#[test]
fn receiver_chains_and_conventions() {
    let f = facts(
        "package p\n\nimport \"html/template\"\n\ntype Context struct {\n\tRequest *http.Request\n\tparams  Params\n}\n\ntype Params struct{ list *List }\n\nfunc (c *Context) Get() {\n\tc.Request.Header.Set(\"a\", \"b\")\n\tc.params.list.Push()\n\tc.params.other.Pop()\n\tw := httptest.NewRecorder()\n\tw.Header().Get(\"x\")\n\tw.Body.String()\n\t_, err := run()\n\terr.Error()\n\tvar e error\n\te.Error()\n\ttemplate.New(\"t\").Parse(\"x\")\n\tr := require.New(t)\n\tr.Equal(1, 2)\n\ttype local struct{}\n\t_ = local{}\n\tvar l local\n\t_ = l\n}\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::Call, "http.Request.Header Set", "Context.Get"),
            (RefKind::Call, "$type:List Push", "Context.Get"),
            (RefKind::Call, "c.params.other Pop", "Context.Get"),
            (RefKind::Call, "httptest.Recorder.Header Get", "Context.Get"),
            (RefKind::Call, "httptest.Recorder.Body String", "Context.Get"),
            (RefKind::Call, "$type:error Error", "Context.Get"),
            (RefKind::Call, "template.New Parse", "Context.Get"),
            (RefKind::Call, "require.New Equal", "Context.Get"),
        ],
    );
    // Local types are neither constructed nor referenced as package names.
    assert_no_ref(&f, "local");
}

#[test]
fn composite_literals_types_reads_and_writes() {
    let f = facts(
        "package p\n\nvar count int\n\nfunc (s *S) f(m map[string]Value) []Item {\n\tx := T{Name: name, Other: s}\n\ty := &pkg.U{1, 2}\n\tz := map[Key]Val{k: v}\n\tl := List[Elem]{}\n\t_ = []Elem{}\n\tcount++\n\tcount = 2\n\ts.n = 1\n\tconv := Celsius(3)\n\t_ = new(Box)\n\t_ = make([]Item, 0)\n\tr.GET(\"/\", handler)\n\t_, _, _, _, _ = x, y, z, l, conv\n\treturn nil\n}\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::New, "T", "S.f"),
            (RefKind::New, "pkg U", "S.f"),
            (RefKind::New, "List", "S.f"),
            (RefKind::Type, "Elem", "S.f"),
            (RefKind::Type, "Key", "S.f"),
            (RefKind::Type, "Val", "S.f"),
            (RefKind::Type, "Value", "S.f"),
            (RefKind::Type, "Item", "S.f"),
            (RefKind::Type, "Box", "S.f"),
            (RefKind::Read, "name", "S.f"),
            (RefKind::Read, "k", "S.f"),
            (RefKind::Read, "v", "S.f"),
            (RefKind::Read, "handler", "S.f"),
            (RefKind::Write, "count", "S.f"),
            (RefKind::Write, "$self n", "S.f"),
            (RefKind::Call, "Celsius", "S.f"),
        ],
    );
    // Struct literal keys are field names.
    assert_no_ref(&f, "Name");
    assert_no_ref(&f, "Other");
    // Locals and receivers are not reads.
    assert_no_ref(&f, "s");
    assert_no_ref(&f, "m");
}

#[test]
fn bridge_env_vars_and_viper_keys() {
    let f = facts(MAIN);
    assert_refs(
        &f,
        &[(RefKind::EnvVar, "DATABASE_URL", "main"), (RefKind::ConfigKey, "viper:shop server.port", "main")],
    );
    // Without `SetConfigName`, the namespace is `viper`; an aliased `os` works;
    // a local named `os` or a non-literal name does not count.
    let f = facts(
        "package p\n\nimport (\n\tsys \"os\"\n\tv \"github.com/spf13/viper\"\n)\n\nfunc f(name string) {\n\tsys.LookupEnv(`HOME`)\n\tsys.Getenv(name)\n\tv.GetInt(\"db.pool\")\n\tos := fake{}\n\tos.Getenv(\"NOPE\")\n}\n",
    );
    assert_refs(&f, &[(RefKind::EnvVar, "HOME", "f"), (RefKind::ConfigKey, "viper db.pool", "f")]);
    assert_eq!(f.references.iter().filter(|r| r.kind == RefKind::EnvVar).count(), 1);
}

// ---------------------------------------------------------------------------
// Imports
// ---------------------------------------------------------------------------

#[test]
fn imports_aliases_dot_and_blank() {
    let f = facts(MAIN);
    let imp = |m: &str| f.imports.iter().find(|i| i.module == m).unwrap_or_else(|| panic!("no import {m}"));
    assert_eq!(imp("fmt").module_alias.as_deref(), Some("fmt"));
    assert_eq!(imp("encoding/json").module_alias.as_deref(), Some("j"));
    assert_eq!(imp("example.com/shop/util").names[0].name, "*");
    assert_eq!(imp("example.com/shop/util").module_alias, None);
    // Package `term` in directory `termx`: re-bound from its use.
    assert_eq!(imp("example.com/shop/pkg/termx").module_alias.as_deref(), Some("term"));
    let f = facts(
        "package p\n\nimport (\n\t_ \"embed\"\n\t\"github.com/json-iterator/go\"\n\t\"gopkg.in/yaml.v3\"\n)\n\nvar x = jsoniter.Config{}\nvar y = yaml.Node{}\n",
    );
    let imp = |m: &str| f.imports.iter().find(|i| i.module == m).unwrap();
    assert!(imp("embed").names.is_empty() && imp("embed").module_alias.is_none());
    assert_eq!(imp("github.com/json-iterator/go").module_alias.as_deref(), Some("jsoniter"));
    assert_eq!(imp("gopkg.in/yaml.v3").module_alias.as_deref(), Some("yaml"));
    assert!(f.imports.iter().all(|i| i.relative_level == 0 && !i.reexport && !i.type_only));
}

#[test]
fn import_resolution() {
    let pack = GoPack::default();
    let paths = ["go.mod", "a.go", "internal/db/db.go", "cmd/x/main.go", "docs/readme.md", "vendor/v/v.go"];
    let project = ProjectFiles {
        paths: paths.into_iter().map(String::from).collect(),
        configs: [("go.mod".to_string(), "module github.com/o/r\n".to_string())].into(),
    };
    let imp = |m: &str| Import {
        module: m.into(),
        relative_level: 0,
        names: vec![],
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Default::default(),
    };
    let r = |m: &str| pack.resolve_import("cmd/x/main.go", &imp(m), &project);
    assert_eq!(r("github.com/o/r").as_deref(), Some(""));
    assert_eq!(r("github.com/o/r/internal/db").as_deref(), Some("internal/db"));
    assert_eq!(r("github.com/o/r/docs"), None, "no .go files");
    assert_eq!(r("github.com/o/r/missing"), None);
    assert_eq!(r("fmt"), None);
    assert_eq!(r("github.com/other/x"), None);
    assert_eq!(pack.module_name("cmd/x/main.go", &project), "github.com/o/r/cmd/x");
    assert_eq!(pack.module_name("a.go", &project), "github.com/o/r");
    assert!(pack.is_builtin("len") && pack.is_builtin("error") && !pack.is_builtin("Len"));
    assert_eq!(pack.self_receivers(), &["$self"]);
    assert_eq!(pack.module_scope(Language::Go), ModuleScope::Package);
    assert!(pack.facts_depend_on_path());
}

// ---------------------------------------------------------------------------
// Robustness and determinism
// ---------------------------------------------------------------------------

#[test]
fn every_fixture_parses_cleanly_and_deterministically() {
    for (path, src) in [
        ("app/main.go", MAIN),
        ("app/server/server.go", SERVER),
        ("app/server/handlers.go", HANDLERS),
        ("app/server/logger.go", LOGGER),
        ("app/server/server_test.go", TESTS),
        ("app/server/server_ext_test.go", EXT_TESTS),
        ("app/internal/store/store.go", STORE),
        ("lib/money/money.go", MONEY),
    ] {
        let a = facts_at(path, src);
        assert_eq!(a.parse_errors, 0, "{path}");
        assert_eq!(a, facts_at(path, src), "{path}");
        for d in &a.definitions {
            assert!(d.span.contains(&d.name_span), "{path}: {}", d.qualified_name);
            if let Some(p) = d.parent {
                assert!((p as usize) < a.definitions.len());
            }
        }
        for r in &a.references {
            if let Some(e) = r.enclosing {
                assert!((e as usize) < a.definitions.len());
            }
        }
    }
}

#[test]
fn broken_and_hostile_input_never_panics() {
    let pack = GoPack::default();
    let cases: Vec<String> = vec![
        String::new(),
        "package".into(),
        "func (".into(),
        "package p\nfunc (s *) M() {}\ntype = struct\nvar ( x = \n".into(),
        "package p\nimport (\n\t\"unterminated\n".into(),
        format!("package p\nfunc f() {{ x := {}1{} }}\n", "(".repeat(2000), ")".repeat(2000)),
        format!("package p\nvar x = {}\n", "[]".repeat(3000) + "int{}"),
        "package p\n\u{0}\u{ff}\u{fe}func f() {}\n".into(),
    ];
    for src in &cases {
        let f = pack.extract("x.go", Language::Go, src.as_bytes(), &Budget::default());
        if let Ok(f) = f {
            assert!(f.references.len() <= MAX_REFS);
        }
    }
    // Wrong language and budgets.
    assert!(pack.extract("x.go", Language::Python, b"", &Budget::default()).is_err());
    let tiny = Budget { max_bytes: 4, ..Budget::default() };
    assert!(matches!(pack.extract("x.go", Language::Go, b"package p", &tiny), Err(ExtractError::TooLarge { .. })));
    let shallow = Budget { max_depth: 3, ..Budget::default() };
    pack.extract("x.go", Language::Go, MAIN.as_bytes(), &shallow).unwrap();
}

#[test]
fn reference_cap() {
    let mut src = String::from("package p\n\nfunc f() {\n");
    for i in 0..(MAX_REFS + 100) {
        src.push_str(&format!("\tg{i}()\n"));
    }
    src.push_str("}\n");
    let f = facts(&src);
    assert_eq!(f.references.len(), MAX_REFS);
}
