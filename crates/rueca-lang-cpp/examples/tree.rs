//! Prints the syntax tree of a file: `cargo run -p rueca-lang-cpp --example tree -- <path> [c]`.
fn main() {
    let path = std::env::args().nth(1).expect("usage: tree <path> [c]");
    let c = std::env::args().nth(2).is_some_and(|a| a == "c");
    let src = std::fs::read(&path).expect("read");
    let lang: tree_sitter::Language = if c { tree_sitter_c::LANGUAGE.into() } else { tree_sitter_cpp::LANGUAGE.into() };
    let mut p = tree_sitter::Parser::new();
    p.set_language(&lang).unwrap();
    let t = p.parse(&src, None).unwrap();
    let mut cursor = t.walk();
    let mut depth = 0usize;
    loop {
        let n = cursor.node();
        if n.is_named() || n.is_error() || n.is_missing() {
            print_node(&cursor, &src, depth);
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        if !skip_subtree(&mut cursor, &mut depth) {
            return;
        }
    }
}

/// Prints the cursor's node at `depth`: field, kind, `MISSING`, the text of
/// a leaf or identifier, and its line.
fn print_node(cursor: &tree_sitter::TreeCursor<'_>, src: &[u8], depth: usize) {
    let n = cursor.node();
    let field = cursor.field_name().map(|f| format!("{f}: ")).unwrap_or_default();
    let text = if n.child_count() == 0 || n.kind().ends_with("identifier") {
        format!(" `{}`", String::from_utf8_lossy(&src[n.start_byte()..n.end_byte()]))
    } else {
        String::new()
    };
    let miss = if n.is_missing() { " MISSING" } else { "" };
    println!("{}{field}{}{miss}{text} [{}]", "  ".repeat(depth), n.kind(), n.start_position().row + 1);
}

/// Moves `cursor` past its node's subtree, tracking `depth`; `false` past
/// the last node.
fn skip_subtree(cursor: &mut tree_sitter::TreeCursor<'_>, depth: &mut usize) -> bool {
    loop {
        if cursor.goto_next_sibling() {
            return true;
        }
        if !cursor.goto_parent() {
            return false;
        }
        *depth -= 1;
    }
}
