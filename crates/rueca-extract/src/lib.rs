//! Extraction contract for Rueca language packs.
//!
//! A [`LanguagePack`] turns one file into [`FileFacts`] (pure, deterministic,
//! no I/O) and knows its language's module semantics: how a path maps to a
//! module name and how an import specifier maps to a file in the project.
//! The generic resolver (`rueca-resolve`) combines facts from every file of a
//! snapshot using these two hooks.
//!
//! # Hooks for the next languages (RFC 0003 §9.2)
//!
//! Every hook below has a default that keeps the alpha behaviour, so a pack
//! overrides only what its language needs:
//!
//! | Hook | Default | Override when |
//! |---|---|---|
//! | [`LanguagePack::module_name_for`] | [`LanguagePack::module_name`] | the module comes from the file itself (Java `package`, Go `package x_test`) |
//! | [`LanguagePack::module_scope`] | [`ModuleScope::File`] | names are visible across the files of a package or directory without imports (Go, Java, Terraform) |
//! | [`LanguagePack::implicit_self`] | `false` | methods call members without `this` (C++, Java) |
//! | [`LanguagePack::overloads`] | `false` | several definitions share a name in one scope (Java, C++) |
//! | [`LanguagePack::overrides`] | `false` | the resolver should add `OVERRIDES` edges (Java, C++) |
//! | [`LanguagePack::facts_depend_on_path`] | `false` | extraction reads the path, not only the bytes (data file kinds chosen by name) |
//! | [`LanguagePack::resolve_import`] returning a directory | — | the import names a package or folder (Go, Java on-demand, Terraform modules) |
//!
//! Reserved texts a pack may put in facts:
//!
//! - Receivers: [`SELF_RECEIVER`] (`$self`, a pack-normalized self
//!   receiver; list it in [`LanguagePack::self_receivers`]),
//!   [`type_receiver`] (`$type:T`, a receiver whose declared type `T` is
//!   known in the file), [`namespace_receiver`] (`$ns:a::b`, a C++
//!   namespace-qualified name) and [`member_receiver`] (`$member:a.b`, a
//!   chain of members of the enclosing type whose types are declared in
//!   another file).
//! - Call and `New` names: [`with_arg_types`] (`value$args:long,?`, the
//!   static types of the arguments, for choosing overloads; the resolver
//!   strips it before any lookup).
//! - `DataRef` receivers: [`DATA_REF_QN`] and [`DATA_REF_ANY`] (see
//!   `rueca_model::RefKind::DataRef`).
//! - [`ExtractError::Skipped`] to keep a file listed but unextracted with a
//!   reason (`skipped:data-shape`).
//!
//! This crate also provides tree-sitter helpers shared by every pack:
//! budget-limited parsing, span conversion, and node text access.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use rueca_model::{FileFacts, Import, Language, Span};
use tree_sitter::{Node, Parser, Tree, TreeCursor};

pub use rueca_model;
pub use tree_sitter;

// ---------------------------------------------------------------------------
// Budget and errors
// ---------------------------------------------------------------------------

/// Resource limits for extracting one file. Files over the limits are recorded
/// as coverage gaps by the indexer, never crash it.
#[derive(Debug, Clone)]
pub struct Budget {
    pub max_bytes: usize,
    pub max_parse_time: Duration,
    /// Maximum syntax-tree nesting depth walked by extractors.
    pub max_depth: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget { max_bytes: 2 * 1024 * 1024, max_parse_time: Duration::from_secs(5), max_depth: 512 }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("file too large ({size} bytes > {limit})")]
    TooLarge { size: usize, limit: usize },
    #[error("parse timed out")]
    Timeout,
    #[error("parser failed: {0}")]
    Parser(String),
    #[error("unsupported language: {0}")]
    Unsupported(Language),
    #[error("invalid query: {0}")]
    Query(String),
    /// The pack decided not to extract the file (RFC 0003 §4.5: `data`,
    /// `generated`, `data-size`, `data-shape`). The indexer records status
    /// `skipped:<reason>`; the file stays listed and content-searchable.
    #[error("skipped: {0}")]
    Skipped(String),
}

// ---------------------------------------------------------------------------
// Reserved texts in facts
// ---------------------------------------------------------------------------

/// Pack-normalized self receiver (RFC 0003 §9.2). Go names its receiver per
/// method (`func (s *Server)`); the pack rewrites it to `$self` and lists
/// `$self` in [`LanguagePack::self_receivers`]. For `$self` the resolver
/// finds the receiver type from the enclosing type definition, else from the
/// method's local qualified-name prefix (`Server` for `Server.Handle`).
pub const SELF_RECEIVER: &str = "$self";

/// Prefix of a typed receiver: `$type:<T>` means "the receiver's type is
/// `T`" as the file says it: a local, parameter or field declared with a
/// type, a value constructed from `T` (`x = new T()`, Python `x = T()`), or
/// the declared return type of a function or method of the same file. `T` is written as in source (`Server`, `http.Server`,
/// `ns::Foo`, `List<String>`); generic arguments, pointers and references
/// are ignored. The resolver resolves `T` like a `Type` reference and looks
/// the member up in it (`type_resolved`, 0.85).
pub const TYPE_RECEIVER_PREFIX: &str = "$type:";

/// `$type:<ty>` receiver text.
pub fn type_receiver(ty: &str) -> String {
    format!("{TYPE_RECEIVER_PREFIX}{ty}")
}

/// Separator between a call's name and the static types of its arguments:
/// `value$args:long`, `fromJson$args:String,?` (see [`with_arg_types`]).
/// `$args:` cannot occur in an identifier of any supported language. The
/// resolver strips it before every lookup and uses the types only to choose
/// among overloads (RFC 0003 §10.1): an overload whose parameter type
/// cannot accept a known argument type is dropped, and of the rest the one
/// whose parameter types equal the known argument types wins.
pub const ARG_TYPES_SEPARATOR: &str = "$args:";

/// A `Call` / `New` reference name carrying the static types of its
/// arguments, in the pack's overload-suffix form (`int`, `String`,
/// `byte[]`); `None` is an unknown type, written `?`. Returns the bare name
/// when no type is known.
pub fn with_arg_types(name: &str, types: &[Option<String>]) -> String {
    if types.iter().all(Option::is_none) {
        return name.to_string();
    }
    let list: Vec<&str> = types.iter().map(|t| t.as_deref().unwrap_or("?")).collect();
    format!("{name}{ARG_TYPES_SEPARATOR}{}", list.join(","))
}

/// Splits a reference name written by [`with_arg_types`] into the name and
/// the argument types (`?` = unknown).
pub fn split_arg_types(name: &str) -> (&str, Option<Vec<&str>>) {
    match name.split_once(ARG_TYPES_SEPARATOR) {
        Some((n, list)) if !list.is_empty() => (n, Some(list.split(',').collect())),
        Some((n, _)) => (n, None),
        None => (name, None),
    }
}

/// Prefix of a namespace-qualified name (C++): `$ns:<path>` means "the
/// name was written `path::name` and `path` is a namespace" (`$ns:` alone
/// for `::name`). The resolver looks up the global qualified name
/// `path::name`, then `path::name` inside each namespace enclosing the
/// reference (innermost first), else resolves the bare name through the
/// usual rungs except implicit self (a qualified name is never an implicit
/// member). Overload suffixes are optional.
pub const NAMESPACE_RECEIVER_PREFIX: &str = "$ns:";

/// `$ns:<path>` receiver text.
pub fn namespace_receiver(path: &str) -> String {
    format!("{NAMESPACE_RECEIVER_PREFIX}{path}")
}

/// Prefix of a member-chain receiver: `$member:a.b` means "the receiver
/// is member `a` of the enclosing type (implicit self, or `this->a`), then
/// member `b` of `a`'s declared type". A pack emits it when the members'
/// types are declared in another file (C++ class fields in a header used in
/// an out-of-line body in a `.cpp`). The resolver finds each member through
/// the enclosing type and its bases, takes the declared type from the
/// member's first `Type` reference, and looks the name up in the last
/// type; when the first segment is not a member it resolves like an
/// unknown receiver.
pub const MEMBER_RECEIVER_PREFIX: &str = "$member:";

/// `$member:<chain>` receiver text (`chain` = member names joined by `.`).
pub fn member_receiver(chain: &str) -> String {
    format!("{MEMBER_RECEIVER_PREFIX}{chain}")
}

/// `DataRef` receiver: the reference name is a global qualified name
/// (`pyproject` `[project.scripts]` → `pkg.mod.main`).
pub const DATA_REF_QN: &str = "$qn";

/// `DataRef` receiver: the key path may be in any data file of the snapshot
/// (Kubernetes `configMapKeyRef` to a ConfigMap in another manifest).
pub const DATA_REF_ANY: &str = "$any";

/// How files share a module namespace (RFC 0003 §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModuleScope {
    /// Every file is its own module (Python, ECMAScript, Markdown, data).
    #[default]
    File,
    /// Every file of the same family with the same module name is one
    /// package (Go and Java): unqualified names resolve across its files, an
    /// import of its directory binds all of them, and top-level definitions
    /// named `T.m` are members of type `T` even when declared in another
    /// file of the package (Go methods).
    Package,
    /// Like [`ModuleScope::Package`], but the package is every file of the
    /// same language in one directory, whatever its module name (Terraform:
    /// a module is the `.tf` files of a directory while each file keeps its
    /// path as module name).
    Directory,
}

// ---------------------------------------------------------------------------
// Project context
// ---------------------------------------------------------------------------

/// What a pack may know about the whole snapshot when computing module names
/// and resolving imports. Built once per snapshot by the indexer.
#[derive(Debug, Clone, Default)]
pub struct ProjectFiles {
    /// Every repository-relative path in the snapshot (all languages, `/` separators).
    pub paths: BTreeSet<String>,
    /// Contents of small configuration files that affect module resolution,
    /// keyed by repository-relative path: `tsconfig*.json`, `jsconfig.json`,
    /// `package.json`, `pyproject.toml`, `setup.cfg`, `setup.py`,
    /// `composer.json`, `go.mod`, `go.work`, `pom.xml`, `build.gradle(.kts)`,
    /// `settings.gradle(.kts)`, `compile_commands.json`, `compile_flags.txt`
    /// (see [`ProjectFiles::is_config_file`], size limits in
    /// [`ProjectFiles::max_config_bytes`]).
    pub configs: BTreeMap<String, String>,
}

impl ProjectFiles {
    pub fn contains(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    /// Config file names the indexer must load into [`ProjectFiles::configs`].
    pub fn is_config_file(path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        matches!(
            name,
            "package.json"
                | "jsconfig.json"
                | "pyproject.toml"
                | "setup.cfg"
                | "setup.py"
                | "composer.json"
                // Go (RFC 0003 §10.2)
                | "go.mod"
                | "go.work"
                // Java (§10.4)
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "settings.gradle"
                | "settings.gradle.kts"
                // C / C++ (§10.3)
                | "compile_commands.json"
                | "compile_flags.txt"
        ) || (name.starts_with("tsconfig") && name.ends_with(".json"))
    }

    /// Largest config file the indexer loads for `path`: 8 MiB for
    /// `compile_commands.json` (one entry per translation unit), 1 MiB
    /// otherwise.
    pub fn max_config_bytes(path: &str) -> u64 {
        let name = path.rsplit('/').next().unwrap_or(path);
        if name == "compile_commands.json" { 8 * 1024 * 1024 } else { 1024 * 1024 }
    }

    /// Whether `path` is a directory of the snapshot (some file lives under it).
    /// The empty path is the repository root.
    pub fn is_dir(&self, path: &str) -> bool {
        if path.is_empty() {
            return !self.paths.is_empty();
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        self.paths.range(prefix.clone()..).next().is_some_and(|p| p.starts_with(&prefix))
    }
}

// ---------------------------------------------------------------------------
// Language pack trait
// ---------------------------------------------------------------------------

pub trait LanguagePack: Send + Sync {
    /// Languages this pack extracts.
    fn languages(&self) -> &'static [Language];

    /// Bumped whenever extraction output changes for the same input bytes.
    /// Part of the facts cache key.
    fn extractor_version(&self) -> u32;

    /// Extracts facts from one file. Must be deterministic: same `path`,
    /// `language`, `source` and version → identical output.
    fn extract(
        &self,
        path: &str,
        language: Language,
        source: &[u8],
        budget: &Budget,
    ) -> Result<FileFacts, ExtractError>;

    /// The module name other files use to refer to `path`.
    /// Python: dotted path relative to its source root (`pkg.sub.mod`,
    /// `pkg` for `pkg/__init__.py`). ECMAScript: the path without extension
    /// (`src/util/index`). An empty module name means the local qualified
    /// names are already global (PHP: definitions carry their namespace,
    /// `App\Models\User::save`; C/C++).
    fn module_name(&self, path: &str, project: &ProjectFiles) -> String;

    /// The module name of `path`, given the file's facts (`None` when the
    /// file was not extracted). This is what the resolver calls; the default
    /// ignores the facts and returns [`LanguagePack::module_name`]. Override
    /// it when the module is declared in the file rather than implied by
    /// its path: Java reads its `package` clause, Go its `package x_test`
    /// (RFC 0003 open question 2, decided in favour of this additive hook).
    fn module_name_for(&self, path: &str, facts: Option<&FileFacts>, project: &ProjectFiles) -> String {
        let _ = facts;
        self.module_name(path, project)
    }

    /// Resolves an import written in `from_path` to the repository-relative
    /// path of the imported file, if it is part of the project. Returns `None`
    /// for third-party / builtin modules.
    ///
    /// The result may also be a **directory** (no trailing `/`; `""` is the
    /// repository root): a Go package, a Java on-demand import, a Terraform
    /// module source, a compose build context. The resolver then adds an
    /// `IMPORTS` edge from the file to the `Folder` node and, for
    /// [`ModuleScope::Package`] packs, binds the package of that directory
    /// (its files of the same family with one module name).
    fn resolve_import(&self, from_path: &str, import: &Import, project: &ProjectFiles) -> Option<String>;

    /// Names that are always in scope (builtins, globals). References to
    /// them are never reported as unresolved and never fuzzy-matched.
    fn is_builtin(&self, name: &str) -> bool;

    /// Receiver texts that denote "the current instance" (`self`, `cls`, `this`;
    /// [`SELF_RECEIVER`] for packs that normalize receivers).
    fn self_receivers(&self) -> &'static [&'static str];

    /// Whether files of `language` share module namespaces by package or
    /// directory (RFC 0003 §9.2). Takes the language because one pack may
    /// serve several (the data pack: Terraform is directory-scoped, YAML is
    /// not).
    fn module_scope(&self, language: Language) -> ModuleScope {
        let _ = language;
        ModuleScope::File
    }

    /// When true, a receiver-less reference inside a method is first looked
    /// up as a member of the enclosing class (and its bases, then outer
    /// classes), after local scope and before imports: `type_resolved` 0.90
    /// (0.85 when inherited). C++ and Java call members without `this`.
    fn implicit_self(&self) -> bool {
        false
    }

    /// When true, definitions of this pack may be overloaded: overloads carry
    /// a parameter suffix in their local qualified name (`Parser.parse(String)`,
    /// `Parser.parse(Reader)`; non-overloaded names stay bare) and the
    /// resolver chooses among them by `Reference::arity` and by the argument
    /// types a reference name carries ([`with_arg_types`]), else links all
    /// of the remaining ones at × 0.8 confidence (RFC 0003 §10.1).
    fn overloads(&self) -> bool {
        false
    }

    /// When true, the resolver adds `OVERRIDES` edges from a method to the
    /// method of a resolved base type with the same name, paired by
    /// signature when the base has overloads (same parameter count from the
    /// overload suffix or the definition's signature, then the same
    /// parameter types), `type_resolved` 0.90 (RFC 0003 §10.1).
    fn overrides(&self) -> bool {
        false
    }

    /// Whether [`LanguagePack::extract`] output depends on `path` as well as
    /// the bytes (a data file's kind is chosen by name: `compose.yaml`,
    /// `.github/workflows/*.yml`, `application.yml`; files under
    /// `fixtures/` are skipped). The facts cache is keyed by blob, so such
    /// packs get the path in the key too; identical files at two paths are
    /// then extracted once per path.
    fn facts_depend_on_path(&self) -> bool {
        false
    }
}

/// Registry of packs, keyed by language.
#[derive(Default)]
pub struct Registry {
    packs: Vec<Box<dyn LanguagePack>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, pack: Box<dyn LanguagePack>) {
        self.packs.push(pack);
    }

    pub fn for_language(&self, language: Language) -> Option<&dyn LanguagePack> {
        self.packs.iter().find(|p| p.languages().contains(&language)).map(|b| b.as_ref())
    }

    pub fn for_path(&self, path: &str) -> Option<(Language, &dyn LanguagePack)> {
        let lang = Language::from_path(path)?;
        self.for_language(lang).map(|p| (lang, p))
    }

    /// The registered packs, in registration order (to wrap them in another
    /// registry, e.g. the organization linker's).
    pub fn into_packs(self) -> Vec<Box<dyn LanguagePack>> {
        self.packs
    }

    /// Cache key component combining every pack's version.
    pub fn versions(&self) -> Vec<(Language, u32)> {
        let mut v: Vec<_> =
            self.packs.iter().flat_map(|p| p.languages().iter().map(move |l| (*l, p.extractor_version()))).collect();
        v.sort();
        v
    }
}

// ---------------------------------------------------------------------------
// tree-sitter helpers
// ---------------------------------------------------------------------------

/// Parses `source` with `language`, enforcing size and time budgets.
pub fn parse(language: &tree_sitter::Language, source: &[u8], budget: &Budget) -> Result<Tree, ExtractError> {
    if source.len() > budget.max_bytes {
        return Err(ExtractError::TooLarge { size: source.len(), limit: budget.max_bytes });
    }
    let mut parser = Parser::new();
    parser.set_language(language).map_err(|e| ExtractError::Parser(e.to_string()))?;
    let deadline = Instant::now() + budget.max_parse_time;
    let mut timed_out = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if Instant::now() > deadline {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let len = source.len();
    let mut read =
        |offset: usize, _: tree_sitter::Point| -> &[u8] { if offset < len { &source[offset..] } else { &[] } };
    let tree = parser.parse_with_options(
        &mut read,
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    );
    match tree {
        Some(t) => Ok(t),
        None if timed_out => Err(ExtractError::Timeout),
        None => Err(ExtractError::Parser("parser returned no tree".into())),
    }
}

/// Converts a tree-sitter node range to a [`Span`].
pub fn span_of(node: Node<'_>) -> Span {
    let s = node.start_position();
    let e = node.end_position();
    Span {
        start_byte: node.start_byte() as u32,
        end_byte: node.end_byte() as u32,
        start_line: s.row as u32,
        start_col: s.column as u32,
        end_line: e.row as u32,
        end_col: e.column as u32,
    }
}

/// UTF-8 text of a node (lossy for invalid bytes).
pub fn text_of<'s>(node: Node<'_>, source: &'s [u8]) -> std::borrow::Cow<'s, str> {
    String::from_utf8_lossy(&source[node.start_byte()..node.end_byte()])
}

/// Counts ERROR and MISSING nodes in a tree.
pub fn count_errors(tree: &Tree) -> u32 {
    if !tree.root_node().has_error() {
        return 0;
    }
    let mut count = 0;
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            count += 1;
        }
        if node.has_error() && cursor.goto_first_child() {
            continue;
        }
        if !skip_subtree(&mut cursor) {
            return count;
        }
    }
}

/// Moves `cursor` to the next node in document order after its node's
/// subtree (the next sibling of the node or of its nearest ancestor that
/// has one); `false` past the last node.
fn skip_subtree(cursor: &mut TreeCursor<'_>) -> bool {
    loop {
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
    }
}

/// Number of lines in `source` (a trailing newline does not start a new line).
pub fn line_count(source: &[u8]) -> u32 {
    if source.is_empty() {
        return 0;
    }
    let n = source.iter().filter(|b| **b == b'\n').count() as u32;
    if source.ends_with(b"\n") { n } else { n + 1 }
}

/// First line of `text`, trimmed and truncated to `max` chars — for signatures.
pub fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > max {
        let mut s: String = line.chars().take(max).collect();
        s.push('…');
        s
    } else {
        line.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_types_in_names() {
        assert_eq!(with_arg_types("value", &[Some("long".into())]), "value$args:long");
        assert_eq!(with_arg_types("f", &[Some("String".into()), None]), "f$args:String,?");
        assert_eq!(with_arg_types("f", &[None, None]), "f");
        assert_eq!(with_arg_types("f", &[]), "f");
        assert_eq!(split_arg_types("f$args:String,?"), ("f", Some(vec!["String", "?"])));
        assert_eq!(split_arg_types("f"), ("f", None));
    }

    #[test]
    fn line_counting() {
        assert_eq!(line_count(b""), 0);
        assert_eq!(line_count(b"a"), 1);
        assert_eq!(line_count(b"a\n"), 1);
        assert_eq!(line_count(b"a\nb"), 2);
    }

    #[test]
    fn config_detection() {
        assert!(ProjectFiles::is_config_file("web/tsconfig.base.json"));
        assert!(ProjectFiles::is_config_file("pyproject.toml"));
        assert!(ProjectFiles::is_config_file("packages/core/composer.json"));
        assert!(!ProjectFiles::is_config_file("src/config.json"));
        for p in [
            "go.mod",
            "tools/go.work",
            "pom.xml",
            "app/build.gradle",
            "app/build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
            "build/compile_commands.json",
            "compile_flags.txt",
        ] {
            assert!(ProjectFiles::is_config_file(p), "{p}");
        }
        assert!(!ProjectFiles::is_config_file("go.sum"));
        assert!(!ProjectFiles::is_config_file("App.csproj"));
        assert_eq!(ProjectFiles::max_config_bytes("build/compile_commands.json"), 8 * 1024 * 1024);
        assert_eq!(ProjectFiles::max_config_bytes("go.mod"), 1024 * 1024);
    }

    #[test]
    fn directories() {
        let p = ProjectFiles {
            paths: ["a/b/c.go", "a/bb.go", "z.txt"].into_iter().map(String::from).collect(),
            configs: Default::default(),
        };
        assert!(p.is_dir("a") && p.is_dir("a/b") && p.is_dir(""));
        assert!(!p.is_dir("a/b/c.go") && !p.is_dir("a/bb") && !p.is_dir("b"));
        assert_eq!(type_receiver("http.Server"), "$type:http.Server");
    }
}
