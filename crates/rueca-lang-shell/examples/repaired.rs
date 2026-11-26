//! Prints the lines the same-length repairs change in a file, as written
//! and as the grammar reads them:
//! `cargo run -p rueca-lang-shell --example repaired -- <file> [--zsh|--bash]`
//! (default: the grammar the pack chooses for the file).

use rueca_lang_shell::{Grammar, repaired_source, stats};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.iter().find(|a| !a.starts_with("--")).expect("usage: repaired <file> [--zsh|--bash]");
    let bytes = std::fs::read(path).expect("read");
    let grammar = if args.iter().any(|a| a == "--zsh") {
        Grammar::Zsh
    } else if args.iter().any(|a| a == "--bash") {
        Grammar::Bash
    } else {
        stats(path, &bytes).expect("parse").grammar
    };
    let repaired = repaired_source(&bytes, grammar);
    let (before, after) = (String::from_utf8_lossy(&bytes), String::from_utf8_lossy(&repaired));
    for (i, (a, b)) in before.lines().zip(after.lines()).enumerate() {
        if a != b {
            println!("{}:\n  - {}\n  + {}", i + 1, a.trim(), b.trim());
        }
    }
}
