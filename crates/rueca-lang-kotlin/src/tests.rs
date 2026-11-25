//! Extraction tests on small Kotlin snippets.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{Definition, FileFacts, Label, Language, RefKind, Reference};

use crate::KotlinPack;

fn facts(src: &str) -> FileFacts {
    KotlinPack.extract("A.kt", Language::Kotlin, src.as_bytes(), &Budget::default()).expect("extract")
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| {
        let all: Vec<&str> = f.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
        panic!("no definition {qn}; have {all:?}")
    })
}

/// References named `name` (argument types stripped) of `kind`.
fn refs<'f>(f: &'f FileFacts, kind: RefKind, name: &str) -> Vec<&'f Reference> {
    f.references.iter().filter(|r| r.kind == kind && rueca_extract::split_arg_types(&r.name).0 == name).collect()
}

/// The receiver of the only `kind` reference named `name`.
fn receiver(f: &FileFacts, kind: RefKind, name: &str) -> Option<String> {
    let rs = refs(f, kind, name);
    assert_eq!(rs.len(), 1, "one {kind:?} {name}: {rs:?}");
    rs[0].receiver.clone()
}

fn enclosing_qn(f: &FileFacts, r: &Reference) -> Option<String> {
    r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone())
}

#[test]
fn classes_objects_and_members() {
    let f = facts(
        r#"package com.acme

/** An order. */
data class Order(val id: Long, var total: Int = 0) : Entity(), Comparable<Order> {
    constructor(id: Long) : this(id, 0)
    val label: String get() = "x"
    override fun compareTo(other: Order): Int = 0
    companion object Factory {
        const val MAX = 10
        fun create(): Order = Order(1L)
    }
    inner class Line { fun f() = 1 }
}
interface Shape
fun interface Pred { fun test(x: Int): Boolean }
enum class Color(val rgb: Int) { RED(1), GREEN(2) }
object Registry {
    fun register() {}
}
annotation class Marker
typealias Handler = (Int) -> Unit
class Plain
"#,
    );
    assert_eq!(f.parse_errors, 0);
    let order = def(&f, "Order");
    assert_eq!(order.label, Label::Class);
    assert_eq!(order.bases, vec!["Entity"]);
    assert_eq!(order.implements, vec!["Comparable"]);
    assert_eq!(order.doc.as_deref(), Some("An order."));
    // Overloaded constructors carry their parameter types.
    assert_eq!(def(&f, "Order.Order(Long,Int)").label, Label::Method);
    assert_eq!(def(&f, "Order.Order(Long)").label, Label::Method);
    assert_eq!(def(&f, "Order.id").label, Label::Field);
    assert_eq!(def(&f, "Order.label").label, Label::Field);
    assert_eq!(def(&f, "Order.Factory").label, Label::Class);
    assert!(def(&f, "Order.Factory").signature.as_deref().unwrap().starts_with("companion object"));
    assert_eq!(def(&f, "Order.Factory.MAX").label, Label::Constant);
    assert_eq!(def(&f, "Order.Factory.create").label, Label::Method);
    assert_eq!(def(&f, "Order.Line.f").label, Label::Method);
    assert_eq!(def(&f, "Shape").label, Label::Interface);
    assert_eq!(def(&f, "Pred").label, Label::Interface);
    assert_eq!(def(&f, "Color").label, Label::Enum);
    assert_eq!(def(&f, "Color.RED").label, Label::Constant);
    assert_eq!(def(&f, "Registry").label, Label::Class);
    assert_eq!(def(&f, "Marker").label, Label::Interface);
    assert_eq!(def(&f, "Handler").label, Label::TypeAlias);
    // Implicit primary constructors; none for interfaces and objects.
    assert_eq!(def(&f, "Plain.Plain").signature.as_deref(), Some("constructor()"));
    assert!(f.definitions.iter().all(|d| d.qualified_name != "Shape.Shape" && d.qualified_name != "Registry.Registry"));
}

#[test]
fn functions_extensions_and_overloads() {
    let f = facts(
        r#"package com.acme
fun String.shout(): String = uppercase()
fun Int.shout(): Int = this
fun total(vararg xs: Int): Int = 0
fun total(a: Int, b: Int): Int = a + b
fun main() {
    fun local() = 1
    local()
}
val Order.double: Int get() = 2
"#,
    );
    // Extensions are named after their receiver; same-named top-level
    // functions get parameter suffixes.
    assert_eq!(def(&f, "String.shout").label, Label::Function);
    assert_eq!(def(&f, "Int.shout").label, Label::Function);
    assert!(def(&f, "total(Int...)").signature.as_deref().unwrap().contains("vararg"));
    def(&f, "total(Int,Int)");
    assert_eq!(def(&f, "main.local").label, Label::Function);
    assert_eq!(def(&f, "double").label, Label::Variable);
}

#[test]
fn imports_and_package() {
    let f = facts(
        "package com.acme.app\nimport com.acme.core.Repo\nimport com.acme.core.Repo.Inner\nimport com.acme.util.*\nimport java.util.List as JList\nimport kotlin.math.max\n",
    );
    assert_eq!(crate::declared_package(&f), Some("com.acme.app"));
    let imp: Vec<(String, String, Option<String>)> = f
        .imports
        .iter()
        .skip(1)
        .map(|i| (i.module.clone(), i.names[0].name.clone(), i.names[0].alias.clone()))
        .collect();
    assert_eq!(
        imp,
        vec![
            ("com.acme.core".into(), "Repo".into(), None),
            ("com.acme.core".into(), "Repo.Inner".into(), None),
            ("com.acme.util".into(), "*".into(), None),
            ("java.util".into(), "List".into(), Some("JList".into())),
            ("kotlin.math".into(), "max".into(), None),
        ]
    );
}

#[test]
fn receivers() {
    let f = facts(
        r#"package app
class Repo { fun save() {} }
fun makeRepo(): Repo = Repo()
class Svc(private val repo: Repo, private val other: Other) {
    private val lazyRepo = Repo()
    fun run(x: Any, s: String) {
        val a = Repo()
        a.save()
        val b = makeRepo()
        b.find()
        repo.load()
        other.ping()
        lazyRepo.flush()
        "x".trim2()
        Repo().make()
        makeRepo().chain()
        Factory.create().build()
        when (x) { is Repo -> x.drop() else -> {} }
        if (x is Svc) x.stop()
        Repo().apply { reset() }
        repo.let { it.touch() }
        this.halt()
        super.toString()
    }
}
"#,
    );
    let r = |n: &str| receiver(&f, RefKind::Call, n);
    assert_eq!(r("save").as_deref(), Some("$type:Repo"));
    assert_eq!(r("find").as_deref(), Some("$type:Repo"));
    assert_eq!(r("load").as_deref(), Some("$type:Repo"));
    assert_eq!(r("ping").as_deref(), Some("$type:Other"));
    assert_eq!(r("flush").as_deref(), Some("$type:Repo"));
    assert_eq!(r("trim2").as_deref(), Some("$type:String"));
    assert_eq!(r("make").as_deref(), Some("$type:Repo"));
    assert_eq!(r("chain").as_deref(), Some("$type:Repo"));
    assert_eq!(r("build").as_deref(), Some("$ret:Factory#create"));
    assert_eq!(r("drop").as_deref(), Some("$type:Repo"));
    assert_eq!(r("stop").as_deref(), Some("$type:Svc"));
    assert_eq!(r("reset").as_deref(), Some("$with:Repo"));
    assert_eq!(r("touch").as_deref(), Some("$type:Repo"));
    assert_eq!(r("halt").as_deref(), Some("this"));
    assert_eq!(r("toString").as_deref(), Some("super"));
    // A class called like a function is a call of its name.
    assert!(refs(&f, RefKind::Call, "Repo").iter().all(|c| c.receiver.is_none()));
}

#[test]
fn constructors_headers_and_init_blocks() {
    let f = facts(
        r#"package app
fun boot() {}
fun size() = 8
open class Base(val n: Int)
class Child(val m: Int = size()) : Base(m) {
    init { boot() }
    constructor() : this(1)
}
"#,
    );
    // Calls in the class header belong to the primary constructor.
    let base = refs(&f, RefKind::Call, "Base");
    assert_eq!(base.len(), 1);
    assert_eq!(enclosing_qn(&f, base[0]).as_deref(), Some("Child.Child(Int)"));
    assert_eq!(enclosing_qn(&f, refs(&f, RefKind::Call, "size")[0]).as_deref(), Some("Child.Child(Int)"));
    // `init` belongs to the class.
    assert_eq!(enclosing_qn(&f, refs(&f, RefKind::Call, "boot")[0]).as_deref(), Some("Child"));
    // `: this(1)` calls the class's constructor.
    let this_call: Vec<&Reference> = refs(&f, RefKind::Call, "Child");
    assert_eq!(this_call.len(), 1);
    assert_eq!(this_call[0].receiver.as_deref(), Some("this"));
    assert_eq!(this_call[0].arity, Some(1));
}

#[test]
fn locals_and_callable_references() {
    let f = facts(
        r#"package app
fun f(g: (Int) -> Int) {
    g(1)
    val h = { x: Int -> x }
    h(2)
    listOf(1).map(::twice)
}
fun twice(x: Int) = x * 2
"#,
    );
    // Function values held by locals are not calls; `::twice` is a read.
    assert!(refs(&f, RefKind::Call, "g").is_empty());
    assert!(refs(&f, RefKind::Call, "h").is_empty());
    assert!(refs(&f, RefKind::Call, "twice").is_empty());
    assert_eq!(refs(&f, RefKind::Read, "twice").len(), 1);
}

#[test]
fn argument_types() {
    let f = facts("fun f(s: String) { g(\"a\", 1, 2L, s, Foo::class, x.toString(), *arr) { } }\n");
    let g = &refs(&f, RefKind::Call, "g")[0];
    assert_eq!(g.name, "g$args:String,Int,Long,String,KClass,String,*,Function");
    assert_eq!(g.arity, Some(8));
}

#[test]
fn tests_are_detected() {
    let f = facts(
        r#"import org.junit.jupiter.api.Test
import org.junit.jupiter.api.BeforeEach
class OrderTest {
    @BeforeEach fun setUp() {}
    @Test fun works() {}
    fun helper() {}
}
class Calc { @Test fun adds() {} }
class OrderSpec : StringSpec({ "adds" { } })
class Prod { fun run() {} }
"#,
    );
    assert!(def(&f, "OrderTest").is_test);
    assert!(def(&f, "OrderTest.works").is_test);
    assert!(def(&f, "OrderTest.setUp").is_test);
    assert!(!def(&f, "OrderTest.helper").is_test);
    assert!(def(&f, "Calc").is_test, "a class declaring a test");
    assert!(def(&f, "OrderSpec").is_test, "a Kotest spec");
    assert!(!def(&f, "Prod").is_test);
    assert!(!def(&f, "Prod.run").is_test);
}

#[test]
fn config_bridge() {
    let f = facts(
        r#"import org.springframework.beans.factory.annotation.Value
import org.springframework.core.env.Environment
class Cfg(@Value("\${app.port:8080}") val port: Int, private val env: Environment) {
    @Value("\${app.name}") lateinit var name: String
    fun read() {
        System.getenv("HOME")
        env.getProperty("db.url")
        System.getenv(dynamic)
    }
}
"#,
    );
    let env: Vec<&str> = refs(&f, RefKind::EnvVar, "HOME").iter().map(|r| r.name.as_str()).collect();
    assert_eq!(env, vec!["HOME"]);
    let keys: Vec<(String, Option<String>)> = f
        .references
        .iter()
        .filter(|r| r.kind == RefKind::ConfigKey)
        .map(|r| (r.name.clone(), enclosing_qn(&f, r)))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("app.port".into(), Some("Cfg.port".into())),
            ("app.name".into(), Some("Cfg.name".into())),
            ("db.url".into(), Some("Cfg.read".into())),
        ]
    );
    // A non-literal argument stays a call.
    assert_eq!(refs(&f, RefKind::Call, "getenv").len(), 1);
}

#[test]
fn spring_value_needs_the_import() {
    let f = facts("class Cfg { @Value(\"\\${a.b}\") val x: String = \"\" }\n");
    assert!(f.references.iter().all(|r| r.kind != RefKind::ConfigKey));
}

#[test]
fn anonymous_objects() {
    let f = facts("fun f() {\n  val r = object : Runnable { override fun run() { go() } }\n}\nfun go() {}\n");
    let anon = def(&f, "f.Runnable$1");
    assert_eq!(anon.implements, vec!["Runnable"]);
    assert_eq!(def(&f, "f.Runnable$1.run").label, Label::Method);
    assert_eq!(enclosing_qn(&f, refs(&f, RefKind::Call, "go")[0]).as_deref(), Some("f.Runnable$1.run"));
}

#[test]
fn kts_scripts_parse() {
    let f = facts("plugins { kotlin(\"jvm\") }\ndependencies { implementation(\"com.squareup.okio:okio:3.9.0\") }\n");
    assert_eq!(f.parse_errors, 0);
    assert_eq!(refs(&f, RefKind::Call, "implementation").len(), 1);
}
