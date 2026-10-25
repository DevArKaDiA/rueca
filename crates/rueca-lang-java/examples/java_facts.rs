//! Prints the facts of one file: `cargo run -p rueca-lang-java --example java_facts -- <path>`.
use rueca_extract::{Budget, LanguagePack};
use rueca_lang_java::JavaPack;
use rueca_model::Language;

fn main() {
    let path = std::env::args().nth(1).expect("usage: java_facts <path>");
    let src = std::fs::read(&path).expect("read");
    let facts = JavaPack::default().extract(&path, Language::Java, &src, &Budget::default()).expect("extract");
    for d in &facts.definitions {
        println!(
            "DEF {:?} {} parent={:?} exp={} test={} sig={:?} doc={:?} dec={:?} bases={:?} impl={:?}",
            d.label,
            d.qualified_name,
            d.parent,
            d.exported,
            d.is_test,
            d.signature,
            d.doc,
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
