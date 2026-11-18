//! End-to-end Rust resolution with the real `RustPack` over the Cargo
//! workspace fixture in `rueca-lang-rust/tests/fixtures/shop`: a library
//! crate (`shop-core`: module files, a `mod.rs`, an inline test module,
//! `pub use` chains and globs, traits with default methods, trait impls,
//! a `macro_rules!`, env reads) and a binary crate (`shop-app`) depending on
//! it through `[workspace.dependencies]`, plus an integration test.

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_rust::RustPack;
use rueca_model::{EdgeKind, Label, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-rust/tests/fixtures/shop")
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
    r.register(Box::new(RustPack::default()));
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
        // A field and a method may share a name (`Item.price`): the callable.
        let callable: Vec<NodeId> =
            hits.iter().copied().filter(|&i| self.0.nodes[i as usize].label.is_callable()).collect();
        if hits.len() > 1 && callable.len() == 1 {
            return callable[0];
        }
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
    fn imports(&self, from: &str, to: &str) {
        let (s, d) = (self.file(from), self.file(to));
        assert_eq!(self.find(s, d, EdgeKind::Imports), Some((Strategy::ImportResolved, 1.0)), "{from} -> {to}");
    }
    fn module_of(&self, path: &str) -> &str {
        let f = self.0.files.iter().position(|f| f.path == path).unwrap();
        &self.0.modules[f]
    }
}

const CORE: &str = "shop_core";
const MODEL: &str = "shop_core::model";
const MEMORY: &str = "shop_core::store::memory";
const APP: &str = "shop_app";

fn q(module: &str, local: &str) -> String {
    format!("{module}::{local}")
}

#[test]
fn module_names_are_module_paths() {
    let out = fixture();
    let g = G(&out.graph);
    assert_eq!(g.module_of("crates/core/src/lib.rs"), CORE);
    assert_eq!(g.module_of("crates/core/src/model.rs"), MODEL);
    assert_eq!(g.module_of("crates/core/src/store/mod.rs"), "shop_core::store");
    assert_eq!(g.module_of("crates/core/src/store/memory.rs"), MEMORY);
    assert_eq!(g.module_of("crates/app/src/main.rs"), APP);
    assert_eq!(g.module_of("crates/app/src/cli.rs"), "shop_app::cli");
    assert_eq!(g.module_of("crates/core/tests/basic.rs"), "basic");
    // Items of an impl block are named after the type; inline modules,
    // nested functions and trait items join with `.`.
    g.id(&q(MODEL, "Item.total"));
    g.id(&q(MEMORY, "tests.sample"));
    g.id(&q(APP, "compute.half"));
    g.id(&q(CORE, "Priced.label"));
    g.id(&q(CORE, "traced"));
    assert_eq!(out.graph.nodes[g.id(&q(MEMORY, "tests")) as usize].label, Label::Module);
}

#[test]
fn imports_reach_the_files_of_their_items() {
    let out = fixture();
    let g = G(&out.graph);
    // `mod` declarations.
    g.imports("crates/core/src/lib.rs", "crates/core/src/model.rs");
    g.imports("crates/core/src/lib.rs", "crates/core/src/store/mod.rs");
    g.imports("crates/core/src/store/mod.rs", "crates/core/src/store/memory.rs");
    g.imports("crates/app/src/main.rs", "crates/app/src/cli.rs");
    // `use` leaves, through `pub use` re-exports and across crates.
    g.imports("crates/core/src/store/memory.rs", "crates/core/src/model.rs");
    g.imports("crates/app/src/main.rs", "crates/core/src/model.rs");
    g.imports("crates/app/src/main.rs", "crates/core/src/lib.rs");
    g.imports("crates/core/tests/basic.rs", "crates/core/src/store/mod.rs");
    // Path anchors used in code (`crate::util::trace`) make no IMPORTS.
    let anchor = (g.file("crates/core/src/store/memory.rs"), g.file("crates/core/src/lib.rs"));
    assert_eq!(g.find(anchor.0, anchor.1, EdgeKind::Imports), None);
}

#[test]
fn calls_resolve_through_modules_and_crates() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let main = q(APP, "main");
    // Same module, nested function, a module declared by `mod`.
    g.has(&main, &q(APP, "make"), Calls, SameModule, 0.9);
    g.has(&q(APP, "compute"), &q(APP, "compute.half"), Calls, SameModule, 0.95);
    g.has(&main, &q("shop_app::cli", "run"), Calls, ImportResolved, 0.9);
    // `super::compute` from a child module.
    g.has(&q("shop_app::cli", "run"), &q(APP, "compute"), Calls, ImportResolved, 0.9);
    // A crate of the workspace by name (`shop_core::open_store()`), and
    // `Type::f()` through `use` and a `pub use` chain.
    g.has(&main, &q(CORE, "open_store"), Calls, ImportResolved, 0.9);
    g.has(&q(APP, "make"), &q(MODEL, "Item.new"), Calls, ImportResolved, 0.85);
    g.has(&q("shop_app::cli", "run"), &q(MODEL, "Price.new"), Calls, ImportResolved, 0.85);
    // `crate::util::trace(…)`, `util::trace` through `use crate::util`.
    g.has(&q(MEMORY, "MemoryStore.find"), &q("shop_core::util", "trace"), Calls, ImportResolved, 0.9);
    g.has(&q(MEMORY, "MemoryStore.save"), &q("shop_core::util", "trace"), Calls, ImportResolved, 0.9);
    // A glob (`use super::*`) inside the inline test module.
    g.has(&q(MEMORY, "tests.saves"), &q(MEMORY, "MemoryStore.new"), Calls, SameModule, 0.85);
    g.has(&q(MEMORY, "tests.saves"), &q(MEMORY, "tests.sample"), Calls, SameModule, 0.95);
    g.has(&q(MEMORY, "tests.saves"), &q(MEMORY, "MemoryStore.new"), Tests, SameModule, 0.85);
    // Closures are not definitions: their calls belong to `main`, and a
    // call of a closure is no edge.
    g.has(&main, &q("shop_app::cli", "report"), Calls, ImportResolved, 0.9);
    g.has(&main, &q(MODEL, "Price.cents"), Calls, TypeResolved, 0.85);
}

#[test]
fn receivers_impls_and_traits() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let main = q(APP, "main");
    // `self.m()` / `self.field` inside impl blocks (the type is in the
    // same file or another one).
    g.has(&q(MODEL, "Item.discounted"), &q(MODEL, "Item.total"), Calls, TypeResolved, 0.9);
    g.has(&q(MEMORY, "MemoryStore.len"), &q(MEMORY, "MemoryStore.items"), References, TypeResolved, 0.9);
    // `self.cache.record()`: field `cache: Arc<Cache>`.
    g.has(&q(MEMORY, "MemoryStore.len"), &q(MEMORY, "Cache.record"), Calls, TypeResolved, 0.85);
    // `self.price.cents()` across files.
    g.has(&q(MODEL, "Item.total"), &q(MODEL, "Price.cents"), Calls, TypeResolved, 0.85);
    // `let item = make(); item.total(2)` (return type of a function of
    // the file), inside a macro argument (`traced!(…)`).
    g.has(&main, &q(MODEL, "Item.total"), Calls, TypeResolved, 0.85);
    g.has(&main, &q(MODEL, "Item.discounted"), Calls, TypeResolved, 0.85);
    // `store: &mut dyn Store`: the trait's methods, default ones too.
    g.has(&q("shop_app::cli", "run"), "shop_core::store::Store.find", Calls, TypeResolved, 0.85);
    g.has(&q("shop_app::cli", "run"), "shop_core::store::Store.save_all", Calls, TypeResolved, 0.85);
    // Trait impls: IMPLEMENTS, OVERRIDES, default methods through the type.
    g.has(&q(MODEL, "Item"), &q(CORE, "Priced"), Implements, ImportResolved, 0.95);
    g.has(&q(MEMORY, "MemoryStore"), "shop_core::store::Store", Implements, ImportResolved, 0.95);
    g.has(&q(MODEL, "Item.price"), &q(CORE, "Priced.price"), Overrides, TypeResolved, 0.9);
    g.has(&q(MEMORY, "MemoryStore.save"), "shop_core::store::Store.save", Overrides, TypeResolved, 0.9);
    g.has("basic::item_label", &q(CORE, "Priced.label"), Calls, TypeResolved, 0.85);
    g.has(&q(MODEL, "Item.fmt"), &q(CORE, "Priced.label"), Calls, TypeResolved, 0.85);
    // A default method calling the trait's own method; its return type.
    g.has(&q(CORE, "Priced.label"), &q(CORE, "Priced.price"), Calls, TypeResolved, 0.9);
    g.has(&q(CORE, "Priced.label"), &q(MODEL, "Price.cents"), Calls, TypeResolved, 0.85);
    g.has("shop_core::store::Store.save_all", "shop_core::store::Store.save", Calls, TypeResolved, 0.9);
    // A value returned by a function of another file (`$ret:`), typed from
    // its signature: `shop_core::open_store() -> MemoryStore`.
    g.has(&main, &q(MEMORY, "MemoryStore.save"), Calls, TypeResolved, 0.85);
    g.lacks(&main, "shop_core::store::Store.save", Calls);
    g.has("basic::item_label", &q(MEMORY, "MemoryStore.save"), Calls, TypeResolved, 0.85);
    // A constructor of another file returning `io::Result<Self>`, through `?`.
    g.has(&q(APP, "reopen"), &q(MEMORY, "MemoryStore.len"), Calls, TypeResolved, 0.85);
}

#[test]
fn macros_and_env_reads() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // A `macro_rules!` invocation is a call; its body is not code.
    g.has(&q(APP, "main"), &q(CORE, "traced"), Calls, ImportResolved, 0.95);
    g.lacks(&q(CORE, "traced"), &q("shop_core::util", "trace"), Calls);
    // `std::env::var`, `env::var` with `use std::env`, `env!`, `option_env!`.
    g.has(&q("shop_core::util", "trace"), "$env:SHOP_TRACE", Reads, Contract, 0.95);
    g.has(&q("shop_core::util", "check_env"), "$env:SHOP_DB", Reads, Contract, 0.95);
    g.has(&q("shop_core::util", "check_env"), "$env:SHOP_BUILD", Reads, Contract, 0.95);
    g.has(&q("shop_app::cli", "run"), "$env:SHOP_MODE", Reads, Contract, 0.95);
    g.has(&q("shop_app::cli", "report"), "$env:CARGO_PKG_NAME", Reads, Contract, 0.95);
}

#[test]
fn resolution_is_deterministic() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    let a = build(files.clone());
    files.reverse();
    let b = build(files);
    assert_eq!(a.graph, b.graph);
    assert!(a.stats.references_resolved * 10 >= a.stats.references_total * 7, "{:?}", a.stats);
}

/// Every `.rs` file and `Cargo.toml` of a checkout (no `target/`, hidden
/// or vendored directories).
fn rust_files(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if p.is_dir() {
            if !name.starts_with('.') && !matches!(name, "target" | "node_modules" | "vendor") {
                rust_files(&p, root, out);
            }
        } else if name.ends_with(".rs") || name == "Cargo.toml" {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read(&p).unwrap()));
        }
    }
}

/// "Rueca can read itself": the whole repository, Rust only, resolved
/// with the real pack — known edges across crates, trait impls, modules
/// and impl blocks in other files than their types.
#[test]
fn rueca_reads_itself() {
    // The workspace manifest and `crates/` (not `bench/`, whose caches hold
    // other repositories).
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = vec![("Cargo.toml".to_string(), std::fs::read(root.join("Cargo.toml")).unwrap())];
    rust_files(&root.join("crates"), &root, &mut files);
    let out = build(files);
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let stats = &out.stats;
    // Almost every reference is resolved or known to be outside the project.
    let unresolved =
        stats.references_total - stats.references_resolved - stats.references_external - stats.references_builtin;
    assert!(unresolved * 50 < stats.references_total, "{stats:?}");
    // `mod rust;` → the file.
    g.imports("crates/rueca-resolve/src/lib.rs", "crates/rueca-resolve/src/rust.rs");
    // The indexer registers every pack: cross-crate calls and references.
    let registry = "rueca_index::default_registry";
    g.has(registry, "rueca_extract::Registry.new", Calls, ImportResolved, 0.85);
    g.has(registry, "rueca_extract::Registry.register", Calls, TypeResolved, 0.85);
    g.has(registry, "rueca_lang_python::PythonPack", References, ImportResolved, 0.9);
    // The CLI calls into the query engine (`rueca_query::Engine::new`).
    g.has("rueca::query_engine", "rueca_query::Engine.new", Calls, ImportResolved, 0.85);
    // Trait impls of the packs.
    g.has("rueca_lang_rust::RustPack", "rueca_extract::LanguagePack", Implements, ImportResolved, 0.95);
    g.has("rueca_lang_rust::RustPack.extract", "rueca_extract::LanguagePack.extract", Overrides, TypeResolved, 0.9);
    // `impl Resolver` blocks spread over modules: a method declared in
    // `rust.rs` is a member of the type declared in `lib.rs`.
    g.has("rueca_resolve::resolve", "rueca_resolve::rust::Resolver.rust_import_edges", Calls, TypeResolved, 0.85);
    g.has(
        "rueca_resolve::rust::Resolver.rust_env",
        "rueca_resolve::rust::Resolver.rust_bound",
        Calls,
        TypeResolved,
        0.9,
    );
}

fn crate_files(files: &[(&str, &str)]) -> Vec<(String, Vec<u8>)> {
    files.iter().map(|(p, s)| (p.to_string(), s.as_bytes().to_vec())).collect()
}

/// Items wrapped in macros (tokio's `cfg_net! { pub mod tcp; pub use …; }`)
/// are items: the `mod` declarations bind their files and the `pub use`
/// chain reaches the type through them.
#[test]
fn items_inside_macro_bodies_resolve() {
    let out = build(crate_files(&[
        ("Cargo.toml", "[package]\nname = \"mini\"\n"),
        (
            "src/lib.rs",
            "macro_rules! cfg_x { ($($i:item)*) => { $($i)* } }\nmod net {\n    cfg_x! {\n        pub mod tcp;\n        pub use tcp::listener::TcpListener;\n    }\n}\nuse crate::net::TcpListener;\nfn f(l: TcpListener) { l.accept(); }\n",
        ),
        ("src/net/tcp.rs", "cfg_x! {\n    pub mod listener;\n}\n"),
        ("src/net/tcp/listener.rs", "pub struct TcpListener;\nimpl TcpListener { pub fn accept(&self) {} }\n"),
    ]));
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    g.imports("src/lib.rs", "src/net/tcp.rs");
    g.imports("src/net/tcp.rs", "src/net/tcp/listener.rs");
    assert_eq!(g.module_of("src/net/tcp/listener.rs"), "mini::net::tcp::listener");
    let ty = "mini::net::tcp::listener::TcpListener";
    g.has("mini::f", ty, References, ImportResolved, 0.95);
    g.has("mini::f", &format!("{ty}.accept"), Calls, TypeResolved, 0.85);
    g.has("mini::net", "mini::cfg_x", Calls, SameModule, 0.9);
}

/// `#[tk::main]` decorates through the crate's re-export (written inside a
/// macro) of the proc-macro crate's `#[proc_macro_attribute] pub fn main`.
#[test]
fn attribute_macros_decorate() {
    let out = build(crate_files(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"tk\", \"tk-macros\", \"app\"]\n"),
        ("tk-macros/Cargo.toml", "[package]\nname = \"tk-macros\"\n[lib]\nproc-macro = true\n"),
        (
            "tk-macros/src/lib.rs",
            "use proc_macro::TokenStream;\n#[proc_macro_attribute]\npub fn main(args: TokenStream, item: TokenStream) -> TokenStream { item }\n#[proc_macro_attribute]\npub fn main_rt(args: TokenStream, item: TokenStream) -> TokenStream { item }\n",
        ),
        ("tk/Cargo.toml", "[package]\nname = \"tk\"\n[dependencies]\ntk-macros = { path = \"../tk-macros\" }\n"),
        (
            "tk/src/lib.rs",
            "macro_rules! cfg_macros { ($($i:item)*) => { $($i)* } }\ncfg_macros! {\n    cfg_rt! {\n        #[cfg(feature = \"rt-multi-thread\")]\n        pub use tk_macros::main;\n        cfg_not_rt_multi_thread! {\n            pub use tk_macros::main_rt as main;\n        }\n    }\n}\n",
        ),
        ("app/Cargo.toml", "[package]\nname = \"app\"\n[dependencies]\ntk = { path = \"../tk\" }\n"),
        ("app/src/main.rs", "#[tk::main]\nasync fn main() {}\n#[tk::main]\nfn other() {}\n"),
    ]));
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // Decorator → decorated definition, like Python decorators.
    g.has("tk_macros::main", "app::main", Decorates, ImportResolved, 0.9);
    g.has("tk_macros::main", "app::other", Decorates, ImportResolved, 0.9);
    g.lacks("app::main", "tk_macros::main", Calls);
}

/// Rust's two namespaces: `mod sleep; pub use sleep::sleep;` binds the
/// module for paths and types, the function for calls (also through a
/// `use` of the leaf, a module path and a call result); a `mod x;` next to
/// a `fn x` in one module splits the same way.
#[test]
fn values_and_types_have_their_own_namespaces() {
    let out = build(crate_files(&[
        ("Cargo.toml", "[package]\nname = \"mini\"\n"),
        ("src/lib.rs", "pub mod time;\nmod app;\nmod same;\n"),
        ("src/time/mod.rs", "mod sleep;\npub use sleep::{sleep, Sleep};\n"),
        (
            "src/time/sleep.rs",
            "pub struct Sleep { at: u64 }\nimpl Sleep { pub fn reset(&self) {} }\npub fn sleep(d: u64) -> Sleep { Sleep { at: d } }\n",
        ),
        (
            "src/app.rs",
            "use crate::time::sleep;\nuse crate::time;\nfn a() { sleep(1); }\nfn b() { crate::time::sleep(1); }\nfn c() { time::sleep(1); }\nfn d(s: time::sleep::Sleep) { s.reset(); }\nfn e() { time::sleep(1).reset(); }\n",
        ),
        ("src/same.rs", "mod x;\npub fn x() {}\nfn f() { x(); }\nfn g(y: x::Y) {}\n"),
        ("src/same/x.rs", "pub struct Y;\n"),
    ]));
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let sleep = "mini::time::sleep::sleep";
    g.has("mini::app::a", sleep, Calls, ImportResolved, 0.95);
    g.has("mini::app::b", sleep, Calls, ImportResolved, 0.9);
    g.has("mini::app::c", sleep, Calls, ImportResolved, 0.9);
    g.has("mini::app::e", sleep, Calls, ImportResolved, 0.9);
    g.has("mini::app::d", "mini::time::sleep::Sleep", References, ImportResolved, 0.9);
    g.has("mini::app::d", "mini::time::sleep::Sleep.reset", Calls, TypeResolved, 0.85);
    g.has("mini::app::e", "mini::time::sleep::Sleep.reset", Calls, TypeResolved, 0.85);
    g.imports("src/app.rs", "src/time/sleep.rs");
    g.has("mini::same::f", "mini::same::x", Calls, SameModule, 0.9);
    g.has("mini::same::g", "mini::same::x::Y", References, ImportResolved, 0.9);
}

/// Three crates of one snapshot share type names: a type resolves in the
/// scope of the file that names it (its items, imports, paths, its own
/// crate by name), never by name in another crate; an `impl` attaches to
/// the type its file sees; a type's own crate's impls come first; test
/// doubles (`#[cfg(test)]` modules, inline or files) never bind for other
/// code.
#[test]
fn types_resolve_in_scope_across_crates() {
    let out = build(crate_files(&[
        ("Cargo.toml", "[workspace]\nmembers = [\"io\", \"rt\", \"app\", \"bytes\", \"a-hyper\", \"web\"]\n"),
        ("io/Cargo.toml", "[package]\nname = \"io\"\n"),
        ("io/src/lib.rs", "pub mod net;\npub mod port;\npub struct Mystery;\nimpl Mystery { pub fn go(&self) {} }\n"),
        ("io/src/net.rs", "pub struct TcpStream;\nimpl TcpStream { pub fn peek(&self) {} }\n"),
        (
            "io/src/port.rs",
            "use std::os::windows::io::*;\npub struct Port;\nimpl AsRawHandle for Port { fn as_raw_handle(&self) -> u32 { 0 } }\n",
        ),
        ("rt/Cargo.toml", "[package]\nname = \"rt\"\n[dependencies]\nio = { path = \"../io\" }\n"),
        (
            "rt/src/lib.rs",
            "pub mod net;\npub mod task;\npub mod fs;\npub mod doc;\nmod hidden;\npub trait AsyncRead { fn poll_read(&self); }\n",
        ),
        (
            "rt/src/net.rs",
            "use crate::AsyncRead;\npub struct TcpStream { io: io::net::TcpStream }\nimpl TcpStream { pub fn peek(&self) {} }\nimpl AsyncRead for TcpStream { fn poll_read(&self) {} }\nfn f(s: TcpStream) { s.peek(); }\nfn g(m: Mystery) { m.go(); }\nfn h(l: Local) { l.run(); }\nfn k(s: Spawner) { s.spawn_it(); }\n",
        ),
        ("rt/src/hidden.rs", "pub struct Local;\nimpl Local { pub fn run(&self) {} }\n"),
        ("rt/src/task.rs", "pub struct JoinHandle;\n"),
        ("rt/src/fs.rs", "#[cfg(test)]\nmod mocks { pub struct JoinHandle; }\n#[cfg(test)]\nmod spawn_mocks;\n"),
        ("rt/src/fs/spawn_mocks.rs", "pub struct Spawner;\nimpl Spawner { pub fn spawn_it(&self) {} }\n"),
        ("rt/src/doc.rs", "pub mod os { pub trait AsRawHandle { fn as_raw_handle(&self) -> u32; } }\n"),
        ("app/Cargo.toml", "[package]\nname = \"app\"\n[dependencies]\nrt = { path = \"../rt\" }\n"),
        (
            "app/src/lib.rs",
            "use rt::task::JoinHandle;\npub trait TaskHandle { fn abort(&self); }\nimpl TaskHandle for JoinHandle { fn abort(&self) {} }\n",
        ),
        ("bytes/Cargo.toml", "[package]\nname = \"bytes\"\n"),
        (
            "bytes/src/lib.rs",
            "pub struct Bytes;\nimpl From<Vec<u8>> for Bytes { fn from(v: Vec<u8>) -> Bytes { Bytes } }\n",
        ),
        ("a-hyper/Cargo.toml", "[package]\nname = \"hyper\"\n[dependencies]\nbytes = { path = \"../bytes\" }\n"),
        (
            "a-hyper/src/lib.rs",
            "use bytes::Bytes;\npub struct Reason;\nimpl From<Reason> for Bytes { fn from(r: Reason) -> Bytes { Bytes } }\n",
        ),
        ("web/Cargo.toml", "[package]\nname = \"web\"\n[dependencies]\nbytes = { path = \"../bytes\" }\n"),
        ("web/src/lib.rs", "use bytes::Bytes;\nfn f() { Bytes::from(Vec::new()); }\n"),
    ]));
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    // The file's own type, not the same-named type of a dependency.
    g.has("rt::net::f", "rt::net::TcpStream.peek", Calls, TypeResolved, 0.85);
    g.lacks("rt::net::f", "io::net::TcpStream.peek", Calls);
    g.has("rt::net::TcpStream", "rt::AsyncRead", Implements, ImportResolved, 0.95);
    g.lacks("io::net::TcpStream", "rt::AsyncRead", Implements);
    // By name: the file's crate first; another crate's type only when the
    // own crate has none, as a guess (`heuristic`, which links drop).
    g.has("rt::net::h", "rt::hidden::Local.run", Calls, TypeResolved, 0.85);
    g.has("rt::net::g", "io::Mystery.go", Calls, Heuristic, 0.5);
    // A trait the file cannot see (a glob of `std`) is not another crate's
    // same-named trait.
    g.lacks("io::port::Port", "rt::doc::os.AsRawHandle", Implements);
    g.lacks("io::port::Port.as_raw_handle", "rt::doc::os.AsRawHandle.as_raw_handle", Overrides);
    // Test doubles.
    g.has("rt::task::JoinHandle", "app::TaskHandle", Implements, SameModule, 0.9);
    g.lacks("rt::fs::mocks.JoinHandle", "app::TaskHandle", Implements);
    assert!(out.graph.nodes[g.id("rt::fs::spawn_mocks::Spawner") as usize].is_test);
    g.lacks("rt::net::k", "rt::fs::spawn_mocks::Spawner.spawn_it", Calls);
    // A type's own crate's impls first.
    g.has("web::f", "bytes::Bytes.from", Calls, ImportResolved, 0.85);
    g.lacks("web::f", "hyper::Bytes.from", Calls);
}
