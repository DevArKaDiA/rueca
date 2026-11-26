//! Variable bindings of a file, driven by the vendored `queries/locals.scm`.
//!
//! The query (adapted from nvim-treesitter; see the file header and
//! `queries/README.md`) marks function scopes (`@local.scope`) and the
//! names assignments, declaration commands and `for` loops bind
//! (`@local.definition.var`). This module runs it once per file with a
//! [`QueryCursor`] (match limit and deadline enforced), adds the names that
//! binding *commands* set (`read`, `getopts`, `mapfile` / `readarray`,
//! `printf -v`, `select`), and applies the shell's scoping rules:
//!
//! - Every assignment is global unless the name is declared `local` (or
//!   with `declare` / `typeset` without `-g`) in the enclosing function;
//!   shell scoping is dynamic, so a name bound anywhere in the file counts
//!   as bound for the whole file.
//! - A name bound outside every function is a file-level variable.
//!
//! If the cursor hits its match limit or deadline the table is partial:
//! affected names look unbound, so they can at worst become environment
//! reads (never missing definitions of functions).

use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use rueca_extract::ExtractError;

use crate::dialect::Grammar;
use rueca_extract::tree_sitter::{
    Node, Query, QueryCursor, QueryCursorOptions, QueryCursorState, StreamingIterator, Tree,
};

/// The vendored, adapted locals query.
pub(crate) const LOCALS_SCM: &str = include_str!("../queries/locals.scm");

/// Maximum number of in-progress matches the query cursor may keep.
const MATCH_LIMIT: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Scope,
    Var,
    Ignore,
}

fn role_of(name: &str) -> Role {
    match name {
        "local.scope" => Role::Scope,
        "local.definition.var" => Role::Var,
        _ => Role::Ignore,
    }
}

/// Compiled query plus the role of each capture index.
pub(crate) struct LocalsQuery {
    pub(crate) query: Query,
    roles: Vec<Role>,
}

/// The same query for tree-sitter-zsh trees (`queries/locals-zsh.scm`).
pub(crate) const LOCALS_ZSH_SCM: &str = include_str!("../queries/locals-zsh.scm");

fn compile(grammar: Grammar) -> Result<LocalsQuery, String> {
    let (scm, file) = match grammar {
        Grammar::Bash => (LOCALS_SCM, "locals.scm"),
        Grammar::Zsh => (LOCALS_ZSH_SCM, "locals-zsh.scm"),
    };
    let query = Query::new(&crate::extract::language(grammar), scm).map_err(|e| format!("{file}: {e}"))?;
    let roles = query.capture_names().iter().map(|n| role_of(n)).collect();
    Ok(LocalsQuery { query, roles })
}

/// Compiles the locals query of `grammar` (once per process each).
pub(crate) fn locals_query(grammar: Grammar) -> Result<&'static LocalsQuery, ExtractError> {
    static BASH: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    static ZSH: OnceLock<Result<LocalsQuery, String>> = OnceLock::new();
    let cell = match grammar {
        Grammar::Bash => &BASH,
        Grammar::Zsh => &ZSH,
    };
    cell.get_or_init(|| compile(grammar)).as_ref().map_err(|e| ExtractError::Query(e.clone()))
}

/// How a file-level name is bound (its first binding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileVarKind {
    /// A plain assignment or `declare` / `typeset` (a `Variable`).
    Variable,
    /// `readonly NAME` or `declare -r` / `typeset -r` (a `Constant`).
    Constant,
    /// `export NAME` / `declare -x` (an environment provider, not a
    /// `Variable`: the extractor emits an `EnvVar` definition for it).
    Exported,
    /// A loop variable or a name a command binds (no definition).
    Other,
}

/// Per-file binding table.
#[derive(Debug, Default)]
pub(crate) struct Locals {
    /// Every name bound anywhere in the file.
    bound: HashSet<String>,
    /// File-level names, first binding first.
    file_level: HashMap<String, FileVarKind>,
    /// Function scopes (byte range) and the names declared local in each.
    scopes: Vec<(u32, u32, HashSet<String>)>,
}

impl Locals {
    /// Whether `name` is bound anywhere in the file.
    pub(crate) fn is_bound(&self, name: &str) -> bool {
        self.bound.contains(name)
    }

    /// How `name` is bound at file level, if it is.
    pub(crate) fn file_level(&self, name: &str) -> Option<FileVarKind> {
        self.file_level.get(name).copied()
    }

    /// Whether `name` is declared local in a function enclosing byte `pos`.
    pub(crate) fn is_local_at(&self, name: &str, pos: u32) -> bool {
        self.scopes.iter().any(|(s, e, names)| *s <= pos && pos < *e && names.contains(name))
    }
}

pub(crate) fn text<'s>(node: Node<'_>, source: &'s [u8]) -> &'s str {
    std::str::from_utf8(&source[node.start_byte()..node.end_byte()]).unwrap_or("")
}

/// The keyword of a `declaration_command` (`local`, `declare`, `typeset`,
/// `export`, `readonly`).
pub(crate) fn declaration_keyword<'s>(decl: Node<'_>, source: &'s [u8]) -> &'s str {
    decl.child(0).map_or("", |k| text(k, source))
}

/// The option letters of a declaration command (`-rx` → `rx`), joined.
pub(crate) fn declaration_flags(decl: Node<'_>, source: &[u8]) -> String {
    let mut flags = String::new();
    let mut cursor = decl.walk();
    for c in decl.named_children(&mut cursor) {
        let t = text(c, source);
        if c.kind() == "word" && t.starts_with('-') {
            flags.push_str(&t[1..]);
        }
    }
    flags
}

/// The declaration command a bound `variable_name` belongs to, if any.
fn declaration_of(node: Node<'_>) -> Option<Node<'_>> {
    let parent = node.parent()?;
    match parent.kind() {
        "declaration_command" => Some(parent),
        "variable_assignment" => parent.parent().filter(|p| p.kind() == "declaration_command"),
        "subscript" => declaration_of(parent.parent()?),
        _ => None,
    }
}

/// Whether a binding inside a function is local to it.
fn is_local_declaration(decl: Node<'_>, source: &[u8]) -> bool {
    match declaration_keyword(decl, source) {
        "local" => true,
        // zsh: `integer` / `float` are `typeset -i` / `typeset -F`
        "declare" | "typeset" | "integer" | "float" => !declaration_flags(decl, source).contains('g'),
        _ => false,
    }
}

/// The file-level kind of a binding.
fn file_var_kind(node: Node<'_>, source: &[u8]) -> FileVarKind {
    let parent_kind = node.parent().map(|p| p.kind());
    if matches!(parent_kind, Some("for_statement" | "foreach_statement" | "terse_for_statement")) {
        return FileVarKind::Other;
    }
    let Some(decl) = declaration_of(node) else { return FileVarKind::Variable };
    let flags = declaration_flags(decl, source);
    if flags.contains(['f', 'F']) {
        return FileVarKind::Other;
    }
    match declaration_keyword(decl, source) {
        "export" => FileVarKind::Exported,
        "readonly" => FileVarKind::Constant,
        _ if flags.contains('x') => FileVarKind::Exported,
        _ if flags.contains('r') => FileVarKind::Constant,
        _ => FileVarKind::Variable,
    }
}

/// Commands that bind the names given as arguments.
const BINDING_COMMANDS: &[&str] = &["read", "getopts", "mapfile", "readarray", "select", "printf"];

/// Whether `s` is a shell variable name.
pub(crate) fn is_name(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The names a binding command sets: `read -r a b`, `getopts "ab:" opt`,
/// `mapfile -t lines`, `printf -v out …`, `select x in …`.
fn command_bindings<'t>(cmd: Node<'t>, source: &[u8]) -> Vec<Node<'t>> {
    let Some(name) = cmd.child_by_field_name("name") else { return Vec::new() };
    let name = text(name, source);
    if !BINDING_COMMANDS.contains(&name) {
        return Vec::new();
    }
    let mut cursor = cmd.walk();
    let args: Vec<Node<'t>> = cmd.children_by_field_name("argument", &mut cursor).collect();
    let words = |n: &Node<'_>| n.kind() == "word" && is_name(text(*n, source));
    match name {
        "printf" => args.windows(2).filter(|w| text(w[0], source) == "-v" && words(&w[1])).map(|w| w[1]).collect(),
        "getopts" => args.get(1).copied().filter(words).into_iter().collect(),
        "select" => args.first().copied().filter(words).into_iter().collect(),
        // read / mapfile / readarray: every plain name that is neither an
        // option nor an option's argument (`-p prompt`, `-d ''`); `read
        // -a NAME` binds NAME.
        _ => {
            let mut out = Vec::new();
            let mut skip_next = false;
            for a in args {
                let t = text(a, source);
                if skip_next {
                    skip_next = false;
                } else if let Some(opts) = t.strip_prefix('-') {
                    skip_next = option_takes_value(name, opts);
                } else if words(&a) {
                    out.push(a);
                }
            }
            out
        }
    }
}

/// Whether the last option letter of `opts` (`rp` of `-rp`) takes the next
/// argument as its value (`read -a NAME` does not: NAME is a binding).
fn option_takes_value(command: &str, opts: &str) -> bool {
    let Some(last) = opts.chars().last() else { return false };
    if command == "read" { "dinNptu".contains(last) } else { "dnOsuCc".contains(last) }
}

/// The scopes and the bound names captured by the locals query.
type Captures<'t> = (Vec<(u32, u32)>, Vec<Node<'t>>);

/// Runs the locals query over `tree`: the function scopes, the bound
/// names, and whether the run was cut (deadline or match limit).
fn run_query<'t>(lq: &LocalsQuery, tree: &'t Tree, source: &[u8], time_budget: Duration) -> (Captures<'t>, bool) {
    let mut scopes: Vec<(u32, u32)> = Vec::new();
    let mut vars: Vec<Node<'_>> = Vec::new();
    let deadline = Instant::now() + time_budget;
    let mut timed_out = false;
    let mut progress = |_: &QueryCursorState| {
        if Instant::now() > deadline {
            timed_out = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(MATCH_LIMIT);
    {
        let options = QueryCursorOptions::new().progress_callback(&mut progress);
        let mut matches = cursor.matches_with_options(&lq.query, tree.root_node(), source, options);
        let mut seen = HashSet::new();
        while let Some(m) = matches.next() {
            for cap in m.captures() {
                match lq.roles.get(cap.index as usize).copied().unwrap_or(Role::Ignore) {
                    Role::Scope if seen.insert(cap.node.id()) => {
                        scopes.push((cap.node.start_byte() as u32, cap.node.end_byte() as u32));
                    }
                    Role::Var => vars.push(cap.node),
                    Role::Scope | Role::Ignore => {}
                }
            }
        }
    }
    let incomplete = timed_out || cursor.did_exceed_match_limit();
    ((scopes, vars), incomplete)
}

/// The names a declaration command declares without a value, where the
/// grammar reads them as plain `word` arguments (tree-sitter-zsh: `local
/// x`, `typeset -a xs`, `export X`; tree-sitter-bash gives `variable_name`
/// nodes, which the query captures).
fn declaration_words<'t>(decl: Node<'t>, source: &[u8]) -> Vec<Node<'t>> {
    let mut cursor = decl.walk();
    decl.named_children(&mut cursor).filter(|c| c.kind() == "word" && is_name(text(*c, source))).collect()
}

/// Every `command` and `declaration_command` node of the tree (for the
/// binding commands and the names declared as words).
fn commands<'t>(tree: &'t Tree) -> Vec<Node<'t>> {
    let mut out = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "command" | "declaration_command") {
            out.push(n);
        }
        let mut cursor = n.walk();
        stack.extend(n.named_children(&mut cursor));
    }
    out
}

impl Locals {
    /// The innermost function scope containing byte `pos`.
    fn innermost_scope(&mut self, pos: u32) -> Option<&mut (u32, u32, HashSet<String>)> {
        self.scopes.iter_mut().filter(|(s, e, _)| *s <= pos && pos < *e).min_by_key(|(s, e, _)| e - s)
    }

    fn add(&mut self, node: Node<'_>, source: &[u8], kind: FileVarKind) {
        let name = text(node, source).to_string();
        let pos = node.start_byte() as u32;
        let local = declaration_of(node).is_some_and(|d| is_local_declaration(d, source));
        match self.innermost_scope(pos) {
            Some(scope) if local => {
                scope.2.insert(name.clone());
            }
            Some(_) => {}
            None => {
                self.file_level.entry(name.clone()).or_insert(kind);
            }
        }
        self.bound.insert(name);
    }
}

/// Builds the binding table of `tree`.
pub(crate) fn build(
    tree: &Tree,
    source: &[u8],
    grammar: Grammar,
    time_budget: Duration,
) -> Result<Locals, ExtractError> {
    let lq = locals_query(grammar)?;
    let ((scopes, vars), _incomplete) = run_query(lq, tree, source, time_budget);
    let mut locals =
        Locals { scopes: scopes.into_iter().map(|(s, e)| (s, e, HashSet::new())).collect(), ..Default::default() };
    let mut ordered: Vec<(Node<'_>, FileVarKind)> = vars.into_iter().map(|v| (v, file_var_kind(v, source))).collect();
    for cmd in commands(tree) {
        if cmd.kind() == "declaration_command" {
            ordered.extend(declaration_words(cmd, source).into_iter().map(|n| (n, file_var_kind(n, source))));
        } else {
            ordered.extend(command_bindings(cmd, source).into_iter().map(|n| (n, FileVarKind::Other)));
        }
    }
    ordered.sort_by_key(|(n, _)| n.start_byte());
    for (node, kind) in ordered {
        locals.add(node, source, kind);
    }
    Ok(locals)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture names the Rust side relies on.
    const REQUIRED_CAPTURES: &[&str] = &["local.scope", "local.definition.var"];

    /// Exercises every pattern of `locals.scm` at least once.
    const GOLDEN: &str = r#"
f() { local x=1; arr[0]=a; declare y; for i in 1 2; do :; done; }
"#;

    fn parse(src: &str) -> Tree {
        let language = tree_sitter_bash::LANGUAGE.into();
        let mut parser = rueca_extract::tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn locals_query_compiles_against_grammar() {
        let lq = locals_query(Grammar::Bash).expect("locals.scm must compile against tree-sitter-bash");
        let names = lq.query.capture_names();
        for required in REQUIRED_CAPTURES {
            assert!(names.contains(required), "capture @{required} missing from locals.scm");
        }
        for (name, role) in names.iter().zip(&lq.roles) {
            assert_ne!(*role, Role::Ignore, "capture @{name} has no role");
        }
    }

    #[test]
    fn every_locals_pattern_matches_the_golden_corpus() {
        let lq = locals_query(Grammar::Bash).unwrap();
        let tree = parse(GOLDEN);
        let mut cursor = QueryCursor::new();
        let mut hit = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN.as_bytes());
        while let Some(m) = matches.next() {
            hit[m.pattern_index] = true;
        }
        let missing: Vec<usize> = hit.iter().enumerate().filter(|(_, h)| !**h).map(|(i, _)| i).collect();
        assert!(missing.is_empty(), "patterns never matched: {missing:?}");
    }

    /// Exercises every pattern of `locals-zsh.scm` at least once.
    const GOLDEN_ZSH: &str =
        "f() { local x=1; arr[1]=a; for i in 1 2; do :; done; foreach j (a b) :; end; for k (a b) :; }\n";

    fn parse_zsh(src: &str) -> Tree {
        let mut parser = rueca_extract::tree_sitter::Parser::new();
        parser.set_language(&crate::extract::language(Grammar::Zsh)).unwrap();
        parser.parse(src, None).unwrap()
    }

    #[test]
    fn zsh_locals_query_compiles_and_every_pattern_matches() {
        let lq = locals_query(Grammar::Zsh).expect("locals-zsh.scm must compile against tree-sitter-zsh");
        for required in REQUIRED_CAPTURES {
            assert!(lq.query.capture_names().contains(required), "capture @{required} missing");
        }
        let tree = parse_zsh(GOLDEN_ZSH);
        assert!(!tree.root_node().has_error());
        let mut cursor = QueryCursor::new();
        let mut hit = vec![false; lq.query.pattern_count()];
        let mut matches = cursor.matches(&lq.query, tree.root_node(), GOLDEN_ZSH.as_bytes());
        while let Some(m) = matches.next() {
            hit[m.pattern_index] = true;
        }
        let missing: Vec<usize> = hit.iter().enumerate().filter(|(_, h)| !**h).map(|(i, _)| i).collect();
        assert!(missing.is_empty(), "patterns never matched: {missing:?}");
        assert!(LOCALS_ZSH_SCM.starts_with("; SPDX-License-Identifier: Apache-2.0"));
    }

    #[test]
    fn zsh_bindings_and_scopes() {
        let src = "A=1\nreadonly B\nexport C\ntypeset -x D\nfor E in x; do :; done\nforeach F (a b) :; end\nlocal top\n\
                   f() { local l; typeset d e=1; typeset -g G; integer n; float q=1.5; g=2; read -r r1; }\n";
        let tree = parse_zsh(src);
        let t = build(&tree, src.as_bytes(), Grammar::Zsh, Duration::from_secs(5)).unwrap();
        assert_eq!(t.file_level("A"), Some(FileVarKind::Variable));
        assert_eq!(t.file_level("B"), Some(FileVarKind::Constant));
        assert_eq!(t.file_level("C"), Some(FileVarKind::Exported));
        assert_eq!(t.file_level("D"), Some(FileVarKind::Exported));
        assert_eq!(t.file_level("E"), Some(FileVarKind::Other));
        assert_eq!(t.file_level("F"), Some(FileVarKind::Other));
        assert_eq!(t.file_level("top"), Some(FileVarKind::Variable));
        let inside = src.find("g=2").unwrap() as u32;
        for n in ["l", "d", "e", "n", "q"] {
            assert!(t.is_bound(n) && t.is_local_at(n, inside), "{n}");
            assert_eq!(t.file_level(n), None, "{n}");
        }
        for n in ["G", "g", "r1"] {
            assert!(t.is_bound(n) && !t.is_local_at(n, inside), "{n}");
        }
        // option words are not names
        assert!(!t.is_bound("-g") && !t.is_bound("x"));
    }

    #[test]
    fn query_header_records_provenance() {
        assert!(LOCALS_SCM.starts_with("; SPDX-License-Identifier: Apache-2.0"));
        assert!(LOCALS_SCM.contains("29212d826054bb2948fa9d3a99eca9bf6a050eb2"));
    }

    fn table(src: &str) -> Locals {
        build(&parse(src), src.as_bytes(), Grammar::Bash, Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn bindings_and_scopes() {
        let src = "A=1\nreadonly B=2\nexport C=3\ndeclare -x D\nfor E in x; do :; done\n\
                   f() { local l=1; declare d; declare -g G=1; g=2; read -r r1 r2; getopts 'a:' opt; printf -v pv %s x; mapfile -t lines; }\n";
        let t = table(src);
        assert_eq!(t.file_level("A"), Some(FileVarKind::Variable));
        assert_eq!(t.file_level("B"), Some(FileVarKind::Constant));
        assert_eq!(t.file_level("C"), Some(FileVarKind::Exported));
        assert_eq!(t.file_level("D"), Some(FileVarKind::Exported));
        assert_eq!(t.file_level("E"), Some(FileVarKind::Other));
        for n in ["l", "d", "G", "g", "r1", "r2", "opt", "pv", "lines"] {
            assert!(t.is_bound(n), "{n}");
            assert_eq!(t.file_level(n), None, "{n}");
        }
        let inside = src.find("g=2").unwrap() as u32;
        assert!(t.is_local_at("l", inside) && t.is_local_at("d", inside));
        assert!(!t.is_local_at("G", inside) && !t.is_local_at("g", inside));
        assert!(!t.is_local_at("l", 0));
        assert!(!t.is_bound("HOME"));
    }
}
