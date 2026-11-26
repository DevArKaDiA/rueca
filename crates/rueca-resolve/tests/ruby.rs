//! End-to-end Ruby resolution with the real `RubyPack` over the gem
//! fixture in `rueca-lang-ruby/tests/fixtures/shop`: a module reopened in
//! every file, a mixin, `module_function`, a compact `class A::B < A::C`
//! with `super`, `require` / `require_relative`, an instance variable typed
//! in `initialize`, an env read and an RSpec spec; plus snippets for
//! constant lookup, singleton methods and aliases.

use std::path::{Path, PathBuf};

use rueca_extract::{Budget, ProjectFiles, Registry};
use rueca_lang_ruby::RubyPack;
use rueca_model::{EdgeKind, Label, Language, NodeId, ResolvedGraph, SnapshotInfo, Strategy, git_blob_sha};
use rueca_resolve::{FileInput, ResolveOutput, resolve};

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../rueca-lang-ruby/tests/fixtures/shop")
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
    r.register(Box::new(RubyPack));
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
}

fn snippet(files: &[(&str, &str)]) -> ResolveOutput {
    build(files.iter().map(|(p, s)| (p.to_string(), s.as_bytes().to_vec())).collect())
}

#[test]
fn ruby_fixture_definitions_and_imports() {
    let out = fixture();
    let g = G(&out.graph);
    for (qn, label) in [
        ("Shop::Inventory#add", Label::Method),
        ("Shop::Store.open", Label::Method),
        ("Shop::Logger.info", Label::Method),
        ("Shop::Store#inventory", Label::Field),
        ("Shop::VERSION", Label::Constant),
        ("Shop::Audit", Label::Class),
        ("Shop::Countable", Label::Module),
    ] {
        assert_eq!(out.graph.nodes[g.id(qn) as usize].label, label, "{qn}");
    }
    g.imports("lib/shop.rb", "lib/shop/version.rb");
    g.imports("lib/shop.rb", "lib/shop/store.rb");
    g.imports("lib/shop/store.rb", "lib/shop/inventory.rb");
    g.imports("spec/store_spec.rb", "lib/shop.rb");
    // Every file but the compact `class Shop::Audit` reopens `Shop`: one node each.
    let shops = out.graph.nodes.iter().filter(|n| n.qualified_name == "Shop" && n.label == Label::Module).count();
    assert_eq!(shops, 7);
}

#[test]
fn ruby_fixture_calls() {
    let out = fixture();
    let g = G(&out.graph);
    // `Store.new` in `module Shop` → the class and its `initialize`.
    g.has("Shop.store", "Shop::Store#initialize", EdgeKind::Calls, Strategy::ImportResolved, 0.85 * 0.95);
    // `new` in a singleton method.
    g.has("Shop::Store.open", "Shop::Store#initialize", EdgeKind::Calls, Strategy::TypeResolved, 0.9 * 0.95);
    // An instance variable typed in `initialize`.
    g.has("Shop::Store#sell", "Shop::Audit#record", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `super` in a compact `class Shop::Audit < Shop::Base`.
    g.has("Shop::Audit#record", "Shop::Base#record", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // A mixin method through `include Countable` (lexical lookup in `Shop`).
    g.has("Shop::Inventory#count", "Shop::Countable#total", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // A private method, then a `module_function` module.
    g.has("Shop::Inventory#add", "Shop::Inventory#log", EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    g.has("Shop::Inventory#log", "Shop::Logger.info", EdgeKind::Calls, Strategy::ImportResolved, 0.85);
    g.has("Shop::Logger.info", "Shop::Logger.format", EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    // The env bridge.
    let sell = g.id("Shop::Store#sell");
    let env = g.id("$env:SHOP_CURRENCY");
    assert!(g.find(sell, env, EdgeKind::Reads).is_some());
}

#[test]
fn ruby_fixture_inheritance_and_tests() {
    let out = fixture();
    let g = G(&out.graph);
    let audit = g.id("Shop::Audit");
    let base = g.id("Shop::Base");
    assert!(g.find(audit, base, EdgeKind::Inherits).is_some());
    let inventory = g.id("Shop::Inventory");
    assert!(g.find(inventory, g.id("Shop::Countable"), EdgeKind::Inherits).is_some());
    let spec = out.graph.nodes.iter().position(|n| n.label == Label::Test && n.name == "sells").unwrap() as NodeId;
    assert!(out.graph.nodes[spec as usize].is_test);
    assert!(g.find(spec, g.id("Shop::Store.open"), EdgeKind::Tests).is_some());
}

#[test]
fn ruby_constant_lookup() {
    let out = snippet(&[(
        "lib/a.rb",
        "class Logger\n  def self.info; end\nend\nmodule App\n  class Logger\n    def self.info; end\n  end\n  class Job\n    def a\n      Logger.info\n    end\n    def b\n      ::Logger.info\n    end\n  end\nend\nclass App::Compact\n  def c\n    Logger.info\n  end\nend\n",
    )]);
    let g = G(&out.graph);
    g.has("App::Job#a", "App::Logger.info", EdgeKind::Calls, Strategy::ImportResolved, 0.85);
    g.has("App::Job#b", "Logger.info", EdgeKind::Calls, Strategy::ImportResolved, 0.85);
    // A compact `class App::Compact` body does not see `App`'s constants.
    g.has("App::Compact#c", "Logger.info", EdgeKind::Calls, Strategy::ImportResolved, 0.85);
}

#[test]
fn ruby_receivers_aliases_and_heuristics() {
    let out = snippet(&[(
        "lib/b.rb",
        "class Conn\n  def query; end\n  alias run_query query\nend\nclass Pool\n  #: -> Conn\n  def checkout\n    Conn.new\n  end\n  def a\n    checkout.query\n  end\n  def b(c)\n    c.run_query\n  end\n  def d\n    x = Conn.new\n    x.run_query\n  end\n  def e(other)\n    other.query\n  end\nend\n",
    )]);
    let g = G(&out.graph);
    g.has("Pool#a", "Conn#query", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // An alias stands for its method.
    g.has("Pool#d", "Conn#query", EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Untyped receivers: the only method of that name, as a guess.
    g.has("Pool#e", "Conn#query", EdgeKind::Calls, Strategy::Heuristic, 0.5);
    g.lacks("Pool#b", "Conn#query", EdgeKind::Calls);
}
