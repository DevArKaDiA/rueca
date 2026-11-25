//! Finds the lines behind a file's parse errors: each line is blanked in
//! turn (same length) and the file parsed again; lines whose removal lowers
//! the ERROR / MISSING count the most are printed.
//! `cargo run --release -p rueca-lang-shell --example culprit -- <file> [--zsh|--bash] [--greedy]`
//! (default: the grammar the pack chooses for the file). With `--greedy`
//! the best line is blanked for good and the search repeated until no
//! error is left (or no line helps): the lines printed are a set whose
//! removal makes the file parse.

use rueca_extract::tree_sitter::{Language, Node, Parser};
use rueca_lang_shell::{Grammar, repaired_source, stats};

fn errors(lang: &Language, src: &[u8]) -> usize {
    let mut parser = Parser::new();
    parser.set_language(lang).expect("grammar");
    let Some(tree) = parser.parse(src, None) else { return usize::MAX };
    let mut n = 0;
    let mut stack: Vec<Node<'_>> = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        if node.is_error() || node.is_missing() {
            n += 1;
        }
        if node.has_error() || node.is_error() {
            let mut c = node.walk();
            stack.extend(node.children(&mut c));
        }
    }
    n
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.iter().find(|a| !a.starts_with("--")).expect("usage: culprit <file> [--zsh|--bash]");
    let bytes = std::fs::read(path).expect("read");
    let grammar = if args.iter().any(|a| a == "--zsh") {
        Grammar::Zsh
    } else if args.iter().any(|a| a == "--bash") {
        Grammar::Bash
    } else {
        stats(path, &bytes).expect("parse").grammar
    };
    let lang: Language = match grammar {
        Grammar::Bash => tree_sitter_bash::LANGUAGE.into(),
        Grammar::Zsh => tree_sitter_zsh::LANGUAGE.into(),
    };
    let mut src = repaired_source(&bytes, grammar);
    let greedy = args.iter().any(|a| a == "--greedy");
    let mut base = errors(&lang, &src);
    println!("{path}: {base} errors ({grammar:?} grammar)");
    loop {
        let mut found: Vec<(usize, usize, usize, usize, String)> = Vec::new();
        let mut start = 0;
        for (i, line) in src.split(|b| *b == b'\n').enumerate() {
            let end = start + line.len();
            if !line.iter().all(u8::is_ascii_whitespace) {
                let mut copy = src.clone();
                copy[start..end].fill(b' ');
                let n = errors(&lang, &copy);
                if n < base {
                    found.push((n, i + 1, start, end, String::from_utf8_lossy(line).trim().to_string()));
                }
            }
            start = end + 1;
        }
        found.sort();
        if !greedy {
            for (n, line, _, _, text) in found.into_iter().take(8) {
                println!("  -> {n} without :{line}: {text}");
            }
            return;
        }
        let Some((n, line, start, end, text)) = found.into_iter().next() else { return };
        println!("  -> {n} without :{line}: {text}");
        src[start..end].fill(b' ');
        base = n;
        if base == 0 {
            return;
        }
    }
}
