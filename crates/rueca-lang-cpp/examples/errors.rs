//! Prints the parse-error locations of a file after preprocessing:
//! `cargo run -p rueca-lang-cpp --example errors -- <path> [c|cpp] [write the parsed text here]`.
fn main() {
    let path = std::env::args().nth(1).expect("usage: errors <path> [c|cpp]");
    let c = std::env::args().nth(2).is_some_and(|a| a == "c");
    let src = std::fs::read(&path).expect("read");
    let language = if c { rueca_model::Language::C } else { rueca_model::Language::Cpp };
    let masked = rueca_lang_cpp::preprocessed(&src, language).expect("preprocess");
    if let Some(out) = std::env::args().nth(3) {
        std::fs::write(out, &masked).expect("write");
    }
    let lang: tree_sitter::Language = if c { tree_sitter_c::LANGUAGE.into() } else { tree_sitter_cpp::LANGUAGE.into() };
    let mut p = tree_sitter::Parser::new();
    p.set_language(&lang).unwrap();
    let t = p.parse(&masked, None).unwrap();
    let mut cursor = t.walk();
    let lines: Vec<&[u8]> = masked.split(|b| *b == b'\n').collect();
    loop {
        let n = cursor.node();
        if n.is_error() || n.is_missing() {
            let row = n.start_position().row;
            println!(
                "{}:{} {} | {}",
                row + 1,
                n.start_position().column,
                if n.is_missing() { format!("MISSING {}", n.kind()) } else { "ERROR".into() },
                String::from_utf8_lossy(lines.get(row).copied().unwrap_or(b"")).trim()
            );
        }
        if n.has_error() && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}
