//! Query engine over local segments.
//!
//! Implements the tools defined in `docs/tools.md` (`search_symbols`,
//! `search_code`, `get_symbol`, `get_snippet`, `trace`, `get_file_outline`,
//! `list_repositories`, `list_contracts`) with the response contract of RFC 0005 and the
//! organization scope of RFC 0006. The MCP server and the CLI both
//! dispatch through [`Engine::respond`] (or [`Engine::call`] for the JSON
//! alone; [`Engine::respond_with`] with a deadline and a visibility per
//! call), so their behavior is identical; [`tool_definitions`] provides
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
//! listing the indexed roots. Ref snapshots (`rueca index --ref`, RFC 0006
//! §9.3: catalog entries with `ref_name`, rooted at an export under the
//! store) are never the default: a repository id, `"*"` and the default root
//! select working trees. `ref` (one name or a list) then picks, for each
//! selected snapshot, the ones of its repository that the refs name, in
//! order: a ref snapshot of its repository recorded under that name (a
//! commit by any prefix of ≥ 7 digits) first — `--ref main` exports the
//! commit `main` points to, while the working tree on `main` may be dirty,
//! as in organization mode — else the selected snapshot itself when it
//! matches ([`snapshot::SnapshotDesc::matches_ref`]: its branch, `HEAD`,
//! `worktree` or its commit), else another snapshot with the same
//! repository id (another worktree, a ref snapshot by commit). A ref a
//! selected repository does not have is an error listing its indexed refs
//! (with `"*"`, a repository is read at the refs it has, and only a ref no
//! repository has is an error; a `pr/<n>` must then name one repository:
//! pull request numbers are per repository).
//!
//! With several snapshots, `search_symbols` returns **one ranking** (RFC 0006
//! §6, no rank fusion): every snapshot's symbol index is scored with BM25
//! statistics summed over all selected snapshots
//! ([`rueca_segment::search_symbols_merged`]), so scores are comparable and
//! the order is the one a single index would give. `search_code` concatenates
//! matches in snapshot order; `get_symbol`, `get_snippet` and `trace` resolve
//! an `id` in its own snapshot and a name in every snapshot (the best match
//! wins, the best match of each other snapshot is a candidate);
//! `get_file_outline` and path snippets need the path to exist in the
//! snapshots of one repository. Responses list only the snapshots their
//! items come from (with one selected snapshot, that snapshot is always
//! listed).
//!
//! **Several snapshots of one repository** (a `ref` list, RFC 0005 §6):
//! `search_symbols`, `get_symbol`, `trace` and `get_file_outline` find the
//! same symbol in each (same label, qualified name and path) and collapse
//! identical versions — same file blob — with [`multiref`]: one item with the
//! snapshots it is `in`, `changed` when it is not identical in every
//! requested snapshot of its repository; search collapses before cutting at
//! `limit`; `trace` walks from every version of the root and returns the
//! union graph. Repositories read at one snapshot are answered exactly as
//! before; `format: "verbose"` never collapses.
//!
//! # Organization mode (RFC 0006)
//!
//! [`Engine::for_org`] serves an organization built by `rueca index --org`
//! ([`rueca_segment::org`]). The repositories are those of the
//! organization's **current generation** (read once per call, so a call sees
//! one generation even while the organization is updated), filtered by the
//! engine's [`Visibility`]. `repo` omitted means the whole organization (the
//! repository of an `id` argument for `get_symbol`, `get_snippet` and
//! `trace`); one repository or a list narrows it. `ref` selects entries of
//! the generation (RFC 0006 §9.3): with `repo`, those repositories at the
//! ref and every other one at its default (§4's view); without `repo`, every
//! repository that has the ref at it; an `id` whose commit is a ref snapshot's
//! reads that snapshot. `search_symbols` runs on the organization symbol
//! index with that view (statistics over the entries it reads, routing with
//! each repository's entry profile): the visible set and the selection are
//! applied inside the index before top-k (statistics over the visible
//! repositories only), so results, totals and scores never depend on a
//! repository the caller cannot read, and a repository named in `repo` that
//! is not visible is reported exactly like one that does not exist (so are
//! its refs); a caller who may read none of them sees an empty organization
//! (no results, no error naming it). `trace` sees link edges through the view: edges into a
//! repository read at a ref are remapped to the ref's nodes. Other tools read
//! the selected entries' segments. In local mode the visibility (default
//! [`Visibility::All`]) applies to the indexed roots: a root of a
//! repository the caller may not read does not exist for the call (never
//! selected, listed or named in an error; a default root the caller may
//! not read is ignored, as if unset).
//!
//! # Per-call options (RFC 0008 §6.4–§6.5)
//!
//! [`Engine::respond_with`] runs a call with [`RespondOptions`]: the
//! caller's [`Visibility`] (instead of the engine's) and a **deadline**.
//! The deadline is cooperative — a blocking thread cannot be cancelled
//! from outside — and cheap: one `Instant::now()` comparison when the call
//! selects its snapshots and opens each view, per tantivy segment and per
//! over-fetch round of a symbol search, per file of `search_code`'s scan,
//! and every 64 items of `trace`'s traversal, `get_file_outline`'s symbols
//! and the multi-ref collapse. Once it has passed the call returns
//! [`DeadlineExceeded`] ("deadline of 10s exceeded; narrow `repo` or lower
//! `limit`"), which a transport recognizes with `downcast_ref` (`rueca
//! serve` answers it with JSON-RPC -32001). A deadline that does not pass
//! changes nothing: the response is byte for byte the one of
//! [`Engine::respond`], which is `respond_with` with no options.
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
mod contracts;
mod deadline;
pub mod multiref;
mod orgsel;
mod refset;
pub mod snapshot;
mod text;
mod tools;
pub mod types;
mod view;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use std::collections::BTreeSet;

use rueca_model::SymbolIdParts;
use rueca_segment::org::{OrgIndex, OrgSelection, RouteParams};
use rueca_segment::{CatalogEntry, Segment, Store};
use serde_json::Value;

pub use args::{
    CommonArgs, ContractKindArg, ContractSideArg, DirectionArg, FormatArg, GetFileOutlineArgs, GetSnippetArgs,
    GetSymbolArgs, LanguageArg, ListContractsArgs, ListRepositoriesArgs, RefArg, RepoArg, SearchCodeArgs,
    SearchSymbolsArgs, TraceArgs,
};
pub use compact::render_text;
pub use deadline::DeadlineExceeded;
pub use text::split_lines;

use deadline::Deadline;

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

pub const TOOL_NAMES: &[&str] = &[
    "search_symbols",
    "search_code",
    "get_symbol",
    "get_snippet",
    "trace",
    "get_file_outline",
    "list_repositories",
    "list_contracts",
];

/// A tool as advertised to MCP clients.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema (`type: object`) of the arguments.
    pub input_schema: Value,
}

/// Every tool, in [`TOOL_NAMES`] order.
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
            ToolDef {
                name: "list_repositories",
                description: DESC_LIST_REPOSITORIES,
                input_schema: args::input_schema::<ListRepositoriesArgs>(),
            },
            ToolDef {
                name: "list_contracts",
                description: DESC_LIST_CONTRACTS,
                input_schema: args::input_schema::<ListContractsArgs>(),
            },
        ]
    })
}

fn schema_for_tool(name: &str) -> Option<&'static Value> {
    definitions().iter().find(|d| d.name == name).map(|d| &d.input_schema)
}

const DESC_SEARCH_SYMBOLS: &str = "Find functions, methods, classes and other definitions by name or by what \
their code does. BM25 ranking over symbol names, qualified names, signatures, docstrings, file paths and the \
symbol's source code; identifiers are split on camelCase, snake_case and path separators, so `invoice issue` finds \
`InvoiceService.issue_invoice`, and `Session.request` ranks that method first. Start here with a name, or with words \
from an error message or issue to locate the code involved. Results are grouped by file (label, name, 1-based lines, \
signature) in one ranking over every selected repository (in an organization, all of them unless `repo` narrows \
it). Continue with `cursor` = `next_cursor`. \
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

const DESC_LIST_REPOSITORIES: &str = "List the repositories this server can read (in an organization, every \
repository you may see): repository id, default ref, every indexed ref with its commit, languages (files per \
language) and counts of files, symbols and edges. Use it to learn what exists and to narrow other tools with `repo` \
and `ref`. Example: {}";

const DESC_LIST_CONTRACTS: &str = "List the service contracts of the repositories: HTTP routes, OpenAPI \
operations and RPC methods they provide, and the HTTP / RPC calls they make, each with its canonical key \
(`GET /users/{}`, `pkg.Service/Method`), framework, the symbol and line that declares or makes it, and `matches`: \
how many counterparts the visible repositories have (providers of a call, callers of a route). `side: \"dangling\"` \
lists calls nothing provides; `query` filters by key, raw text or symbol. Follow an entry across repositories with \
`trace` (`edge_types: [\"CALLS\", \"IMPLEMENTS\"]`). Example: {\"side\": \"consumed\", \"query\": \"/users\"}";

const DESC_GET_FILE_OUTLINE: &str = "Outline of one file: its classes, functions, methods and other definitions, \
nested by containment and ordered by line, with signatures and 1-based line ranges. Cheaper than reading the file; \
use it to orient yourself before `get_snippet`. Example: {\"path\": \"src/billing/service.py\"}";

/// Which repositories a caller may read (RFC 0001 §13, RFC 0006 §5). Applied
/// inside the organization query path: before top-k, before counts and
/// before BM25 statistics.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Visibility {
    /// Every repository (local mode).
    #[default]
    All,
    /// Only these repository ids.
    Repos(BTreeSet<String>),
}

impl Visibility {
    pub fn allows(&self, repo: &str) -> bool {
        match self {
            Visibility::All => true,
            Visibility::Repos(set) => set.contains(repo),
        }
    }
}

/// Per-call options of [`Engine::respond_with`]. The default (no deadline,
/// the engine's visibility) is [`Engine::respond`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RespondOptions {
    /// When the call must stop (RFC 0008 §6.5). The engine checks it where
    /// work loops and, once it has passed, returns [`DeadlineExceeded`]
    /// ("deadline of 10s exceeded; narrow `repo` or lower `limit`").
    pub deadline: Option<Instant>,
    /// What the caller may read, instead of the engine's own
    /// [`Engine::visibility`] for this call.
    pub visibility: Option<Visibility>,
}

/// The query engine. See the crate docs.
///
/// Cloning is cheap and the clones share the opened segments and the
/// organization index: a service runs each call with the caller's
/// [`Visibility`] through [`Engine::respond_with`] (or a clone,
/// [`Engine::with_visibility`]).
#[derive(Clone)]
pub struct Engine {
    pub store_dir: PathBuf,
    /// Root used when a request does not name a repository.
    pub default_root: Option<PathBuf>,
    /// Organization served (organization mode, RFC 0006).
    pub org: Option<String>,
    visibility: Visibility,
    /// How a query's repository relevance weighs organization rankings.
    route: RouteParams,
    segments: Arc<Mutex<HashMap<String, Arc<Segment>>>>,
    /// The last opened generation of the organization index.
    org_index: Arc<Mutex<Option<Arc<OrgIndex>>>>,
    /// The deadline of the call this engine runs (set by
    /// [`Engine::respond_with`] on its per-call copy).
    deadline: Deadline,
}

/// The snapshots a call reads, plus the organization context in organization
/// mode.
pub(crate) struct Selection {
    pub views: Vec<View>,
    pub org: Option<OrgCtx>,
    /// Local mode: the store's catalog (every indexed snapshot).
    pub catalog: Vec<CatalogEntry>,
}

/// The organization generation a call reads, the repositories (indexes
/// into its `repos`) the caller may see, and which entries of each it reads.
pub(crate) struct OrgCtx {
    pub index: Arc<OrgIndex>,
    pub visible: Vec<usize>,
    /// The entries read (RFC 0006 §9.3): the `ref` argument's, or an `id`'s
    /// ref snapshot; every other repository at its default. Search
    /// statistics, routing and link edges see the organization through it.
    pub view: OrgSelection,
}

impl Engine {
    pub fn new(store_dir: PathBuf, default_root: Option<PathBuf>) -> anyhow::Result<Engine> {
        let default_root = default_root.map(|r| std::fs::canonicalize(&r).unwrap_or(r));
        Ok(Engine {
            store_dir,
            default_root,
            org: None,
            visibility: Visibility::All,
            route: RouteParams::from_env(),
            segments: Arc::new(Mutex::new(HashMap::new())),
            org_index: Arc::new(Mutex::new(None)),
            deadline: Deadline::default(),
        })
    }

    /// An engine serving organization `name` of the store (see the crate
    /// docs, *Organization mode*). The organization must exist when a tool
    /// is called, not when the engine is created.
    pub fn for_org(store_dir: PathBuf, name: &str) -> anyhow::Result<Engine> {
        rueca_segment::org::validate_name(name)?;
        let mut e = Engine::new(store_dir, None)?;
        e.org = Some(name.to_string());
        Ok(e)
    }

    /// Restricts what callers of this engine may read.
    pub fn with_visibility(mut self, visibility: Visibility) -> Engine {
        self.visibility = visibility;
        self
    }

    pub fn visibility(&self) -> &Visibility {
        &self.visibility
    }

    /// Sets the repository routing parameters (default:
    /// [`RouteParams::from_env`]).
    pub fn with_routing(mut self, route: RouteParams) -> Engine {
        self.route = route;
        self
    }

    pub fn routing(&self) -> &RouteParams {
        &self.route
    }

    /// Runs tool `name` with JSON `args` and returns the JSON result described
    /// in `docs/tools.md`. Unknown tools and invalid arguments are errors.
    pub fn call(&self, name: &str, args: Value) -> anyhow::Result<Value> {
        Ok(self.respond(name, args)?.json)
    }

    /// Like [`Engine::call`], with the text rendering of the result.
    pub fn respond(&self, name: &str, args: Value) -> anyhow::Result<Response> {
        self.respond_with(name, args, RespondOptions::default())
    }

    /// [`Engine::respond`] with per-call options: a deadline and the
    /// caller's visibility (RFC 0008 §6.4–§6.5). The deadline is
    /// cooperative: it is checked when the call selects its snapshots, per
    /// segment and per over-fetch round of a symbol search, per file of
    /// `search_code`'s scan, and every 64 items of `trace`'s traversal,
    /// `get_file_outline` and the multi-ref collapse; once it has passed
    /// the call returns [`DeadlineExceeded`]. Nothing else changes: with a
    /// deadline that does not pass, the response is the one `respond`
    /// gives.
    pub fn respond_with(&self, name: &str, args: Value, opts: RespondOptions) -> anyhow::Result<Response> {
        let RespondOptions { deadline, visibility } = opts;
        if deadline.is_none() && visibility.is_none() {
            return self.dispatch(name, args);
        }
        let call = self.for_call(visibility, Deadline::new(deadline));
        call.dispatch(name, args).map_err(|err| call.deadline.explain(err))
    }

    /// A copy of the engine for one call (shares the opened segments and
    /// the organization index).
    fn for_call(&self, visibility: Option<Visibility>, deadline: Deadline) -> Engine {
        Engine {
            store_dir: self.store_dir.clone(),
            default_root: self.default_root.clone(),
            org: self.org.clone(),
            visibility: visibility.unwrap_or_else(|| self.visibility.clone()),
            route: self.route,
            segments: self.segments.clone(),
            org_index: self.org_index.clone(),
            deadline,
        }
    }

    fn dispatch(&self, name: &str, args: Value) -> anyhow::Result<Response> {
        let Some(schema) = schema_for_tool(name) else {
            anyhow::bail!("unknown tool `{name}` (available: {})", TOOL_NAMES.join(", "));
        };
        match name {
            "search_symbols" => self.search_symbols(args::parse_args(name, schema, args)?),
            "search_code" => self.search_code(args::parse_args(name, schema, args)?),
            "get_symbol" => self.get_symbol(args::parse_args(name, schema, args)?),
            "get_snippet" => self.get_snippet(args::parse_args(name, schema, args)?),
            "trace" => self.trace(args::parse_args(name, schema, args)?),
            "get_file_outline" => self.get_file_outline(args::parse_args(name, schema, args)?),
            "list_repositories" => self.list_repositories(args::parse_args(name, schema, args)?),
            "list_contracts" => self.list_contracts(args::parse_args(name, schema, args)?),
            _ => anyhow::bail!("unknown tool `{name}` (available: {})", TOOL_NAMES.join(", ")),
        }
    }

    /// The snapshots a request reads (RFC 0005 §5, RFC 0006 §6), in response
    /// order: the `repo` argument (one, a list, or `"*"`), else the
    /// repository of an `id` argument, else (organization mode) every
    /// visible repository of the organization, else the default root, else
    /// the only indexed root; then filtered by `ref`.
    ///
    /// Local mode has one snapshot per indexed root, so a repository has as
    /// many snapshots as roots indexing it (worktrees); the code below works
    /// on lists of snapshots per repository so a catalog keeping several per
    /// (repository, ref) slots in.
    pub(crate) fn scope(&self, c: &CommonArgs, id: Option<&SymbolIdParts>) -> anyhow::Result<Vec<View>> {
        Ok(self.select(c, id)?.views)
    }

    pub(crate) fn select(&self, c: &CommonArgs, id: Option<&SymbolIdParts>) -> anyhow::Result<Selection> {
        if matches!(&c.ref_, Some(RefArg::Many(list)) if list.is_empty()) {
            anyhow::bail!("`ref` must not be an empty list (omit it for the default ref)");
        }
        self.deadline.check()?;
        let store = Store::open(&self.store_dir)?;
        if let Some(name) = &self.org {
            return self.select_org(&store, name, c, id);
        }
        let mut entries = store.entries()?;
        self.evict_stale(entries.iter().map(|e| e.segment_id.as_str()));
        // A root the caller may not read does not exist for this call.
        entries.retain(|e| self.visibility.allows(&e.repo));
        if entries.is_empty() {
            anyhow::bail!(
                "no repositories are indexed in store `{}`; run `rueca index <path>` first",
                self.store_dir.display()
            );
        }
        let (mut selected, star) = self.selected_entries(&store, &entries, c, id)?;
        let refs = c.ref_names();
        if !refs.is_empty() {
            selected = self.pick_refs(&store, &entries, selected, &refs, star)?;
        }
        let mut views = Vec::with_capacity(selected.len());
        for e in selected {
            self.deadline.check()?;
            views.push(View::new(self.open_cached(&store, &e.segment_id)?, e));
        }
        Ok(Selection { views, org: None, catalog: entries })
    }

    /// Local mode: for each selected entry, the snapshots of its repository
    /// that refs `names` name, in order (see the crate docs, *Snapshot
    /// selection*). A repository without one of them is an error; with
    /// `"*"`, a repository is read at the refs it has and skipped when it
    /// has none, and a ref no repository has is an error.
    fn pick_refs(
        &self,
        store: &Store,
        entries: &[CatalogEntry],
        selected: Vec<CatalogEntry>,
        names: &[&str],
        star: bool,
    ) -> anyhow::Result<Vec<CatalogEntry>> {
        let mut picked = Vec::with_capacity(selected.len() * names.len());
        let mut found = vec![false; names.len()];
        if star {
            check_pr_refs(names, |r| {
                let mut repos: Vec<&str> = selected
                    .iter()
                    .filter(|b| self.ref_snapshot(store, entries, b, r).is_some())
                    .map(|b| b.repo.as_str())
                    .collect();
                repos.sort_unstable();
                repos.dedup();
                repos.into_iter().map(str::to_string).collect()
            })?;
        }
        for base in &selected {
            for (k, r) in names.iter().enumerate() {
                match self.ref_snapshot(store, entries, base, r) {
                    Some(e) => {
                        found[k] = true;
                        add_entry(&mut picked, e);
                    }
                    None if !star => return Err(self.unknown_ref(store, entries, r, &base.repo)),
                    None => {}
                }
            }
        }
        if let Some(k) = found.iter().position(|f| !f)
            && let Some(base) = selected.first()
        {
            return Err(self.unknown_ref(store, entries, names[k], &base.repo));
        }
        Ok(picked)
    }

    /// The snapshot of `base`'s repository that ref `r` names: a ref
    /// snapshot recorded under that name, else `base` when `r` names it,
    /// else the first other entry of its repository that `r` names.
    fn ref_snapshot(
        &self,
        store: &Store,
        entries: &[CatalogEntry],
        base: &CatalogEntry,
        r: &str,
    ) -> Option<CatalogEntry> {
        let siblings = || entries.iter().filter(|e| e.repo == base.repo && e.root != base.root);
        let all = || std::iter::once(base).chain(siblings());
        let named = |e: &&CatalogEntry| e.ref_name.as_deref().is_some_and(|n| names_recorded_ref(n, r));
        all().find(named).or_else(|| all().find(|e| self.entry_matches_ref(store, e, r))).cloned()
    }

    fn entry_matches_ref(&self, store: &Store, e: &CatalogEntry, r: &str) -> bool {
        self.open_cached(store, &e.segment_id)
            .is_ok_and(|seg| snapshot::SnapshotDesc::from_info(seg.snapshot()).matches_ref(r))
    }

    /// The error for a ref repository `repo` has no snapshot of.
    fn unknown_ref(&self, store: &Store, entries: &[CatalogEntry], r: &str, repo: &str) -> anyhow::Error {
        let refs = self.indexed_refs(store, entries, repo);
        anyhow::anyhow!(
            "ref `{r}` is not indexed for repository `{repo}`; indexed refs: {refs}. `rueca index <path> --ref \
             <name>` indexes another ref (a branch, `pr/<n>`, a tag or a commit)"
        )
    }

    /// The catalog of organization `name`: the default entries of the
    /// repositories the caller may read, and the organization context (every
    /// repository at its default).
    fn org_entries(&self, name: &str) -> anyhow::Result<(Vec<CatalogEntry>, OrgCtx)> {
        let index = self.org_index(name)?;
        let mut entries = Vec::new();
        let mut visible = Vec::new();
        for (i, r) in index.generation().repos.iter().enumerate() {
            if self.visibility.allows(&r.repo) {
                visible.push(i);
                entries.push(CatalogEntry {
                    root: r.default.root.clone(),
                    repo: r.repo.clone(),
                    segment_id: r.default.segment_id.clone(),
                    indexed_at: r.default.snapshot.indexed_at,
                    ref_name: None,
                });
            }
        }
        // A caller who may read none of them sees an empty organization
        // (RFC 0008 §6.4: deny by default, nothing named).
        if index.generation().repos.is_empty() {
            anyhow::bail!("organization `{name}` has no repositories");
        }
        Ok((entries, OrgCtx { index, visible, view: OrgSelection::default() }))
    }

    /// The entries the `repo` argument (else the `id`, else the default)
    /// selects, each root once, and whether `"*"` (or the organization
    /// default) selected every one.
    fn selected_entries(
        &self,
        store: &Store,
        entries: &[CatalogEntry],
        c: &CommonArgs,
        id: Option<&SymbolIdParts>,
    ) -> anyhow::Result<(Vec<CatalogEntry>, bool)> {
        let mut selected: Vec<CatalogEntry> = Vec::new();
        let mut star = false;
        match &c.repo {
            None => match id {
                Some(p) => add_entry(&mut selected, self.entry_for_id(store, entries, p)?),
                None if self.org.is_some() => {
                    star = true;
                    add_entries(&mut selected, &default_entries(entries));
                }
                None => add_entry(&mut selected, self.select_entry(store, entries, None, None)?),
            },
            Some(RepoArg::One(r)) if r.trim() == "*" => {
                star = true;
                add_entries(&mut selected, &default_entries(entries));
            }
            Some(RepoArg::One(r)) => add_entry(&mut selected, self.select_entry(store, entries, Some(r), None)?),
            Some(RepoArg::Many(list)) => {
                if list.is_empty() {
                    anyhow::bail!("`repo` must not be an empty list (pass `\"*\"` for every indexed repository)");
                }
                for r in list {
                    if r.trim() == "*" {
                        star = true;
                        add_entries(&mut selected, &default_entries(entries));
                    } else {
                        add_entry(&mut selected, self.select_entry(store, entries, Some(r), None)?);
                    }
                }
            }
        }
        Ok((selected, star))
    }

    /// The current generation of organization `name`, opened (cached while
    /// the generation does not change).
    fn org_index(&self, name: &str) -> anyhow::Result<Arc<OrgIndex>> {
        let Some(generation) = rueca_segment::org::current_generation(&self.store_dir, name)? else {
            anyhow::bail!(
                "organization `{name}` is not built in store `{}`; run `rueca index --org <file>` first",
                self.store_dir.display()
            );
        };
        let mut cache = self.org_index.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(idx) = cache.as_ref()
            && idx.generation().generation == generation.generation
            && idx.generation().name == generation.name
        {
            return Ok(idx.clone());
        }
        let idx = Arc::new(OrgIndex::open_generation(&self.store_dir, generation)?);
        *cache = Some(idx.clone());
        Ok(idx)
    }

    /// Indexed refs of repository `repo`, for error messages: working trees
    /// first, then ref snapshots.
    fn indexed_refs(&self, store: &Store, entries: &[CatalogEntry], repo: &str) -> String {
        let mut mine: Vec<&CatalogEntry> = entries.iter().filter(|e| e.repo == repo).collect();
        mine.sort_by_key(|e| e.is_ref_export());
        let refs: Vec<String> = mine
            .into_iter()
            .filter_map(|e| Some((e, self.open_cached(store, &e.segment_id).ok()?)))
            .map(|(e, seg)| {
                let d = snapshot::SnapshotDesc::from_info(seg.snapshot());
                if e.is_ref_export() {
                    format!("`{}` ({}, ref snapshot)", d.ref_name, d.id_commit())
                } else {
                    d.describe()
                }
            })
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
            if let Some(org) = &self.org {
                anyhow::bail!("repository `{}` of symbol id is not indexed in organization `{org}`", p.repo);
            }
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
            // The entry of the selection whose root is that path's root.
            if let Some(e) = store.entry_for_path(&canon)?
                && let Some(mine) = entries.iter().find(|x| x.root == e.root)
            {
                return Ok(mine.clone());
            }
            if let Some(e) = entries.iter().find(|e| e.root == r) {
                return Ok(e.clone());
            }
            if let Some(org) = &self.org {
                anyhow::bail!(
                    "repository `{r}` is not indexed in organization `{org}`; `list_repositories` lists them"
                );
            }
            anyhow::bail!("repository `{r}` is not indexed; indexed: {}", roots());
        }
        if let Some(r) = id_repo
            && let Some(e) = self.entry_by_repo_id(entries, r)?
        {
            return Ok(e);
        }
        if let Some(e) = self.default_root_entry(store, entries)? {
            return Ok(e);
        }
        // The only root: ref snapshots are not roots of their own.
        let mut own = entries.iter().filter(|e| !e.is_ref_export());
        if let (Some(e), None) = (own.next(), own.next()) {
            return Ok(e.clone());
        }
        if entries.len() == 1 {
            return Ok(entries[0].clone());
        }
        anyhow::bail!("several repositories are indexed; pass `repo` (a root path or repository id): {}", roots())
    }

    /// The entry of the default root, when there is one and the caller may
    /// read it; a default root the caller may not read is ignored, as if
    /// unset (it is never named).
    fn default_root_entry(&self, store: &Store, entries: &[CatalogEntry]) -> anyhow::Result<Option<CatalogEntry>> {
        let Some(root) = &self.default_root else { return Ok(None) };
        match store.entry_for_path(root)? {
            Some(e) if entries.iter().any(|x| x.root == e.root) => Ok(Some(e)),
            Some(_) => Ok(None),
            None => {
                let roots = entries.iter().map(|e| format!("{} ({})", e.root, e.repo)).collect::<Vec<_>>().join(", ");
                anyhow::bail!(
                    "root `{}` is not indexed (run `rueca index {}`); indexed: {roots}",
                    root.display(),
                    root.display()
                )
            }
        }
    }

    /// Entry whose repository id is `repo`: its working tree (ref snapshots
    /// only when the repository has no working tree indexed; then the first).
    /// If several working trees share the id, the one containing the default
    /// root wins; otherwise it is ambiguous.
    fn entry_by_repo_id(&self, entries: &[CatalogEntry], repo: &str) -> anyhow::Result<Option<CatalogEntry>> {
        let all: Vec<&CatalogEntry> = entries.iter().filter(|e| e.repo == repo).collect();
        let own: Vec<&CatalogEntry> = all.iter().copied().filter(|e| !e.is_ref_export()).collect();
        let hits = if own.is_empty() { all.into_iter().take(1).collect() } else { own };
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

    /// Drops cached segments that are not in `live`.
    fn evict_stale<'a>(&self, live: impl IntoIterator<Item = &'a str>) {
        let live: BTreeSet<&str> = live.into_iter().collect();
        let mut cache = self.segments.lock().unwrap_or_else(|p| p.into_inner());
        cache.retain(|id, _| live.contains(id.as_str()));
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

/// Adds `e` to `selected` unless its root is there.
fn add_entry(selected: &mut Vec<CatalogEntry>, e: CatalogEntry) {
    if !selected.iter().any(|x| x.root == e.root) {
        selected.push(e);
    }
}

/// The entries `"*"` selects: every working tree, and the first ref snapshot
/// of a repository that has no working tree indexed.
fn default_entries(entries: &[CatalogEntry]) -> Vec<CatalogEntry> {
    let own: BTreeSet<&str> = entries.iter().filter(|e| !e.is_ref_export()).map(|e| e.repo.as_str()).collect();
    let mut seen = BTreeSet::new();
    entries
        .iter()
        .filter(|e| !e.is_ref_export() || (!own.contains(e.repo.as_str()) && seen.insert(e.repo.as_str())))
        .cloned()
        .collect()
}

/// Whether ref name `given` names the ref snapshot recorded as `recorded`:
/// equal (`refs/heads/<name>` too), or both commits with one a prefix of at
/// least 7 digits of the other (a commit is recorded under its 12-digit
/// form).
fn names_recorded_ref(recorded: &str, given: &str) -> bool {
    let given = given.trim();
    let given = given.strip_prefix("refs/heads/").unwrap_or(given);
    rueca_segment::org::ref_names_match(recorded, given)
}

/// `ref` without `repo` (or `"*"`): pull request numbers are per
/// repository, so a `pr/<n>` among `names` must be held by exactly one of
/// the selected repositories (`holders` lists those holding a ref, all of
/// them visible to the caller). Zero is left to the unknown-ref error.
pub(crate) fn check_pr_refs(names: &[&str], holders: impl Fn(&str) -> Vec<String>) -> anyhow::Result<()> {
    for r in names.iter().filter(|r| is_pr_ref(r)) {
        let repos = holders(r);
        if repos.len() > 1 {
            anyhow::bail!("`{r}` exists in several repositories ({}): name `repo`", repos.join(", "));
        }
    }
    Ok(())
}

/// `pr/<n>`.
fn is_pr_ref(r: &str) -> bool {
    r.trim().strip_prefix("pr/").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Adds every entry whose root is not in `selected` yet.
fn add_entries(selected: &mut Vec<CatalogEntry>, entries: &[CatalogEntry]) {
    for e in entries {
        add_entry(selected, e.clone());
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
