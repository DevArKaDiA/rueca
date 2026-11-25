//! One shell file → [`FileFacts`]. The rules are documented in the crate
//! root; this module walks the tree-sitter-bash syntax tree.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use rueca_extract::tree_sitter::{Node, Parser, Tree};
use rueca_extract::{Budget, ExtractError, count_errors, first_line, line_count, span_of};
use rueca_model::{Definition, FileFacts, Import, ImportedName, Label, LocalDefId, RefKind, Reference, Span};

use crate::bats::{self, TestBlock};
use crate::builtins::is_env_name;
use crate::dialect::{self, Dialect, Grammar};
use crate::locals::{self, FileVarKind, Locals, declaration_flags, declaration_keyword, is_name, text};
use crate::paths::{PathEval, PathPattern};

pub use rueca_extract::SCRIPT_RECEIVER;

/// At most this many references per file.
pub const MAX_REFS: usize = 20_000;
/// Doc (leading comment) length cap, in characters.
const MAX_DOC_CHARS: usize = 1000;
/// Signature length cap, in characters.
const MAX_SIGNATURE_CHARS: usize = 200;
/// Value preview length in variable signatures, in characters.
const MAX_PREVIEW_CHARS: usize = 80;
/// Masked value of an environment provider or a secret-looking variable.
const MASK: &str = "•••";
/// Time budget of the locals query.
const LOCALS_TIME: Duration = Duration::from_secs(2);

/// Commands that run their first operand as a command of the same shell
/// (functions included); `noglob` and `nocorrect` are zsh's precommand
/// modifiers.
const SAME_SHELL_RUNNERS: &[&str] = &["time", "!", "noglob", "nocorrect"];
/// Commands that run their first operand as an external program: a
/// script path there is a script invocation, a bare name never a function.
const EXTERNAL_RUNNERS: &[&str] =
    &["exec", "command", "nohup", "xargs", "sudo", "env", "nice", "timeout", "stdbuf", "watch", "strace", "caffeinate"];
/// Interpreters whose first operand is a script file.
const INTERPRETERS: &[&str] = &["bash", "sh", "dash", "ash", "ksh", "mksh", "zsh", "bats"];
/// Extensions that make a command name a script path.
const SCRIPT_EXTENSIONS: &[&str] = &[".sh", ".bash", ".zsh", ".ksh", ".bats"];
/// shunit2 hooks (test code in a file that sources shunit2).
const SHUNIT2_HOOKS: &[&str] = &["setUp", "tearDown", "oneTimeSetUp", "oneTimeTearDown", "suite"];

pub fn extract(path: &str, source: &[u8], budget: &Budget) -> Result<FileFacts, ExtractError> {
    let (dialect, _) = dialect::detect(path, source);
    facts_of(path, source, dialect, parse_file(dialect, source, budget)?, budget)
}

/// The facts of a file read with `grammar` whatever its dialect's (for the
/// grammar comparison of `examples/grammars.rs`).
pub fn extract_with(path: &str, source: &[u8], grammar: Grammar, budget: &Budget) -> Result<FileFacts, ExtractError> {
    let (dialect, _) = dialect::detect(path, source);
    facts_of(path, source, dialect, parse_with(grammar, source, budget)?, budget)
}

/// `written`: the file as written (signatures, value previews and docs
/// quote it; the tree is of the repaired text, of the same length).
fn facts_of(
    path: &str,
    written: &[u8],
    dialect: Dialect,
    parsed: Parsed<'_>,
    budget: &Budget,
) -> Result<FileFacts, ExtractError> {
    let source: &[u8] = &parsed.source;
    let locals = locals::build(&parsed.tree, source, parsed.grammar, LOCALS_TIME.min(budget.max_parse_time))?;
    let mut ex = Extractor::new(path, source, written, dialect, locals, budget.max_depth);
    ex.prescan(parsed.tree.root_node());
    ex.program(parsed.tree.root_node());
    let mut facts = ex.facts;
    facts.parse_errors = parsed.errors;
    facts.line_count = line_count(source);
    Ok(facts)
}

/// A file as one grammar reads it: the repaired text, the tree and its
/// ERROR / MISSING count.
pub(crate) struct Parsed<'s> {
    pub(crate) grammar: Grammar,
    pub(crate) source: std::borrow::Cow<'s, [u8]>,
    pub(crate) tree: Tree,
    pub(crate) errors: u32,
}

pub(crate) fn language(grammar: Grammar) -> rueca_extract::tree_sitter::Language {
    match grammar {
        Grammar::Bash => tree_sitter_bash::LANGUAGE.into(),
        Grammar::Zsh => tree_sitter_zsh::LANGUAGE.into(),
    }
}

fn parse_with<'s>(grammar: Grammar, source: &'s [u8], budget: &Budget) -> Result<Parsed<'s>, ExtractError> {
    let repaired = crate::repair::repair(source, grammar);
    let tree = rueca_extract::parse(&language(grammar), &repaired, budget)?;
    let errors = count_errors(&tree);
    Ok(Parsed { grammar, source: repaired, tree, errors })
}

/// Parses with the dialect's grammar; when that leaves errors and the
/// other grammar reads the whole file without any, that tree is kept
/// (tree-sitter-zsh is a fork of tree-sitter-bash that also accepts some
/// bash the original rejects, and a mislabelled file is still read). A
/// file both grammars reject keeps its dialect's tree: error counts of two
/// broken trees say nothing about which one is closer. A bats file is only
/// read as bash (`@test` blocks, `bats.rs`).
pub(crate) fn parse_file<'s>(dialect: Dialect, source: &'s [u8], budget: &Budget) -> Result<Parsed<'s>, ExtractError> {
    let first = parse_with(dialect.grammar(), source, budget)?;
    if first.errors == 0 || dialect == Dialect::Bats {
        return Ok(first);
    }
    match parse_with(dialect.grammar().other(), source, budget) {
        Ok(second) if second.errors == 0 => Ok(second),
        _ => Ok(first),
    }
}

/// Parses a fragment (a `trap` handler, an `eval` string) on its own.
fn parse_fragment(code: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_bash::LANGUAGE.into()).ok()?;
    parser.parse(code, None)
}

/// Test files by path: a test directory, `*_test.sh` / `test_*.sh` (any
/// shell extension), `*_spec.sh`, `*.bats`.
pub(crate) fn is_test_file(path: &str) -> bool {
    if rueca_model::is_test_path(path) || path.ends_with(".bats") {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.starts_with("test_") || stem.ends_with("_test") || stem.ends_with("_spec")
}

/// Where the walk is: the innermost definition, and whether it is inside a
/// function body.
#[derive(Clone, Copy)]
struct Ctx {
    enclosing: Option<LocalDefId>,
    in_function: bool,
}

const TOP: Ctx = Ctx { enclosing: None, in_function: false };

/// A source text with the position of its first byte in the file (for
/// fragments parsed on their own).
#[derive(Clone, Copy)]
struct Origin {
    byte: u32,
    line: u32,
    col: u32,
}

impl Origin {
    fn of(node: Node<'_>, skip: u32) -> Origin {
        let s = span_of(node);
        Origin { byte: s.start_byte + skip, line: s.start_line, col: s.start_col + skip }
    }

    fn map(self, mut s: Span) -> Span {
        if s.start_line == 0 {
            s.start_col += self.col;
        }
        if s.end_line == 0 {
            s.end_col += self.col;
        }
        s.start_byte += self.byte;
        s.end_byte += self.byte;
        s.start_line += self.line;
        s.end_line += self.line;
        s
    }
}

struct Extractor<'s> {
    /// The text the grammar read (after the same-length repairs).
    src: &'s [u8],
    /// The text as written: what signatures, previews and docs quote.
    written: &'s [u8],
    locals: Locals,
    max_depth: usize,
    dialect: Dialect,
    facts: FileFacts,
    test_file: bool,
    bats_file: bool,
    shunit2: bool,
    /// Variables the file assigns a path to (`DIR=$(dirname "$0")`).
    dir_vars: HashMap<String, PathPattern>,
    /// Variables with conflicting path values.
    dir_conflicts: HashSet<String>,
    /// `Read` / `EnvVar` references already emitted, by (enclosing, kind, name).
    seen_refs: HashSet<(Option<LocalDefId>, bool, String)>,
    /// Env providers already emitted, by (parent, name).
    providers: HashSet<(Option<LocalDefId>, String)>,
    /// File-level variables already defined.
    variables: HashSet<String>,
    imports: HashSet<String>,
}

impl<'s> Extractor<'s> {
    fn new(path: &str, src: &'s [u8], written: &'s [u8], dialect: Dialect, locals: Locals, max_depth: usize) -> Self {
        Extractor {
            src,
            written,
            locals,
            max_depth,
            dialect,
            facts: FileFacts::default(),
            test_file: is_test_file(path) || dialect == Dialect::Bats,
            bats_file: dialect == Dialect::Bats,
            shunit2: false,
            dir_vars: HashMap::new(),
            dir_conflicts: HashSet::new(),
            seen_refs: HashSet::new(),
            providers: HashSet::new(),
            variables: HashSet::new(),
            imports: HashSet::new(),
        }
    }

    fn text(&self, node: Node<'_>) -> &'s str {
        text(node, self.src)
    }

    /// The node's text as written in the file.
    fn written(&self, node: Node<'_>) -> &'s str {
        text(node, self.written)
    }

    // -- pre-scan: path variables, shunit2 -----------------------------------

    /// Path-valued variables (in document order) and whether the file
    /// sources shunit2.
    fn prescan(&mut self, root: Node<'_>) {
        let mut stack = vec![root];
        let mut order = Vec::new();
        while let Some(n) = stack.pop() {
            match n.kind() {
                "variable_assignment" => order.push(n),
                "command" if self.sources_shunit2(n) => self.shunit2 = true,
                _ => {}
            }
            let mut cursor = n.walk();
            let children: Vec<Node<'_>> = n.named_children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
        order.sort_by_key(|n| n.start_byte());
        for a in order {
            self.path_variable(a);
        }
        for name in &self.dir_conflicts {
            self.dir_vars.remove(name);
        }
    }

    fn sources_shunit2(&self, cmd: Node<'_>) -> bool {
        matches!(self.command_word(cmd).as_deref(), Some("." | "source"))
            && self.text(cmd).rsplit('/').next().is_some_and(|t| t.contains("shunit2"))
    }

    /// Records `NAME=<path>` when the value is a path the file determines.
    fn path_variable(&mut self, assignment: Node<'_>) {
        let (Some(name), Some(value)) =
            (assignment.child_by_field_name("name"), assignment.child_by_field_name("value"))
        else {
            return;
        };
        let name = self.text(name).to_string();
        let pattern = PathEval { source: self.src, vars: &self.dir_vars }.value(value);
        match (pattern, self.dir_vars.get(&name)) {
            (Some(p), Some(old)) if *old != p => {
                self.dir_conflicts.insert(name);
            }
            (Some(p), None) => {
                self.dir_vars.insert(name, p);
            }
            _ => {}
        }
    }

    fn eval_path(&self, node: Node<'_>) -> Option<PathPattern> {
        PathEval { source: self.src, vars: &self.dir_vars }.eval(node)
    }

    // -- walk ----------------------------------------------------------------

    /// The file's statements; in a `.bats` file, `@test "…" { … }` blocks
    /// are tests (see `bats.rs`).
    fn program(&mut self, root: Node<'_>) {
        let blocks = if self.bats_file { bats::test_blocks(root, self.src) } else { Vec::new() };
        let mut cursor = root.walk();
        let children: Vec<Node<'_>> = root.named_children(&mut cursor).collect();
        let mut current: Option<(usize, LocalDefId)> = None;
        for (i, child) in children.iter().enumerate() {
            if let Some(b) = blocks.iter().find(|b| b.head == i) {
                current = Some((b.last, self.test_block(b, &children)));
                continue;
            }
            let ctx = match current {
                Some((last, id)) if i <= last => Ctx { enclosing: Some(id), in_function: true },
                _ => TOP,
            };
            if current.is_some_and(|(last, _)| i == last) && bats::is_block_end(*child, self.src) {
                continue;
            }
            self.walk(*child, ctx, 0);
        }
    }

    /// A bats test block → a `Test` definition spanning its statements.
    fn test_block(&mut self, b: &TestBlock, children: &[Node<'_>]) -> LocalDefId {
        let head = children[b.head];
        let mut span = span_of(head);
        let end = span_of(children[b.last]);
        span.end_byte = end.end_byte;
        span.end_line = end.end_line;
        span.end_col = end.end_col;
        let def = Definition {
            label: Label::Test,
            name: b.name.clone(),
            qualified_name: b.name.clone(),
            parent: None,
            span,
            name_span: span_of(head),
            signature: Some(first_line(&format!("@test \"{}\"", b.name), MAX_SIGNATURE_CHARS)),
            doc: self.doc_above(head),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: false,
            is_test: true,
            is_declaration: false,
        };
        self.push_def(def)
    }

    fn walk(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        match node.kind() {
            "function_definition" => self.function(node, ctx, depth),
            "command" => self.command(node, ctx, depth),
            "declaration_command" => self.declaration(node, ctx, depth),
            "variable_assignment" => self.assignment(node, ctx, depth),
            "simple_expansion" | "variable_ref" | "expansion" => self.expansion(node, ctx, depth),
            "comment" => {}
            _ => self.walk_children(node, ctx, depth),
        }
    }

    fn walk_children(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for c in children {
            self.walk(c, ctx, depth + 1);
        }
    }

    // -- definitions ---------------------------------------------------------

    fn push_def(&mut self, def: Definition) -> LocalDefId {
        self.facts.definitions.push(def);
        (self.facts.definitions.len() - 1) as LocalDefId
    }

    /// `f() { … }`, `function f { … }`, `function f() { … }` → `Function`
    /// (file level whatever its nesting: shell functions are global).
    fn function(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        let Some(name_node) = node.child_by_field_name("name") else { return self.walk_children(node, ctx, depth) };
        let name = self.text(name_node).to_string();
        let is_test =
            self.test_file || (self.shunit2 && (name.starts_with("test") || SHUNIT2_HOOKS.contains(&name.as_str())));
        let def = Definition {
            label: Label::Function,
            name: name.clone(),
            qualified_name: name,
            parent: None,
            span: span_of(node),
            name_span: span_of(name_node),
            signature: Some(self.function_signature(node)),
            doc: self.doc_above(node),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test,
            is_declaration: false,
        };
        let id = self.push_def(def);
        if let Some(body) = node.child_by_field_name("body") {
            self.walk(body, Ctx { enclosing: Some(id), in_function: true }, depth + 1);
        }
    }

    /// `f()` / `function f` / `function f()`: the text before the body,
    /// then the file's dialect as a comment (`f() # zsh`).
    fn function_signature(&self, node: Node<'_>) -> String {
        let body_start = node.child_by_field_name("body").map_or(node.end_byte(), |b| b.start_byte());
        let head = String::from_utf8_lossy(&self.written[node.start_byte()..body_start]);
        format!("{} # {}", first_line(head.trim(), MAX_SIGNATURE_CHARS), self.dialect.as_str())
    }

    /// The `#` comment block right above `node` (the shebang excluded).
    fn doc_above(&self, node: Node<'_>) -> Option<String> {
        let mut lines = Vec::new();
        let mut expected = node.start_position().row;
        let mut cur = node.prev_sibling();
        while let Some(c) = cur.filter(|c| c.kind() == "comment" && c.end_position().row + 1 == expected) {
            let t = self.written(c);
            if t.starts_with("#!") && c.start_position().row == 0 {
                break;
            }
            lines.push(t.trim_start_matches('#').trim().to_string());
            expected = c.start_position().row;
            cur = c.prev_sibling();
        }
        lines.reverse();
        let doc = lines.join("\n").trim().to_string();
        (!doc.is_empty()).then(|| doc.chars().take(MAX_DOC_CHARS).collect())
    }

    /// A file-level `NAME=value` (or `declare` / `readonly`) → `Variable` /
    /// `Constant`, first binding only.
    fn variable_def(&mut self, name_node: Node<'_>, stmt: Node<'_>, value: Option<Node<'_>>) {
        let name = self.text(name_node).to_string();
        if !is_name(&name) {
            return;
        }
        let label = match self.locals.file_level(&name) {
            Some(FileVarKind::Variable) => Label::Variable,
            Some(FileVarKind::Constant) => Label::Constant,
            _ => return,
        };
        if !self.variables.insert(name.clone()) {
            return;
        }
        let preview = value.map(|v| self.preview(&name, v)).unwrap_or_default();
        let def = Definition {
            label,
            name: name.clone(),
            qualified_name: name.clone(),
            parent: None,
            span: span_of(stmt),
            name_span: span_of(name_node),
            signature: Some(first_line(&format!("{name}={preview}"), MAX_SIGNATURE_CHARS)),
            doc: self.doc_above(stmt),
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: self.test_file,
            is_declaration: false,
        };
        self.push_def(def);
    }

    /// A value as written (first line, ≤ 80 chars), masked for
    /// secret-looking names.
    fn preview(&self, name: &str, value: Node<'_>) -> String {
        if is_secret_name(name) {
            return MASK.to_string();
        }
        let v = first_line(self.written(value), MAX_PREVIEW_CHARS);
        if v.is_empty() { "''".to_string() } else { v }
    }

    /// An environment provider (`export NAME=…`, `declare -x NAME`, `NAME=…
    /// cmd`) → `EnvVar` definition, child of the enclosing definition.
    fn provider(&mut self, name_node: Node<'_>, stmt: Node<'_>, has_value: bool, ctx: Ctx) {
        let name = self.text(name_node).to_string();
        if !is_name(&name) || !self.providers.insert((ctx.enclosing, name.clone())) {
            return;
        }
        let qualified_name = match ctx.enclosing {
            Some(p) => format!("{}.{name}", self.facts.definitions[p as usize].qualified_name),
            None => name.clone(),
        };
        let signature = if has_value { format!("export {name}={MASK}") } else { format!("export {name}") };
        let def = Definition {
            label: Label::EnvVar,
            name,
            qualified_name,
            parent: ctx.enclosing,
            span: span_of(stmt),
            name_span: span_of(name_node),
            signature: Some(signature),
            doc: if ctx.enclosing.is_none() { self.doc_above(stmt) } else { None },
            decorators: Vec::new(),
            bases: Vec::new(),
            implements: Vec::new(),
            exported: true,
            is_test: self.test_file,
            is_declaration: false,
        };
        self.push_def(def);
    }

    // -- statements ------------------------------------------------------------

    /// A standalone assignment: a file-level variable definition.
    fn assignment(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        if !ctx.in_function
            && let Some(name) = assignment_name(node)
        {
            self.variable_def(name, node, node.child_by_field_name("value"));
        }
        if let Some(v) = node.child_by_field_name("value") {
            self.walk(v, ctx, depth + 1);
        }
        if let Some(idx) = node.child_by_field_name("name").and_then(|n| n.child_by_field_name("index")) {
            self.walk(idx, ctx, depth + 1);
        }
    }

    /// `export` / `declare` / `local` / `readonly` / `typeset`.
    fn declaration(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        let keyword = declaration_keyword(node, self.src);
        let flags = declaration_flags(node, self.src);
        // `export -f f` / `declare -f f` name functions, not variables.
        if flags.contains(['f', 'F']) {
            return;
        }
        let exports = keyword == "export" || flags.contains('x');
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for c in children {
            match c.kind() {
                "variable_assignment" => self.declared(c, node, Some(c), exports, ctx, depth),
                "variable_name" => self.declared(c, node, None, exports, ctx, depth),
                // tree-sitter-zsh reads a bare name as a `word` argument
                "word" if is_name(self.text(c)) => self.declared(c, node, None, exports, ctx, depth),
                "word" => {}
                _ => self.walk(c, ctx, depth + 1),
            }
        }
    }

    /// One name of a declaration command.
    #[allow(clippy::too_many_arguments)]
    fn declared(
        &mut self,
        item: Node<'_>,
        stmt: Node<'_>,
        assignment: Option<Node<'_>>,
        exports: bool,
        ctx: Ctx,
        depth: usize,
    ) {
        let name = match assignment {
            Some(a) => assignment_name(a),
            None => Some(item),
        };
        let Some(name) = name else { return };
        let value = assignment.and_then(|a| a.child_by_field_name("value"));
        if exports {
            self.provider(name, stmt, value.is_some(), ctx);
        } else if !ctx.in_function {
            self.variable_def(name, stmt, value);
        }
        if let Some(v) = value {
            self.walk(v, ctx, depth + 1);
        }
    }

    /// `$NAME` / `${NAME…}`: a `Read` of a file-level variable inside a
    /// function, or an environment read of an unbound upper-case name.
    fn expansion(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        if let Some(name_node) = expansion_name(node) {
            let name = self.text(name_node).to_string();
            let pos = name_node.start_byte() as u32;
            let span = span_of(name_node);
            if self.locals.is_bound(&name) {
                let file_level = self.locals.file_level(&name).is_some_and(|k| k != FileVarKind::Other);
                if ctx.in_function && file_level && !self.locals.is_local_at(&name, pos) {
                    self.reference(RefKind::Read, name, None, ctx, span);
                }
            } else if is_env_name(&name) {
                self.reference(RefKind::EnvVar, name, None, ctx, span);
            }
        }
        // Operands of `${X:-$(f)}`, `${X/$a/$b}`, subscripts.
        let mut cursor = node.walk();
        let children: Vec<Node<'_>> = node.named_children(&mut cursor).collect();
        for c in children.into_iter().filter(|c| !matches!(c.kind(), "variable_name" | "simple_variable_name")) {
            self.walk(c, ctx, depth + 1);
        }
    }

    fn reference(&mut self, kind: RefKind, name: String, receiver: Option<String>, ctx: Ctx, span: Span) {
        if self.facts.references.len() >= MAX_REFS {
            return;
        }
        if matches!(kind, RefKind::Read | RefKind::EnvVar)
            && !self.seen_refs.insert((ctx.enclosing, kind == RefKind::Read, name.clone()))
        {
            return;
        }
        self.facts.references.push(Reference { kind, name, receiver, enclosing: ctx.enclosing, span, arity: None });
    }

    // -- commands --------------------------------------------------------------

    /// The command word, `\` escapes removed (`\. file` is `.`).
    fn command_word(&self, cmd: Node<'_>) -> Option<String> {
        let name = cmd.child_by_field_name("name")?;
        Some(self.text(name).trim_start_matches('\\').to_string())
    }

    fn arguments<'t>(cmd: Node<'t>) -> Vec<Node<'t>> {
        let mut cursor = cmd.walk();
        cmd.children_by_field_name("argument", &mut cursor).collect()
    }

    fn command(&mut self, node: Node<'_>, ctx: Ctx, depth: usize) {
        let args = Self::arguments(node);
        // `NAME=value cmd`: env providers for that command.
        let mut cursor = node.walk();
        let prefixes: Vec<Node<'_>> =
            node.named_children(&mut cursor).filter(|c| c.kind() == "variable_assignment").collect();
        for a in &prefixes {
            if let Some(name) = assignment_name(*a) {
                self.provider(name, node, true, ctx);
            }
        }
        if let Some(name_node) = node.child_by_field_name("name") {
            self.command_head(node, name_node, &args, ctx);
            self.walk_children(name_node, ctx, depth);
        }
        for a in prefixes.iter().filter_map(|a| a.child_by_field_name("value")) {
            self.walk(a, ctx, depth + 1);
        }
        let mut cursor = node.walk();
        let rest: Vec<Node<'_>> = node
            .named_children(&mut cursor)
            .filter(|c| !matches!(c.kind(), "command_name" | "variable_assignment"))
            .collect();
        for c in rest {
            self.walk(c, ctx, depth + 1);
        }
    }

    /// What the command word means: a source, a call, a script run, a
    /// `trap` / `eval` fragment, or a runner of another command.
    fn command_head(&mut self, cmd: Node<'_>, name_node: Node<'_>, args: &[Node<'_>], ctx: Ctx) {
        let word = self.text(name_node).trim_start_matches('\\').to_string();
        match word.as_str() {
            "." | "source" => self.source(cmd, args.first().copied(), false),
            "load" | "bats_load_library" if self.bats_file => self.source(cmd, args.first().copied(), true),
            "trap" => self.trap(args, ctx),
            "eval" => self.fragments(args, ctx, RefKind::Call),
            "run" if self.test_file => self.operand(args, ctx, true),
            w if SAME_SHELL_RUNNERS.contains(&w) => self.operand(args, ctx, true),
            w if EXTERNAL_RUNNERS.contains(&w) => self.operand(args, ctx, false),
            w if INTERPRETERS.contains(&w) => self.interpreted(args, ctx),
            _ => self.invocation(name_node, ctx, true),
        }
    }

    /// `source FILE` / `. FILE` (bats: `load NAME` → `NAME.bash` next to
    /// the test file) → a star import of the file's functions.
    fn source(&mut self, cmd: Node<'_>, arg: Option<Node<'_>>, bats_load: bool) {
        let Some(arg) = arg else { return };
        let Some(mut pattern) = self.eval_path(arg) else { return };
        if bats_load {
            if !pattern.rest.ends_with(".bash") {
                pattern.rest.push_str(".bash");
            }
            if pattern.anchor == crate::paths::Anchor::Literal && !pattern.rest.starts_with('/') {
                pattern = PathPattern { anchor: crate::paths::Anchor::ScriptDir, rest: format!("/{}", pattern.rest) };
            }
        }
        let module = pattern.encode();
        if !self.imports.insert(module.clone()) {
            return;
        }
        self.facts.imports.push(Import {
            module,
            relative_level: 0,
            names: vec![ImportedName { name: "*".into(), alias: None }],
            module_alias: None,
            type_only: false,
            reexport: false,
            span: span_of(cmd),
        });
    }

    /// A command word in command position: a function call (a literal
    /// name), a script run (a path), or nothing (dynamic).
    fn invocation(&mut self, word: Node<'_>, ctx: Ctx, functions: bool) {
        let raw = self.text(word);
        let literal = matches!(word.kind(), "word" | "command_name") && !raw.contains(['$', '`', '"', '\'']);
        let name = raw.trim_start_matches('\\');
        if literal && is_command_name(name) && !is_script_path(name) {
            if functions {
                self.reference(RefKind::Call, name.to_string(), None, ctx, span_of(word));
            }
            return;
        }
        let target = if word.kind() == "command_name" { word.named_child(0).unwrap_or(word) } else { word };
        if let Some(p) = self.eval_path(target).filter(|p| is_script_path(&p.rest) || p.rest.contains('/')) {
            self.reference(RefKind::Call, p.encode(), Some(SCRIPT_RECEIVER.into()), ctx, span_of(word));
        }
    }

    /// The first operand of a runner (`time f`, `run f`, `exec ./x.sh`,
    /// `nohup ./x.sh`), options and `VAR=value` operands skipped.
    fn operand(&mut self, args: &[Node<'_>], ctx: Ctx, functions: bool) {
        let first = args.iter().find(|a| {
            let t = self.text(**a);
            !t.starts_with('-') && !(t.contains('=') && is_name(t.split('=').next().unwrap_or(""))) && t != "!"
        });
        if let Some(a) = first {
            self.invocation(*a, ctx, functions);
        }
    }

    /// `bash FILE`, `sh -e FILE`: a script run (not `bash -c CODE`).
    fn interpreted(&mut self, args: &[Node<'_>], ctx: Ctx) {
        for a in args {
            let t = self.text(*a);
            if t == "-c" {
                return;
            }
            if t.starts_with('-') || t.starts_with('+') {
                continue;
            }
            if let Some(p) = self.eval_path(*a) {
                self.reference(RefKind::Call, p.encode(), Some(SCRIPT_RECEIVER.into()), ctx, span_of(*a));
            }
            return;
        }
    }

    /// `trap HANDLER SIGNAL…`: the handler's commands are references (the
    /// function runs later, on the signal: a callback, not a call).
    fn trap(&mut self, args: &[Node<'_>], ctx: Ctx) {
        let args = match args.first() {
            Some(a) if self.text(*a) == "--" => &args[1..],
            _ => args,
        };
        // `trap - SIG` resets, `trap -p` / `trap -l` list: no handler.
        if let Some(h) = args.first().filter(|h| args.len() >= 2 && !self.text(**h).starts_with('-')) {
            self.fragments(std::slice::from_ref(h), ctx, RefKind::Read);
        }
    }

    /// Literal code in arguments (`eval 'f x'`, a `trap` handler): parsed
    /// and walked in place, each command word a `kind` reference. Arguments
    /// with expansions are not code the file determines.
    fn fragments(&mut self, args: &[Node<'_>], ctx: Ctx, kind: RefKind) {
        for a in args {
            let (code, skip) = match a.kind() {
                "word" => (self.text(*a).to_string(), 0),
                "raw_string" => (self.text(*a).trim_matches('\'').to_string(), 1),
                "string" if a.named_children(&mut a.walk()).all(|c| c.kind() == "string_content") => {
                    (self.text(*a).trim_matches('"').to_string(), 1)
                }
                _ => continue,
            };
            let Some(tree) = parse_fragment(&code) else { continue };
            let mut sub = Fragment { code: code.as_bytes(), kind, outer: self };
            sub.walk(tree.root_node(), ctx, Origin::of(*a, skip));
        }
    }
}

/// Walks a fragment parsed on its own: its commands only (calls and
/// script runs), with spans mapped back into the file.
struct Fragment<'a, 's> {
    code: &'a [u8],
    /// `Call` (`eval`: runs now) or `Read` (a `trap` handler: a function
    /// named for later, like a callback).
    kind: RefKind,
    outer: &'a mut Extractor<'s>,
}

impl Fragment<'_, '_> {
    fn walk(&mut self, node: Node<'_>, ctx: Ctx, origin: Origin) {
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            if n.kind() == "command"
                && let Some(name) = n.child_by_field_name("name")
            {
                let word = text(name, self.code);
                if is_command_name(word) && !is_script_path(word) {
                    let span = origin.map(span_of(name));
                    self.outer.reference(self.kind, word.to_string(), None, ctx, span);
                }
            }
            let mut cursor = n.walk();
            let children: Vec<Node<'_>> = n.named_children(&mut cursor).collect();
            stack.extend(children.into_iter().rev());
        }
    }
}

/// The variable a `variable_assignment` assigns (`a` of `a[1]=x`).
fn assignment_name(node: Node<'_>) -> Option<Node<'_>> {
    let name = node.child_by_field_name("name")?;
    match name.kind() {
        "variable_name" => Some(name),
        "subscript" => name.child_by_field_name("name"),
        _ => None,
    }
}

/// The variable an expansion reads (`X` of `$X`, `${X:-d}`, `${#X}`,
/// `${X[@]}`); `None` for special and positional parameters. In a
/// tree-sitter-zsh tree the name is a `simple_variable_name`, directly or
/// under the node of the expansion's operator (`expansion_default`, …).
fn expansion_name(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    for c in node.named_children(&mut cursor) {
        match c.kind() {
            "variable_name" | "simple_variable_name" => return Some(c),
            "subscript" => return c.child_by_field_name("name"),
            "special_variable_name" => return None,
            k if k.starts_with("expansion_") => match c.child_by_field_name("name") {
                Some(n) if n.kind() == "simple_variable_name" => return Some(n),
                Some(_) => return None,
                None => {}
            },
            _ => {}
        }
    }
    None
}

/// A word that can name a function: no path, no expansion, not a
/// keyword-like token.
fn is_command_name(s: &str) -> bool {
    !s.is_empty()
        && !s.contains(['/', '$', '=', '(', ')', '{', '}', '[', ']', '<', '>', '*', '?', '"', '\'', '`', ' '])
        && !s.starts_with('-')
        && s.chars().any(|c| c.is_alphabetic())
}

/// A word that names a script file: a path, or a shell script extension.
fn is_script_path(s: &str) -> bool {
    s.contains('/') || SCRIPT_EXTENSIONS.iter().any(|e| s.ends_with(e))
}

/// Names whose values are never shown (the data pack's rule).
fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["pass", "secret", "token", "apikey", "api_key", "api-key", "private_key", "credential"]
        .iter()
        .any(|k| n.contains(k))
}
