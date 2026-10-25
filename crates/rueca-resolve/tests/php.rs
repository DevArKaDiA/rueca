//! End-to-end PHP resolution with the real `PhpPack` over the Laravel-style
//! fixture project in `rueca-lang-php/tests/fixtures/laravel` (Composer
//! PSR-4 autoload, controller / model / service / trait / enum / interface,
//! PHPUnit and Pest tests, Symfony attributes, an HTML template, block
//! namespaces).

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_php::PhpPack;
use rueca_lang_python::PythonPack;
use rueca_model::{EdgeKind, Label, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-php/tests/fixtures/laravel")
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
    r.register(Box::new(PhpPack::default()));
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
                language: Language::from_path(path),
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
    fn imports(&self, from: &str, to: &str) {
        let (s, d) = (self.file(from), self.file(to));
        assert!(self.find(s, d, EdgeKind::Imports).is_some(), "missing IMPORTS {from} -> {to}");
    }
}

#[test]
fn laravel_style_project_resolves_through_psr4() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;
    let ctrl = "App\\Http\\Controllers\\UserController";

    // Inheritance, traits.
    g.has(ctrl, "App\\Http\\Controllers\\Controller", Inherits, ImportResolved, 0.95);
    g.has("App\\Models\\User", "App\\Models\\Concerns\\HasUuid", Inherits, ImportResolved, 0.95);

    // Controller → service through the promoted, typed `$users` property.
    g.has(&format!("{ctrl}::index"), "App\\Services\\UserService::active", Calls, TypeResolved, 0.85);
    g.has(&format!("{ctrl}::store"), "App\\Services\\UserService::register", Calls, TypeResolved, 0.85);
    g.has(&format!("{ctrl}::index"), &format!("{ctrl}::$users"), References, TypeResolved, 0.9);
    // Inherited method through `$this`; class constant through `self`.
    g.has(&format!("{ctrl}::index"), "App\\Http\\Controllers\\Controller::authorizeAccess", Calls, TypeResolved, 0.85);
    g.has(&format!("{ctrl}::index"), &format!("{ctrl}::PER_PAGE"), References, TypeResolved, 0.9);
    // `use function`, enum case through an imported enum.
    g.has(&format!("{ctrl}::store"), "App\\Support\\format_money", Calls, ImportResolved, 0.95);
    g.has(&format!("{ctrl}::show"), "App\\Enums\\Status::Banned", References, ImportResolved, 0.85);
    g.has(&format!("{ctrl}::show"), "App\\Models\\User", References, ImportResolved, 0.95);

    // Service → repository and interface via promoted properties.
    g.has(
        "App\\Services\\UserService::register",
        "App\\Repositories\\UserRepository::create",
        Calls,
        TypeResolved,
        0.85,
    );
    g.has("App\\Services\\UserService::register", "App\\Contracts\\Notifier::notify", Calls, TypeResolved, 0.85);
    // `$user->promote()` on an untyped variable: the only `promote` method.
    g.has("App\\Services\\UserService::register", "App\\Models\\User::promote", Calls, Heuristic, 0.5);
    // Static calls on imported classes.
    g.has("App\\Repositories\\UserRepository::active", "App\\Models\\User::active", Calls, ImportResolved, 0.85);
    // Model internals: trait method via `$this`, enum case, `X::class`.
    g.has("App\\Models\\User::promote", "App\\Models\\Concerns\\HasUuid::ensureUuid", Calls, TypeResolved, 0.85);
    g.has("App\\Models\\User::promote", "App\\Enums\\Status::Active", References, ImportResolved, 0.85);
    g.has("App\\Models\\User::orders", "App\\Models\\Order", References, ImportResolved, 0.95);
    g.has("App\\Models\\Order::user", "App\\Models\\User", References, ImportResolved, 0.95);
    // Namespaced constant read from a namespaced function (unqualified name).
    g.has("App\\Support\\format_money", "App\\Support\\CURRENCY", References, ImportResolved, 0.95);
}

#[test]
fn tests_templates_routes_and_attributes() {
    let out = fixture();
    let g = G(&out.graph);
    use EdgeKind::*;
    use Strategy::*;

    // PHPUnit: typed `$service` property, `@test` / `#[Test]` methods.
    let t = "Tests\\Unit\\UserServiceTest";
    g.has(t, "Tests\\TestCase", Inherits, ImportResolved, 0.95);
    g.has(&format!("{t}::registers"), "App\\Services\\UserService::register", Tests, TypeResolved, 0.85);
    g.has(&format!("{t}::it_lists_active_users"), "App\\Services\\UserService::active", Tests, TypeResolved, 0.85);
    g.has(&format!("{t}::setUp"), "App\\Services\\UserService", Calls, ImportResolved, 0.95);
    g.has(&format!("{t}::setUp"), "App\\Services\\UserService::__construct", Calls, ImportResolved, 0.95 * 0.95);
    // Pest: test nodes call project code.
    g.has("users > balance starts at zero", "App\\Models\\User::balance", Tests, Heuristic, 0.5);
    g.has("users > promotes a user", "App\\Models\\User::promote", Calls, Heuristic, 0.5);

    // Template and routes: file-level code.
    let view = g.file("resources/views/users.php");
    let active = g.id("App\\Models\\User::active");
    assert_eq!(g.find(view, active, Calls), Some((ImportResolved, 0.85)));
    g.imports("resources/views/users.php", "resources/views/partials/footer.php");
    let routes = g.file("routes/web.php");
    let ctrl = g.id("App\\Http\\Controllers\\UserController");
    assert_eq!(g.find(routes, ctrl, References), Some((ImportResolved, 0.95)));
    g.imports("routes/web.php", "app/Http/Controllers/UserController.php");
    g.imports("routes/web.php", "app/Support/helpers.php");

    // `use` imports become IMPORTS edges through PSR-4 (app/ and tests/).
    g.imports("app/Http/Controllers/UserController.php", "app/Models/User.php");
    g.imports("app/Http/Controllers/UserController.php", "app/Enums/Status.php");
    g.imports("tests/Unit/UserServiceTest.php", "tests/TestCase.php");
    g.imports("src/Controller/BlogController.php", "src/Entity/Post.php");

    // Symfony: a typed parameter of an entity in another PSR-4 root.
    g.has(
        "Acme\\Blog\\Controller\\BlogController::show",
        "Acme\\Blog\\Entity\\Post::publish",
        Calls,
        TypeResolved,
        0.85,
    );
    // Block namespaces, grouped `use`, aliased namespace.
    g.has("Acme\\Legacy\\make", "Acme\\Blog\\Entity\\Post", Calls, ImportResolved, 0.95);
    g.has("Acme\\Legacy\\make", "Acme\\Blog\\Entity\\Post::__construct", Calls, ImportResolved, 0.95 * 0.95);
    g.has("legacy_boot", "Acme\\Legacy\\make", Calls, ImportResolved, 0.95);
    g.has("Acme\\Legacy\\make", "Acme\\Blog\\Entity\\Post", References, ImportResolved, 0.95);
}

#[test]
fn statistics_and_expected_misses() {
    let out = fixture();
    let s = &out.stats;
    // Builtins (`strlen`, `number_format`, `\DomainException`, …) and vendor
    // names (`Illuminate\…`, `Symfony\…`, Laravel helpers) are expected misses.
    assert!(s.references_builtin >= 8, "{s:?}");
    assert!(s.references_external >= 15, "{s:?}");
    assert!(s.references_resolved * 2 > s.references_total - s.references_builtin - s.references_external, "{s:?}");
    for strategy in ["type_resolved", "import_resolved", "heuristic"] {
        assert!(s.resolved_by_strategy.contains_key(strategy), "{s:?}");
    }
    // Vendor calls are never fuzzy-matched: `Cache::remember` has no edge.
    let g = G(&out.graph);
    let index = g.id("App\\Http\\Controllers\\UserController::index");
    let targets: Vec<&str> = out
        .graph
        .edges
        .iter()
        .filter(|e| e.src == index && e.kind == EdgeKind::Calls)
        .map(|e| out.graph.nodes[e.dst as usize].qualified_name.as_str())
        .collect();
    assert_eq!(
        targets.iter().copied().collect::<std::collections::BTreeSet<_>>(),
        ["App\\Http\\Controllers\\Controller::authorizeAccess", "App\\Services\\UserService::active"]
            .into_iter()
            .collect()
    );
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
fn php_and_python_do_not_cross() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    files.push(("tools/promote.py".into(), b"def promote():\n    pass\n\ndef run(x):\n    x.balance()\n".to_vec()));
    let out = build(files);
    let g = G(&out.graph);
    // The Python `promote` does not make the PHP heuristic ambiguous, and
    // the Python `x.balance()` does not reach the PHP method.
    g.has(
        "App\\Services\\UserService::register",
        "App\\Models\\User::promote",
        EdgeKind::Calls,
        Strategy::Heuristic,
        0.5,
    );
    let run = g.id("tools.promote.run");
    assert!(!out.graph.edges.iter().any(|e| e.src == run && e.kind == EdgeKind::Calls));
}
