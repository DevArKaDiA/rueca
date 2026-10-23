//! Turns the facts of every file in a snapshot into a [`ResolvedGraph`].
//!
//! Contract (RFC 0001 §7 step 3): deterministic — same inputs in the same
//! order produce an identical graph (sorted iteration, stable tie-breaks).
//! Inputs are sorted by path first, so the input order does not matter either.
//!
//! # Graph shape
//!
//! - One `Repository` node (qualified name = repo id), one `Folder` node per
//!   directory that (transitively) contains files, one `File` node per input
//!   file (even for unsupported languages), and one node per [`Definition`].
//! - Definition nodes get a global qualified name: `pack.module_name(path)` +
//!   separator + local qualified name. The separator is `.` for Python and `:`
//!   for ECMAScript (paths may contain dots). Identical `(label,
//!   qualified_name, file)` triples are merged, keeping the first definition
//!   (Python redefinitions); later local ids map to the kept node.
//! - Structural edges (`Syntactic`, 1.0): `CONTAINS` Repository→Folder/File and
//!   Folder→Folder/File; `DEFINES` File→top-level definition and
//!   parent→child definition.
//! - `IMPORTS` File→File for every import that resolves to a project file
//!   (`ImportResolved`, 1.0, evidence = import span). Python `from m import x`
//!   where `m.x` is a submodule also imports that submodule.
//!
//! # Binding environments
//!
//! Each file gets a module-scope environment built from its imports
//! (top-level definitions are looked up separately, see strategy 4):
//!
//! - Python `import a.b.c` binds `a` (to `a` if it is a project module, else to
//!   a namespace prefix walked lazily); `import a.b as x` binds `x` to `a.b`;
//!   `from m import x` binds `x` to the symbol `x` of `m` or, failing that, to
//!   the submodule `m.x`; `from m import *` binds every exported top-level
//!   definition of `m`. Explicit imports override star imports.
//! - ECMAScript named imports bind the exported symbol of that name, `default`
//!   binds the target's default export (a definition named `default`, else the
//!   single exported class/function named after the file, else the only
//!   exported top-level definition), namespace imports bind the module.
//! - Imports of non-project modules bind the name as *external*: references
//!   through such names are reported unresolved and are never fuzzy-matched.
//!
//! Looking a name up in a module checks its own top-level definitions, then
//! its re-exports (`export { x } from`, `export * from`, `export * as ns
//! from`), then the names its own imports bind (package `__init__.py`
//! re-exports; ECMAScript `import { X } from "./x"; export { X }`), then —
//! Python only — submodules. Lookups are
//! transitive, cycle-safe and bounded to depth 10.
//!
//! # Reference strategies
//!
//! Strategies are tried in order; the first success wins.
//!
//! | # | Strategy | When | Edge strategy | Confidence |
//! |---|---|---|---|---|
//! | 1 | Local scope | no receiver; name defined by an enclosing function (walking `parent`) | `SameModule` | 0.95 |
//! | 2 | Self receiver | receiver ∈ `pack.self_receivers()`: member of the enclosing class | `TypeResolved` | 0.90 |
//! | 2 | Self receiver, inherited | …found in a resolved base class (depth-first, left-to-right, depth ≤ 10) | `TypeResolved` | 0.85 |
//! | 2 | `super` receiver | `super()` / `super`: member of a base class | `TypeResolved` | 0.85 |
//! | 3 | Imported binding | no receiver; name bound by an import | `ImportResolved` | 0.95 |
//! | 3 | Imported module | receiver is (a dotted path through) an imported project module | `ImportResolved` | 0.90 |
//! | 3 | Imported class | receiver is an imported class (static member) | `ImportResolved` | 0.85 |
//! | 4 | Same module | no receiver; top-level definition of the same file | `SameModule` | 0.90 |
//! | 4 | Same-module class | receiver is a top-level class of the same file | `SameModule` | 0.85 |
//! | 6 | Unique name | no receiver; exactly one compatible non-test definition with that name in the same language family, not a builtin | `UniqueName` | 0.60 |
//! | 6 | Unique method | call with an unknown receiver; exactly one compatible method with that name, name ≥ 4 chars, not a builtin and not a common builtin-type method (`update`, `keys`, `then`, `forEach`, …) | `Heuristic` | 0.50 |
//! | — | Constructor | a `Call`/`New` that hits a class also calls its `__init__` / `constructor` (own or inherited) | as the class edge | × 0.95 |
//!
//! (Strategy 5, constructor-based receiver typing, needs assignment facts and
//! is not implemented in the alpha.) Self receivers that miss, and names bound
//! to external modules or builtin objects, stop resolution without falling back
//! to strategy 6.
//!
//! Label compatibility for strategy 6: without a receiver, `Call`/`Decorator`
//! → Function, Class (a bare call never targets a method); `New` → Class,
//! Function; `Type` → Class, Interface, TypeAlias, Enum, Struct, Trait;
//! `Throw` → Class; `Read` → any symbol; `Write` never. With an unknown
//! receiver only calls are matched: `Call`/`Decorator` → Method, `New` →
//! Class. Candidates are definitions of the same language family that are
//! neither tests nor local to a function or test body.
//!
//! Class members are looked up in the class, then in its resolved bases
//! depth-first, left-to-right (cycle-safe, depth ≤ 10). This approximates the
//! Python MRO (C3 linearization differs only for diamonds) and is exact for
//! ECMAScript single inheritance.
//!
//! # Edge kinds
//!
//! `Call`/`New` → `CALLS` (a call that targets a class also `CALLS` its
//! `__init__`/`constructor`, own or inherited, confidence × 0.95);
//! `Decorator` → `DECORATES` from the decorator to the decorated definition;
//! `Type`/`Read` → `REFERENCES`, except references that come from a class's own
//! base list, which become `INHERITS` / `IMPLEMENTS`; `Throw` → `THROWS`;
//! `Write` → `WRITES`. `Definition::bases` / `implements` are also resolved
//! directly (as `Type` references in the class's outer scope) into `INHERITS` /
//! `IMPLEMENTS`. A test definition that `CALLS` a non-test definition also
//! gets a `TESTS` edge with the same provenance.
//!
//! Edges are deduplicated by `(src, dst, kind)`: the highest-precedence
//! strategy and the maximum confidence are kept, together with the first
//! evidence span in `(file, byte offset)` order. Structural self-loops are
//! never emitted; recursive `CALLS` self-loops are. Every reference edge's
//! evidence is `(file, reference span)`.
//!
//! # Statistics
//!
//! [`ResolveStats`] counts every reference (plus decorators synthesized from
//! `Definition::decorators` when a pack reports no decorator references), the
//! resolved ones per strategy, and two classes of expected misses: builtins and
//! names reached through non-project modules. Unresolved references are logged
//! at `trace` level (`tracing` target `rueca_resolve`).
//!
//! # Prior art
//!
//! The design follows RFC 0002 §4.1. Scope handling (enclosing-function
//! chains, hoisted module-level definitions, import bindings per file) follows
//! the per-file scope graph model of bloop's `scope_resolution` (Apache-2.0)
//! and Sourcegraph scip-syntax locals (MIT). The Python LEGB lookup with member
//! resolution through base classes, and the layering "local scope → imports →
//! module → project-wide name match with explicit confidence", are adapted from
//! codanna's per-language resolvers (Apache-2.0, <https://github.com/bartolli/codanna>).
//! No code is copied; the algorithms are re-implemented over Rueca's facts.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use rueca_extract::{LanguagePack, ProjectFiles, Registry};
use rueca_model::{
    Definition, Edge, EdgeKind, EdgeMeta, FileEntry, FileFacts, FileId, Import, Label, Language, LanguageFamily,
    LocalDefId, Node, NodeId, RefKind, Reference, ResolvedGraph, SnapshotInfo, Span, Strategy,
};

/// One file of the snapshot as seen by the resolver.
#[derive(Debug, Clone)]
pub struct FileInput {
    /// Repository-relative path, `/` separators.
    pub path: String,
    pub blob_sha: String,
    pub language: Option<Language>,
    pub size: u64,
    /// `None` when the file was not extracted (unsupported language or skipped).
    pub facts: Option<FileFacts>,
    /// `ok`, `partial`, or `skipped:<reason>`; copied into `FileEntry::status`.
    pub status: String,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveStats {
    pub references_total: u64,
    pub references_resolved: u64,
    /// Count per strategy name (`import_resolved`, `same_module`, ...).
    pub resolved_by_strategy: std::collections::BTreeMap<String, u64>,
    /// Unresolved references whose name is a language builtin (`print`, `console`).
    pub references_builtin: u64,
    /// Unresolved references that go through a name bound to a non-project
    /// module (`os.path.join`, `React.useState`).
    pub references_external: u64,
}

pub struct ResolveOutput {
    pub graph: ResolvedGraph,
    pub stats: ResolveStats,
}

/// Maximum depth for transitive lookups (re-exports, base classes).
const MAX_DEPTH: usize = 10;

/// Resolves a snapshot. `files` may be in any order; the resolver sorts by path.
pub fn resolve(
    snapshot: SnapshotInfo,
    mut files: Vec<FileInput>,
    project: &ProjectFiles,
    registry: &Registry,
) -> ResolveOutput {
    files.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.blob_sha.cmp(&b.blob_sha)));
    files.dedup_by(|later, first| later.path == first.path);

    let mut r = Resolver::new(&files, project, registry, &snapshot);
    r.structural_edges();
    r.import_edges();
    r.build_envs();
    r.resolve_bases();
    r.resolve_references();
    r.tests_edges();

    let Resolver { nodes, edges, stats, line_counts, file_node, .. } = r;
    let file_entries = files
        .iter()
        .enumerate()
        .map(|(i, f)| FileEntry {
            path: f.path.clone(),
            blob_sha: f.blob_sha.clone(),
            language: f.language,
            size: f.size,
            line_count: line_counts[i],
            node: file_node[i],
            status: f.status.clone(),
        })
        .collect();
    let edges = edges.into_iter().map(|((src, kind, dst), meta)| Edge { src, dst, kind, meta }).collect();
    ResolveOutput { graph: ResolvedGraph { snapshot, nodes, edges, files: file_entries }, stats }
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

/// What a name is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Symbol(NodeId),
    /// A project module (file).
    Module(FileId),
    /// Python `import a.b` where `a` is not itself a project file (namespace
    /// package); walked segment by segment.
    Prefix(String),
    /// A non-project module or a name imported from one.
    External,
}

/// Environment entry: `None` = imported from a project module but the name
/// could not be found there (later strategies may still apply).
type Env = BTreeMap<String, Option<Target>>;

enum Outcome {
    Hit(NodeId, Strategy, f32),
    External,
    Miss,
}

/// A reference to resolve, plus where its edge goes.
struct RefItem<'a> {
    kind: RefKind,
    name: &'a str,
    receiver: Option<&'a str>,
    enclosing: Option<LocalDefId>,
    span: Span,
    /// For decorators: the decorated definition, when already known.
    decorated: Option<LocalDefId>,
}

struct Resolver<'a> {
    files: &'a [FileInput],
    project: &'a ProjectFiles,
    packs: Vec<Option<&'a dyn LanguagePack>>,
    families: Vec<Option<LanguageFamily>>,
    file_by_path: HashMap<&'a str, FileId>,
    module_names: Vec<String>,
    line_counts: Vec<u32>,

    nodes: Vec<Node>,
    repo_node: NodeId,
    folder_nodes: BTreeMap<String, NodeId>,
    file_node: Vec<NodeId>,
    /// Per file: local definition id → node id (after dedup).
    def_node: Vec<Vec<NodeId>>,
    /// Per file: sanitized parent of each definition.
    parents: Vec<Vec<Option<LocalDefId>>>,
    /// Per file: children of each definition, in definition order.
    children: Vec<Vec<Vec<LocalDefId>>>,
    /// Per file: top-level definitions, in definition order.
    top_level: Vec<Vec<LocalDefId>>,
    /// Per file: definitions with a non-empty base or implements list.
    with_bases: Vec<Vec<LocalDefId>>,
    /// Per file: first child of a definition by name.
    child_by_name: Vec<HashMap<(LocalDefId, &'a str), LocalDefId>>,
    /// Per file: top-level definition by name (first exported one, else first).
    top_by_name: Vec<HashMap<&'a str, NodeId>>,
    /// Members of each (canonical) definition node, by name; first wins.
    members: HashMap<NodeId, BTreeMap<String, NodeId>>,
    /// Non-test definitions by simple name, for strategy 6.
    by_name: HashMap<String, Vec<NodeId>>,
    /// Per file, per import: resolved target file.
    import_targets: Vec<Vec<Option<FileId>>>,
    envs: Vec<Env>,
    /// Resolved base classes (INHERITS targets) per class node, in order.
    bases: HashMap<NodeId, Vec<NodeId>>,

    /// Key: (module, name, reached through `export *`).
    lookup_memo: RefCell<HashMap<(FileId, String, bool), Option<Target>>>,
    module_memo: RefCell<HashMap<(FileId, u32, String), Option<FileId>>>,

    edges: BTreeMap<(NodeId, EdgeKind, NodeId), EdgeMeta>,
    stats: ResolveStats,
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn parent_dir(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(d, _)| d)
}

/// Splits a raw base-class expression into `(receiver, name)`:
/// `abc.ABC` → (`abc`, `ABC`), `Generic[T]` → (None, `Generic`). Keyword
/// arguments (`metaclass=M`) and complex expressions yield `None`.
fn split_base(text: &str) -> Option<(Option<&str>, &str)> {
    let t = text.trim();
    if t.contains('=') {
        return None;
    }
    let t = match t.find(['[', '<', '(']) {
        Some(i) => t[..i].trim(),
        None => t,
    };
    if t.is_empty() || !t.split('.').all(is_ident) {
        return None;
    }
    Some(match t.rsplit_once('.') {
        Some((r, n)) => (Some(r), n),
        None => (None, t),
    })
}

/// Splits a raw decorator expression (`app.route("/x")`, `@dataclass`).
fn split_decorator(text: &str) -> Option<(Option<&str>, &str)> {
    let t = text.trim().trim_start_matches('@');
    let t = match t.find('(') {
        Some(i) => &t[..i],
        None => t,
    };
    split_base(t)
}

fn normalize_stem(s: &str) -> String {
    s.chars().filter(|c| *c != '-' && *c != '_' && *c != '.').flat_map(|c| c.to_lowercase()).collect()
}

fn is_structural(kind: EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Contains | EdgeKind::Defines | EdgeKind::Imports | EdgeKind::Inherits | EdgeKind::Implements
    )
}

fn module_import(module: String, relative_level: u32) -> Import {
    Import {
        module,
        relative_level,
        names: Vec::new(),
        module_alias: None,
        type_only: false,
        reexport: false,
        span: Span::default(),
    }
}

/// Methods of the builtin container, string, iterator and promise types of
/// Python and ECMAScript. A call with an unknown receiver and one of these
/// names is far more likely to hit the builtin type than a project method, so
/// the unknown-receiver heuristic skips them (names under 4 chars are skipped
/// anyway).
const BUILTIN_METHODS: &[&str] = &[
    // Python dict / list / set / str / bytes / file
    "append",
    "clear",
    "copy",
    "count",
    "decode",
    "discard",
    "encode",
    "endswith",
    "extend",
    "format",
    "index",
    "insert",
    "items",
    "join",
    "keys",
    "lower",
    "lstrip",
    "partition",
    "popitem",
    "read",
    "readline",
    "readlines",
    "remove",
    "replace",
    "reverse",
    "rsplit",
    "rstrip",
    "setdefault",
    "sort",
    "split",
    "splitlines",
    "startswith",
    "strip",
    "title",
    "union",
    "update",
    "upper",
    "values",
    "write",
    "close",
    "flush",
    "seek",
    "tell",
    // ECMAScript Array / String / Map / Set / Promise / Object / Function
    "apply",
    "bind",
    "call",
    "catch",
    "concat",
    "entries",
    "every",
    "fill",
    "filter",
    "finally",
    "find",
    "findIndex",
    "flat",
    "flatMap",
    "forEach",
    "has",
    "includes",
    "indexOf",
    "lastIndexOf",
    "padEnd",
    "padStart",
    "push",
    "reduce",
    "shift",
    "slice",
    "some",
    "splice",
    "test",
    "then",
    "toFixed",
    "toLowerCase",
    "toString",
    "toUpperCase",
    "trim",
    "trimEnd",
    "trimStart",
    "unshift",
    "valueOf",
    "delete",
    "match",
    "matchAll",
    "charAt",
    "charCodeAt",
    "substring",
    "substr",
    "localeCompare",
];

/// Label compatibility for strategy 6.
fn compatible(kind: RefKind, label: Label, with_receiver: bool) -> bool {
    use Label::*;
    match kind {
        // A bare call never targets a method (no implicit receiver in Python
        // or ECMAScript).
        RefKind::Call | RefKind::Decorator => {
            if with_receiver {
                label == Method
            } else {
                matches!(label, Function | Class)
            }
        }
        RefKind::New => matches!(label, Class | Function) && (!with_receiver || label == Class),
        // Unknown-receiver matching is for method calls only.
        _ if with_receiver => false,
        RefKind::Type => matches!(label, Class | Interface | TypeAlias | Enum | Struct | Trait),
        RefKind::Throw => label == Class,
        RefKind::Read => !matches!(label, Repository | Folder | File | Module | Package),
        RefKind::Write => false,
    }
}

impl<'a> Resolver<'a> {
    fn new(files: &'a [FileInput], project: &'a ProjectFiles, registry: &'a Registry, snapshot: &SnapshotInfo) -> Self {
        let packs: Vec<Option<&dyn LanguagePack>> =
            files.iter().map(|f| f.language.and_then(|l| registry.for_language(l))).collect();
        let families = files.iter().map(|f| f.language.map(|l| l.family())).collect();
        let file_by_path = files.iter().enumerate().map(|(i, f)| (f.path.as_str(), i as FileId)).collect();
        let module_names = files
            .iter()
            .zip(&packs)
            .map(|(f, p)| match p {
                Some(p) => p.module_name(&f.path, project),
                None => f.path.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or_else(|| f.path.clone()),
            })
            .collect();
        let line_counts = files.iter().map(|f| f.facts.as_ref().map_or(0, |x| x.line_count)).collect();

        let mut r = Resolver {
            files,
            project,
            packs,
            families,
            file_by_path,
            module_names,
            line_counts,
            nodes: Vec::new(),
            repo_node: 0,
            folder_nodes: BTreeMap::new(),
            file_node: Vec::new(),
            def_node: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
            top_level: Vec::new(),
            with_bases: Vec::new(),
            top_by_name: Vec::new(),
            child_by_name: Vec::new(),
            members: HashMap::new(),
            by_name: HashMap::new(),
            import_targets: Vec::new(),
            envs: Vec::new(),
            bases: HashMap::new(),
            lookup_memo: RefCell::new(HashMap::new()),
            module_memo: RefCell::new(HashMap::new()),
            edges: BTreeMap::new(),
            stats: ResolveStats::default(),
        };
        r.build_nodes(snapshot);
        r.build_tables();
        r
    }

    fn defs(&self, f: FileId) -> &'a [Definition] {
        self.files[f as usize].facts.as_ref().map_or(&[], |x| x.definitions.as_slice())
    }

    fn imports(&self, f: FileId) -> &'a [Import] {
        self.files[f as usize].facts.as_ref().map_or(&[], |x| x.imports.as_slice())
    }

    fn is_python(&self, f: FileId) -> bool {
        self.families[f as usize] == Some(LanguageFamily::Python)
    }

    fn node_of(&self, f: FileId, local: LocalDefId) -> NodeId {
        self.def_node[f as usize][local as usize]
    }

    // -- nodes ---------------------------------------------------------------

    fn build_nodes(&mut self, snapshot: &SnapshotInfo) {
        let blank = |label, name: &str, qn: &str| Node {
            label,
            name: name.to_string(),
            qualified_name: qn.to_string(),
            file: None,
            span: None,
            language: None,
            signature: None,
            doc: None,
            exported: false,
            is_test: false,
        };
        let repo_name = snapshot.repo.rsplit('/').next().unwrap_or(&snapshot.repo);
        self.nodes.push(blank(Label::Repository, repo_name, &snapshot.repo));
        self.repo_node = 0;

        let mut folders = BTreeSet::new();
        for f in self.files {
            let mut p = parent_dir(&f.path);
            while let Some(d) = p {
                if d.is_empty() || !folders.insert(d.to_string()) {
                    break;
                }
                p = parent_dir(d);
            }
        }
        for d in folders {
            let id = self.nodes.len() as NodeId;
            self.nodes.push(blank(Label::Folder, basename(&d), &d));
            self.folder_nodes.insert(d, id);
        }

        for (i, f) in self.files.iter().enumerate() {
            let id = self.nodes.len() as NodeId;
            let mut n = blank(Label::File, basename(&f.path), &f.path);
            n.file = Some(i as FileId);
            n.language = f.language;
            self.nodes.push(n);
            self.file_node.push(id);
        }

        let mut seen: HashMap<(Label, String, FileId), NodeId> = HashMap::new();
        for (i, f) in self.files.iter().enumerate() {
            let fid = i as FileId;
            let defs = self.defs(fid);
            let sep = match self.families[i] {
                Some(LanguageFamily::Python) => ".",
                _ => ":",
            };
            let module = &self.module_names[i];
            let mut map = Vec::with_capacity(defs.len());
            for d in defs {
                let qn = if module.is_empty() {
                    d.qualified_name.clone()
                } else {
                    format!("{module}{sep}{}", d.qualified_name)
                };
                let key = (d.label, qn, fid);
                if let Some(&existing) = seen.get(&key) {
                    map.push(existing);
                    continue;
                }
                let id = self.nodes.len() as NodeId;
                self.nodes.push(Node {
                    label: d.label,
                    name: d.name.clone(),
                    qualified_name: key.1.clone(),
                    file: Some(fid),
                    span: Some(d.span),
                    language: f.language,
                    signature: d.signature.clone(),
                    doc: d.doc.clone(),
                    exported: d.exported,
                    is_test: d.is_test,
                });
                seen.insert(key, id);
                map.push(id);
            }
            self.def_node.push(map);
        }
    }

    fn build_tables(&mut self) {
        for i in 0..self.files.len() {
            let fid = i as FileId;
            let defs = self.defs(fid);
            let n = defs.len();
            let mut parents: Vec<Option<LocalDefId>> = defs
                .iter()
                .enumerate()
                .map(|(j, d)| d.parent.filter(|&p| (p as usize) < n && p as usize != j))
                .collect();
            // Break malformed parent cycles.
            for j in 0..n {
                let mut cur = parents[j];
                let mut steps = 0;
                while let Some(p) = cur {
                    steps += 1;
                    if p as usize == j || steps > n {
                        parents[j] = None;
                        break;
                    }
                    cur = parents[p as usize];
                }
            }
            let mut children = vec![Vec::new(); n];
            let mut top = Vec::new();
            for (j, p) in parents.iter().enumerate() {
                match p {
                    Some(p) => children[*p as usize].push(j as LocalDefId),
                    None => top.push(j as LocalDefId),
                }
            }
            for (j, p) in parents.iter().enumerate() {
                if let Some(p) = p {
                    let owner = self.def_node[i][*p as usize];
                    let member = self.def_node[i][j];
                    if owner != member {
                        self.members.entry(owner).or_default().entry(defs[j].name.clone()).or_insert(member);
                    }
                }
            }
            for (j, d) in defs.iter().enumerate() {
                if d.is_test || d.label == Label::Test {
                    continue;
                }
                // Definitions local to a function or test body are only
                // visible through the local scope chain (strategy 1).
                let mut local = false;
                let mut cur = parents[j];
                while let Some(p) = cur {
                    let pd = &defs[p as usize];
                    if pd.label.is_callable() || pd.is_test {
                        local = true;
                        break;
                    }
                    cur = parents[p as usize];
                }
                if local {
                    continue;
                }
                let node = self.def_node[i][j];
                let v = self.by_name.entry(d.name.clone()).or_default();
                if !v.contains(&node) {
                    v.push(node);
                }
            }
            self.with_bases.push(
                (0..n as LocalDefId)
                    .filter(|&j| !defs[j as usize].bases.is_empty() || !defs[j as usize].implements.is_empty())
                    .collect(),
            );
            let mut by_name: HashMap<&'a str, (bool, NodeId)> = HashMap::new();
            for &j in &top {
                let d = &defs[j as usize];
                let node = self.def_node[i][j as usize];
                match by_name.get(d.name.as_str()) {
                    Some((true, _)) => {}
                    Some((false, _)) if !d.exported => {}
                    _ => {
                        by_name.insert(d.name.as_str(), (d.exported, node));
                    }
                }
            }
            self.top_by_name.push(by_name.into_iter().map(|(k, (_, n))| (k, n)).collect());
            let mut child_by_name = HashMap::new();
            for (p, kids) in children.iter().enumerate() {
                for &c in kids {
                    child_by_name.entry((p as LocalDefId, defs[c as usize].name.as_str())).or_insert(c);
                }
            }
            self.child_by_name.push(child_by_name);
            self.parents.push(parents);
            self.children.push(children);
            self.top_level.push(top);
        }

        // Import targets.
        let mut targets = Vec::with_capacity(self.files.len());
        for (i, f) in self.files.iter().enumerate() {
            let fid = i as FileId;
            let t: Vec<Option<FileId>> = match self.packs[i] {
                Some(pack) => self
                    .imports(fid)
                    .iter()
                    .map(|imp| pack.resolve_import(&f.path, imp, self.project).and_then(|p| self.file_id(&p)))
                    .collect(),
                None => vec![None; self.imports(fid).len()],
            };
            targets.push(t);
        }
        self.import_targets = targets;
    }

    fn file_id(&self, path: &str) -> Option<FileId> {
        self.file_by_path.get(path).copied()
    }

    // -- edges ---------------------------------------------------------------

    fn add_edge(&mut self, src: NodeId, dst: NodeId, kind: EdgeKind, meta: EdgeMeta) {
        if src == dst && is_structural(kind) {
            return;
        }
        match self.edges.entry((src, kind, dst)) {
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert(meta);
            }
            std::collections::btree_map::Entry::Occupied(mut o) => {
                let cur = o.get_mut();
                cur.strategy = cur.strategy.min(meta.strategy);
                cur.confidence = cur.confidence.max(meta.confidence);
                let key = |e: &Option<(FileId, Span)>| e.map(|(f, s)| (f, s.start_byte, s.end_byte));
                cur.evidence = match (cur.evidence, meta.evidence) {
                    (Some(a), Some(b)) => Some(if key(&Some(b)) < key(&Some(a)) { b } else { a }),
                    (a, b) => a.or(b),
                };
            }
        }
    }

    fn syntactic() -> EdgeMeta {
        EdgeMeta { confidence: 1.0, strategy: Strategy::Syntactic, evidence: None }
    }

    fn container_of(&self, path: &str) -> NodeId {
        match parent_dir(path) {
            Some(d) if !d.is_empty() => self.folder_nodes.get(d).copied().unwrap_or(self.repo_node),
            _ => self.repo_node,
        }
    }

    fn structural_edges(&mut self) {
        let folders: Vec<(String, NodeId)> = self.folder_nodes.iter().map(|(k, v)| (k.clone(), *v)).collect();
        for (path, id) in folders {
            let parent = self.container_of(&path);
            self.add_edge(parent, id, EdgeKind::Contains, Self::syntactic());
        }
        for i in 0..self.files.len() {
            let parent = self.container_of(&self.files[i].path);
            let fnode = self.file_node[i];
            self.add_edge(parent, fnode, EdgeKind::Contains, Self::syntactic());
            let fid = i as FileId;
            for j in 0..self.def_node[i].len() {
                let child = self.def_node[i][j];
                let src = match self.parents[i][j] {
                    Some(p) => self.node_of(fid, p),
                    None => fnode,
                };
                self.add_edge(src, child, EdgeKind::Defines, Self::syntactic());
            }
        }
    }

    fn import_edges(&mut self) {
        for i in 0..self.files.len() {
            let fid = i as FileId;
            let fnode = self.file_node[i];
            for (k, imp) in self.imports(fid).iter().enumerate() {
                let meta =
                    EdgeMeta { confidence: 1.0, strategy: Strategy::ImportResolved, evidence: Some((fid, imp.span)) };
                if let Some(t) = self.import_targets[i][k] {
                    self.add_edge(fnode, self.file_node[t as usize], EdgeKind::Imports, meta);
                }
                if self.is_python(fid) {
                    for n in &imp.names {
                        if n.name == "*" {
                            continue;
                        }
                        let sub = Self::join_module(&imp.module, &n.name);
                        if let Some(t) = self.resolve_module(fid, imp.relative_level, &sub) {
                            self.add_edge(fnode, self.file_node[t as usize], EdgeKind::Imports, meta);
                        }
                    }
                }
            }
        }
    }

    // -- module lookups ------------------------------------------------------

    fn join_module(base: &str, name: &str) -> String {
        if base.is_empty() { name.to_string() } else { format!("{base}.{name}") }
    }

    /// Resolves a module specifier written in file `f` to a project file.
    fn resolve_module(&self, f: FileId, level: u32, module: &str) -> Option<FileId> {
        let key = (f, level, module.to_string());
        if let Some(v) = self.module_memo.borrow().get(&key) {
            return *v;
        }
        let pack = self.packs[f as usize]?;
        let imp = module_import(module.to_string(), level);
        let v = pack.resolve_import(&self.files[f as usize].path, &imp, self.project).and_then(|p| self.file_id(&p));
        self.module_memo.borrow_mut().insert(key, v);
        v
    }

    /// First top-level definition of `f` named `name`, preferring exported ones.
    fn top_level_def(&self, f: FileId, name: &str) -> Option<NodeId> {
        self.top_by_name[f as usize].get(name).copied()
    }

    /// ECMAScript default export of `f` (best effort, see crate docs).
    fn default_export(&self, f: FileId) -> Option<NodeId> {
        if let Some(n) = self.top_level_def(f, "default") {
            return Some(n);
        }
        let path = &self.files[f as usize].path;
        let file = basename(path);
        let mut stem = file.split('.').next().unwrap_or(file);
        if stem == "index"
            && let Some(d) = parent_dir(path)
        {
            stem = basename(d);
        }
        let stem = normalize_stem(stem);
        let defs = self.defs(f);
        let exported: Vec<LocalDefId> =
            self.top_level[f as usize].iter().copied().filter(|&j| defs[j as usize].exported).collect();
        let is_cf = |j: LocalDefId| matches!(defs[j as usize].label, Label::Class | Label::Function);
        let named: Vec<LocalDefId> =
            exported.iter().copied().filter(|&j| is_cf(j) && normalize_stem(&defs[j as usize].name) == stem).collect();
        if named.len() == 1 {
            return Some(self.node_of(f, named[0]));
        }
        if exported.len() == 1 && is_cf(exported[0]) {
            return Some(self.node_of(f, exported[0]));
        }
        None
    }

    /// Looks `name` up in module `f`: own definitions, re-exports, (Python)
    /// import bindings and submodules. Transitive, cycle-safe, bounded.
    fn lookup_in_module(&self, f: FileId, name: &str, visited: &mut Vec<(FileId, String)>) -> Option<Target> {
        self.lookup_in_module_as(f, name, false, visited)
    }

    /// `via_star`: reached through an ECMAScript `export *`, where only
    /// names the module itself exports count (not its plain imports).
    fn lookup_in_module_as(
        &self,
        f: FileId,
        name: &str,
        via_star: bool,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        let via_star = via_star && !self.is_python(f);
        let memo_key = (f, name.to_string(), via_star);
        if let Some(v) = self.lookup_memo.borrow().get(&memo_key) {
            return v.clone();
        }
        let key = (f, memo_key.1.clone());
        if visited.len() > MAX_DEPTH || visited.contains(&key) {
            return None;
        }
        visited.push(key);
        let result = self.lookup_in_module_uncached(f, name, via_star, visited);
        visited.pop();
        if visited.is_empty() {
            self.lookup_memo.borrow_mut().insert(memo_key, result.clone());
        }
        result
    }

    fn lookup_in_module_uncached(
        &self,
        f: FileId,
        name: &str,
        via_star: bool,
        visited: &mut Vec<(FileId, String)>,
    ) -> Option<Target> {
        let python = self.is_python(f);
        if let Some(n) = self.top_level_def(f, name) {
            return Some(Target::Symbol(n));
        }
        if !python
            && name == "default"
            && let Some(n) = self.default_export(f)
        {
            return Some(Target::Symbol(n));
        }
        // Re-exports, in order. Explicit names first, then star re-exports.
        let imports = self.imports(f);
        let mut stars = Vec::new();
        for (k, imp) in imports.iter().enumerate() {
            if !imp.reexport {
                continue;
            }
            let target = self.import_targets[f as usize][k];
            if imp.names.is_empty() {
                match &imp.module_alias {
                    Some(a) if a == name => return Some(target.map_or(Target::External, Target::Module)),
                    Some(_) => {}
                    None => stars.push(target),
                }
                continue;
            }
            for n in &imp.names {
                let local = n.alias.as_deref().unwrap_or(&n.name);
                if n.name == "*" {
                    match &n.alias {
                        Some(a) if a == name => return Some(target.map_or(Target::External, Target::Module)),
                        Some(_) => {}
                        None => stars.push(target),
                    }
                } else if local == name {
                    return match target {
                        Some(t) => self.lookup_in_module(t, &n.name, visited),
                        None => Some(Target::External),
                    };
                }
            }
        }
        if name != "default" {
            for t in stars.into_iter().flatten() {
                if let Some(found) = self.lookup_in_module_as(t, name, true, visited)
                    && found != Target::External
                {
                    return Some(found);
                }
            }
        }
        // Names bound by this module's own imports (last binding wins):
        // Python package `__init__.py` re-exports, and ECMAScript
        // `import { X } from "./x"; export { X }` (packs record the local
        // export only as a flag, so an imported name that another module
        // imports from here is taken to be exported — valid code guarantees it).
        let mut found = None;
        for (k, imp) in imports.iter().enumerate() {
            if imp.reexport || via_star {
                continue;
            }
            if let Some(b) = self.import_binding(f, k, Some(name), visited).pop() {
                found = Some(b.1);
            }
        }
        if let Some(b) = found {
            return b;
        }
        if python {
            // Submodule `<module>.<name>`.
            let sub = Self::join_module(&self.module_names[f as usize], name);
            if let Some(t) = self.resolve_module(f, 0, &sub) {
                return Some(Target::Module(t));
            }
        }
        None
    }

    /// Names bound in `f` by its `k`-th import (only `want` if given).
    fn import_binding(
        &self,
        f: FileId,
        k: usize,
        want: Option<&str>,
        visited: &mut Vec<(FileId, String)>,
    ) -> Vec<(String, Option<Target>)> {
        let imp = &self.imports(f)[k];
        let target = self.import_targets[f as usize][k];
        let python = self.is_python(f);
        let wants = |n: &str| want.is_none_or(|w| w == n);
        let mut out = Vec::new();
        if imp.names.is_empty() {
            match &imp.module_alias {
                Some(a) => {
                    if wants(a) {
                        out.push((a.clone(), Some(target.map_or(Target::External, Target::Module))));
                    }
                }
                None if python && imp.relative_level == 0 => {
                    let root = imp.module.split('.').next().unwrap_or("");
                    if !root.is_empty() && wants(root) {
                        let t = if let Some(m) = self.resolve_module(f, 0, root) {
                            Target::Module(m)
                        } else if target.is_some() {
                            Target::Prefix(root.to_string())
                        } else {
                            Target::External
                        };
                        out.push((root.to_string(), Some(t)));
                    }
                }
                None => {}
            }
            return out;
        }
        for n in &imp.names {
            if n.name == "*" {
                if let Some(a) = &n.alias {
                    // `import * as ns` represented as a named entry.
                    if wants(a) {
                        out.push((a.clone(), Some(target.map_or(Target::External, Target::Module))));
                    }
                    continue;
                }
                let Some(t) = target else { continue };
                if !python {
                    continue;
                }
                // Python star import: exported top-level definitions.
                let defs = self.defs(t);
                for &j in &self.top_level[t as usize] {
                    let d = &defs[j as usize];
                    if d.exported && !d.name.starts_with('_') && wants(&d.name) {
                        out.push((d.name.clone(), Some(Target::Symbol(self.node_of(t, j)))));
                    }
                }
                continue;
            }
            let local = n.alias.as_deref().unwrap_or(&n.name);
            if !wants(local) {
                continue;
            }
            let bound = match target {
                Some(t) => self.lookup_in_module(t, &n.name, visited),
                None => None,
            };
            let bound = match bound {
                Some(b) => Some(b),
                None if python => {
                    let sub = Self::join_module(&imp.module, &n.name);
                    match self.resolve_module(f, imp.relative_level, &sub) {
                        Some(m) => Some(Target::Module(m)),
                        None if target.is_none() => Some(Target::External),
                        None => None,
                    }
                }
                None if target.is_none() => Some(Target::External),
                None => None,
            };
            out.push((local.to_string(), bound));
        }
        out
    }

    fn build_envs(&mut self) {
        let mut envs = Vec::with_capacity(self.files.len());
        for i in 0..self.files.len() {
            let fid = i as FileId;
            let python = self.is_python(fid);
            let mut env = Env::new();
            for (k, imp) in self.imports(fid).iter().enumerate() {
                if imp.reexport && !python {
                    continue;
                }
                let star = imp.names.iter().any(|n| n.name == "*" && n.alias.is_none());
                for (name, t) in self.import_binding(fid, k, None, &mut Vec::new()) {
                    if star {
                        env.entry(name).or_insert(t);
                    } else {
                        env.insert(name, t);
                    }
                }
            }
            envs.push(env);
        }
        self.envs = envs;
    }

    // -- members and bases ---------------------------------------------------

    /// Member `name` of class `class`, searching resolved bases depth-first,
    /// left-to-right. Returns the member and whether it was inherited.
    fn find_member(&self, class: NodeId, name: &str) -> Option<(NodeId, bool)> {
        let mut visited = Vec::new();
        self.find_member_rec(class, name, 0, &mut visited).map(|(n, d)| (n, d > 0))
    }

    fn find_member_rec(
        &self,
        class: NodeId,
        name: &str,
        depth: usize,
        visited: &mut Vec<NodeId>,
    ) -> Option<(NodeId, usize)> {
        if depth > MAX_DEPTH || visited.contains(&class) {
            return None;
        }
        visited.push(class);
        if let Some(m) = self.members.get(&class).and_then(|m| m.get(name)) {
            return Some((*m, depth));
        }
        for &b in self.bases.get(&class).map_or(&[][..], |v| v.as_slice()) {
            if let Some(found) = self.find_member_rec(b, name, depth + 1, visited) {
                return Some(found);
            }
        }
        None
    }

    fn resolve_bases(&mut self) {
        for i in 0..self.files.len() {
            let fid = i as FileId;
            let defs = self.defs(fid);
            for (j, d) in defs.iter().enumerate() {
                if d.bases.is_empty() && d.implements.is_empty() {
                    continue;
                }
                let class = self.node_of(fid, j as LocalDefId);
                let scope = self.parents[i][j];
                let lists = [(EdgeKind::Inherits, &d.bases), (EdgeKind::Implements, &d.implements)];
                for (kind, list) in lists {
                    for text in list.iter() {
                        let Some((receiver, name)) = split_base(text) else { continue };
                        let item = RefItem {
                            kind: RefKind::Type,
                            name,
                            receiver,
                            enclosing: scope,
                            span: d.name_span,
                            decorated: None,
                        };
                        if let Outcome::Hit(target, strategy, confidence) = self.resolve_item(fid, &item, scope) {
                            if target == class {
                                continue;
                            }
                            let meta = EdgeMeta { confidence, strategy, evidence: Some((fid, d.name_span)) };
                            self.add_edge(class, target, kind, meta);
                            if kind == EdgeKind::Inherits {
                                let v = self.bases.entry(class).or_default();
                                if !v.contains(&target) {
                                    v.push(target);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // -- reference resolution ------------------------------------------------

    /// Strategy 1: a name defined by an enclosing function.
    fn local_scope(&self, f: FileId, scope: Option<LocalDefId>, name: &str) -> Option<NodeId> {
        let defs = self.defs(f);
        let mut cur = scope;
        while let Some(a) = cur {
            if !defs[a as usize].label.is_type()
                && let Some(&c) = self.child_by_name[f as usize].get(&(a, name))
            {
                return Some(self.node_of(f, c));
            }
            cur = self.parents[f as usize][a as usize];
        }
        None
    }

    /// Nearest enclosing type definition.
    fn enclosing_type(&self, f: FileId, scope: Option<LocalDefId>) -> Option<NodeId> {
        let defs = self.defs(f);
        let mut cur = scope;
        while let Some(a) = cur {
            if defs[a as usize].label.is_type() {
                return Some(self.node_of(f, a));
            }
            cur = self.parents[f as usize][a as usize];
        }
        None
    }

    /// Walks a dotted path from a starting binding.
    fn walk(&self, f: FileId, start: Target, segs: &[&str]) -> Option<Target> {
        let mut cur = start;
        for seg in segs {
            cur = match cur {
                Target::Module(m) => self.lookup_in_module(m, seg, &mut Vec::new())?,
                Target::Symbol(n) => {
                    if !self.nodes[n as usize].label.is_type() {
                        return None;
                    }
                    Target::Symbol(self.find_member(n, seg)?.0)
                }
                Target::Prefix(p) => {
                    let next = format!("{p}.{seg}");
                    match self.resolve_module(f, 0, &next) {
                        Some(m) => Target::Module(m),
                        None => Target::Prefix(next),
                    }
                }
                Target::External => return Some(Target::External),
            };
        }
        Some(cur)
    }

    fn resolve_item(&self, f: FileId, item: &RefItem<'_>, scope: Option<LocalDefId>) -> Outcome {
        let name = item.name;
        let pack = self.packs[f as usize];
        let selfs: &[&str] = pack.map_or(&[], |p| p.self_receivers());
        let Some(receiver) = item.receiver.map(str::trim) else {
            // 1. Local scope.
            if let Some(n) = self.local_scope(f, scope, name) {
                return Outcome::Hit(n, Strategy::SameModule, 0.95);
            }
            // 3. Imported binding.
            match self.envs[f as usize].get(name) {
                Some(Some(Target::Symbol(n))) => return Outcome::Hit(*n, Strategy::ImportResolved, 0.95),
                Some(Some(_)) => return Outcome::External,
                Some(None) | None => {}
            }
            // 4. Same module.
            if let Some(n) = self.top_level_def(f, name) {
                return Outcome::Hit(n, Strategy::SameModule, 0.9);
            }
            // 6. Unique name.
            return self.unique_name(f, item.kind, name, false);
        };

        // 2. Self receiver.
        if selfs.contains(&receiver) {
            return match self.enclosing_type(f, scope).and_then(|c| self.find_member(c, name)) {
                Some((n, false)) => Outcome::Hit(n, Strategy::TypeResolved, 0.9),
                Some((n, true)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
                None => Outcome::Miss,
            };
        }
        if receiver == "super()" || receiver == "super" {
            let found = self
                .enclosing_type(f, scope)
                .and_then(|c| self.bases.get(&c)?.iter().find_map(|&b| self.find_member(b, name)));
            return match found {
                Some((n, _)) => Outcome::Hit(n, Strategy::TypeResolved, 0.85),
                None => Outcome::Miss,
            };
        }

        let segs: Vec<&str> = receiver.split('.').collect();
        if segs.iter().all(|s| is_ident(s)) && !selfs.contains(&segs[0]) {
            let head = segs[0];
            match self.envs[f as usize].get(head) {
                // 3. Receiver bound by an import.
                Some(Some(start)) => match self.walk(f, start.clone(), &segs[1..]) {
                    Some(Target::Module(m)) => match self.lookup_in_module(m, name, &mut Vec::new()) {
                        Some(Target::Symbol(n)) => return Outcome::Hit(n, Strategy::ImportResolved, 0.9),
                        Some(_) => return Outcome::External,
                        None => {}
                    },
                    Some(Target::Symbol(c)) if self.nodes[c as usize].label.is_type() => {
                        if let Some((n, _)) = self.find_member(c, name) {
                            return Outcome::Hit(n, Strategy::ImportResolved, 0.85);
                        }
                    }
                    Some(Target::External) | Some(Target::Prefix(_)) => return Outcome::External,
                    _ => {}
                },
                Some(None) => {}
                // 4. Receiver is a class of the same module.
                None => {
                    let local = self.top_level_def(f, head);
                    if local.is_none() && pack.is_some_and(|p| p.is_builtin(head)) {
                        // `console.log`, `Math.max`, `str.join`: a builtin object.
                        return Outcome::External;
                    }
                    if let Some(c) = local
                        && self.nodes[c as usize].label.is_type()
                        && let Some(Target::Symbol(c)) = self.walk(f, Target::Symbol(c), &segs[1..])
                        && self.nodes[c as usize].label.is_type()
                        && let Some((n, _)) = self.find_member(c, name)
                    {
                        return Outcome::Hit(n, Strategy::SameModule, 0.85);
                    }
                }
            }
        }
        // 6. Unique method name.
        self.unique_name(f, item.kind, name, true)
    }

    /// Strategy 6.
    fn unique_name(&self, f: FileId, kind: RefKind, name: &str, with_receiver: bool) -> Outcome {
        if kind == RefKind::Write || name.is_empty() {
            return Outcome::Miss;
        }
        if self.packs[f as usize].is_some_and(|p| p.is_builtin(name)) {
            return Outcome::Miss;
        }
        if with_receiver && (name.chars().count() < 4 || BUILTIN_METHODS.contains(&name)) {
            return Outcome::Miss;
        }
        let family = self.families[f as usize];
        let Some(cands) = self.by_name.get(name) else { return Outcome::Miss };
        let mut found = None;
        for &n in cands {
            let node = &self.nodes[n as usize];
            let same_family = node.language.map(|l| l.family()) == family;
            if same_family && compatible(kind, node.label, with_receiver) {
                if found.is_some() {
                    return Outcome::Miss;
                }
                found = Some(n);
            }
        }
        match found {
            Some(n) if with_receiver => Outcome::Hit(n, Strategy::Heuristic, 0.5),
            Some(n) => Outcome::Hit(n, Strategy::UniqueName, 0.6),
            None => Outcome::Miss,
        }
    }

    /// The definition a decorator reference applies to: the first decorated
    /// definition whose name starts after the decorator.
    fn decorated_def(&self, f: FileId, r: &RefItem<'_>) -> Option<LocalDefId> {
        if r.decorated.is_some() {
            return r.decorated;
        }
        let defs = self.defs(f);
        let found = defs
            .iter()
            .enumerate()
            .filter(|(_, d)| !d.decorators.is_empty() && d.name_span.start_byte >= r.span.end_byte)
            .min_by_key(|(j, d)| (d.name_span.start_byte, *j))
            .map(|(j, _)| j as LocalDefId);
        found.or(r.enclosing)
    }

    /// If `r` names one of a class's own bases, returns the class and the edge kind.
    fn base_ref(&self, f: FileId, r: &RefItem<'_>) -> Option<(LocalDefId, EdgeKind)> {
        let defs = self.defs(f);
        let mut best: Option<LocalDefId> = None;
        for &j in &self.with_bases[f as usize] {
            let d = &defs[j as usize];
            if !d.span.contains(&r.span) {
                continue;
            }
            if best.is_some_and(|b| !defs[b as usize].span.contains(&d.span)) {
                continue;
            }
            let inside_child =
                self.children[f as usize][j as usize].iter().any(|&c| defs[c as usize].span.contains(&r.span));
            if !inside_child {
                best = Some(j);
            }
        }
        let c = best?;
        let d = &defs[c as usize];
        let matches = |text: &String| split_base(text) == Some((r.receiver, r.name));
        if d.bases.iter().any(matches) {
            Some((c, EdgeKind::Inherits))
        } else if d.implements.iter().any(matches) {
            Some((c, EdgeKind::Implements))
        } else {
            None
        }
    }

    fn resolve_references(&mut self) {
        let files: &'a [FileInput] = self.files;
        for (i, file) in files.iter().enumerate() {
            let fid = i as FileId;
            let Some(facts) = file.facts.as_ref() else { continue };
            let n_defs = facts.definitions.len();
            let valid = |e: Option<LocalDefId>| e.filter(|&x| (x as usize) < n_defs);
            let mut items: Vec<RefItem<'a>> = facts
                .references
                .iter()
                .map(|r: &'a Reference| RefItem {
                    kind: r.kind,
                    name: &r.name,
                    receiver: r.receiver.as_deref(),
                    enclosing: valid(r.enclosing),
                    span: r.span,
                    decorated: None,
                })
                .collect();
            // Packs that do not report decorator references: synthesize them
            // from the raw decorator texts.
            if !facts.references.iter().any(|r| r.kind == RefKind::Decorator) {
                for (j, d) in facts.definitions.iter().enumerate() {
                    for text in &d.decorators {
                        if let Some((receiver, name)) = split_decorator(text) {
                            items.push(RefItem {
                                kind: RefKind::Decorator,
                                name,
                                receiver,
                                enclosing: self.parents[i][j],
                                span: d.name_span,
                                decorated: Some(j as LocalDefId),
                            });
                        }
                    }
                }
            }
            for item in &items {
                self.resolve_one(fid, item);
            }
        }
    }

    fn resolve_one(&mut self, f: FileId, item: &RefItem<'_>) {
        self.stats.references_total += 1;
        let fnode = self.file_node[f as usize];
        let caller = item.enclosing.map_or(fnode, |e| self.node_of(f, e));
        let base = match item.kind {
            RefKind::Type | RefKind::Read => self.base_ref(f, item),
            _ => None,
        };
        let (outcome, src, dst_override, edge_kind) = match (item.kind, base) {
            (RefKind::Decorator, _) => {
                let decorated = self.decorated_def(f, item);
                let scope = match decorated {
                    Some(d) if item.enclosing == Some(d) => self.parents[f as usize][d as usize],
                    _ => item.enclosing,
                };
                let dst = decorated.map(|d| self.node_of(f, d));
                (self.resolve_item(f, item, scope), None, dst, EdgeKind::Decorates)
            }
            // A name in a class's own base / implements list.
            (_, Some((c, kind))) => {
                let scope = self.parents[f as usize][c as usize];
                (self.resolve_item(f, item, scope), Some(self.node_of(f, c)), None, kind)
            }
            (kind, None) => {
                let edge = match kind {
                    RefKind::Call | RefKind::New => EdgeKind::Calls,
                    RefKind::Throw => EdgeKind::Throws,
                    RefKind::Write => EdgeKind::Writes,
                    _ => EdgeKind::References,
                };
                (self.resolve_item(f, item, item.enclosing), None, None, edge)
            }
        };
        let (target, strategy, confidence) = match outcome {
            Outcome::Hit(t, s, c) => (t, s, c),
            Outcome::External => {
                self.stats.references_external += 1;
                return;
            }
            Outcome::Miss => {
                tracing::trace!(
                    kind = ?item.kind,
                    receiver = item.receiver.unwrap_or("-"),
                    name = item.name,
                    path = %self.files[f as usize].path,
                    "unresolved"
                );
                if item.receiver.is_none() && self.packs[f as usize].is_some_and(|p| p.is_builtin(item.name)) {
                    self.stats.references_builtin += 1;
                }
                return;
            }
        };
        self.stats.references_resolved += 1;
        *self.stats.resolved_by_strategy.entry(strategy.as_str().to_string()).or_default() += 1;
        let meta = EdgeMeta { confidence, strategy, evidence: Some((f, item.span)) };
        match edge_kind {
            EdgeKind::Decorates => {
                if let Some(dst) = dst_override {
                    self.add_edge(target, dst, EdgeKind::Decorates, meta);
                }
            }
            EdgeKind::Inherits | EdgeKind::Implements => {
                let src = src.unwrap_or(caller);
                if src != target {
                    self.add_edge(src, target, edge_kind, meta);
                }
            }
            EdgeKind::Calls => {
                self.add_edge(caller, target, EdgeKind::Calls, meta);
                if self.nodes[target as usize].label.is_type() {
                    let ctor = self.find_member(target, "__init__").or_else(|| self.find_member(target, "constructor"));
                    if let Some((c, _)) = ctor {
                        let m = EdgeMeta { confidence: confidence * 0.95, ..meta };
                        self.add_edge(caller, c, EdgeKind::Calls, m);
                    }
                }
            }
            kind => self.add_edge(caller, target, kind, meta),
        }
    }

    fn tests_edges(&mut self) {
        let calls: Vec<(NodeId, NodeId, EdgeMeta)> = self
            .edges
            .iter()
            .filter(|((src, kind, dst), _)| {
                *kind == EdgeKind::Calls && self.nodes[*src as usize].is_test && !self.nodes[*dst as usize].is_test
            })
            .map(|((src, _, dst), meta)| (*src, *dst, *meta))
            .collect();
        for (src, dst, meta) in calls {
            let file_like = matches!(self.nodes[dst as usize].label, Label::File | Label::Folder | Label::Repository);
            if !file_like {
                self.add_edge(src, dst, EdgeKind::Tests, meta);
            }
        }
    }
}

#[cfg(test)]
mod tests;
