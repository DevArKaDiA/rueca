//! Result types of the tools, serialized exactly as `docs/tools.md`
//! specifies. Fields documented as optional are omitted when absent; every
//! other field is always present (possibly `null`).

use schemars::JsonSchema;
use serde::Serialize;

/// Brief symbol, used inside every result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Brief {
    /// Stable symbol id: `<repo>@<commit>:<path>#<local-qualified-name>`.
    pub id: String,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct TraceResult {
    /// Snapshot (view index) of the root.
    #[serde(skip)]
    #[schemars(skip)]
    pub root_view: usize,
    pub root: Brief,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct FileOutlineResult {
    pub path: String,
    pub language: Option<String>,
    pub line_count: u32,
    pub symbols: Vec<OutlineSymbol>,
}

/// Converts an `f32` to the `f64` with the same shortest decimal form, so
/// `0.9f32` serializes as `0.9` rather than `0.8999999761581421`.
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
    /// Every indexed ref of the repository (its snapshots).
    pub refs: Vec<String>,
    /// Indexed files per language.
    pub languages: std::collections::BTreeMap<String, u32>,
    pub files: u32,
    /// Nodes that are not containers (files, folders, modules, packages).
    pub symbols: u32,
    pub edges: u32,
    /// Packages the repository publishes (organization mode).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
    /// Repositories of the organization it depends on (it imports packages they publish).
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

pub(crate) fn f32_to_f64(x: f32) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    format!("{x}").parse().unwrap_or(x as f64)
}
