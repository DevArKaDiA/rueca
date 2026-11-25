//! Ruby language pack for Rueca (`tree-sitter-ruby` 0.23).
//!
//! See `rueca_extract::LanguagePack` for the contract. This crate turns one
//! Ruby file (`.rb`, `.rake`, `.gemspec`, `.ru`, `Rakefile`, `Gemfile`)
//! into [`FileFacts`] and maps `require` paths to files. Constants, method
//! lookup through ancestors and mixins, and receivers are resolved by the
//! Ruby part of `rueca-resolve` (its crate docs, *Ruby*).
//!
//! What is assembled and what is Rueca code (RFC 0002 §3.4, §4.1):
//!
//! - **Grammar**: `tree-sitter-ruby` 0.23 (MIT), linked against the
//!   tree-sitter 0.27 runtime (it depends only on `tree-sitter-language`).
//! - **Local variables** come from a vendored, adapted `queries/locals.scm`
//!   (nvim-treesitter, MIT; provenance in `queries/README.md`, semantics
//!   in `locals.rs`). They decide whether a bare identifier is a local or a
//!   method call (`helper` is `self.helper` unless it was assigned).
//! - Definitions, constant paths, receivers, declared types (Sorbet `sig`s
//!   and RBS comments), imports and test blocks are Rueca code
//!   (`extract.rs`, `refs.rs`, `receivers.rs`, `types.rs`, `tests.rs`).
//!
//! # Module names and qualified names
//!
//! Ruby constants are global: a class is named by its constant path
//! wherever it is defined, and a class may be reopened in many files. So
//! the module name of every file is empty and local qualified names are
//! global, like PHP and C++:
//!
//! | Definition | Label | Qualified name |
//! |---|---|---|
//! | `module A; class B` / `class A::B` | `Module` / `Class` | `A`, `A::B` (lexical nesting and compact paths joined; `class ::C` is `C`) |
//! | `def m` in a class or module | `Method` | `A::B#m` |
//! | `def self.m`, `def` inside `class << self`, `def A.m` | `Method` | `A::B.m` |
//! | `def m` after `module_function` (or `module_function :m`) | `Method` | `A.m`, signature `module_function def m` |
//! | `def m` at top level (a private method of `Object`) | `Function` | `m` |
//! | `attr_reader :a` / `attr_writer` / `attr_accessor` / `attr` | `Field` | `A::B#a` (one per name), signature `attr_reader :a` |
//! | `alias n o` / `alias_method :n, :o` | `Method` (calls `o`) | `A::B#n` |
//! | `define_method(:m) { … }` with a literal name | `Method` (the block is its body) | `A::B#m` |
//! | `X = …` in a class body or at top level | `Constant` | `A::B::X` |
//! | `X = Struct.new(:a) do … end`, `Data.define`, `Class.new(Base)`, `Module.new` | `Class` / `Module`; members `Field`s | `A::X` |
//! | RSpec `describe` / `context` / `it` / …, `test "…" do` (in test files) | `Test` | the title, nested as `outer > inner` |
//!
//! - `bases` of a class: its superclass as written (`Base`, `::A::Base`;
//!   for `Struct.new(…)` / `Migration[7.0]` the constant it starts from),
//!   then its mixins in body order as `include:M`, `extend:M`,
//!   `prepend:M` (`extend:self` for `extend self`; `include` inside
//!   `class << self` is `extend`). A reopened class lists the mixins of
//!   its own body.
//! - `signature`: the header on one line (`def m(a, b = 1)`, `class C <
//!   Base`), prefixed with `private ` / `protected ` /
//!   `module_function `, and suffixed with ` #: -> T` when a Sorbet `sig`
//!   or an RBS comment declares the return type `T` (the resolver reads it
//!   for `$ret:` receivers; `self` / `T.attached_class` become `::<the
//!   class>`). An accessor of an instance variable whose type is known
//!   gets ` #: -> T` too.
//! - `doc`: the comment lines right above (RBS `#:` lines and `# @…`
//!   annotations dropped), ≤ 1000 characters.
//! - `exported`: public (the visibility in effect: `private` / `protected`
//!   with no arguments, `private :m`, `private def m`).
//! - `is_test`: everything in a test file (`test/`, `spec/`, `*_test.rb`,
//!   `*_spec.rb`), test blocks, and `test_*` methods of a test class (a
//!   superclass ending in `Test` / `TestCase`). Because this depends on the
//!   path, [`LanguagePack::facts_depend_on_path`] is true.
//!
//! # References
//!
//! `enclosing` is the innermost definition (a method, a test block, a
//! class body for class-level code, a constant for its value), `None` at
//! top level.
//!
//! - `Call`: `m(…)`, `recv.m`, `recv&.m`, and a bare identifier that is
//!   not a local variable (receiver below); `super` / `super(…)` as a
//!   call of the enclosing method's name with receiver `$super`;
//!   `send(:m)` / `public_send(:m)` / `__send__(:m)` also call `m` on the
//!   same receiver; `&method(:m)` calls `m`. `&:sym` is not followed (its
//!   receiver is a block element of unknown type).
//! - `Read`: constants (`Foo`, `A::B` → `B` with receiver `A`, `::Foo` →
//!   receiver `::`), also the constant receiver of a call.
//! - `Type`: classes in `rescue` clauses. `Throw`: the class of `raise
//!   Foo` / `raise Foo.new(…)`.
//! - `Write`: `self.a = v` / `obj.a = v` (the attribute `a`).
//! - `EnvVar`: `ENV["X"]`, `ENV.fetch("X")` (with or without a default),
//!   `ENV.key?("X")` and the other one-name readers — literal names only
//!   (RFC 0003 §7.2).
//! - `Read` / `Type` / `Write` are deduplicated per `(enclosing, receiver,
//!   name)`; at most 20,000 references per file.
//!
//! Receivers: see `receivers.rs` (`self`, `self.class`, constant paths,
//! `$type:T`, `$super`, `$ret:<receiver>#<name>`, `?`).
//!
//! # Imports
//!
//! `require "a/b"`, `require_dependency`, `load "x.rb"`, `autoload :X,
//! "a/b"` → `module` = the path, `relative_level` 0; `require_relative
//! "x"`, `require File.expand_path("x", __dir__)`, `File.join(__dir__,
//! "x")` → `relative_level` 1 (relative to the file's directory). No names
//! are bound (Ruby constants are global). [`LanguagePack::resolve_import`]
//! maps a relative path to `<dir>/<x>.rb`, and a load-path one to
//! `<ancestor>/lib/<x>.rb` (the nearest ancestor of the requiring file
//! first, so a gem of a monorepo finds its own `lib/`), then
//! `<ancestor>/test|spec/<x>.rb` (`require "test_helper"`), then the only
//! `…/lib/<x>.rb` of the snapshot; anything else is external (a gem or the
//! standard library).
//!
//! # Package identity
//!
//! [`parse_gem_manifest`] reads a `*.gemspec` (`spec.name`,
//! `add_dependency` / `add_runtime_dependency` /
//! `add_development_dependency`) and a `Gemfile` (`gem "x"`) with the Ruby
//! grammar, literal strings only; the organization layer
//! (`rueca-index`, `link.rs`) turns them into the gem's package and its
//! requirements.
//!
//! # Limitations
//!
//! - No metaprogramming beyond `define_method` / `alias_method` with
//!   literal names: `method_missing`, `class_eval` strings, Rails DSLs
//!   (`has_many`, `delegate`, `scope`) and RSpec `let` do not define
//!   methods.
//! - Extensionless scripts with a `#!…ruby` shebang are not detected.
//! - `tree-sitter-ruby` 0.23 does not parse an endless method whose body is
//!   a command call (`def m = raise E, "x"`); a same-length rewrite repairs
//!   it (`repair.rs`). Other syntax it misses stays a parse error.
//! - Types are file-local: a variable is typed when every assignment of it
//!   in its `def` builds the same class, or by a `sig` / RBS comment; the
//!   resolver follows declared return types across files.

mod builtins;
mod extract;
mod gems;
mod locals;
mod receivers;
mod refs;
mod repair;
mod tests;
mod types;

use rueca_extract::{Budget, ExtractError, LanguagePack, ProjectFiles};
use rueca_model::{FileFacts, Import, Language};

pub use extract::MAX_REFS;
pub use gems::{GemManifest, parse_gem_manifest};
pub use tests::is_test_file;

/// Ruby pack.
#[derive(Debug, Default, Clone, Copy)]
pub struct RubyPack;

impl LanguagePack for RubyPack {
    fn languages(&self) -> &'static [Language] {
        &[Language::Ruby]
    }

    fn extractor_version(&self) -> u32 {
        1
    }

    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError> {
        if language != Language::Ruby {
            return Err(ExtractError::Unsupported(language));
        }
        extract::extract_file(path, source, budget)
    }

    /// Ruby constants are global: every file's module name is empty.
    fn module_name(&self, _path: &str, _project: &ProjectFiles) -> String {
        String::new()
    }

    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
        resolve_require(from_path, import, project)
    }

    fn is_builtin(&self, name: &str) -> bool {
        builtins::is_builtin(name)
    }

    fn self_receivers(&self) -> &'static [&'static str] {
        &["self"]
    }

    fn facts_depend_on_path(&self) -> bool {
        true
    }
}

/// `a/./b/../c` → `a/c`; `None` above the root.
fn normalize_path(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

fn with_rb(path: &str) -> String {
    if path.ends_with(".rb") { path.to_string() } else { format!("{path}.rb") }
}

/// The file a `require` / `require_relative` / `load` / `autoload` names
/// (crate docs, *Imports*).
fn resolve_require(from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String> {
    let module = import.module.trim();
    if module.is_empty() {
        return None;
    }
    let dir = from_path.rsplit_once('/').map_or("", |(d, _)| d);
    if import.relative_level > 0 {
        let p = normalize_path(&format!("{dir}/{}", with_rb(module)))?;
        return project.contains(&p).then_some(p);
    }
    let rel = with_rb(module.trim_start_matches("./"));
    if let Some(p) = on_load_path(dir, &rel, project) {
        return Some(p);
    }
    let suffix = format!("/lib/{rel}");
    let mut hits = project.paths.iter().filter(|p| p.ends_with(&suffix));
    let first = hits.next()?;
    hits.next().is_none().then(|| first.clone())
}

/// `<ancestor>/{lib,test,spec}/<rel>` for the nearest ancestor of `dir`
/// that has it, or `<dir>/<rel>` when `dir` is itself a `lib/` root.
fn on_load_path(dir: &str, rel: &str, project: &ProjectFiles) -> Option<String> {
    let mut ancestor = Some(dir);
    while let Some(a) = ancestor {
        let found = ["lib", "test", "spec"]
            .iter()
            .map(|root| if a.is_empty() { format!("{root}/{rel}") } else { format!("{a}/{root}/{rel}") })
            .find(|p| project.contains(p));
        if found.is_some() {
            return found;
        }
        let own = format!("{a}/{rel}");
        if (a == "lib" || a.ends_with("/lib")) && project.contains(&own) {
            return Some(own);
        }
        ancestor = if a.is_empty() { None } else { Some(a.rsplit_once('/').map_or("", |(d, _)| d)) };
    }
    None
}

#[cfg(test)]
mod lib_tests {
    use super::*;
    use rueca_model::Span;

    fn imp(module: &str, level: u32) -> Import {
        Import {
            module: module.into(),
            relative_level: level,
            names: vec![],
            module_alias: None,
            type_only: false,
            reexport: false,
            span: Span::default(),
        }
    }

    #[test]
    fn requires_resolve_to_files() {
        let project = ProjectFiles {
            paths: [
                "lib/rack.rb",
                "lib/rack/utils.rb",
                "lib/rack/mock.rb",
                "test/helper.rb",
                "test/spec_utils.rb",
                "gems/other/lib/other/x.rb",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            configs: Default::default(),
        };
        let r = |from: &str, m: &str, l: u32| resolve_require(from, &imp(m, l), &project);
        assert_eq!(r("lib/rack/mock.rb", "utils", 1).as_deref(), Some("lib/rack/utils.rb"));
        assert_eq!(r("lib/rack/mock.rb", "../rack", 1).as_deref(), Some("lib/rack.rb"));
        assert_eq!(r("lib/rack/mock.rb", "rack/utils", 0).as_deref(), Some("lib/rack/utils.rb"));
        assert_eq!(r("test/spec_utils.rb", "rack/utils", 0).as_deref(), Some("lib/rack/utils.rb"));
        assert_eq!(r("test/spec_utils.rb", "helper", 0).as_deref(), Some("test/helper.rb"));
        assert_eq!(r("lib/rack.rb", "other/x", 0).as_deref(), Some("gems/other/lib/other/x.rb"));
        assert_eq!(r("lib/rack.rb", "json", 0), None);
        assert_eq!(r("lib/rack.rb", "../../x", 1), None);
    }
}
