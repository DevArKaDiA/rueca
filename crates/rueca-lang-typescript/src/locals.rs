//! Local-binding detection driven by the vendored `locals.scm` queries
//! (RFC 0002 §3.4).
//!
//! The queries in `queries/` (nvim-treesitter + tree-sitter-javascript,
//! adapted; provenance in `queries/README.md`) mark `@local.scope` nodes and
//! `@local.definition*` binding identifiers. nvim-treesitter's `; inherits:`
//! directive is resolved here by concatenation: JavaScript compiles
//! `ecma + javascript`, TypeScript `ecma + typescript`, TSX
//! `ecma + typescript + tsx`.
//!
//! Rueca does not need per-block precision: the walker asks "is this name a
//! local binding of the current function (or an outer function)?". So every
//! definition is attributed to an **owner**:
//!
//! 1. its scope is the innermost `@local.scope` node containing it — or the
//!    next one out when the pattern sets `definition.var.scope = parent`
//!    (a function's name binds in the scope around the function);
//! 2. the owner is the nearest function-like scope at or above that scope
//!    (anything captured as a scope except the block scopes in
//!    [`BLOCK_SCOPES`]);
//! 3. with no function-like scope the owner is the module, but only when the
//!    binding sits inside a block (`if`, `for`, `catch`, bare `{}`): plain
//!    top-level declarations are module symbols, not locals.
//!
//! `@rueca.namespace_body` nodes (TS namespace / ambient module bodies) are
//! not scopes: declarations in them are namespace members.

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::Instant;

use rueca_extract::{ExtractError, text_of};
use rueca_model::Language;
use tree_sitter::{Node, Query, QueryCursor, QueryCursorOptions, StreamingIterator};

pub(crate) const ECMA_LOCALS: &str = include_str!("../queries/ecma-locals.scm");
pub(crate) const JAVASCRIPT_LOCALS: &str = include_str!("../queries/javascript-locals.scm");
pub(crate) const TYPESCRIPT_LOCALS: &str = include_str!("../queries/typescript-locals.scm");
pub(crate) const TSX_LOCALS: &str = include_str!("../queries/tsx-locals.scm");

/// Scopes that are blocks inside a function rather than functions.
const BLOCK_SCOPES: &[&str] = &["catch_clause", "for_in_statement", "for_statement", "statement_block"];

/// Bound on in-progress query matches (tree-sitter `set_match_limit`).
const MATCH_LIMIT: u32 = 256;

pub(crate) fn grammar(language: Language) -> Option<tree_sitter::Language> {
    match language {
        Language::JavaScript => Some(tree_sitter_javascript::LANGUAGE.into()),
        Language::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        Language::Tsx => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        Language::Python => None,
    }
}

/// Query source for `language`, with `; inherits:` resolved.
pub(crate) fn query_source(language: Language) -> String {
    match language {
        Language::JavaScript => [ECMA_LOCALS, JAVASCRIPT_LOCALS].join("\n"),
        Language::Tsx => [ECMA_LOCALS, TYPESCRIPT_LOCALS, TSX_LOCALS].join("\n"),
        _ => [ECMA_LOCALS, TYPESCRIPT_LOCALS].join("\n"),
    }
}

pub(crate) struct LocalsQuery {
    query: Query,
    scope: Option<u32>,
    namespace_body: Option<u32>,
    /// Per capture index: is a `@local.definition*` capture.
    is_definition: Vec<bool>,
    /// Per pattern index: has `#set! definition.var.scope parent`.
    parent_scope: Vec<bool>,
}

impl LocalsQuery {
    fn compile(language: Language) -> Result<LocalsQuery, String> {
        let grammar = grammar(language).ok_or_else(|| format!("no grammar for {language}"))?;
        let query = Query::new(&grammar, &query_source(language)).map_err(|e| format!("{language} locals.scm: {e}"))?;
        let is_definition = query.capture_names().iter().map(|n| n.starts_with("local.definition")).collect();
        let parent_scope = (0..query.pattern_count())
            .map(|i| {
                query
                    .property_settings(i)
                    .iter()
                    .any(|p| &*p.key == "definition.var.scope" && p.value.as_deref() == Some("parent"))
            })
            .collect();
        Ok(LocalsQuery {
            scope: query.capture_index_for_name("local.scope"),
            namespace_body: query.capture_index_for_name("rueca.namespace_body"),
            query,
            is_definition,
            parent_scope,
        })
    }
}

/// The compiled query for `language` (compiled once per process).
pub(crate) fn locals_query(language: Language) -> Result<&'static LocalsQuery, ExtractError> {
    static JS: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    static TS: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    static TSX: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    let cell = match language {
        Language::JavaScript => &JS,
        Language::TypeScript => &TS,
        Language::Tsx => &TSX,
        Language::Python => return Err(ExtractError::Unsupported(language)),
    };
    cell.get_or_init(|| LocalsQuery::compile(language)).as_ref().map_err(|e| ExtractError::Query(e.clone()))
}

/// Local bindings of one file, grouped by owner (function-like node id, or
/// the root node id for module-level block bindings).
#[derive(Debug, Default)]
pub(crate) struct Locals {
    by_owner: HashMap<usize, HashSet<String>>,
}

impl Locals {
    pub(crate) fn contains(&self, owner: usize, name: &str) -> bool {
        self.by_owner.get(&owner).is_some_and(|s| s.contains(name))
    }

    #[cfg(test)]
    pub(crate) fn names(&self, owner: usize) -> Vec<String> {
        let mut v: Vec<String> = self.by_owner.get(&owner).map(|s| s.iter().cloned().collect()).unwrap_or_default();
        v.sort();
        v
    }

    /// Runs the locals query over `root`. Stops early (keeping what it found)
    /// when `deadline` passes.
    pub(crate) fn compute(q: &LocalsQuery, root: Node<'_>, src: &[u8], deadline: Instant) -> Locals {
        struct Def<'t> {
            node: Node<'t>,
            parent: bool,
        }
        let mut scopes: HashMap<usize, Node<'_>> = HashMap::new();
        let mut transparent: HashSet<usize> = HashSet::new();
        let mut defs: Vec<Def<'_>> = Vec::new();

        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(MATCH_LIMIT);
        let mut progress = |_: &tree_sitter::QueryCursorState| {
            if Instant::now() > deadline { ControlFlow::Break(()) } else { ControlFlow::Continue(()) }
        };
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches = cursor.matches_with_options(&q.query, root, src, options);
        while let Some(m) = matches.next() {
            let parent = q.parent_scope.get(m.pattern_index).copied().unwrap_or(false);
            for c in m.captures() {
                if Some(c.index) == q.scope {
                    scopes.insert(c.node.id(), c.node);
                } else if Some(c.index) == q.namespace_body {
                    transparent.insert(c.node.id());
                } else if q.is_definition.get(c.index as usize).copied().unwrap_or(false) {
                    defs.push(Def { node: c.node, parent });
                }
            }
        }

        // Scopes sorted outermost-first among equal starts, so a containment
        // stack can be swept in document order.
        let mut scope_list: Vec<Node<'_>> =
            scopes.into_iter().filter(|(id, _)| !transparent.contains(id)).map(|(_, n)| n).collect();
        scope_list.sort_by_key(|n| (n.start_byte(), std::cmp::Reverse(n.end_byte()), n.id()));
        defs.sort_by_key(|d| (d.node.start_byte(), d.node.end_byte(), d.node.id(), d.parent));

        let root_id = root.id();
        let mut by_owner: HashMap<usize, HashSet<String>> = HashMap::new();
        let mut stack: Vec<Node<'_>> = Vec::new();
        let mut next = 0;
        for d in &defs {
            let (ds, de) = (d.node.start_byte(), d.node.end_byte());
            while next < scope_list.len() && scope_list[next].start_byte() <= ds {
                let s = scope_list[next];
                while stack.last().is_some_and(|t| t.end_byte() < s.end_byte()) {
                    stack.pop();
                }
                stack.push(s);
                next += 1;
            }
            while stack.last().is_some_and(|t| t.end_byte() < de || t.start_byte() > ds) {
                stack.pop();
            }
            // Innermost scope strictly containing the definition node.
            let mut level = stack.len();
            while level > 0 && stack[level - 1].id() == d.node.id() {
                level -= 1;
            }
            if d.parent && level > 0 {
                level -= 1;
            }
            let chain = &stack[..level];
            let owner = match chain.iter().rev().find(|s| !BLOCK_SCOPES.contains(&s.kind())) {
                Some(f) => f.id(),
                None if !chain.is_empty() => root_id,
                None => continue,
            };
            let name = text_of(d.node, src).into_owned();
            by_owner.entry(owner).or_default().insert(name);
        }
        Locals { by_owner }
    }
}
