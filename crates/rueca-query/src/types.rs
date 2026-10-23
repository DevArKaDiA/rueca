//! Result types of the six tools, serialized exactly as `docs/tools.md`
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
    pub root: Brief,
    pub nodes: Vec<TraceNode>,
    pub edges: Vec<TraceEdge>,
    pub truncated: bool,
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
pub(crate) fn f32_to_f64(x: f32) -> f64 {
    if !x.is_finite() {
        return 0.0;
    }
    format!("{x}").parse().unwrap_or(x as f64)
}
