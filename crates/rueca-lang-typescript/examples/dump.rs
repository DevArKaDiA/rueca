//! Prints the syntax tree of stdin with field names, for writing extraction
//! rules and queries: `cargo run --example dump -- ts|tsx|js < file`.
use std::io::Read;

fn print(node: tree_sitter::Node, src: &[u8], depth: usize, field: Option<&str>) {
    let indent = "  ".repeat(depth);
    let f = field.map(|f| format!("{f}: ")).unwrap_or_default();
    let text = if node.child_count() == 0 {
        format!(" {:?}", String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]))
    } else {
        String::new()
    };
    if node.is_named() {
        println!("{indent}{f}{}{text}", node.kind());
    } else {
        println!("{indent}{f}'{}'", node.kind());
    }
    let mut c = node.walk();
    for (i, ch) in node.children(&mut c).enumerate() {
        print(ch, src, depth + 1, node.field_name_for_child(i as u32));
    }
}

fn main() {
    let lang = std::env::args().nth(1).unwrap_or_else(|| "ts".into());
    let mut src = Vec::new();
    std::io::stdin().read_to_end(&mut src).expect("read stdin");
    let l: tree_sitter::Language = match lang.as_str() {
        "ts" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        _ => tree_sitter_javascript::LANGUAGE.into(),
    };
    let mut p = tree_sitter::Parser::new();
    p.set_language(&l).expect("grammar");
    let t = p.parse(&src, None).expect("parse");
    print(t.root_node(), &src, 0, None);
}
