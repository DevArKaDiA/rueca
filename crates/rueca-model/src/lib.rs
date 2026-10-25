//! Core data model shared by every Rueca crate.
//!
//! Three layers of data flow through the system:
//!
//! 1. [`FileFacts`] — what an extractor learns from one file in isolation
//!    (definitions, unresolved references, imports). Pure function of the file
//!    bytes and the extractor version.
//! 2. [`ResolvedGraph`] — the per-snapshot graph produced by the resolver from
//!    all facts of a snapshot: nodes, edges with provenance, and the file table.
//! 3. Segments (in `rueca-segment`) — the immutable on-disk form of a
//!    [`ResolvedGraph`] plus its search indexes.
//!
//! This crate has no I/O and no dependencies beyond `serde`.

use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Languages
// ---------------------------------------------------------------------------

/// Languages with an extractor. `JavaScript` covers `.js/.jsx/.mjs/.cjs`;
/// `Tsx` is TypeScript with JSX. New variants are appended (segments store
/// the variant index).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Php,
    Markdown,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Php => "php",
            Language::Markdown => "markdown",
        }
    }

    /// Detects the language from a repository-relative path's extension.
    pub fn from_path(path: &str) -> Option<Language> {
        let ext = path.rsplit_once('.').map(|(_, e)| e)?;
        match ext {
            "py" | "pyi" => Some(Language::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
            "ts" | "mts" | "cts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "php" => Some(Language::Php),
            "md" | "markdown" => Some(Language::Markdown),
            _ => None,
        }
    }

    /// TypeScript, TSX and JavaScript share module semantics.
    pub fn family(self) -> LanguageFamily {
        match self {
            Language::Python => LanguageFamily::Python,
            Language::JavaScript | Language::TypeScript | Language::Tsx => LanguageFamily::EcmaScript,
            Language::Php => LanguageFamily::Php,
            Language::Markdown => LanguageFamily::Markdown,
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageFamily {
    Python,
    EcmaScript,
    Php,
    Markdown,
}

impl LanguageFamily {
    /// Separator between a module name and a definition's local qualified
    /// name in global qualified names: Python `pkg.mod.Class.method`,
    /// ECMAScript `src/util/index:formatAmount`, PHP `App\Models\User::save`
    /// (module = namespace), Markdown `docs/guide.md:Install > Linux`.
    pub fn module_separator(self) -> &'static str {
        match self {
            LanguageFamily::Python => ".",
            LanguageFamily::EcmaScript | LanguageFamily::Markdown => ":",
            LanguageFamily::Php => "\\",
        }
    }
}

// ---------------------------------------------------------------------------
// Spans
// ---------------------------------------------------------------------------

/// A source range. Bytes are 0-based offsets; lines and columns are 0-based
/// (columns in bytes). Display layers convert lines to 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Span {
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub start_col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

impl Span {
    pub fn contains(&self, other: &Span) -> bool {
        self.start_byte <= other.start_byte && other.end_byte <= self.end_byte
    }
    /// 1-based inclusive line range, for display.
    pub fn lines_1based(&self) -> (u32, u32) {
        (self.start_line + 1, self.end_line + 1)
    }
}

// ---------------------------------------------------------------------------
// Labels and edge kinds
// ---------------------------------------------------------------------------

/// Node labels (RFC 0001 §6.2). The alpha only produces the code subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Label {
    Repository,
    Folder,
    File,
    Module,
    Package,
    Function,
    Method,
    Class,
    Interface,
    Trait,
    Struct,
    Enum,
    TypeAlias,
    Field,
    Variable,
    Constant,
    Route,
    RpcMethod,
    Topic,
    EnvVar,
    Test,
    /// A heading section of a document (Markdown); nested by `DEFINES`.
    Section,
}

impl Label {
    pub const ALL: &'static [Label] = &[
        Label::Repository,
        Label::Folder,
        Label::File,
        Label::Module,
        Label::Package,
        Label::Function,
        Label::Method,
        Label::Class,
        Label::Interface,
        Label::Trait,
        Label::Struct,
        Label::Enum,
        Label::TypeAlias,
        Label::Field,
        Label::Variable,
        Label::Constant,
        Label::Route,
        Label::RpcMethod,
        Label::Topic,
        Label::EnvVar,
        Label::Test,
        Label::Section,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Label::Repository => "Repository",
            Label::Folder => "Folder",
            Label::File => "File",
            Label::Module => "Module",
            Label::Package => "Package",
            Label::Function => "Function",
            Label::Method => "Method",
            Label::Class => "Class",
            Label::Interface => "Interface",
            Label::Trait => "Trait",
            Label::Struct => "Struct",
            Label::Enum => "Enum",
            Label::TypeAlias => "TypeAlias",
            Label::Field => "Field",
            Label::Variable => "Variable",
            Label::Constant => "Constant",
            Label::Route => "Route",
            Label::RpcMethod => "RpcMethod",
            Label::Topic => "Topic",
            Label::EnvVar => "EnvVar",
            Label::Test => "Test",
            Label::Section => "Section",
        }
    }

    pub fn parse(s: &str) -> Option<Label> {
        Label::ALL.iter().copied().find(|l| l.as_str().eq_ignore_ascii_case(s))
    }

    /// Labels that can be the source of a `CALLS` edge.
    pub fn is_callable(self) -> bool {
        matches!(self, Label::Function | Label::Method | Label::Test)
    }

    /// Labels that own members (methods, fields).
    pub fn is_type(self) -> bool {
        matches!(self, Label::Class | Label::Interface | Label::Trait | Label::Struct | Label::Enum | Label::TypeAlias)
    }
}

impl fmt::Display for Label {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Edge types (RFC 0001 §6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeKind {
    Contains,
    Defines,
    Imports,
    Calls,
    References,
    Inherits,
    Implements,
    Overrides,
    Decorates,
    Reads,
    Writes,
    Throws,
    Tests,
}

impl EdgeKind {
    pub const ALL: &'static [EdgeKind] = &[
        EdgeKind::Contains,
        EdgeKind::Defines,
        EdgeKind::Imports,
        EdgeKind::Calls,
        EdgeKind::References,
        EdgeKind::Inherits,
        EdgeKind::Implements,
        EdgeKind::Overrides,
        EdgeKind::Decorates,
        EdgeKind::Reads,
        EdgeKind::Writes,
        EdgeKind::Throws,
        EdgeKind::Tests,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::Defines => "DEFINES",
            EdgeKind::Imports => "IMPORTS",
            EdgeKind::Calls => "CALLS",
            EdgeKind::References => "REFERENCES",
            EdgeKind::Inherits => "INHERITS",
            EdgeKind::Implements => "IMPLEMENTS",
            EdgeKind::Overrides => "OVERRIDES",
            EdgeKind::Decorates => "DECORATES",
            EdgeKind::Reads => "READS",
            EdgeKind::Writes => "WRITES",
            EdgeKind::Throws => "THROWS",
            EdgeKind::Tests => "TESTS",
        }
    }

    pub fn parse(s: &str) -> Option<EdgeKind> {
        EdgeKind::ALL.iter().copied().find(|k| k.as_str().eq_ignore_ascii_case(s))
    }

    /// Dense index, used by segment formats for per-kind adjacency.
    pub fn index(self) -> usize {
        EdgeKind::ALL.iter().position(|k| *k == self).unwrap()
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an edge was established (RFC 0001 §6.3). Ordered from strongest to
/// weakest; when two strategies produce the same edge the earlier one wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Strategy {
    /// Structural fact from the syntax tree (containment, definition).
    Syntactic,
    Scip,
    TypeResolved,
    ImportResolved,
    SameModule,
    UniqueName,
    Heuristic,
}

impl Strategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Syntactic => "syntactic",
            Strategy::Scip => "scip",
            Strategy::TypeResolved => "type_resolved",
            Strategy::ImportResolved => "import_resolved",
            Strategy::SameModule => "same_module",
            Strategy::UniqueName => "unique_name",
            Strategy::Heuristic => "heuristic",
        }
    }
}

// ---------------------------------------------------------------------------
// Facts (extractor output for one file)
// ---------------------------------------------------------------------------

/// Index of a [`Definition`] within its file's `definitions` vector.
pub type LocalDefId = u32;

/// A symbol defined in a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Definition {
    pub label: Label,
    /// Simple name, e.g. `issue`.
    pub name: String,
    /// Name qualified within the file's module, e.g. `InvoiceService.issue`.
    /// Never includes the module path itself. PHP has no file modules (its
    /// module name is empty): the name carries the declared namespace,
    /// `App\Models\User::save`.
    pub qualified_name: String,
    /// Innermost containing definition in the same file, if any.
    pub parent: Option<LocalDefId>,
    /// Full extent of the definition (signature + body).
    pub span: Span,
    /// Extent of the name identifier only.
    pub name_span: Span,
    /// One-line signature text, e.g. `def issue(self, invoice: Invoice) -> None`.
    pub signature: Option<String>,
    /// Docstring / leading doc comment, trimmed.
    pub doc: Option<String>,
    /// Raw decorator expressions without `@`, e.g. `app.route("/x")`.
    pub decorators: Vec<String>,
    /// Raw base-class / extended / implemented type expressions, e.g. `Base`, `abc.ABC`.
    pub bases: Vec<String>,
    /// Raw implemented interfaces (TS `implements`); empty for Python.
    pub implements: Vec<String>,
    /// Whether the symbol is visible to other modules (TS `export`; Python:
    /// top-level and not underscore-prefixed).
    pub exported: bool,
    /// True if this is a test (pytest `test_*`, `Test*` classes, `it`/`test` blocks).
    pub is_test: bool,
}

/// Kind of an unresolved reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RefKind {
    /// Function or method call: `foo()`, `obj.foo()`.
    Call,
    /// Constructor call: `new Foo()` (TS). Python class calls are `Call`.
    New,
    /// Read of a name in an expression position.
    Read,
    /// Assignment target.
    Write,
    /// Type annotation position.
    Type,
    /// Decorator application.
    Decorator,
    /// `raise X` / `throw new X`.
    Throw,
}

/// A use of a name that the resolver must link to a definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    pub kind: RefKind,
    /// Last identifier of the referenced expression: `join` for `os.path.join`.
    pub name: String,
    /// Text of the receiver / qualifier, if any: `os.path`, `self`, `this.repo`.
    pub receiver: Option<String>,
    /// Innermost enclosing definition (the caller), if any. `None` = module level.
    pub enclosing: Option<LocalDefId>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportedName {
    /// Name in the source module (`default` for TS default imports, `*` for star).
    pub name: String,
    /// Local binding, if different from `name`.
    pub alias: Option<String>,
}

/// An import statement, unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// Raw module specifier: `os.path`, `.models`, `./util`, `react`.
    pub module: String,
    /// Python relative level (number of leading dots); 0 for absolute and for TS.
    pub relative_level: u32,
    /// Imported names. Empty means the module itself is bound
    /// (`import os.path`, `import * as ns from "x"` uses `alias` on the module).
    pub names: Vec<ImportedName>,
    /// Local binding for whole-module imports (`import numpy as np`,
    /// `import * as ns from "x"`).
    pub module_alias: Option<String>,
    /// True for TS `import type` / `export type`.
    pub type_only: bool,
    /// True if this is a re-export (`export { x } from "y"`, `export * from "y"`).
    pub reexport: bool,
    pub span: Span,
}

/// Everything an extractor learns from one file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FileFacts {
    pub definitions: Vec<Definition>,
    pub references: Vec<Reference>,
    pub imports: Vec<Import>,
    /// Number of tree-sitter ERROR / MISSING nodes encountered.
    pub parse_errors: u32,
    pub line_count: u32,
}

// ---------------------------------------------------------------------------
// Resolved graph (resolver output for one snapshot)
// ---------------------------------------------------------------------------

/// Index into [`ResolvedGraph::nodes`].
pub type NodeId = u32;
/// Index into [`ResolvedGraph::files`].
pub type FileId = u32;

/// Snapshot identity. For a local working tree `commit` is the HEAD commit
/// (or `"worktree"` when not a git repo) and `dirty` tells whether
/// uncommitted changes were indexed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotInfo {
    /// `host/owner/name`, or `local/<dirname>` for an unhosted working tree.
    pub repo: String,
    pub commit: String,
    pub dirty: bool,
    /// Absolute root path the snapshot was indexed from (local mode only).
    pub root: Option<String>,
    /// Unix seconds.
    pub indexed_at: u64,
    pub rueca_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Repository-relative path with `/` separators.
    pub path: String,
    /// Git blob SHA-1 (hex) of the content as indexed.
    pub blob_sha: String,
    pub language: Option<Language>,
    pub size: u64,
    pub line_count: u32,
    /// Node id of the `File` node.
    pub node: NodeId,
    /// Extraction outcome: `ok`, `partial` (parse errors), `skipped:<reason>`.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub label: Label,
    pub name: String,
    /// Globally qualified name.
    /// - Python symbols: `<module>.<local qualified name>` (`pkg.mod.Class.method`).
    /// - ECMAScript symbols: `<module>:<local qualified name>` where the module
    ///   is the path without extension (`src/util/index:Class.method`); a colon
    ///   separates them because paths may contain dots.
    /// - PHP symbols: the fully qualified PHP name, members after `::`
    ///   (`App\Models\User::save`, `App\Models\User::$email`).
    /// - Files and folders: the repository-relative path. Repository: the repo id.
    pub qualified_name: String,
    pub file: Option<FileId>,
    pub span: Option<Span>,
    pub language: Option<Language>,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub exported: bool,
    pub is_test: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EdgeMeta {
    /// 0.0–1.0.
    pub confidence: f32,
    pub strategy: Strategy,
    /// Where the edge was observed (file + span), e.g. the call site.
    pub evidence: Option<(FileId, Span)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub src: NodeId,
    pub dst: NodeId,
    pub kind: EdgeKind,
    pub meta: EdgeMeta,
}

/// The per-snapshot graph. Invariants (checked by [`ResolvedGraph::validate`]):
/// every edge endpoint and every `file` reference is in range; every file has
/// a `File` node; nodes are deduplicated by `(label, qualified_name, file)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedGraph {
    pub snapshot: SnapshotInfo,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub files: Vec<FileEntry>,
}

impl ResolvedGraph {
    pub fn validate(&self) -> Result<(), String> {
        let n = self.nodes.len() as u32;
        for (i, e) in self.edges.iter().enumerate() {
            if e.src >= n || e.dst >= n {
                return Err(format!("edge {i} endpoint out of range"));
            }
            if let Some((f, _)) = e.meta.evidence
                && f as usize >= self.files.len()
            {
                return Err(format!("edge {i} evidence file out of range"));
            }
        }
        for (i, node) in self.nodes.iter().enumerate() {
            if let Some(f) = node.file
                && f as usize >= self.files.len()
            {
                return Err(format!("node {i} file out of range"));
            }
        }
        for (i, f) in self.files.iter().enumerate() {
            match self.nodes.get(f.node as usize) {
                Some(node) if node.label == Label::File => {}
                _ => return Err(format!("file {i} has no File node")),
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stable symbol identifiers (RFC 0001 §6.1)
// ---------------------------------------------------------------------------

/// Formats `<repo>@<commit>:<path>#<qualified-name>`. `qualified_name` is the
/// node's module-relative name; pass `None` for file nodes.
pub fn symbol_id(repo: &str, commit: &str, path: &str, qualified_name: Option<&str>) -> String {
    match qualified_name {
        Some(q) if !q.is_empty() => format!("{repo}@{commit}:{path}#{q}"),
        _ => format!("{repo}@{commit}:{path}"),
    }
}

/// Parsed form of a symbol identifier. `commit` may be absent (`repo:path#q`
/// is accepted too, meaning "the indexed snapshot").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolIdParts {
    pub repo: String,
    pub commit: Option<String>,
    pub path: String,
    pub qualified_name: Option<String>,
}

pub fn parse_symbol_id(id: &str) -> Option<SymbolIdParts> {
    let (head, qualified_name) = match id.split_once('#') {
        Some((h, q)) => (h, Some(q.to_string())),
        None => (id, None),
    };
    let (repo_commit, path) = head.split_once(':')?;
    let (repo, commit) = match repo_commit.rsplit_once('@') {
        Some((r, c)) => (r.to_string(), Some(c.to_string())),
        None => (repo_commit.to_string(), None),
    };
    if repo.is_empty() || path.is_empty() {
        return None;
    }
    Some(SymbolIdParts { repo, commit, path: path.to_string(), qualified_name })
}

/// Git blob SHA-1 of `content` (`sha1("blob <len>\0" + content)`), as hex.
/// Implemented without dependencies so every crate agrees on it.
pub fn git_blob_sha(content: &[u8]) -> String {
    let mut data = Vec::with_capacity(content.len() + 32);
    data.extend_from_slice(format!("blob {}\0", content.len()).as_bytes());
    data.extend_from_slice(content);
    let digest = sha1::digest(&data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

mod sha1 {
    //! Minimal SHA-1 (FIPS 180-4). Only used for git blob ids, never for security.
    pub fn digest(data: &[u8]) -> [u8; 20] {
        let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
        let bit_len = (data.len() as u64).wrapping_mul(8);
        let mut msg = data.to_vec();
        msg.push(0x80);
        while msg.len() % 64 != 56 {
            msg.push(0);
        }
        msg.extend_from_slice(&bit_len.to_be_bytes());
        for chunk in msg.chunks(64) {
            let mut w = [0u32; 80];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
            }
            for i in 16..80 {
                w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            }
            let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
            for (i, wi) in w.iter().enumerate() {
                let (f, k) = match i {
                    0..=19 => ((b & c) | ((!b) & d), 0x5A827999),
                    20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                    40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                    _ => (b ^ c ^ d, 0xCA62C1D6),
                };
                let temp = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
                e = d;
                d = c;
                c = b.rotate_left(30);
                b = a;
                a = temp;
            }
            h[0] = h[0].wrapping_add(a);
            h[1] = h[1].wrapping_add(b);
            h[2] = h[2].wrapping_add(c);
            h[3] = h[3].wrapping_add(d);
            h[4] = h[4].wrapping_add(e);
        }
        let mut out = [0u8; 20];
        for (i, v) in h.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_sha_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(git_blob_sha(b"hello\n"), "ce013625030ba8dba906f756967f9e9ca394464a");
        assert_eq!(git_blob_sha(b""), "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391");
    }

    #[test]
    fn symbol_id_round_trip() {
        let id = symbol_id("github.com/acme/billing", "3f2a91c", "src/a.py", Some("A.run"));
        assert_eq!(id, "github.com/acme/billing@3f2a91c:src/a.py#A.run");
        let p = parse_symbol_id(&id).unwrap();
        assert_eq!(p.repo, "github.com/acme/billing");
        assert_eq!(p.commit.as_deref(), Some("3f2a91c"));
        assert_eq!(p.path, "src/a.py");
        assert_eq!(p.qualified_name.as_deref(), Some("A.run"));
        let p = parse_symbol_id("local/x:src/a.py").unwrap();
        assert_eq!(p.commit, None);
        assert_eq!(p.qualified_name, None);
    }

    #[test]
    fn language_detection() {
        assert_eq!(Language::from_path("a/b.py"), Some(Language::Python));
        assert_eq!(Language::from_path("a/b.tsx"), Some(Language::Tsx));
        assert_eq!(Language::from_path("a/b.mjs"), Some(Language::JavaScript));
        assert_eq!(Language::from_path("Makefile"), None);
    }
}
