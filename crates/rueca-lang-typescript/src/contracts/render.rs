//! The URL an expression builds: literal pieces and holes (template
//! substitutions, concatenated unknowns), parameter holes named for the
//! one-hop rule, and the HTTP key of it.

use std::collections::HashMap;

use rueca_extract::text_of;
use rueca_extract::tree_sitter::Node;
use rueca_model::contract::{Part, http_key_from_parts};

use super::{Ctx, string_value};

/// Owned [`Part`]; a hole may stand for a parameter of the enclosing
/// function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Piece {
    Lit(String),
    Hole(Option<String>),
}

/// How deep [`Ctx::render`] follows names and nested expressions.
const MAX_RENDER_DEPTH: usize = 4;

/// Function-like nodes (their parameters and `const`s scope a render).
pub(super) const FUNCTIONS: &[&str] = &[
    "function_declaration",
    "function_expression",
    "function",
    "arrow_function",
    "method_definition",
    "generator_function_declaration",
    "generator_function",
];

impl<'t> Ctx<'t, '_> {
    /// The pieces `node` builds: string literals, template literals, `+`
    /// concatenation (an unknown side is a hole), a `const` of the
    /// enclosing function assigned once, a parameter (a named hole).
    pub(super) fn render(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        if depth > MAX_RENDER_DEPTH {
            return None;
        }
        match node.kind() {
            "string" => Some(vec![Piece::Lit(string_value(node, self.src))]),
            "template_string" => Some(self.template(node)),
            "parenthesized_expression" => self.render(node.named_child(0)?, depth + 1),
            "binary_expression" => self.render_concat(node, depth),
            "identifier" => self.render_name(node, depth),
            _ => None,
        }
    }

    fn template(&self, node: Node<'t>) -> Vec<Piece> {
        let params = enclosing_function(node).map(|f| params(f, self.src)).unwrap_or_default();
        let mut out = Vec::new();
        let mut c = node.walk();
        for ch in node.named_children(&mut c) {
            match ch.kind() {
                "template_substitution" => {
                    let inner = ch.named_child(0).filter(|i| i.kind() == "identifier").map(|i| self.text(i));
                    out.push(Piece::Hole(inner.filter(|n| params.contains(n))));
                }
                _ => out.push(Piece::Lit(self.text(ch))),
            }
        }
        out
    }

    fn render_concat(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        let op = node.child_by_field_name("operator")?;
        if text_of(op, self.src) != "+" {
            return None;
        }
        let l = self.render(node.child_by_field_name("left")?, depth + 1);
        let r = self.render(node.child_by_field_name("right")?, depth + 1);
        if l.is_none() && r.is_none() {
            return None;
        }
        let mut out = l.unwrap_or_else(|| vec![Piece::Hole(None)]);
        out.extend(r.unwrap_or_else(|| vec![Piece::Hole(None)]));
        Some(out)
    }

    /// A `const` / `let` assigned once in the enclosing function, else a
    /// parameter of it (a named hole).
    fn render_name(&mut self, node: Node<'t>, depth: usize) -> Option<Vec<Piece>> {
        let func = enclosing_function(node)?;
        let name = self.text(node);
        if !self.assigned.contains_key(&func.id()) {
            let table = declarations(func, self.src, self.max_depth);
            self.assigned.insert(func.id(), table);
        }
        match self.assigned.get(&func.id()).and_then(|t| t.get(&name)).copied() {
            Some(Some(value)) => self.render(value, depth + 1),
            Some(None) => None,
            None => params(func, self.src).contains(&name).then(|| vec![Piece::Hole(Some(name))]),
        }
    }
}

/// The nearest function around `node`.
pub(super) fn enclosing_function(node: Node<'_>) -> Option<Node<'_>> {
    let mut cur = node.parent();
    while let Some(n) = cur {
        if FUNCTIONS.contains(&n.kind()) {
            return Some(n);
        }
        cur = n.parent();
    }
    None
}

/// Parameter names of a function, in order (`""` for a destructured one).
pub(super) fn params(func: Node<'_>, src: &[u8]) -> Vec<String> {
    let Some(ps) = func.child_by_field_name("parameters") else {
        // `x => …`
        return func.child_by_field_name("parameter").map(|p| vec![text_of(p, src).into_owned()]).unwrap_or_default();
    };
    let mut c = ps.walk();
    ps.named_children(&mut c)
        .filter(|p| p.kind() != "comment")
        .map(|p| {
            let pat = match p.kind() {
                "required_parameter" | "optional_parameter" => p.child_by_field_name("pattern"),
                "assignment_pattern" => p.child_by_field_name("left"),
                _ => Some(p),
            };
            pat.filter(|n| n.kind() == "identifier").map(|n| text_of(n, src).into_owned()).unwrap_or_default()
        })
        .collect()
}

/// `name = value` declarations of a function body: the value when the name
/// is declared once, `None` when more often.
fn declarations<'t>(func: Node<'t>, src: &[u8], max_depth: usize) -> HashMap<String, Option<Node<'t>>> {
    let mut out: HashMap<String, Option<Node<'t>>> = HashMap::new();
    let Some(body) = func.child_by_field_name("body") else { return out };
    let mut stack = vec![(body, 0usize)];
    while let Some((n, depth)) = stack.pop() {
        if n.kind() == "variable_declarator"
            && let (Some(name), Some(value)) = (n.child_by_field_name("name"), n.child_by_field_name("value"))
            && name.kind() == "identifier"
        {
            out.entry(text_of(name, src).into_owned()).and_modify(|v| *v = None).or_insert(Some(value));
        }
        if depth < max_depth {
            let mut c = n.walk();
            stack.extend(n.named_children(&mut c).map(|ch| (ch, depth + 1)));
        }
    }
    out
}

/// The pieces joined, holes written `{}` unless `subst` names their
/// parameter.
pub(super) fn joined(pieces: &[Piece], subst: &HashMap<String, String>) -> Vec<Piece> {
    pieces
        .iter()
        .map(|p| match p {
            Piece::Hole(Some(name)) => subst.get(name).map_or(Piece::Hole(None), |v| Piece::Lit(v.clone())),
            other => other.clone(),
        })
        .collect()
}

/// Whether the pieces render something an HTTP client could send: `/…`,
/// `http(s)://…`, or an interpolated base URL followed by `/…`.
pub(super) fn looks_like_path(pieces: &[Piece]) -> bool {
    let joined: String = pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(s) => s.as_str(),
            Piece::Hole(_) => "{}",
        })
        .collect();
    !joined.contains(char::is_whitespace)
        && joined.len() <= 300
        && ["/", "http://", "https://", "{}/"].iter().any(|p| joined.starts_with(p))
}

/// HTTP key of `method` (`*` = unknown) and the rendered URL.
pub(super) fn http_key(method: &str, pieces: &[Piece]) -> Option<String> {
    let parts: Vec<Part<'_>> = pieces
        .iter()
        .map(|p| match p {
            Piece::Lit(s) => Part::Lit(s),
            Piece::Hole(_) => Part::Hole,
        })
        .collect();
    http_key_from_parts((method != "*").then_some(method), &parts)
}
