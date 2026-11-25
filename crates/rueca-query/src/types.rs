//! Result types of the tools, serialized exactly as `docs/tools.md`
//! specifies. Fields documented as optional are omitted when absent; every
//! other field is always present (possibly `null`).

use schemars::JsonSchema;
use serde::Serialize;

/// Where an item of a response that reads several snapshots of one
/// repository comes from (RFC 0005 §6): the snapshots (view indexes of the
/// call, renumbered in the envelope) whose version is identical to this one,
/// and whether it is not identical in every requested snapshot of its
/// repository (another version, or absent: added or deleted on a ref).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RefMark {
    /// View indexes, ascending.
    pub in_: Vec<usize>,
    pub changed: bool,
    /// Every requested snapshot of the item's repository (view indexes): the
    /// envelope lists them all, so `in` and `changed` read against them.
    pub peers: Vec<usize>,
}

/// Brief symbol, used inside every result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Brief {
    /// Stable symbol id: `<repo>@<commit>:<path>#<local-qualified-name>`.
    pub id: String,
    /// Repository of the snapshot the symbol was read from (compact only).
    #[serde(skip)]
    #[schemars(skip)]
    pub repo: String,
    /// Ref of that snapshot (compact only).
    #[serde(skip)]
    #[schemars(skip)]
    pub ref_name: String,
    pub name: String,
    /// Global qualified name (`pkg.mod.Class.method`, `src/util/index:Class.method`).
    pub qualified_name: String,
    /// Node label (`Function`, `Method`, `Class`, …).
    pub label: String,
    /// Repository-relative path of the defining file.
    pub path: Option<String>,
    /// 1-based first line.
    pub start_line: Option<u32>,
    /// 1-based last line (inclusive).
    pub end_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SymbolSearchHit {
    #[serde(flatten)]
    pub brief: Brief,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SearchSymbolsResult {
    pub results: Vec<SymbolSearchHit>,
    pub truncated: bool,
    pub next_offset: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CodeMatch {
    pub path: String,
    /// 1-based line of the match.
    pub line: u32,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Vec<String>>,
    /// Innermost definition enclosing the match, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<Brief>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SearchCodeResult {
    pub matches: Vec<CodeMatch>,
    pub truncated: bool,
    pub files_scanned: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SymbolDetail {
    #[serde(flatten)]
    pub brief: Brief,
    pub language: Option<String>,
    pub signature: Option<String>,
    pub doc: Option<String>,
    pub exported: bool,
    pub is_test: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct GetSymbolResult {
    pub symbol: SymbolDetail,
    /// The defining container (class, function or file), via `DEFINES`.
    pub parent: Option<Brief>,
    pub children: Vec<Brief>,
    /// `INHERITS` / `IMPLEMENTS` targets.
    pub bases: Vec<Brief>,
    /// Reverse `INHERITS` / `IMPLEMENTS`.
    pub subtypes: Vec<Brief>,
    pub callers_count: u32,
    pub callees_count: u32,
    /// `EnvVar` and `Key` symbols only: distinct nodes that read it (incoming `READS`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readers_count: Option<u32>,
    /// `EnvVar` and `Key` symbols only: distinct nodes that set it (incoming `WRITES`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub writers_count: Option<u32>,
    /// Other matches when the lookup was ambiguous (max 20).
    pub candidates: Vec<Brief>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SnippetResult {
    pub path: String,
    pub language: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
    pub text: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<Brief>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TraceNode {
    #[serde(flatten)]
    pub brief: Brief,
    pub depth: u32,
    /// `callers` or `callees`.
    pub direction: String,
    /// Snapshot (index into the selection's views) the node is in.
    #[serde(skip)]
    #[schemars(skip)]
    pub view: usize,
    /// Several refs of the node's repository were read (compact only).
    #[serde(skip)]
    #[schemars(skip)]
    pub mark: Option<RefMark>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CallSite {
    pub path: String,
    /// 1-based line.
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TraceEdge {
    pub src: String,
    pub dst: String,
    pub kind: String,
    pub confidence: f64,
    pub strategy: String,
    pub call_site: Option<CallSite>,
    /// Several refs of the edge's repository were read (compact only): the
    /// snapshots whose walk found the edge.
    #[serde(skip)]
    #[schemars(skip)]
    pub mark: Option<RefMark>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TraceResult {
    /// Snapshot (view index) of the root.
    #[serde(skip)]
    #[schemars(skip)]
    pub root_view: usize,
    pub root: Brief,
    /// Several refs of the root's repository were read (compact only).
    #[serde(skip)]
    #[schemars(skip)]
    pub root_mark: Option<RefMark>,
    pub nodes: Vec<TraceNode>,
    pub edges: Vec<TraceEdge>,
    pub truncated: bool,
    /// Distinct test neighbours left out because `include_tests` was false.
    pub tests_excluded: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct OutlineSymbol {
    #[serde(flatten)]
    pub brief: Brief,
    pub signature: Option<String>,
    pub children: Vec<OutlineSymbol>,
    /// Several refs of the file's repository were read and the symbol's text
    /// is not the same in all of them (compact only).
    #[serde(skip)]
    #[schemars(skip)]
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FileOutlineResult {
    pub path: String,
    pub language: Option<String>,
    pub line_count: u32,
    pub symbols: Vec<OutlineSymbol>,
}

/// Another version of a `get_symbol` answer when several refs of its
/// repository are read (compact only; RFC 0005 §6).
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolVersion {
    /// View the version was read from.
    pub view: usize,
    pub symbol: SymbolDetail,
    pub callers_count: u32,
    pub callees_count: u32,
    pub mark: RefMark,
}

/// Another version of a `get_file_outline` answer when several refs of its
/// repository are read (compact only; RFC 0005 §6).
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineVersion {
    /// View the version was read from.
    pub view: usize,
    pub outline: FileOutlineResult,
    pub mark: RefMark,
}

/// One indexed snapshot of a repository in `list_repositories`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefSummary {
    /// The ref name (`main`, `pr/7`, `v1.2`, a commit).
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// First 12 hex digits of the commit (`worktree` outside git).
    pub commit: String,
}

/// One repository of `list_repositories`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepositorySummary {
    pub repo: String,
    /// Indexed root (local mode).
    pub root: Option<String>,
    /// The ref read when `ref` is not given.
    pub default_ref: String,
    /// Full commit of the default ref.
    pub commit: String,
    /// Every indexed snapshot of the repository, the default ref first.
    pub refs: Vec<RefSummary>,
    /// Indexed files per language.
    pub languages: std::collections::BTreeMap<String, u32>,
    pub files: u32,
    /// Nodes that are not containers (files, folders, modules, packages).
    pub symbols: u32,
    pub edges: u32,
    /// Packages the repository publishes (organization mode).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// Repositories of the organization it depends on (it imports packages they publish, or calls
    /// contracts they provide).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Relevance for `query` (0–1, 1 = the best repository).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListRepositoriesResult {
    /// Organization served, if any.
    pub org: Option<String>,
    pub repositories: Vec<RepositorySummary>,
}

/// One row of a contract table (`list_contracts`): a route, spec operation
/// or RPC method a symbol provides, or a call to one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ContractEntry {
    pub repo: String,
    /// The ref of the snapshot it comes from.
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// Repository-relative path of the file holding the declaration or call.
    pub path: Option<String>,
    /// 1-based line of the declaration or call.
    pub line: u32,
    /// Qualified name of the providing / calling symbol (the file for file-level code).
    pub symbol: String,
    /// Symbol id of that symbol.
    pub id: String,
    pub label: String,
    /// `http`, `rpc` or `topic`.
    pub kind: String,
    /// `provides` or `consumes`.
    pub side: String,
    /// Canonical key: `GET /users/{}`, `pkg.Service/Method`, a topic.
    pub key: String,
    /// Framework or spec: `flask`, `requests`, `openapi`, `proto`, …
    pub framework: String,
    /// The text as written (route, URL expression, `rpc` line).
    pub raw: String,
    /// Counterpart rows in the visible repositories: providers matching a consumed key, consumers
    /// of a provided one.
    pub matches: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListContractsResult {
    pub contracts: Vec<ContractEntry>,
    pub truncated: bool,
    pub next_offset: Option<u64>,
}

/// Converts an `f32` to the `f64` with the same shortest decimal form, so
/// `0.9f32` serializes as `0.9` rather than `0.8999999761581421`.
pub(crate) fn f32_to_f64(x: f32) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    format!("{x}").parse().unwrap_or(x as f64)
}
