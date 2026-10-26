//! Query engine over local segments.
//!
//! Implements the six tools defined in `docs/tools.md` (`search_symbols`,
//! `search_code`, `get_symbol`, `get_snippet`, `trace`, `get_file_outline`)
//! with the response contract of RFC 0005. The MCP server and the CLI both
//! dispatch through [`Engine::respond`] (or [`Engine::call`] for the JSON
//! alone), so their behavior is identical; [`tool_definitions`] provides
//! names, descriptions and JSON input schemas for the MCP layer.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let engine = rueca_query::Engine::new("/tmp/store".into(), Some("/src/billing".into()))?;
//! let hits = engine.respond("search_symbols", serde_json::json!({"query": "issue invoice"}))?;
//! println!("{}", hits.text); // compact rendering; `hits.json` is the envelope
//! # Ok(()) }
//! ```
//!
//! The engine is `Send + Sync`. Opened segments are cached by segment id; the
//! catalog is re-read on every call so a re-index is picked up immediately
//! and replaced segments are evicted.
//!
//! # Responses (RFC 0005)
//!
//! By default every tool returns the **envelope**: `snapshots` (each
//! snapshot read: `s`, `repo`, `ref`, full `commit`, `overlay`, and a host
//! `url` template for GitHub / GitLab / Bitbucket repositories), `results`
//! (grouped by snapshot and file, short field names `qn`, `sig`, `lines`;
//! `trace` nodes as a list with edges `[src, dst, kind, confidence, strategy,
//! call_site_line(, path)]` by node index), `truncated`, `next_cursor` and
//! `budget`. [`compact`] builds it from the alpha results and renders the
//! text form ([`render_text`]); the budget (`budget_tokens`, default 2,000)
//! is counted on that text as `ceil(bytes / 4)`. `format: "verbose"` returns
//! the alpha shapes and defaults unchanged (one release).
//!
//! Ids are pinned: `repo@commit12[+]:path#local-name`, `+` marking a working
//! tree with uncommitted changes (the snapshot's `dirty` flag). Ids are
//! accepted with the full commit, a prefix of at least 7 hex digits, the
//! 12-digit form, or a ref name; a commit that is not indexed resolves
//! against the repository's current snapshot (local mode, so ids survive a
//! re-index).
//!
//! # Snapshot selection
//!
//! `repo` (every tool) is a repository id or an indexed root path (a path
//! inside a root selects that root), a list of them, or `"*"` for every
//! indexed root. Without it: the repository of an `id` argument (the root
//! whose commit the id names when several roots index that repository), else
//! the engine's default root, else the only indexed root, else an error
//! listing the indexed roots. `ref` then keeps the snapshots it names
//! ([`snapshot::SnapshotDesc::matches_ref`]); local mode has one snapshot per
//! root (its working tree, named by the checked-out branch, `HEAD` when
//! detached, `worktree` outside git), and any other ref is an error listing
//! the indexed refs (with `"*"`, snapshots without the ref are skipped).
//!
//! With several snapshots, `search_symbols` merges the per-snapshot rankings
//! by reciprocal rank fusion (k = 60) and re-applies the global priors (×0.5
//! tests, ×0.5 fields / variables / constants); `search_code` concatenates
//! matches in snapshot order; `get_symbol`, `get_snippet` and `trace` resolve
//! an `id` in its own snapshot and a name in every snapshot (the best match
//! wins, the best match of each other snapshot is a candidate);
//! `get_file_outline` and path snippets need the path to exist in exactly one
//! selected snapshot. [`multiref`] holds the collapse of identical symbols
//! across refs (RFC 0005 §6), which no local tool reaches yet.
//!
//! # Symbol lookup
//!
//! `id` → `qualified_name` (exact, then suffix on a `.`, `:`, `/` or `\`
//! boundary, so `util:formatAmount` finds `src/util:formatAmount` and
//! `User::save` finds `App\Models\User::save`) →
//! `name` (narrowed by `path`, a file, directory or path suffix). Among several matches the pick is deterministic: non-test,
//! exported, symbols before containers (files, folders), shorter qualified
//! name, lower node id; up to 20 others are returned as `candidates`.
//! Symbol ids use the module-local name computed by walking `DEFINES` parents
//! up to the `File` node; PHP names are already file-independent, so a PHP
//! id carries the fully qualified name (`…/User.php#App\Models\User::save`).
//! For data, Go, C/C++ and Java symbols the local name is the qualified name
//! minus the file's module name recorded in the segment (`files.modules`):
//! `…/handle.go#Server.Handle`, `…/Parser.java#Parser.parse(String)`,
//! `…/foo.cpp#ns::Foo::bar`, `…/app.yaml#server.port`. Canonical env vars
//! have no file and use the pseudo-path `$env` (`…@c:$env#DATABASE_URL`).
//! Qualified-name suffixes also match after a key-path accessor
//! (`["DATABASE_URL"]`), and for Java and C/C++ an overload's parameter
//! suffix is optional (`Parser.parse` → every `parse(…)` overload).
//!
//! # Interpretations of `docs/tools.md`
//!
//! - Integer arguments outside their documented range are clamped, not
//!   rejected; unknown arguments, wrong types and unknown labels / edge types
//!   are errors.
//! - `trace` (verbose) orders `nodes` by (depth, direction, qualified_name, node id)
//!   with `callers` before `callees` (the order the contract lists them), not
//!   alphabetically. `edges` (order unspecified by the contract) are sorted
//!   by (src qualified name, dst qualified name, kind) and contain every
//!   traversed edge between reported nodes, keeping the highest-confidence
//!   one per (src, dst, kind). `limit` counts nodes over both directions; a
//!   node reachable both ways appears once per direction. The compact form
//!   lists each node once (`dir` `in` / `out` / `both`, `d` its smallest
//!   depth), the root first.
//! - Compact pages: `next_cursor` is an item offset (search results, matches,
//!   `trace` nodes after the root, outline symbols in pre-order) or, for
//!   `get_snippet`, the next line. A `trace` edge is listed on the page of its
//!   later endpoint, repeating an endpoint from an earlier page when needed.
//!   `get_symbol` cuts its lists (children, bases, subtypes, candidates, in
//!   that order) and reports what it left out in `omitted`, without a cursor.
//! - `trace` neighbour filter: tests (`is_test` or label `Test`) are skipped
//!   unless `include_tests` is true or `edge_types` contains `TESTS`; with
//!   `labels`, only neighbours with those labels are kept. Skipped nodes are
//!   neither reported nor expanded and their edges are dropped; the root is
//!   never filtered. `tests_excluded` counts distinct skipped tests over all
//!   levels. Other labels stay: a `File` caller is module-level code, a
//!   `Class` / `Variable` / `Constant` / `Field` caller is code in a class
//!   body or an initializer, a `Class` callee is a constructor call (its
//!   `__init__` / `constructor` is listed too).
//! - `get_snippet` also accepts `path` alone (from line 1, bounded by
//!   `max_lines`) and unique path suffixes; `start_line` past the end of the
//!   file is an error.
//! - Brief fields `path`, `start_line`, `end_line` are `null` for nodes
//!   without a file or span (never for symbols produced by the extractors).

mod args;
pub mod compact;
pub mod multiref;
pub mod snapshot;
mod text;
mod tools;
pub mod types;
mod view;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use rueca_model::SymbolIdParts;
use rueca_segment::{CatalogEntry, Segment, Store};
use serde_json::Value;

pub use args::{
    CommonArgs, DirectionArg, FormatArg, GetFileOutlineArgs, GetSnippetArgs, GetSymbolArgs, LanguageArg, RepoArg,
    SearchCodeArgs, SearchSymbolsArgs, TraceArgs,
};
pub use compact::render_text;
pub use text::split_lines;

use view::View;

/// A tool response: the JSON result (the envelope, or the alpha shape with
/// `format: verbose`) and its text rendering ([`render_text`]; compact JSON
/// for `verbose`).
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub json: Value,
    pub text: String,
}

impl Response {
    fn verbose(json: Value) -> Response {
        let text = json.to_string();
        Response { json, text }
    }
}

pub const TOOL_NAMES: &[&str] =
    &["search_symbols", "search_code", "get_symbol", "get_snippet", "trace", "get_file_outline"];

/// A tool as advertised to MCP clients.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema (`type: object`) of the arguments.
    pub input_schema: Value,
}

/// The six tools, in [`TOOL_NAMES`] order.
pub fn tool_definitions() -> Vec<ToolDef> {
    definitions().to_vec()
}

fn definitions() -> &'static [ToolDef] {
    static DEFS: OnceLock<Vec<ToolDef>> = OnceLock::new();
    DEFS.get_or_init(|| {
        vec![
            ToolDef {
                name: "search_symbols",
                description: DESC_SEARCH_SYMBOLS,
                input_schema: args::input_schema::<SearchSymbolsArgs>(),
            },
            ToolDef {
                name: "search_code",
                description: DESC_SEARCH_CODE,
                input_schema: args::input_schema::<SearchCodeArgs>(),
            },
            ToolDef {
                name: "get_symbol",
                description: DESC_GET_SYMBOL,
                input_schema: args::input_schema::<GetSymbolArgs>(),
            },
            ToolDef {
                name: "get_snippet",
                description: DESC_GET_SNIPPET,
                input_schema: args::input_schema::<GetSnippetArgs>(),
            },
            ToolDef { name: "trace", description: DESC_TRACE, input_schema: args::input_schema::<TraceArgs>() },
            ToolDef {
                name: "get_file_outline",
                description: DESC_GET_FILE_OUTLINE,
                input_schema: args::input_schema::<GetFileOutlineArgs>(),
            },
        ]
    })
}

fn schema_for_tool(name: &str) -> &'static Value {
    &definitions().iter().find(|d| d.name == name).expect("known tool").input_schema
}

const DESC_SEARCH_SYMBOLS: &str = "Find functions, methods, classes and other definitions by name or by what \
their code does. BM25 ranking over symbol names, qualified names, signatures, docstrings, file paths and the \
symbol's source code; identifiers are split on camelCase, snake_case and path separators, so `invoice issue` finds \
`InvoiceService.issue_invoice`, and `Session.request` ranks that method first. Start here with a name, or with words \
from an error message or issue to locate the code involved. Results are grouped by file (label, name, 1-based lines, \
signature); `repo: \"*\"` searches every indexed repository. Continue with `cursor` = `next_cursor`. \
Example: {\"query\": \"parse config\", \"labels\": [\"Function\", \"Method\"], \"path_glob\": \"src/**\"}";

const DESC_SEARCH_CODE: &str = "Grep the indexed file contents: literal text (default) or a regular expression \
(`regex: true`). Use it for string literals, error messages, config keys, TODOs or anything that is not a symbol \
name. Matches are grouped by file, with the 1-based line, the line text, optional context lines and the innermost \
enclosing definition. Example: {\"pattern\": \"retry_after\", \"path_glob\": \"**/*.py\", \"context_lines\": 2}";

const DESC_GET_SYMBOL: &str = "Details of one symbol: signature, docstring, exported/test flags, its parent \
container, children (methods, nested definitions), base types, subtypes and caller/callee counts (plus reader/writer \
counts for env vars and config keys). Name it by `id` \
(from another tool's result), by `qualified_name` (or a suffix such as `Session.request`, or the name another tool \
printed), or by `name` (+ `path` to disambiguate). Other matches of an ambiguous lookup are listed in `candidates`. \
Example: {\"qualified_name\": \"InvoiceService.issue\"}";

const DESC_GET_SNIPPET: &str = "Source code of a symbol (by `id` / `qualified_name` / `name`) or of a line range \
(`path` + `start_line` [+ `end_line`]), from the indexed snapshot. Lines are 1-based and inclusive; `context_lines` \
adds surrounding lines and `max_lines` (default 300) bounds the output; a cut snippet continues with `cursor`. \
Example: {\"qualified_name\": \"billing.service.InvoiceService.issue\", \"context_lines\": 2}";

const DESC_TRACE: &str = "Walk the call graph from a symbol: who calls it (`callers`), what it calls (`callees`) or \
both, up to `depth` hops (default 1). Node 0 is the root; each other node shows its direction (in = caller, out = \
callee), depth and its edges to earlier nodes (→j: calls node j, ←j: called by node j) with confidence (0–1), \
resolution strategy and call-site line; raise `min_confidence` to drop guesses. Tests are left out (counted) unless \
`include_tests` is true; `labels` keeps only some neighbour labels (e.g. [\"Function\", \"Method\"]). A `File` caller \
is module-level code; a `Class` callee is a constructor call. Other relations can be followed with `edge_types` \
(e.g. [\"CALLS\", \"INHERITS\", \"IMPORTS\", \"TESTS\"]; [\"READS\", \"WRITES\"] for who reads or sets an env var or \
config key). Use it for impact analysis before changing a function. \
Example: {\"qualified_name\": \"InvoiceService.issue\", \"direction\": \"callers\", \"depth\": 3}";

const DESC_GET_FILE_OUTLINE: &str = "Outline of one file: its classes, functions, methods and other definitions, \
nested by containment and ordered by line, with signatures and 1-based line ranges. Cheaper than reading the file; \
use it to orient yourself before `get_snippet`. Example: {\"path\": \"src/billing/service.py\"}";

/// The query engine. See the crate docs.
pub struct Engine {
    pub store_dir: PathBuf,
    /// Root used when a request does not name a repository.
    pub default_root: Option<PathBuf>,
    segments: Mutex<HashMap<String, Arc<Segment>>>,
}

impl Engine {
    pub fn new(store_dir: PathBuf, default_root: Option<PathBuf>) -> anyhow::Result<Engine> {
        let default_root = default_root.map(|r| std::fs::canonicalize(&r).unwrap_or(r));
        Ok(Engine { store_dir, default_root, segments: Mutex::new(HashMap::new()) })
    }

    /// Runs tool `name` with JSON `args` and returns the JSON result described
    /// in `docs/tools.md`. Unknown tools and invalid arguments are errors.
    pub fn call(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        Ok(self.respond(name, args)?.json)
    }

    /// Like [`Engine::call`], with the text rendering of the result.
    pub fn respond(&self, name: &str, args: Value) -> anyhow::Result<Response> {
        if !TOOL_NAMES.contains(&name) {
            anyhow::bail!("unknown tool `{name}` (available: {})", TOOL_NAMES.join(", "));
        }
        let schema = schema_for_tool(name);
        match name {
            "search_symbols" => self.search_symbols(args::parse_args(name, schema, args)?),
            "search_code" => self.search_code(args::parse_args(name, schema, args)?),
            "get_symbol" => self.get_symbol(args::parse_args(name, schema, args)?),
            "get_snippet" => self.get_snippet(args::parse_args(name, schema, args)?),
            "trace" => self.trace(args::parse_args(name, schema, args)?),
            "get_file_outline" => self.get_file_outline(args::parse_args(name, schema, args)?),
            _ => unreachable!(),
        }
    }

    /// The snapshots a request reads (RFC 0005 §5), in response order: the
    /// `repo` argument (one, a list, or `"*"`), else the repository of an `id`
    /// argument, else the default root, else the only indexed root; then
    /// filtered by `ref`.
    ///
    /// Local mode has one snapshot per indexed root, so a repository has as
    /// many snapshots as roots indexing it (worktrees); the code below works
    /// on lists of snapshots per repository so a catalog keeping several per
    /// (repository, ref) slots in.
    fn scope(&self, c: &CommonArgs, id: Option<&SymbolIdParts>) -> anyhow::Result<Vec<View>> {
        let store = Store::open(&self.store_dir)?;
        let entries = store.entries()?;
        self.evict_stale(&entries);
        if entries.is_empty() {
            anyhow::bail!(
                "no repositories are indexed in store `{}`; run `rueca index <path>` first",
                self.store_dir.display()
            );
        }
        let mut star = false;
        let mut selected: Vec<CatalogEntry> = Vec::new();
        let add = |e: CatalogEntry, selected: &mut Vec<CatalogEntry>| {
            if !selected.iter().any(|x| x.root == e.root) {
                selected.push(e);
            }
        };
        match &c.repo {
            None => match id {
                Some(p) => add(self.entry_for_id(&store, &entries, p)?, &mut selected),
                None => add(self.select_entry(&store, &entries, None, None)?, &mut selected),
            },
            Some(RepoArg::One(r)) if r.trim() == "*" => {
                star = true;
                entries.iter().cloned().for_each(|e| add(e, &mut selected));
            }
            Some(RepoArg::One(r)) => add(self.select_entry(&store, &entries, Some(r), None)?, &mut selected),
            Some(RepoArg::Many(list)) => {
                if list.is_empty() {
                    anyhow::bail!("`repo` must not be an empty list (pass `\"*\"` for every indexed repository)");
                }
                for r in list {
                    if r.trim() == "*" {
                        star = true;
                        entries.iter().cloned().for_each(|e| add(e, &mut selected));
                    } else {
                        add(self.select_entry(&store, &entries, Some(r), None)?, &mut selected);
                    }
                }
            }
        }
        let mut views = Vec::with_capacity(selected.len());
        for e in selected {
            let seg = self.open_cached(&store, &e.segment_id)?;
            views.push(View::new(seg, e));
        }
        let Some(r) = c.ref_.as_deref() else { return Ok(views) };
        let (keep, miss): (Vec<View>, Vec<View>) = views.into_iter().partition(|v| v.snap.matches_ref(r));
        if let Some(v) = miss.first()
            && (!star || keep.is_empty())
        {
            let refs = self.indexed_refs(&store, &entries, v.repo());
            anyhow::bail!(
                "ref `{r}` is not indexed for repository `{}`; indexed refs: {refs}. Local mode indexes the \
                 working tree of each root (its checked-out branch plus uncommitted changes); index another \
                 worktree to read another ref",
                v.repo()
            );
        }
        Ok(keep)
    }

    /// Indexed refs of repository `repo`, for error messages.
    fn indexed_refs(&self, store: &Store, entries: &[CatalogEntry], repo: &str) -> String {
        let refs: Vec<String> = entries
            .iter()
            .filter(|e| e.repo == repo)
            .filter_map(|e| self.open_cached(store, &e.segment_id).ok())
            .map(|seg| snapshot::SnapshotDesc::from_info(seg.snapshot()).describe())
            .collect();
        if refs.is_empty() { "none".to_string() } else { refs.join(", ") }
    }

    /// The entry an `id` argument names when no `repo` is given: the only root
    /// of its repository, or among several the one whose snapshot matches the
    /// id's commit (or ref), else the one containing the default root. An id
    /// of a repository that is not indexed falls back to the default
    /// selection, whose lookup reports the mismatch.
    fn entry_for_id(&self, store: &Store, entries: &[CatalogEntry], p: &SymbolIdParts) -> anyhow::Result<CatalogEntry> {
        let same: Vec<&CatalogEntry> = entries.iter().filter(|e| e.repo == p.repo).collect();
        if same.len() > 1
            && let Some(c) = &p.commit
        {
            let matching: Vec<&CatalogEntry> = same
                .iter()
                .copied()
                .filter(|e| {
                    self.open_cached(store, &e.segment_id)
                        .is_ok_and(|seg| snapshot::SnapshotDesc::from_info(seg.snapshot()).matches_commit(c))
                })
                .collect();
            if matching.len() == 1 {
                return Ok(matching[0].clone());
            }
        }
        if same.is_empty() {
            return self.select_entry(store, entries, None, None);
        }
        self.select_entry(store, entries, None, Some(&p.repo))
    }

    fn select_entry(
        &self,
        store: &Store,
        entries: &[CatalogEntry],
        repo: Option<&str>,
        id_repo: Option<&str>,
    ) -> anyhow::Result<CatalogEntry> {
        let roots = || entries.iter().map(|e| format!("{} ({})", e.root, e.repo)).collect::<Vec<_>>().join(", ");
        if let Some(r) = repo {
            if let Some(e) = self.entry_by_repo_id(entries, r)? {
                return Ok(e);
            }
            let p = Path::new(r);
            let canon = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
            if let Some(e) = store.entry_for_path(&canon)? {
                return Ok(e);
            }
            if let Some(e) = entries.iter().find(|e| e.root == r) {
                return Ok(e.clone());
            }
            anyhow::bail!("repository `{r}` is not indexed; indexed: {}", roots());
        }
        if let Some(r) = id_repo
            && let Some(e) = self.entry_by_repo_id(entries, r)?
        {
            return Ok(e);
        }
        if let Some(root) = &self.default_root {
            return match store.entry_for_path(root)? {
                Some(e) => Ok(e),
                None => anyhow::bail!(
                    "root `{}` is not indexed (run `rueca index {}`); indexed: {}",
                    root.display(),
                    root.display(),
                    roots()
                ),
            };
        }
        if entries.len() == 1 {
            return Ok(entries[0].clone());
        }
        anyhow::bail!("several repositories are indexed; pass `repo` (a root path or repository id): {}", roots())
    }

    /// Entry whose repository id is `repo`. If several roots share the id, the
    /// one containing the default root wins; otherwise it is ambiguous.
    fn entry_by_repo_id(&self, entries: &[CatalogEntry], repo: &str) -> anyhow::Result<Option<CatalogEntry>> {
        let hits: Vec<&CatalogEntry> = entries.iter().filter(|e| e.repo == repo).collect();
        match hits.len() {
            0 => Ok(None),
            1 => Ok(Some(hits[0].clone())),
            _ => {
                if let Some(root) = &self.default_root
                    && let Some(e) = hits.iter().find(|e| root.starts_with(&e.root))
                {
                    return Ok(Some((*e).clone()));
                }
                let roots: Vec<&str> = hits.iter().map(|e| e.root.as_str()).collect();
                anyhow::bail!("repository id `{repo}` is indexed at several roots; pass one of: {}", roots.join(", "))
            }
        }
    }

    fn evict_stale(&self, entries: &[CatalogEntry]) {
        let mut cache = self.segments.lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|id, _| entries.iter().any(|e| &e.segment_id == id));
    }

    fn open_cached(&self, store: &Store, segment_id: &str) -> anyhow::Result<Arc<Segment>> {
        if let Some(s) = self.segments.lock().unwrap_or_else(|p| p.into_inner()).get(segment_id) {
            return Ok(s.clone());
        }
        let seg = Arc::new(store.open_segment(segment_id)?);
        self.segments.lock().unwrap_or_else(|p| p.into_inner()).insert(segment_id.to_string(), seg.clone());
        Ok(seg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Engine>();
    }

    #[test]
    fn definitions_cover_all_tools() {
        let defs = tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name).collect();
        assert_eq!(names, TOOL_NAMES);
        for d in &defs {
            assert_eq!(d.input_schema["type"], "object", "{}", d.name);
            assert!(!d.description.is_empty());
        }
    }

    #[test]
    fn unknown_tool_is_an_error() {
        let e = Engine::new("/nonexistent".into(), None).unwrap();
        let err = e.call("nope", Value::Null).unwrap_err().to_string();
        assert!(err.contains("unknown tool `nope`"), "{err}");
    }
}
