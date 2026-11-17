//! Macro invocations and the code inside them.
//!
//! tree-sitter-rust keeps a macro's arguments as an opaque `token_tree`, but
//! most macro arguments are ordinary expressions that the compiler (and
//! rust-analyzer's SCIP index) resolves at their real positions:
//! `println!("{}", total())`, `assert_eq!(v.len(), 3)`, `vec![make()]`,
//! `write!(f, "{}", self.name())`. Each argument list is therefore parsed
//! again on its own, wrapped so that it is valid Rust — `(…)` as a tuple
//! expression, `[…]` as an array, `{…}` as a block — and visited like the
//! file's own code. The argument text is copied byte for byte, so a node
//! of the re-parsed text maps back to the file by one fixed offset (`Cx`);
//! locals are looked up at the mapped positions, so `x` in
//! `println!("{x:?}", x = y)` keeps the scoping of the surrounding function.
//! Re-parsing nests (a macro inside a macro argument, ≤ 4 levels) and is
//! bounded per file by the bytes re-parsed (four times the file plus
//! 64 KiB).
//!
//! The bodies of `macro_rules!` rules are not visited: they are token
//! patterns, not code, and rust-analyzer records no references in them (a
//! macro's callers call the macro).
//!
//! Not re-parsed: `stringify!`, `concat!`, `include*!` and `quote!`-style
//! token builders, whose arguments are not code the compiler resolves.

use rueca_extract::tree_sitter::{self, Node};
use rueca_model::RefKind;

use crate::builtins::{is_opaque_macro, is_std_macro};
use crate::extract::{Cx, Extractor, named_children};
use crate::refs::string_value;

/// Deepest nesting of re-parsed macro arguments.
const MAX_MACRO_DEPTH: usize = 4;

/// Bytes re-parsed beyond four times the file size.
const EXTRA_BYTES: usize = 64 * 1024;

/// The text a macro argument list is wrapped in, by its opening delimiter.
fn wrapper(open: u8) -> (&'static [u8], &'static [u8]) {
    match open {
        b'(' => (b"fn __rueca_macro__() { (", b",); }"),
        b'[' => (b"fn __rueca_macro__() { [", b"]; }"),
        _ => (b"fn __rueca_macro__() { {", b"} }"),
    }
}

impl Extractor<'_> {
    /// `name!(…)`, `path::name!(…)`: a call of a `macro_rules!` of the
    /// project (standard macros are not references unless the file defines
    /// one with that name), env reads for `env!` / `option_env!`, and the
    /// references inside the arguments.
    pub(crate) fn macro_invocation(&mut self, cx: Cx<'_>, node: Node<'_>, depth: usize) {
        let Some(m) = node.child_by_field_name("macro") else { return };
        let (name, receiver) = match m.kind() {
            "identifier" => (cx.text(m).to_string(), None),
            "scoped_identifier" => match m.child_by_field_name("name") {
                Some(n) => (cx.text(n).to_string(), self_qualifier(self, cx, m)),
                None => return,
            },
            _ => return,
        };
        let tt = named_children(node).into_iter().find(|c| c.kind() == "token_tree");
        let std_path = receiver.as_deref().is_none_or(|r| matches!(r, "std" | "core" | "alloc"));
        if std_path && matches!(name.as_str(), "env" | "option_env") {
            return self.env_macro(cx, node, tt);
        }
        if self.file.macros.contains(&name) || !(std_path && is_std_macro(&name)) {
            let span = self.span(cx, m);
            self.push_ref(RefKind::Call, &name, receiver, span);
        }
        if let Some(tt) = tt.filter(|_| !is_opaque_macro(&name)) {
            self.reparse(cx, tt, depth);
        }
    }

    /// `env!("X")` / `option_env!("X")`: an `EnvVar` read.
    fn env_macro(&mut self, cx: Cx<'_>, node: Node<'_>, tt: Option<Node<'_>>) {
        let Some(lit) = tt.and_then(|t| named_children(t).into_iter().next()).and_then(|l| string_value(cx, l)) else {
            return;
        };
        let span = self.span(cx, node);
        self.push_ref(RefKind::EnvVar, &lit, None, span);
    }

    /// Parses the inside of token tree `tt` as code and visits it (see the
    /// module docs).
    pub(crate) fn reparse(&mut self, cx: Cx<'_>, tt: Node<'_>, depth: usize) {
        if self.macro_depth >= MAX_MACRO_DEPTH || depth > self.max_depth {
            return;
        }
        let raw = &cx.src[tt.start_byte()..tt.end_byte()];
        if raw.len() < 2 {
            return;
        }
        let inner = &raw[1..raw.len() - 1];
        if self.macro_bytes + inner.len() > 4 * self.src.len() + EXTRA_BYTES {
            return;
        }
        self.macro_bytes += inner.len();
        let (pre, post) = wrapper(raw[0]);
        let mut buf = Vec::with_capacity(pre.len() + inner.len() + post.len());
        buf.extend_from_slice(pre);
        buf.extend_from_slice(inner);
        buf.extend_from_slice(post);
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let Ok(tree) = rueca_extract::parse(&language, &buf, &self.budget) else { return };
        let delta = i64::from(cx.pos(tt)) + 1 - pre.len() as i64;
        let sub = Cx { src: &buf, delta, snippet: true };
        let Some(body) = tree.root_node().named_child(0).and_then(|f| f.child_by_field_name("body")) else { return };
        let Ok(locals) = crate::locals::build(&tree, &buf, self.budget.max_parse_time) else { return };
        self.snippet_locals.push((locals, delta));
        self.macro_depth += 1;
        for c in named_children(body) {
            self.visit(sub, c, depth + 1);
        }
        self.macro_depth -= 1;
        self.snippet_locals.pop();
    }
}

/// The receiver text of a macro path's qualifier.
fn self_qualifier(ex: &mut Extractor<'_>, cx: Cx<'_>, path: Node<'_>) -> Option<String> {
    let segs = crate::types::path_segments(cx, path.child_by_field_name("path")?)?;
    ex.path_receiver(cx, &segs, path)
}
