//! Contract hints of a PHP file (RFC 0006 phase 4; keys and matching in
//! `rueca_model::contract`). One pass over the syntax tree after the
//! definitions are known, so every hint can name its owner definition.
//!
//! - **HTTP providers** (`routes.rs`): Laravel `Route::get('/x', …)`
//!   (`post`, `put`, `patch`, `delete`, `options`, `any` = `*`,
//!   `match([…], …)`, `resource` / `apiResource` expanded), chained
//!   (`Route::middleware('a')->get(…)`), composed with the
//!   `Route::prefix('/api')->group(…)` / `Route::group(['prefix' => …], …)`
//!   closures around it; Symfony `#[Route('/x', methods: […])]` on methods,
//!   composed with the class's `#[Route('/prefix')]` (no `methods` = `*`);
//!   Slim / Lumen `$app->get('/x', handler)` on `$app` / `$router`.
//!   Owner: the method for attributes; else the enclosing definition, else
//!   the file (a Laravel route file).
//! - **HTTP consumers** (`clients.rs`): Guzzle-style `$client->get('/x')`,
//!   `->request('POST', '/x')` (`*Async` too), Laravel `Http::get('…')` and
//!   chains rooted at `Http::`, `curl_setopt($ch, CURLOPT_URL, '…')`
//!   (method `*`), with a path-like URL (`/…`, `http(s)://…`, an
//!   interpolated base followed by `/…`): literals, interpolated strings,
//!   `.` concatenation, `sprintf`, a variable assigned once in the
//!   function. Owner: the enclosing definition.

mod clients;
mod render;
mod routes;

use std::collections::HashMap;

use rueca_extract::tree_sitter::Node;
use rueca_extract::{span_of, text_of};
use rueca_model::contract::truncate_raw;
use rueca_model::{ContractHint, ContractKind, ContractSide, Definition, Label, LocalDefId};

/// Contract hints of a parsed file; `defs` are the file's definitions.
pub(crate) fn extract(root: Node<'_>, src: &[u8], defs: &[Definition], max_depth: usize) -> Vec<ContractHint> {
    let mut cx = Ctx::new(root, src, defs, max_depth);
    let mut stack = vec![(root, 0usize)];
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            continue;
        }
        cx.visit(node);
        let mut c = node.walk();
        stack.extend(node.named_children(&mut c).map(|ch| (ch, depth + 1)));
    }
    cx.hints.sort_by_key(|h| h.span.start_byte);
    cx.hints
}

/// What the file says about names, plus the hints found so far.
pub(super) struct Ctx<'t, 's> {
    src: &'s [u8],
    defs: &'s [Definition],
    max_depth: usize,
    /// Local class name / alias → fully qualified name of its `use`.
    uses: HashMap<String, String>,
    /// Single-assignment tables of functions, by function node id.
    assigned: HashMap<usize, HashMap<String, Option<Node<'t>>>>,
    hints: Vec<ContractHint>,
}

impl<'t, 's> Ctx<'t, 's> {
    fn new(root: Node<'t>, src: &'s [u8], defs: &'s [Definition], max_depth: usize) -> Self {
        let mut cx = Ctx { src, defs, max_depth, uses: HashMap::new(), assigned: HashMap::new(), hints: Vec::new() };
        cx.scan_uses(root);
        cx
    }

    /// `use A\B\C;`, `use A\B\C as D;` at the top level or in a namespace.
    fn scan_uses(&mut self, root: Node<'t>) {
        let mut stack = vec![(root, 0usize)];
        while let Some((n, depth)) = stack.pop() {
            match n.kind() {
                "namespace_use_clause" => self.scan_use_clause(n),
                "program" | "namespace_definition" | "compound_statement" | "namespace_use_declaration"
                    if depth < 4 =>
                {
                    let mut c = n.walk();
                    stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
                }
                _ => {}
            }
        }
    }

    fn scan_use_clause(&mut self, n: Node<'t>) {
        let mut c = n.walk();
        let Some(name) = n.named_children(&mut c).find(|ch| matches!(ch.kind(), "qualified_name" | "name")) else {
            return;
        };
        let fqn = self.text(name).trim_start_matches('\\').to_string();
        let local = match n.child_by_field_name("alias") {
            Some(a) => self.text(a),
            None => fqn.rsplit('\\').next().unwrap_or_default().to_string(),
        };
        self.uses.insert(local, fqn);
    }

    fn visit(&mut self, node: Node<'t>) {
        match node.kind() {
            "scoped_call_expression" | "member_call_expression" | "nullsafe_member_call_expression" => {
                if !routes::route_call(self, node) {
                    clients::http_call(self, node);
                }
            }
            "function_call_expression" => clients::curl(self, node),
            "class_declaration" => routes::symfony_class(self, node),
            _ => {}
        }
    }

    // ---------------------------------------------------------------------
    // Shared helpers
    // ---------------------------------------------------------------------

    fn text(&self, n: Node<'_>) -> String {
        text_of(n, self.src).into_owned()
    }

    /// The fully qualified name a class name refers to by the file's `use`s
    /// (`Route` → `Illuminate\Support\Facades\Route`), else as written.
    fn class_fqn(&self, name: &str) -> String {
        let name = name.trim_start_matches('\\');
        let (first, rest) = name.split_once('\\').map_or((name, ""), |(f, r)| (f, r));
        match self.uses.get(first) {
            Some(fqn) if rest.is_empty() => fqn.clone(),
            Some(fqn) => format!("{fqn}\\{rest}"),
            None => name.to_string(),
        }
    }

    /// Records a hint whose evidence is `node` (its text, one line).
    fn push(
        &mut self,
        side: ContractSide,
        kind: ContractKind,
        key: String,
        node: Node<'_>,
        owner: Option<LocalDefId>,
        framework: &str,
    ) {
        let raw = text_of(node, self.src).split_whitespace().collect::<Vec<_>>().join(" ");
        self.hints.push(ContractHint {
            side,
            kind,
            key,
            raw: truncate_raw(&raw),
            span: span_of(node),
            owner,
            framework: framework.to_string(),
        });
    }

    /// The definition whose name is the node `name`.
    fn def_named_at(&self, name: Node<'_>) -> Option<LocalDefId> {
        let (s, e) = (name.start_byte() as u32, name.end_byte() as u32);
        self.defs.iter().position(|d| d.name_span.start_byte == s && d.name_span.end_byte == e).map(|i| i as LocalDefId)
    }

    /// The innermost function, method or class around `node`.
    fn enclosing(&self, node: Node<'_>) -> Option<LocalDefId> {
        let span = span_of(node);
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| is_scope_label(d.label) && d.span.contains(&span))
            .max_by_key(|(_, d)| d.span.start_byte)
            .map(|(i, _)| i as LocalDefId)
    }
}

fn is_scope_label(label: Label) -> bool {
    matches!(label, Label::Function | Label::Method | Label::Class | Label::Test)
}

/// The value of each argument of a call / attribute, with its name for a
/// named argument (`methods: [...]`).
fn arguments<'t>(call: Node<'t>, src: &[u8]) -> Vec<(Option<String>, Node<'t>)> {
    let args = call.child_by_field_name("arguments").or_else(|| call.child_by_field_name("parameters"));
    let Some(args) = args else { return Vec::new() };
    let mut c = args.walk();
    args.named_children(&mut c)
        .filter(|a| a.kind() == "argument")
        .filter_map(|a| {
            let name = a.child_by_field_name("name").map(|n| text_of(n, src).into_owned());
            let count = a.named_child_count();
            let value = a.named_child(u32::try_from(count.checked_sub(1)?).ok()?)?;
            Some((name, value))
        })
        .collect()
}

/// Positional argument values.
fn positional<'t>(call: Node<'t>, src: &[u8]) -> Vec<Node<'t>> {
    arguments(call, src).into_iter().filter(|(n, _)| n.is_none()).map(|(_, v)| v).collect()
}

/// The value of named argument `name`.
fn named<'t>(call: Node<'t>, name: &str, src: &[u8]) -> Option<Node<'t>> {
    arguments(call, src).into_iter().find(|(n, _)| n.as_deref() == Some(name)).map(|(_, v)| v)
}

/// The method name of a call node.
fn call_name(call: Node<'_>, src: &[u8]) -> Option<String> {
    call.child_by_field_name("name").map(|n| text_of(n, src).into_owned())
}

#[cfg(test)]
mod tests;
