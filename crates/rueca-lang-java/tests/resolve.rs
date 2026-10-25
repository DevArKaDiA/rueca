//! End-to-end Java resolution with the real `JavaPack` over the
//! multi-module fixture in `tests/fixtures/maven`: a Maven parent with
//! `core` and `app` modules, a Gradle module (`tools`) with a declared
//! source directory, interfaces with default methods, a record, an enum
//! with constant bodies, overloads and overrides, nested / inner classes,
//! static and on-demand imports, JUnit 5 tests and Spring-style
//! annotations.

mod common;

use common::*;
use rueca_model::{EdgeKind, Label, Strategy};

const CORE: &str = "com.acme.core";
const APP: &str = "com.acme.app";

fn c(local: &str) -> String {
    format!("{CORE}.{local}")
}
fn a(local: &str) -> String {
    format!("{APP}.{local}")
}

#[test]
fn modules_are_packages() {
    let out = fixture();
    let g = G(&out.graph);
    assert_eq!(g.module_of("core/src/main/java/com/acme/core/Parser.java"), CORE);
    // Test sources share the package of the main sources.
    assert_eq!(g.module_of("core/src/test/java/com/acme/core/ParserTest.java"), CORE);
    assert_eq!(g.module_of("app/src/main/java/com/acme/app/web/OwnerController.java"), "com.acme.app.web");
    // A Gradle source directory outside the conventions.
    assert_eq!(g.module_of("tools/source/org/acme/tools/Cli.java"), "org.acme.tools");
    // Build files are not Java.
    assert_eq!(g.module_of("pom.xml"), "");
    // Qualified names: package + local name.
    g.id(&c("Parser.Options.withLimit"));
    g.id(&c("Parser.Cursor"));
    g.id(&c("ParserHelper.create"));
}

#[test]
fn imports_files_folders_and_external() {
    let out = fixture();
    let g = G(&out.graph);
    let app = "app/src/main/java/com/acme/app/App.java";
    // Single-type and static imports → the class file; on demand → the package folder.
    g.has(app, "core/src/main/java/com/acme/core/Parser.java", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has(app, "core/src/main/java/com/acme/core/Util.java", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    g.has(app, "core/src/main/java/com/acme/core", EdgeKind::Imports, Strategy::ImportResolved, 1.0);
    // Across build systems: the Gradle module imports from the Maven one.
    g.has(
        "tools/source/org/acme/tools/Cli.java",
        "core/src/main/java/com/acme/core/Parser.java",
        EdgeKind::Imports,
        Strategy::ImportResolved,
        1.0,
    );
    // JDK and third-party imports have no edge (external).
    assert!(out.stats.references_external > 0);
    let imports_to_files = out.graph.edges.iter().filter(|e| e.kind == EdgeKind::Imports).count();
    assert_eq!(imports_to_files, 6);
}

#[test]
fn package_scope_on_demand_and_static_imports() {
    let out = fixture();
    let g = G(&out.graph);
    let start = a("App.start");
    // Same package without imports (another file of com.acme.core).
    g.has(&c("ParserHelper.create"), &c("Parser"), EdgeKind::Calls, Strategy::SameModule, 0.9);
    g.has(&c("Parser.parse(String)"), &c("Shape"), EdgeKind::References, Strategy::SameModule, 0.9);
    // On-demand import binds the package's public classes.
    g.has(&start, &c("Circle"), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.has(&start, &c("Color.parse"), EdgeKind::Calls, Strategy::ImportResolved, 0.85);
    g.has(&start, &c("Color.RED"), EdgeKind::References, Strategy::ImportResolved, 0.85);
    // Single static import of an overloaded method: both arities match → fan-out × 0.8.
    g.has(&start, &c("Util.max(int,int)"), EdgeKind::Calls, Strategy::ImportResolved, 0.95 * 0.8);
    g.has(&start, &c("Util.max(long,long)"), EdgeKind::Calls, Strategy::ImportResolved, 0.95 * 0.8);
    // Static on-demand import (`import static com.acme.core.Util.*`).
    g.has(&start, &c("Util.join"), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    // A nested class imported by name.
    g.has(&a("App.parser"), &c("Parser.Options"), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
}

#[test]
fn constructors_overloads_and_typed_receivers() {
    let out = fixture();
    let g = G(&out.graph);
    let start = a("App.start");
    // `new Circle(2.0)` → the class and the one-argument constructor.
    g.has(&start, &c("Circle"), EdgeKind::Calls, Strategy::ImportResolved, 0.95);
    g.has(&start, &c("Circle.Circle(double)"), EdgeKind::Calls, Strategy::ImportResolved, 0.95 * 0.95);
    g.lacks(&start, &c("Circle.Circle(double,String)"), EdgeKind::Calls);
    // `this(…)` / `super(…)`-style constructor chaining, chosen by arity.
    g.has(&c("Parser.Parser()"), &c("Parser.Parser(Options)"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    g.has(
        &c("Circle.Circle(double)"),
        &c("Circle.Circle(double,String)"),
        EdgeKind::Calls,
        Strategy::TypeResolved,
        0.9,
    );
    // Implicit this + overload by arity.
    g.has(&c("Parser.parse(String)"), &c("Parser.parse(String,int)"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    g.lacks(&c("Parser.parse(String)"), &c("Parser.parse(String)"), EdgeKind::Calls);
    // Typed receivers: a field (`parser.run()`), `this.field`, a local, `var x = new T()`.
    g.has(&start, &c("Parser.run"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has(&start, &c("Parser.parse(String)"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has(&start, &c("Shape.describe"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has(&start, &c("Registry.register"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Pattern variable (`o instanceof Circle circle`).
    g.has(&start, &c("Circle.area"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // `new TextParser().parse("x")`: the receiver is typed by the `new`.
    g.has(&start, &c("TextParser.parse"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Enhanced-for variable of the enclosing type itself (`code` is both a
    // field and a method of `Color`).
    let src = g.id(&c("Color.parse"));
    let call = out.graph.edges.iter().find(|e| {
        let d = &out.graph.nodes[e.dst as usize];
        e.src == src && e.kind == EdgeKind::Calls && d.qualified_name == c("Color.code")
    });
    let call = call.expect("Color.parse -> Color.code()");
    assert_eq!(out.graph.nodes[call.dst as usize].label, Label::Method);
    assert_eq!((call.meta.strategy, call.meta.confidence), (Strategy::TypeResolved, 0.85));
    // Inner class: implicit this reaches the outer class's inherited method.
    g.has(&c("Parser.Cursor.advance"), &c("Base.log"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Method reference `App::print`.
    g.has(&start, &a("App.print"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Enum constants call the enum's constructor.
    g.has(&c("Color.RED"), &c("Color.Color"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
}

#[test]
fn inheritance_overrides_and_super() {
    let out = fixture();
    let g = G(&out.graph);
    g.has(&c("Parser"), &c("Base"), EdgeKind::Inherits, Strategy::SameModule, 0.9);
    g.has(&c("TextParser"), &c("Parser"), EdgeKind::Inherits, Strategy::SameModule, 0.9);
    g.has(&c("Circle"), &c("Shape"), EdgeKind::Implements, Strategy::SameModule, 0.9);
    g.has(&c("Parser.run"), &c("Base.run"), EdgeKind::Overrides, Strategy::TypeResolved, 0.9);
    g.has(&c("Circle.area"), &c("Shape.area"), EdgeKind::Overrides, Strategy::TypeResolved, 0.9);
    g.has(&c("TextParser.parse"), &c("Parser.parse(String)"), EdgeKind::Overrides, Strategy::TypeResolved, 0.9);
    // `super.run()` and `super.parse(…)`.
    g.has(&c("Parser.run"), &c("Base.run"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    g.has(&c("TextParser.parse"), &c("Parser.parse(String)"), EdgeKind::Calls, Strategy::TypeResolved, 0.85);
    // Default methods call abstract interface methods through implicit this.
    g.has(&c("Shape.describe"), &c("Shape.name"), EdgeKind::Calls, Strategy::TypeResolved, 0.9);
    // A type whose name is an enclosing class never reaches a constructor.
    let ctor_refs = out.graph.edges.iter().filter(|e| {
        e.kind == EdgeKind::References && out.graph.nodes[e.dst as usize].label == Label::Method && {
            let n = &out.graph.nodes[e.dst as usize];
            n.qualified_name.ends_with(&format!(".{}", n.name))
                && n.qualified_name.trim_end_matches(&format!(".{}", n.name)).ends_with(&n.name)
        }
    });
    assert_eq!(ctor_refs.count(), 0, "REFERENCES edges to constructors");
}

#[test]
fn fields_tests_decorators_and_bridge() {
    let out = fixture();
    let g = G(&out.graph);
    // Field reads and writes.
    g.has(&c("Parser.Parser(Options)"), &c("Parser.options"), EdgeKind::Writes, Strategy::TypeResolved, 0.9);
    g.has(&c("Parser.run"), &c("Parser.DEFAULT"), EdgeKind::References, Strategy::TypeResolved, 0.9);
    g.has(&c("Base.run"), &c("Base.runs"), EdgeKind::References, Strategy::TypeResolved, 0.9);
    // Tests: JUnit 5 test and fixture methods, @Nested classes.
    let test = |qn: &str| out.graph.nodes[g.id(qn) as usize].is_test;
    assert!(test(&c("ParserTest")) && test(&c("ParserTest.parsesDefault")) && test(&c("ParserTest.setUp")));
    assert!(test(&c("ParserTest.Options.limit")) && !test(&c("ParserTest.helper")));
    g.has(&c("ParserTest.parsesMany"), &c("Parser.parse(String,int)"), EdgeKind::Tests, Strategy::TypeResolved, 0.85);
    // A project annotation decorates the field it annotates.
    g.has(&c("Inject"), "com.acme.app.web.OwnerController.parser", EdgeKind::Decorates, Strategy::ImportResolved, 0.95);
    // System.getenv → the canonical env var.
    g.has("com.acme.app.web.OwnerController.owners", "$env:OWNER_HOME", EdgeKind::Reads, Strategy::Contract, 0.95);
}

#[test]
fn resolution_is_deterministic_under_shuffled_inputs() {
    let root = fixture_root();
    let mut files = Vec::new();
    walk(&root, &root, &mut files);
    let baseline = build(files.clone()).graph;
    let mut seed = 0x9e3779b97f4a7c15u64;
    for _ in 0..4 {
        // Fisher–Yates with a xorshift generator.
        for i in (1..files.len()).rev() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            files.swap(i, (seed % (i as u64 + 1)) as usize);
        }
        let g = build(files.clone()).graph;
        assert_eq!(g.nodes, baseline.nodes);
        assert_eq!(g.edges, baseline.edges);
        assert_eq!(g.modules, baseline.modules);
    }
}

#[test]
#[ignore = "prints the fixture graph"]
fn dump_fixture_graph() {
    let out = fixture();
    for l in G(&out.graph).dump() {
        println!("{l}");
    }
    println!("{:?}", out.stats);
}
