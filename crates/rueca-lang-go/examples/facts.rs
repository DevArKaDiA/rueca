//! Prints the facts of one file: `cargo run -p rueca-lang-go --example facts -- <path>`.
use rueca_extract::{Budget, LanguagePack};
use rueca_lang_go::GoPack;
use rueca_model::Language;

fn main() {
    let path = std::env::args().nth(1).expect("usage: facts <path>");
    let src = std::fs::read(&path).expect("read");
    let facts = GoPack::default().extract(&path, Language::Go, &src, &Budget::default()).expect("extract");
    for d in &facts.definitions {
        println!(
            "DEF {:?} {} parent={:?} exp={} test={} sig={:?} doc={:?} bases={:?}",
            d.label, d.qualified_name, d.parent, d.exported, d.is_test, d.signature, d.doc, d.bases
        );
    }
    for r in &facts.references {
        println!(
            "REF {:?} {} recv={:?} enc={:?} arity={:?} line={}",
            r.kind,
            r.name,
            r.receiver,
            r.enclosing.map(|e| facts.definitions[e as usize].qualified_name.as_str()),
            r.arity,
            r.span.start_line + 1
        );
    }
    for i in &facts.imports {
        println!("IMP {} names={:?} alias={:?}", i.module, i.names, i.module_alias);
    }
    println!("errors={} lines={}", facts.parse_errors, facts.line_count);
}
