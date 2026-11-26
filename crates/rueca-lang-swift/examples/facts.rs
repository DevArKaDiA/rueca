//! Prints the facts of one file: `cargo run -p rueca-lang-swift --example facts -- <path>`.
use rueca_extract::{Budget, LanguagePack};
use rueca_lang_swift::SwiftPack;
use rueca_model::Language;

fn main() {
    let path = std::env::args().nth(1).expect("usage: facts <path>");
    let src = std::fs::read(&path).expect("read");
    let facts = SwiftPack.extract(&path, Language::Swift, &src, &Budget::default()).expect("extract");
    for d in &facts.definitions {
        println!(
            "DEF {:?} {} parent={:?} exp={} test={} line={} sig={:?} dec={:?} bases={:?} impl={:?}",
            d.label,
            d.qualified_name,
            d.parent,
            d.exported,
            d.is_test,
            d.name_span.start_line + 1,
            d.signature,
            d.decorators,
            d.bases,
            d.implements
        );
    }
    for r in &facts.references {
        println!(
            "REF {:?} {} recv={:?} enc={:?} arity={:?} line={}",
            r.kind,
            r.name,
            r.receiver,
            r.enclosing,
            r.arity,
            r.span.start_line + 1
        );
    }
    for i in &facts.imports {
        println!("IMP {} names={:?}", i.module, i.names.iter().map(|n| n.name.as_str()).collect::<Vec<_>>());
    }
    println!("errors={} lines={}", facts.parse_errors, facts.line_count);
}
