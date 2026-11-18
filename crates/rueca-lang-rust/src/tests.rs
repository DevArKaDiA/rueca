//! Extraction tests, one group per row of the construct tables in the crate
//! docs. The module model lives in `modules.rs`, scopes in `locals.rs`;
//! cross-file resolution is tested end to end in
//! `rueca-resolve/tests/rust.rs`.

use super::*;
use rueca_model::{Definition, Label, RefKind, Reference};

fn facts_at(path: &str, src: &str) -> FileFacts {
    RustPack::default().extract(path, Language::Rust, src.as_bytes(), &Budget::default()).unwrap()
}

fn facts(src: &str) -> FileFacts {
    facts_at("src/x.rs", src)
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

const MODEL: &str = fixture!("crates/core/src/model.rs");
const LIB: &str = fixture!("crates/core/src/lib.rs");
const MEMORY: &str = fixture!("crates/core/src/store/memory.rs");
const MAIN: &str = fixture!("crates/app/src/main.rs");

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

#[test]
fn functions_nested_functions_and_inline_modules() {
    let f = facts(
        "/// Adds.\npub fn add(a: u8) -> u8 {\n    fn twice(x: u8) -> u8 { x * 2 }\n    twice(a)\n}\nmod m {\n    pub(crate) fn f() {}\n    mod n { fn g() {} }\n}\n",
    );
    assert_eq!(
        summary(&f),
        [
            (Label::Function, "add", None),
            (Label::Function, "add.twice", Some("add")),
            (Label::Module, "m", None),
            (Label::Function, "m.f", Some("m")),
            (Label::Module, "m.n", Some("m")),
            (Label::Function, "m.n.g", Some("m.n")),
        ]
    );
    let add = def(&f, "add");
    assert_eq!(add.signature.as_deref(), Some("pub fn add(a: u8) -> u8"));
    assert_eq!(add.doc.as_deref(), Some("Adds."));
    assert!(add.exported && !def(&f, "add.twice").exported && def(&f, "m.f").exported);
    assert!(add.span.contains(&add.name_span));
    // A nested function is called through local scope.
    assert_refs(&f, &[(RefKind::Call, "twice", "add")]);
}

#[test]
fn impl_items_are_named_after_their_type() {
    let f = facts(
        "impl<T: Clone> Vec<T> { pub fn first(&self) {} }\nimpl<'a> std::fmt::Display for &Foo<'a> { fn fmt(&self) {} }\nmod m { impl Display for Foo { fn fmt(&self) {} const K: u8 = 1; type Out = u8; } }\nimpl !Send for Bar {}\n",
    );
    let first = def(&f, "Vec.first");
    assert_eq!((first.label, first.parent, first.bases.clone()), (Label::Method, None, vec!["Vec".to_string()]));
    assert!(first.implements.is_empty() && first.exported);
    let fmt = def(&f, "Foo.fmt");
    assert_eq!(
        (fmt.bases.clone(), fmt.implements.clone()),
        (vec!["Foo".to_string()], vec!["std.fmt.Display".to_string()])
    );
    // Trait impl items are exported; inside a module, prefixed and parented.
    assert!(fmt.exported);
    let inner = def(&f, "m.Foo.fmt");
    assert_eq!(inner.parent.map(|p| f.definitions[p as usize].qualified_name.as_str()), Some("m"));
    assert_eq!(def(&f, "m.Foo.K").label, Label::Constant);
    // Associated types of impls and negative impls make no definitions.
    assert!(f.definitions.iter().all(|d| d.name != "Out" && !d.qualified_name.starts_with("Bar")));
    // The self type and the trait of the header are references.
    assert_refs(&f, &[(RefKind::Type, "Foo", ""), (RefKind::Type, "std.fmt Display", "")]);
}

#[test]
fn types_traits_and_values() {
    let f = facts(MODEL);
    assert_eq!(def(&f, "Item").label, Label::Struct);
    assert_eq!(def(&f, "Item").doc.as_deref(), Some("An item for sale."));
    assert_eq!(def(&f, "Item.price").label, Label::Field);
    assert_eq!(def(&f, "Item.price").signature.as_deref(), Some("pub price: Price"));
    assert_eq!(def(&f, "Price").signature.as_deref(), Some("pub struct Price(u64)"));
    let f = facts(
        "pub enum E { A, B(u32), C { x: S } }\nunion U { a: u32 }\npub trait Tr: Base + Send { fn m(&self) -> u8; fn d(&self) {} type Item; const K: u8; }\ntype Alias = crate::x::Map<u8>;\nconst MAX: u32 = 3;\nstatic mut COUNT: u32 = 0;\nmacro_rules! twice { ($e:expr) => { $e * 2 } }\nextern \"C\" { fn ext(x: i32); }\n",
    );
    let labels: Vec<(Label, &str)> = f.definitions.iter().map(|d| (d.label, d.qualified_name.as_str())).collect();
    for want in [
        (Label::Enum, "E"),
        (Label::Constant, "E.A"),
        (Label::Constant, "E.B"),
        (Label::Constant, "E.C"),
        (Label::Struct, "U"),
        (Label::Field, "U.a"),
        (Label::Interface, "Tr"),
        (Label::Method, "Tr.m"),
        (Label::Method, "Tr.d"),
        (Label::TypeAlias, "Tr.Item"),
        (Label::Constant, "Tr.K"),
        (Label::TypeAlias, "Alias"),
        (Label::Constant, "MAX"),
        (Label::Variable, "COUNT"),
        (Label::Function, "twice"),
        (Label::Function, "ext"),
    ] {
        assert!(labels.contains(&want), "{want:?} not in {labels:#?}");
    }
    assert_eq!(def(&f, "Tr").bases, ["Base", "Send"]);
    assert!(def(&f, "Tr.m").exported && def(&f, "E.B").exported);
    assert_eq!(def(&f, "Alias").bases, ["crate.x.Map"]);
    assert_eq!(def(&f, "twice").signature.as_deref(), Some("macro_rules! twice"));
    // Variant field types are references of the variant.
    assert_refs(&f, &[(RefKind::Type, "S", "E.C")]);
}

#[test]
fn tests_are_flagged() {
    let f = facts(MEMORY);
    assert!(def(&f, "tests").is_test && def(&f, "tests.sample").is_test && def(&f, "tests.saves").is_test);
    assert!(!def(&f, "MemoryStore.new").is_test);
    let f = facts(
        "#[tokio::test]\nasync fn a() {}\n#[test_case(1)]\nfn b() {}\nfn c() {}\n#[cfg(all(test, unix))]\nmod t { fn d() {} }\n#[cfg(not(test))]\nmod u { fn e() {} }\n",
    );
    assert!(def(&f, "a").is_test && def(&f, "b").is_test && def(&f, "t.d").is_test);
    assert!(!def(&f, "c").is_test && !def(&f, "u.e").is_test);
    // Every definition of an integration test or bench file.
    let f = facts_at("tests/it.rs", "fn helper() {}\n");
    assert!(def(&f, "helper").is_test);
    let f = facts_at("crates/x/benches/b.rs", "fn helper() {}\n");
    assert!(def(&f, "helper").is_test);
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[test]
fn calls_paths_and_constructors() {
    let f = facts(
        "fn f<T: Tr>(s: S) {\n    g();\n    a::b::h();\n    crate::util::trace();\n    super::up();\n    self::own();\n    T::new();\n    Self::assoc();\n    let p = Point(1, 2);\n    let e = E::B(3);\n    let s = S { a: g(), ..S::default() };\n    let v = <Vec<u8>>::new();\n    let w = <T as Tr>::make();\n    let z = <Widget as Tr>::make();\n    std::mem::drop(s);\n    Some(1);\n}\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::Call, "g", "f"),
            (RefKind::Call, "a.b h", "f"),
            (RefKind::Call, "crate.util trace", "f"),
            (RefKind::Call, "super$1 up", "f"),
            (RefKind::Call, "own", "f"),
            (RefKind::Call, "$type:Tr new", "f"),
            (RefKind::Call, "$self assoc", "f"),
            // Tuple structs and variants are constructed, not called.
            (RefKind::Read, "Point", "f"),
            (RefKind::Read, "E B", "f"),
            // A struct expression is a reference to the type.
            (RefKind::Type, "S", "f"),
            (RefKind::Call, "S default", "f"),
            (RefKind::Call, "$type:Vec new", "f"),
            // `<T as Tr>`: the trait for a generic `T`, else the type.
            (RefKind::Call, "$type:Tr make", "f"),
            (RefKind::Call, "$type:Widget make", "f"),
        ],
    );
    assert_no_ref(&f, "Some");
    // Path anchors get synthetic imports.
    let anchors: Vec<(&str, u32, Option<&str>)> = f
        .imports
        .iter()
        .filter(|i| i.type_only)
        .map(|i| (i.module.as_str(), i.relative_level, i.module_alias.as_deref()))
        .collect();
    assert_eq!(anchors, [("a", 0, Some("a")), ("crate", 0, Some("crate")), ("", 2, Some("super$1"))]);
}

#[test]
fn locals_closures_and_shadowing() {
    let f = facts(
        "fn helper() {}\nfn f(cb: fn()) {\n    let helper = || 1;\n    helper();\n    cb();\n    let g = |x: u8| other(x);\n    g(2);\n    for item in items() { item.go(); }\n    if let Some(v) = maybe() { v.run(); }\n}\nfn h() { helper(); }\n",
    );
    // Calls of locals (closures, function parameters) are no references;
    // calls inside closures belong to the enclosing function.
    assert_refs(
        &f,
        &[(RefKind::Call, "other", "f"), (RefKind::Call, "helper", "h"), (RefKind::Call, "(item) go", "f")],
    );
    let calls: Vec<String> =
        refs(&f).into_iter().filter(|(k, _, e)| *k == RefKind::Call && e == "f").map(|(_, t, _)| t).collect();
    assert!(
        !calls.contains(&"helper".to_string())
            && !calls.contains(&"cb".to_string())
            && !calls.contains(&"g".to_string()),
        "{calls:?}"
    );
    assert_refs(&f, &[(RefKind::Call, "(v) run", "f")]);
}

#[test]
fn receivers_are_typed_from_the_file() {
    let f = facts(MEMORY);
    assert_refs(
        &f,
        &[
            // `self.cache.record()` with `cache: Arc<Cache>`.
            (RefKind::Call, "$type:Cache record", "MemoryStore.len"),
            (RefKind::Read, "$self items", "MemoryStore.len"),
            (RefKind::Call, "$type:MemoryStore save", "tests.saves"),
        ],
    );
    let f = facts(
        "struct S { inner: Box<Inner>, list: Vec<Inner> }\nimpl S {\n    fn get(&self) -> Option<Inner> { None }\n    fn run(&self, p: &mut Param, q: Option<Q>, r: impl Tr, d: &dyn Dyn, b: Box<dyn Bx>) {\n        self.inner.go();\n        self.other.go();\n        p.call();\n        q.map();\n        r.tr();\n        d.dy();\n        b.bx();\n        self.get().unwrap().after();\n        let made = make()?;\n        made.m();\n        let n = S::new();\n        n.m2();\n        for i in &self.list { i.each(); }\n        if let Some(x) = self.get() { x.found(); }\n    }\n}\nfn make() -> Result<Made, E> { todo!() }\nfn gen<T: Bound, U>(t: T, u: U) where U: Other { t.a(); u.b(); T::c(); }\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::Call, "$type:Inner go", "S.run"),
            (RefKind::Call, "$member:other go", "S.run"),
            (RefKind::Call, "$type:Param call", "S.run"),
            (RefKind::Call, "$type:Option map", "S.run"),
            (RefKind::Call, "$type:Tr tr", "S.run"),
            (RefKind::Call, "$type:Dyn dy", "S.run"),
            (RefKind::Call, "$type:Bx bx", "S.run"),
            (RefKind::Call, "$type:Inner after", "S.run"),
            (RefKind::Call, "$type:Made m", "S.run"),
            (RefKind::Call, "$type:S m2", "S.run"),
            (RefKind::Call, "$type:Inner each", "S.run"),
            (RefKind::Call, "$type:Inner found", "S.run"),
            (RefKind::Call, "$type:Bound a", "gen"),
            (RefKind::Call, "$type:Other b", "gen"),
            (RefKind::Call, "$type:Bound c", "gen"),
        ],
    );
    // Generic parameters are never type references.
    assert_no_ref(&f, "T");
}

#[test]
fn macro_arguments_are_code() {
    let src = "fn f(v: Vec<u8>) {\n    println!(\"{} {}\", total(), v.len());\n    assert_eq!(count(&v), 3, \"{}\", label());\n    let w = vec![make(); 2];\n    tracing::info!(n = size(), \"x\");\n    my_macro!(inner());\n    println!(\"{}\", format!(\"{}\", nested()));\n    stringify!(not_code());\n}\n";
    let f = facts(src);
    assert_refs(
        &f,
        &[
            (RefKind::Call, "total", "f"),
            (RefKind::Call, "count", "f"),
            (RefKind::Call, "label", "f"),
            (RefKind::Call, "make", "f"),
            (RefKind::Call, "size", "f"),
            (RefKind::Call, "tracing info", "f"),
            (RefKind::Call, "my_macro", "f"),
            (RefKind::Call, "inner", "f"),
            (RefKind::Call, "nested", "f"),
        ],
    );
    // Standard macros are not references; opaque ones are not re-parsed.
    assert_no_ref(&f, "println");
    assert_no_ref(&f, "not_code");
    assert_no_ref(&f, "n");
    // Positions are the file's: `total` on line 2, after `println!("{} {}", `.
    let total = f.references.iter().find(|r| r.name == "total").unwrap();
    let col = src.lines().nth(1).unwrap().find("total").unwrap() as u32;
    assert_eq!((total.span.start_line, total.span.start_col), (1, col));
    let off = src.find("total").unwrap() as u32;
    assert_eq!((total.span.start_byte, total.span.end_byte), (off, off + 5));
    // A closure parameter inside a macro argument is a local there.
    let f = facts("fn f(xs: Vec<u8>) { println!(\"{:?}\", xs.iter().map(|x| conv(x)).collect::<Vec<_>>()); }\n");
    assert_refs(&f, &[(RefKind::Call, "conv", "f")]);
    assert_no_ref(&f, "x");
}

#[test]
fn macro_definitions_and_their_bodies() {
    let f = facts(LIB);
    let traced = def(&f, "traced");
    assert_eq!(traced.label, Label::Function);
    // A rule's body is a token pattern, not code: no references (as in
    // rust-analyzer's index).
    assert!(f.references.iter().all(|r| r.enclosing.is_none_or(|e| f.definitions[e as usize].name != "traced")));
    assert_no_ref(&f, "crate.util trace");
    // A repository macro invoked in another file is a call.
    let f = facts(MAIN);
    assert_refs(&f, &[(RefKind::Call, "traced", "main"), (RefKind::Call, "$type:Item total", "main")]);
}

const MACRO_ITEMS: &str = "cfg_net! {
    pub mod tcp;
    pub use tcp::listener::TcpListener;
    /// Spawns a task.
    pub fn spawn() { helper(); }
}
mod m {
    cfg_rt! {
        cfg_unstable! { pub struct Deep; }
        fn inner() -> Deep { Deep }
    }
}
fn outer() {
    cfg_x! { fn local() {} }
    println!(\"{}\", value());
}
cfg_if::cfg_if! {
    if #[cfg(unix)] {
        mod unix;
        pub use unix::Sys;
    } else if #[cfg(windows)] {
        mod windows;
        pub use windows::Sys;
    } else {
        fn fallback() {}
    }
}
cfg_paren!(
    pub const K: u8 = 1;
);
impl S {
    cfg_y! { pub fn method(&self) {} }
}
";

#[test]
fn items_inside_macro_bodies_are_items() {
    let f = facts(MACRO_ITEMS);
    assert_eq!(f.parse_errors, 0);
    let s = summary(&f);
    for want in [
        (Label::Function, "spawn", None),
        (Label::Module, "m", None),
        (Label::Struct, "m.Deep", Some("m")),
        (Label::Function, "m.inner", Some("m")),
        (Label::Function, "outer.local", Some("outer")),
        (Label::Function, "fallback", None),
        (Label::Constant, "K", None),
        (Label::Method, "S.method", None),
    ] {
        assert!(s.contains(&want), "{want:?} not in {s:#?}");
    }
    // Exported by their own `pub`; doc, spans and lines are the file's.
    let spawn = def(&f, "spawn");
    assert!(spawn.exported && !def(&f, "m.inner").exported);
    assert_eq!(spawn.doc.as_deref(), Some("Spawns a task."));
    assert_eq!(spawn.signature.as_deref(), Some("pub fn spawn()"));
    let off = MACRO_ITEMS.find("spawn()").unwrap() as u32;
    assert_eq!((spawn.name_span.start_byte, spawn.name_span.start_line), (off, 4));
    assert_eq!(spawn.name_span.start_col, 11);
    // Their bodies are code; the macros stay calls, expression macros are
    // still re-parsed, and nothing is visited twice.
    assert_refs(
        &f,
        &[
            (RefKind::Call, "helper", "spawn"),
            (RefKind::Call, "cfg_net", ""),
            (RefKind::Call, "cfg_rt", "m"),
            (RefKind::Call, "cfg_unstable", "m"),
            (RefKind::Call, "cfg_x", "outer"),
            (RefKind::Call, "value", "outer"),
            (RefKind::Type, "Deep", "m.inner"),
        ],
    );
    assert_eq!(f.references.iter().filter(|r| r.name == "helper").count(), 1);
}

#[test]
fn imports_inside_macro_bodies() {
    let f = facts(MACRO_ITEMS);
    let rows: Vec<(String, Vec<String>, Option<String>, bool)> = f
        .imports
        .iter()
        .map(|i| {
            (i.module.clone(), i.names.iter().map(|n| n.name.clone()).collect(), i.module_alias.clone(), i.reexport)
        })
        .collect();
    let want = |m: &str, names: &[&str], alias: Option<&str>, re: bool| {
        (m.to_string(), names.iter().map(|s| s.to_string()).collect::<Vec<_>>(), alias.map(str::to_string), re)
    };
    for w in [
        want("tcp", &[], Some("tcp"), false),
        want("tcp::listener", &["TcpListener"], None, true),
        want("unix", &[], Some("unix"), false),
        want("windows", &[], Some("windows"), false),
        want("unix", &["Sys"], None, true),
        want("windows", &["Sys"], None, true),
    ] {
        assert!(rows.contains(&w), "{w:?} not in {rows:#?}");
    }
}

#[test]
fn macro_bodies_that_are_not_items() {
    // Expressions, token patterns and token builders stay as they were.
    let src = "fn f() { assert!(ok()); let v = vec![make()]; }\nquote! { fn not_an_item() {} }\nmacro_rules! m { () => { fn gen() {} } }\nbitflags! { pub struct Flags: u32 { const A = 1; } }\nlet_it! { let x = 1; }\nm!{fn tight() {}}\n";
    let f = facts(src);
    assert_refs(&f, &[(RefKind::Call, "ok", "f"), (RefKind::Call, "make", "f")]);
    let names: Vec<&str> = f.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
    assert_eq!(names, ["f", "m"]);
}

#[test]
fn attribute_macros_are_decorators() {
    let f = facts(
        "use tracing::instrument;\n#[tokio::main(flavor = \"current_thread\")]\nasync fn main() {}\n#[tokio::test]\nasync fn t() {}\n#[rstest]\n#[inline]\nfn case() {}\n#[derive(Debug, Serialize)]\n#[serde(rename_all = \"camelCase\")]\npub struct S;\n#[async_trait::async_trait]\npub trait Tr {}\nimpl S {\n    #[instrument(skip(self))]\n    #[allow(dead_code)]\n    fn m(&self) {}\n}\n#[cfg(test)]\n#[rustfmt::skip]\nmod tests {}\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::Decorator, "tokio main", "main"),
            (RefKind::Decorator, "tokio test", "t"),
            (RefKind::Decorator, "rstest", "case"),
            (RefKind::Decorator, "async_trait async_trait", "Tr"),
            (RefKind::Decorator, "instrument", "S.m"),
        ],
    );
    let decorators: Vec<&str> =
        f.references.iter().filter(|r| r.kind == RefKind::Decorator).map(|r| r.name.as_str()).collect();
    assert_eq!(decorators.len(), 5, "{decorators:?}");
    assert_eq!(def(&f, "main").decorators, ["tokio::main(flavor = \"current_thread\")"]);
    assert_eq!(def(&f, "case").decorators, ["rstest"]);
    // Built-in, tool and derive helper attributes are not decorators.
    assert!(def(&f, "S").decorators.is_empty() && def(&f, "tests").decorators.is_empty());
    // A test attribute still marks the function as a test; the crate the
    // macro comes from is a path anchor.
    assert!(def(&f, "t").is_test && def(&f, "case").is_test && !def(&f, "main").is_test);
    assert!(f.imports.iter().any(|i| i.type_only && i.module == "tokio"));
}

#[test]
fn env_reads() {
    let f = facts(
        "use std::env;\nuse std::env::var_os as vo;\nfn f() {\n    env::var(\"A\");\n    std::env::var(\"B\");\n    vo(\"C\");\n    env!(\"D\");\n    option_env!(\"E\");\n    std::env::var(name);\n    other::var(\"F\");\n}\n",
    );
    let envs: Vec<&str> = f.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| r.name.as_str()).collect();
    assert_eq!(envs, ["A", "B", "C", "D", "E"]);
    // Without `use std::env`, `env::var` is not std's.
    let f = facts("fn f() { env::var(\"X\"); var(\"Y\"); }\n");
    assert!(f.references.iter().all(|r| r.kind != RefKind::EnvVar));
}

/// An import as (module, level, names, alias, re-export).
type ImportRow = (String, u32, Vec<String>, Option<String>, bool);

#[test]
fn imports_and_module_declarations() {
    let f = facts(
        "use crate::a::{b::{self, C as D}, e::*};\nuse super::x;\npub use self::m::Y;\nuse serde;\nuse ::log::info;\nextern crate foo as bar;\nmod m;\n#[path = \"p.rs\"] mod q;\nmod inner { use super::*; use super::super::up; mod deep; }\n",
    );
    let got: Vec<ImportRow> = f
        .imports
        .iter()
        .map(|i| {
            let names = i
                .names
                .iter()
                .map(|n| match &n.alias {
                    Some(a) => format!("{} as {a}", n.name),
                    None => n.name.clone(),
                })
                .collect();
            (i.module.clone(), i.relative_level, names, i.module_alias.clone(), i.reexport)
        })
        .collect();
    let want = |m: &str, l: u32, names: &[&str], alias: Option<&str>, re: bool| {
        (m.to_string(), l, names.iter().map(|s| s.to_string()).collect::<Vec<_>>(), alias.map(str::to_string), re)
    };
    for w in [
        want("crate::a", 0, &["b"], None, false),
        want("crate::a::b", 0, &["C as D"], None, false),
        want("crate::a::e", 0, &["*"], None, false),
        want("", 2, &["x"], None, false),
        want("m", 1, &["Y"], None, true),
        want("", 0, &["serde"], None, false),
        want("::log", 0, &["info"], None, false),
        want("::foo", 0, &[], Some("bar"), false),
        want("m", 1, &[], Some("m"), false),
        want("\"p.rs\"", 1, &[], Some("q"), false),
        // Inside `mod inner`: `super` is the file, `super::super` its parent.
        want("", 1, &["*"], Some("$in:inner"), false),
        want("", 2, &["up"], Some("$in:inner"), false),
        want("inner::deep", 1, &[], Some("deep"), false),
    ] {
        assert!(got.contains(&w), "{w:?} not in {got:#?}");
    }
}

#[test]
fn patterns_and_field_reads() {
    let f = facts(
        "fn f(e: E) {\n    match e { E::A => {}, E::B(x) => use_it(x), E::C { y } => {}, MAX => {}, None => {} }\n    let Point { a, .. } = p();\n    self_read();\n}\nimpl S { fn g(&mut self) { self.count += 1; let c = self.count; } }\n",
    );
    assert_refs(
        &f,
        &[
            (RefKind::Read, "E A", "f"),
            (RefKind::Read, "E B", "f"),
            (RefKind::Type, "E C", "f"),
            (RefKind::Read, "MAX", "f"),
            (RefKind::Type, "Point", "f"),
            (RefKind::Write, "$self count", "S.g"),
            (RefKind::Read, "$self count", "S.g"),
        ],
    );
    assert_no_ref(&f, "None");
    assert_no_ref(&f, "x");
}

#[test]
fn parse_errors_and_determinism() {
    let f = facts("const _: () = assert(check());\nfn ok() {}\nfn broken( {\n");
    assert_refs(&f, &[(RefKind::Call, "check", "")]);
    assert!(f.parse_errors > 0);
    def(&f, "ok");
    assert_eq!(facts(MODEL), facts(MODEL));
    // Edition 2024 syntax: let chains, async closures, `gen` blocks.
    let f = facts("fn f() { if let Some(x) = a() && x > 1 && let Ok(y) = b() { c(y); } let k = async |z| z; }\n");
    assert_eq!(f.parse_errors, 0);
    assert_refs(&f, &[(RefKind::Call, "c", "f")]);
}

#[test]
fn unsupported_language_is_an_error() {
    let r = RustPack::default().extract("x.go", Language::Go, b"package x", &Budget::default());
    assert!(matches!(r, Err(ExtractError::Unsupported(Language::Go))));
}

/// `#[cfg(test)] mod x;` (or `all(test, …)`, also inside a test module) is
/// followed by a `$test` marker import for the resolver; `any(test, …)` is
/// not test-only and has none.
#[test]
fn test_module_declarations_are_marked() {
    let f = facts(
        "#[cfg(test)]\nmod mocks;\n#[cfg(all(test, unix))]\nmod unix_mocks;\n#[cfg(any(test, feature = \"x\"))]\npub mod helpers;\nmod real;\n#[cfg(test)]\nmod tests { mod fixtures; }\n",
    );
    let marked: Vec<(&str, u32)> = f
        .imports
        .iter()
        .filter(|i| i.type_only && i.module_alias.as_deref() == Some("$test"))
        .map(|i| (i.module.as_str(), i.relative_level))
        .collect();
    assert_eq!(marked, [("mocks", 1), ("unix_mocks", 1), ("tests::fixtures", 1)]);
    // The `mod` import itself is unchanged and comes first.
    let k = f.imports.iter().position(|i| i.module_alias.as_deref() == Some("$test")).unwrap();
    assert_eq!(f.imports[k - 1].module_alias.as_deref(), Some("mocks"));
    assert!(!f.imports[k - 1].type_only);
}
