//! Items inside macro invocations.
//!
//! Crates gate whole groups of items behind macros that pass them through:
//! tokio's `cfg_net! { pub mod tcp; pub use tcp::listener::TcpListener; }`,
//! `cfg_if! { if #[cfg(unix)] { mod unix; } else { mod other; } }`.
//! tree-sitter keeps a macro's body as an opaque `token_tree`, so these
//! items would be neither definitions, nor `mod` declarations, nor imports.
//!
//! Before the file is walked, a macro invocation in item or statement
//! position whose body **parses as items** (on its own, as a source file:
//! no errors, every top-level node an item, a `use`, a `mod`, an attribute,
//! a macro invocation or a comment) is opened in place: the delimiters are
//! blanked and the invocation keeps an empty body (`cfg_net!{}`, still a
//! call of the macro), so the items become items of the enclosing file,
//! inline module or function and every rule of the walk applies to them
//! unchanged. A body of the shape `if #[attr] { … } else if #[attr] { … }
//! else { … }` (`cfg_if!`) opens every branch: `#[cfg]` is ignored, as it is
//! everywhere else.
//!
//! The rewrites are **same-length**: bytes become spaces (two of them `{}`)
//! and newlines are kept, so offsets, lines and the text read for names and
//! signatures are the file's own and no span is translated. The file is
//! parsed again after each round; a round that would add parse errors is
//! dropped. Rounds repeat for macros nested in opened bodies (≤ 4), and the
//! bodies parsed on their own count against the per-file re-parse budget
//! of `macros.rs` (four times the file plus 64 KiB).
//!
//! Not opened: bodies that are expressions (`println!`, `vec!`,
//! `assert_eq!`; their arguments are visited by `macros.rs`), invocations
//! in expression position, `macro_rules!` definitions, the token builders
//! of `is_opaque_macro`, and an invocation with no whitespace around its
//! opening delimiter (`m!{fn f() {}}`: no room for the empty body).

use rueca_extract::tree_sitter::{self, Node, Tree};
use rueca_extract::{Budget, count_errors};

use crate::builtins::is_opaque_macro;

/// Rounds of opening (macros nested in opened bodies).
const MAX_ROUNDS: usize = 4;

/// Bytes parsed on their own beyond four times the file size.
const EXTRA_BYTES: usize = 64 * 1024;

/// Top-level nodes an item-shaped body may hold.
const ITEM_KINDS: &[&str] = &[
    "associated_type",
    "attribute_item",
    "block_comment",
    "const_item",
    "empty_statement",
    "enum_item",
    "extern_crate_declaration",
    "foreign_mod_item",
    "function_item",
    "function_signature_item",
    "impl_item",
    "inner_attribute_item",
    "line_comment",
    "macro_definition",
    "macro_invocation",
    "mod_item",
    "static_item",
    "struct_item",
    "trait_item",
    "type_item",
    "union_item",
    "use_declaration",
];

/// The text and tree the walk reads, and the bytes parsed on their own.
pub(crate) struct Expansion {
    /// The rewritten text, when a macro was opened.
    pub(crate) text: Option<Vec<u8>>,
    pub(crate) tree: Tree,
    pub(crate) bytes: usize,
}

/// Opens the item-shaped macro bodies of a file (see the module docs).
pub(crate) fn expand(source: &[u8], tree: Tree, budget: &Budget) -> Expansion {
    let mut out = Expansion { text: None, tree, bytes: 0 };
    let limit = 4 * source.len() + EXTRA_BYTES;
    for _ in 0..MAX_ROUNDS {
        if !out.round(source, budget, limit) {
            break;
        }
    }
    out
}

impl Expansion {
    /// One round: opens the macros the current tree shows in item position;
    /// false when nothing was opened.
    fn round(&mut self, source: &[u8], budget: &Budget, limit: usize) -> bool {
        let cur = self.text.as_deref().unwrap_or(source);
        let mut finder = Finder { src: cur, budget, bytes: self.bytes, limit, edits: Vec::new() };
        finder.walk(self.tree.root_node());
        if finder.edits.is_empty() {
            return false;
        }
        let mut next = cur.to_vec();
        for e in &finder.edits {
            e.apply(&mut next);
        }
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let Ok(tree) = rueca_extract::parse(&language, &next, budget) else { return false };
        if count_errors(&tree) > count_errors(&self.tree) {
            return false;
        }
        self.bytes = finder.bytes;
        self.text = Some(next);
        self.tree = tree;
        true
    }
}

/// The rewrite that opens one invocation.
struct Edit {
    /// Byte ranges turned into spaces (newlines kept).
    blank: Vec<(usize, usize)>,
    /// Where the empty body `{}` is written.
    open: usize,
}

impl Edit {
    fn apply(&self, text: &mut [u8]) {
        for &(a, b) in &self.blank {
            for byte in &mut text[a..b] {
                if !matches!(*byte, b'\n' | b'\r') {
                    *byte = b' ';
                }
            }
        }
        text[self.open..self.open + 2].copy_from_slice(b"{}");
    }
}

/// Finds the invocations to open in one tree.
struct Finder<'a> {
    src: &'a [u8],
    budget: &'a Budget,
    bytes: usize,
    limit: usize,
    edits: Vec<Edit>,
}

impl Finder<'_> {
    fn walk(&mut self, root: Node<'_>) {
        let mut stack = vec![(root, 0usize)];
        while let Some((node, depth)) = stack.pop() {
            if depth > self.budget.max_depth {
                continue;
            }
            match node.kind() {
                "macro_invocation" => self.invocation(node),
                "token_tree" | "macro_definition" => {}
                _ => {
                    let mut cursor = node.walk();
                    stack.extend(node.named_children(&mut cursor).map(|c| (c, depth + 1)));
                }
            }
        }
    }

    /// An invocation in item or statement position whose body is items.
    fn invocation(&mut self, node: Node<'_>) {
        if !in_item_position(node) || is_opaque_macro(&macro_name(node, self.src)) {
            return;
        }
        let Some(tt) = node.child_by_field_name("macro").and_then(|_| token_tree(node)) else { return };
        let regions = if_chain(tt, self.src).unwrap_or_else(|| vec![(tt.start_byte() + 1, tt.end_byte() - 1)]);
        if regions.iter().all(|&(a, b)| is_blank(&self.src[a..b])) {
            return;
        }
        let size: usize = regions.iter().map(|&(a, b)| b - a).sum();
        if self.bytes + size > self.limit || !regions.iter().all(|&(a, b)| self.item_shaped(&self.src[a..b])) {
            return;
        }
        if let Some(edit) = self.edit(node, tt, &regions) {
            self.bytes += size;
            self.edits.push(edit);
        }
    }

    /// Whether `text` parses on its own as items.
    fn item_shaped(&self, text: &[u8]) -> bool {
        if is_blank(text) {
            return true;
        }
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        let Ok(tree) = rueca_extract::parse(&language, text, self.budget) else { return false };
        let root = tree.root_node();
        let mut cursor = root.walk();
        !root.has_error() && root.named_children(&mut cursor).all(|c| ITEM_KINDS.contains(&c.kind()))
    }

    /// Blanks everything of the invocation's body but `regions`, and finds
    /// room for the empty body between `!` and the first item.
    fn edit(&self, node: Node<'_>, tt: Node<'_>, regions: &[(usize, usize)]) -> Option<Edit> {
        let bang = bang_end(node)?;
        if !is_blank(&self.src[bang..tt.start_byte()]) {
            return None;
        }
        let mut blank = Vec::with_capacity(regions.len() + 2);
        let mut at = tt.start_byte();
        for &(a, b) in regions {
            blank.push((at, a));
            at = b;
        }
        blank.push((at, self.trailing_semicolon(tt)));
        let first = regions[0].0 + leading_blank(&self.src[regions[0].0..regions[0].1]);
        let open =
            (bang..first.saturating_sub(1)).find(|&i| !is_newline(self.src[i]) && !is_newline(self.src[i + 1]))?;
        Some(Edit { blank, open })
    }

    /// End of the body, past a `;` after `)` / `]` (`m!( … );`, whose
    /// opened form `m!{}` takes none).
    fn trailing_semicolon(&self, tt: Node<'_>) -> usize {
        let end = tt.end_byte();
        if self.src.get(tt.start_byte()) == Some(&b'{') {
            return end;
        }
        let ws = leading_blank(&self.src[end..]);
        if self.src.get(end + ws) == Some(&b';') { end + ws + 1 } else { end }
    }
}

/// Whether an invocation stands where items or statements do.
fn in_item_position(node: Node<'_>) -> bool {
    let holds_items = |n: Node<'_>| matches!(n.kind(), "source_file" | "declaration_list" | "block");
    let Some(parent) = node.parent() else { return false };
    match parent.kind() {
        "expression_statement" => parent.parent().is_some_and(holds_items),
        _ => holds_items(parent),
    }
}

/// The last segment of an invocation's macro path.
fn macro_name(node: Node<'_>, src: &[u8]) -> String {
    let Some(m) = node.child_by_field_name("macro") else { return String::new() };
    let n = m.child_by_field_name("name").unwrap_or(m);
    String::from_utf8_lossy(&src[n.start_byte()..n.end_byte()]).into_owned()
}

fn token_tree(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let tt = node.named_children(&mut cursor).find(|c| c.kind() == "token_tree");
    tt.filter(|t| t.end_byte() >= t.start_byte() + 2)
}

/// End of the `!` of an invocation.
fn bang_end(node: Node<'_>) -> Option<usize> {
    let mut cursor = node.walk();
    let bang = node.children(&mut cursor).find(|c| c.kind() == "!");
    bang.map(|b| b.end_byte())
}

/// The braced branches of a body `if #[attr] { … } (else if #[attr] { … })*
/// (else { … })?`, inside their braces.
fn if_chain(tt: Node<'_>, src: &[u8]) -> Option<Vec<(usize, usize)>> {
    let mut cursor = tt.walk();
    let all: Vec<Node<'_>> = tt.children(&mut cursor).collect();
    let tokens: Vec<Node<'_>> = all
        .get(1..all.len().saturating_sub(1))?
        .iter()
        .copied()
        .filter(|n| !crate::extract::is_comment(n.kind()))
        .collect();
    let text = |n: &Node<'_>| &src[n.start_byte()..n.end_byte()];
    let mut regions = Vec::new();
    let mut i = 0;
    loop {
        i = chain_condition(&tokens, i, &text)?;
        regions.push(braced(tokens.get(i), src)?);
        i += 1;
        match tokens.get(i).map(text) {
            None => return Some(regions),
            Some(b"else") if tokens.get(i + 1).map(text) == Some(b"if") => i += 1,
            Some(b"else") => {
                regions.push(braced(tokens.get(i + 1), src)?);
                return (i + 2 == tokens.len()).then_some(regions);
            }
            Some(_) => return None,
        }
    }
}

/// `if #[attr]…` from token `i`: the index of the branch's block.
fn chain_condition<'t>(tokens: &[Node<'t>], i: usize, text: &impl Fn(&Node<'t>) -> &'t [u8]) -> Option<usize> {
    if tokens.get(i).map(text) != Some(b"if") {
        return None;
    }
    let mut j = i + 1;
    while tokens.get(j).map(text) == Some(b"#") && tokens.get(j + 1).is_some_and(|t| text(t).first() == Some(&b'[')) {
        j += 2;
    }
    (j > i + 1).then_some(j)
}

/// The inside of a `{ … }` token tree.
fn braced(node: Option<&Node<'_>>, src: &[u8]) -> Option<(usize, usize)> {
    let n = node.filter(|n| n.kind() == "token_tree" && src.get(n.start_byte()) == Some(&b'{'))?;
    Some((n.start_byte() + 1, n.end_byte() - 1))
}

fn is_blank(text: &[u8]) -> bool {
    text.iter().all(u8::is_ascii_whitespace)
}

/// Length of the leading whitespace of `text`.
fn leading_blank(text: &[u8]) -> usize {
    text.iter().take_while(|b| b.is_ascii_whitespace()).count()
}

fn is_newline(b: u8) -> bool {
    matches!(b, b'\n' | b'\r')
}
