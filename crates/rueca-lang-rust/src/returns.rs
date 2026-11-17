//! `$ret:` receivers: the result of a call whose return type the file does
//! not declare (a function or method of another file, a constructor that
//! is not `new`), left to the resolver to type from the callee's signature.
//!
//! `$ret:<receiver>#<name>` is the value returned by calling `name` with
//! `receiver` (any receiver text: a path, `$type:T`, `$self`, another
//! `$ret:`; empty for a bare call), and a trailing `?` means the value is
//! unwrapped (`?`, `.unwrap()`, `.expect(…)`: `Result<T, _>` / `Option<T>`
//! → `T`). `Store::open(p).context("…")?` is `$ret:Store#open?`,
//! `make().build()` is `$ret:$ret:#make#build`. Calls that keep a value's
//! type (`.clone()`, `.context(…)`, `.map_err(…)`) are seen through.

use rueca_extract::tree_sitter::Node;

use crate::extract::{Cx, Extractor, named_children};
use crate::types::path_segments;

/// Prefix of a call-result receiver.
pub(crate) const RET_RECEIVER_PREFIX: &str = "$ret:";

/// Deepest nesting of call results in one receiver.
const MAX_RET_DEPTH: usize = 6;

/// Methods returning their receiver's `Ok` / `Some` value.
fn unwraps(m: &str) -> bool {
    matches!(m, "unwrap" | "expect" | "unwrap_or_default" | "unwrap_or_else" | "unwrap_or" | "unwrap_unchecked")
}

/// Methods returning a value of their receiver's type (or the same
/// `Result` with another error).
fn keeps_type(m: &str) -> bool {
    matches!(m, "clone" | "to_owned" | "context" | "with_context" | "map_err" | "as_mut" | "as_ref" | "by_ref")
}

impl Extractor<'_> {
    /// The `$ret:` receiver of an expression whose value is a call result,
    /// if any (see the module docs).
    pub(crate) fn ret_receiver(&mut self, cx: Cx<'_>, expr: Node<'_>, depth: usize) -> Option<String> {
        if depth > MAX_RET_DEPTH {
            return None;
        }
        match expr.kind() {
            "parenthesized_expression" | "await_expression" | "reference_expression" | "unary_expression" => {
                let inner = named_children(expr).into_iter().next_back()?;
                self.ret_receiver(cx, inner, depth + 1)
            }
            "try_expression" => {
                let inner = *named_children(expr).first()?;
                self.ret_receiver(cx, inner, depth + 1).map(unwrapped)
            }
            "identifier" => {
                let name = cx.text(expr);
                self.binding(cx, expr, name).and_then(|b| b.ret.clone())
            }
            "call_expression" => self.call_ret(cx, expr, depth),
            _ => None,
        }
    }

    /// `$ret:` of a call expression.
    fn call_ret(&mut self, cx: Cx<'_>, call: Node<'_>, depth: usize) -> Option<String> {
        let mut func = call.child_by_field_name("function")?;
        if func.kind() == "generic_function" {
            func = func.child_by_field_name("function")?;
        }
        match func.kind() {
            "identifier" => {
                let name = cx.text(func);
                let local = self.binding(cx, func, name).is_some();
                let ty = name.starts_with(|c: char| c.is_uppercase());
                (!local && !ty).then(|| format!("{RET_RECEIVER_PREFIX}#{name}"))
            }
            "scoped_identifier" => {
                let segs = path_segments(cx, func)?;
                let (name, qualifier) = segs.split_last()?;
                if name.starts_with(|c: char| c.is_uppercase()) {
                    return None;
                }
                let recv = if qualifier.is_empty() { None } else { self.path_receiver(cx, qualifier, func) };
                Some(format!("{RET_RECEIVER_PREFIX}{}#{name}", recv.unwrap_or_default()))
            }
            "field_expression" => self.method_ret(cx, func, depth),
            _ => None,
        }
    }

    /// `$ret:` of `value.m(…)`.
    fn method_ret(&mut self, cx: Cx<'_>, func: Node<'_>, depth: usize) -> Option<String> {
        let value = func.child_by_field_name("value")?;
        let m = cx.text(func.child_by_field_name("field")?).to_string();
        if unwraps(&m) {
            return self.ret_receiver(cx, value, depth + 1).map(unwrapped);
        }
        if keeps_type(&m) {
            return self.ret_receiver(cx, value, depth + 1);
        }
        let recv = self.known_receiver(cx, value, depth + 1)?;
        Some(format!("{RET_RECEIVER_PREFIX}{recv}#{m}"))
    }

    /// The receiver text of `value` when the file says something about it
    /// (`$self`, `$type:`, `$member:`, `$ret:`, a path); `None` for an
    /// untyped local or an unknown expression.
    fn known_receiver(&mut self, cx: Cx<'_>, value: Node<'_>, depth: usize) -> Option<String> {
        if let Some(r) = self.ret_receiver(cx, value, depth) {
            return Some(r);
        }
        let r = self.receiver_of(cx, value);
        (!r.starts_with('(')).then_some(r)
    }

    /// The `$ret:` of an untyped `let x = <call>;` binding.
    pub(crate) fn binding_ret(&mut self, root: Node<'_>, i: usize) -> Option<String> {
        let pos = self.locals.bindings[i].pos as usize;
        let ident = root.descendant_for_byte_range(pos, pos + 1)?;
        let decl = ident.parent().filter(|p| p.kind() == "let_declaration")?;
        if decl.child_by_field_name("type").is_some() {
            return None;
        }
        let value = decl.child_by_field_name("value")?;
        self.ret_receiver(Cx::main(self.src), value, 0)
    }
}

/// A receiver marked unwrapped (one `?` is enough: `Result<Option<T>>`
/// unwrapped twice is rare and `T` is still the guess).
fn unwrapped(spec: String) -> String {
    if spec.ends_with('?') { spec } else { format!("{spec}?") }
}
