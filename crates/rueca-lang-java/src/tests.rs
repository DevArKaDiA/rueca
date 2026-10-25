//! Extraction tests, one group per row of the construct tables in the crate
//! docs.

use super::*;
use rueca_extract::split_arg_types;
use rueca_model::{Definition, Label, RefKind, Reference};

/// A reference's name without its argument types.
fn bare(r: &Reference) -> &str {
    split_arg_types(&r.name).0
}

fn facts(src: &str) -> FileFacts {
    JavaPack::default().extract("A.java", Language::Java, src.as_bytes(), &Budget::default()).expect("extract")
}

#[track_caller]
fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    let v: Vec<&Definition> = f.definitions.iter().filter(|d| d.qualified_name == qn).collect();
    assert_eq!(v.len(), 1, "expected one definition {qn}; have {:?}", qns(f));
    v[0]
}

fn qns(f: &FileFacts) -> Vec<String> {
    f.definitions.iter().map(|d| format!("{:?} {}", d.label, d.qualified_name)).collect()
}

fn refs<'f>(f: &'f FileFacts, kind: RefKind, name: &str) -> Vec<&'f Reference> {
    f.references.iter().filter(|r| r.kind == kind && bare(r) == name).collect()
}

/// (kind, receiver, name) of every reference, for compact assertions.
fn ref_triples(f: &FileFacts) -> Vec<(RefKind, Option<String>, String)> {
    f.references.iter().map(|r| (r.kind, r.receiver.clone(), r.name.clone())).collect()
}

#[track_caller]
fn has_ref(f: &FileFacts, kind: RefKind, receiver: Option<&str>, name: &str) {
    let found = f.references.iter().any(|r| r.kind == kind && bare(r) == name && r.receiver.as_deref() == receiver);
    assert!(found, "missing {kind:?} {receiver:?} {name}; have {:#?}", ref_triples(f));
}

#[track_caller]
fn lacks_ref(f: &FileFacts, kind: RefKind, name: &str) {
    let found: Vec<_> = refs(f, kind, name);
    assert!(found.is_empty(), "unexpected {kind:?} {name}: {found:#?}");
}

fn enclosing_qn<'f>(f: &'f FileFacts, r: &Reference) -> Option<&'f str> {
    r.enclosing.map(|e| f.definitions[e as usize].qualified_name.as_str())
}

// ---------------------------------------------------------------------------
// Types, members, qualified names
// ---------------------------------------------------------------------------

#[test]
fn classes_nested_and_inner() {
    let f = facts(
        r#"package com.acme;

/** The outer class. */
@Deprecated
public final class Outer<T> extends Base implements Runnable, java.io.Serializable {
    static class Nested {
        void run() {}
    }
    class Inner {
        class Deeper {}
    }
    private class Hidden {}
    protected interface Callback {}
    public void run() {
        class Local {
            void go() {}
        }
    }
}

class PackagePrivate {}
"#,
    );
    let outer = def(&f, "Outer");
    assert_eq!(outer.label, Label::Class);
    assert!(outer.exported);
    assert_eq!(outer.doc.as_deref(), Some("The outer class."));
    assert_eq!(outer.decorators, vec!["Deprecated"]);
    assert_eq!(outer.bases, vec!["Base"]);
    // The fully qualified interface is bound by a synthetic import and
    // written without its package.
    assert_eq!(outer.implements, vec!["Runnable", "Serializable"]);
    assert!(f.imports.iter().any(|i| i.module == "java.io" && i.names[0].name == "Serializable"));
    assert_eq!(
        outer.signature.as_deref(),
        Some("public final class Outer<T> extends Base implements Runnable, java.io.Serializable")
    );
    assert_eq!(def(&f, "Outer.Nested").parent, Some(0));
    assert!(!def(&f, "Outer.Nested").exported);
    def(&f, "Outer.Nested.run");
    def(&f, "Outer.Inner.Deeper");
    assert!(!def(&f, "Outer.Hidden").exported);
    assert!(def(&f, "Outer.Callback").exported);
    assert_eq!(def(&f, "Outer.Callback").label, Label::Interface);
    // Local classes are children of their method.
    let local = def(&f, "Outer.run.Local");
    assert_eq!(f.definitions[local.parent.unwrap() as usize].qualified_name, "Outer.run");
    def(&f, "Outer.run.Local.go");
    assert!(!def(&f, "PackagePrivate").exported);
    // Spans cover the declaration, names are inside.
    for d in &f.definitions {
        assert!(d.span.contains(&d.name_span), "{}", d.qualified_name);
    }
    // The package clause is the first import, without names.
    assert_eq!(declared_package(&f), Some("com.acme"));
    assert!(f.imports[0].names.is_empty());
}

#[test]
fn interfaces_default_methods_and_annotation_types() {
    let f = facts(
        r#"interface Shape extends Comparable<Shape>, Named {
    double PI_ISH = 3.14;
    double area();
    default String describe() { return name() + area(); }
    static Shape unit() { return null; }
    private void helper() {}
}

@interface Marker {
    String value() default "";
    int[] codes() default {};
}
"#,
    );
    let shape = def(&f, "Shape");
    assert_eq!(shape.label, Label::Interface);
    assert_eq!(shape.bases, vec!["Comparable", "Named"]);
    assert!(shape.implements.is_empty());
    assert_eq!(def(&f, "Shape.PI_ISH").label, Label::Constant);
    // Interface members are implicitly public, except private ones.
    assert!(def(&f, "Shape.area").exported);
    assert!(def(&f, "Shape.describe").exported);
    assert!(def(&f, "Shape.unit").exported);
    assert!(!def(&f, "Shape.helper").exported);
    assert_eq!(def(&f, "Shape.describe").signature.as_deref(), Some("default String describe()"));
    // Implicit-this calls in a default method.
    has_ref(&f, RefKind::Call, None, "name");
    has_ref(&f, RefKind::Call, None, "area");
    assert_eq!(def(&f, "Marker").label, Label::Interface);
    assert_eq!(def(&f, "Marker.value").label, Label::Method);
    def(&f, "Marker.codes");
}

#[test]
fn enums_constants_and_bodies() {
    let f = facts(
        r#"public enum Color implements Coded {
    /** Red. */
    RED("r"),
    GREEN("g"),
    BLUE("b") {
        @Override public String code() { return helper(); }
        private String helper() { return "blue"; }
    },
    PLAIN;

    private final String code;
    Color(String code) { this.code = code; }
    Color() { this("?"); }
    public String code() { return code; }
}
"#,
    );
    let color = def(&f, "Color");
    assert_eq!(color.label, Label::Enum);
    assert_eq!(color.implements, vec!["Coded"]);
    let red = def(&f, "Color.RED");
    assert_eq!(red.label, Label::Constant);
    assert!(red.exported);
    assert_eq!(red.doc.as_deref(), Some("Red."));
    def(&f, "Color.PLAIN");
    // Constant bodies are anonymous subclasses of the enum: a class
    // `Color$1` under the constant, whose methods are definitions.
    let body = def(&f, "Color.BLUE.Color$1");
    assert_eq!(body.label, Label::Class);
    assert_eq!(body.bases, vec!["Color"]);
    assert_eq!(def(&f, "Color.BLUE.Color$1.helper").label, Label::Method);
    def(&f, "Color.BLUE.Color$1.code");
    // Constants with arguments call the enum's constructor.
    let ctor_calls = refs(&f, RefKind::Call, "Color");
    assert!(ctor_calls.iter().any(|r| r.receiver.as_deref() == Some("this") && r.arity == Some(1)));
    assert!(ctor_calls.iter().any(|r| enclosing_qn(&f, r) == Some("Color.RED")));
    // Constructors are overloaded (two): suffixes; `code` the field and
    // `code()` the method share a name but not a scope of callables.
    def(&f, "Color.Color(String)");
    def(&f, "Color.Color()");
    assert_eq!(f.definitions.iter().filter(|d| d.qualified_name == "Color.code").count(), 2);
    // Code in the constant body belongs to its methods.
    let helper = refs(&f, RefKind::Call, "helper");
    assert_eq!(enclosing_qn(&f, helper[0]), Some("Color.BLUE.Color$1.code"));
}

#[test]
fn records_components_and_compact_constructors() {
    let f = facts(
        r#"public record Point(int x, int y, Label label) implements Shape {
    public Point {
        if (x < 0) throw new IllegalArgumentException();
    }
    public Point(int x) { this(x, 0, null); }
    public double dist() { return Math.sqrt(x * x + y * y); }
    void show() { label.render(); }
}
"#,
    );
    assert_eq!(def(&f, "Point").label, Label::Class);
    for c in ["Point.x", "Point.y", "Point.label"] {
        let d = def(&f, c);
        assert_eq!(d.label, Label::Field);
        assert!(d.exported);
    }
    assert_eq!(def(&f, "Point.label").signature.as_deref(), Some("Label label"));
    // The compact constructor takes the components' types.
    def(&f, "Point.Point(int,int,Label)");
    def(&f, "Point.Point(int)");
    has_ref(&f, RefKind::Call, Some("this"), "Point");
    // Components type receivers like fields.
    has_ref(&f, RefKind::Call, Some("$type:Label"), "render");
    has_ref(&f, RefKind::Type, None, "Label");
}

#[test]
fn overload_suffixes() {
    let f = facts(
        r#"class Parser {
    Parser() {}
    Parser(java.util.Map<String, int[]> options, Object... rest) {}
    Object parse(String s) { return null; }
    Object parse(final @NonNull String s, int limit) { return null; }
    Object parse(char[] chars, int[] offsets[]) { return null; }
    void unique(java.util.List<String> xs) {}
}
"#,
    );
    def(&f, "Parser.Parser()");
    def(&f, "Parser.Parser(Map,Object...)");
    def(&f, "Parser.parse(String)");
    def(&f, "Parser.parse(String,int)");
    def(&f, "Parser.parse(char[],int[][])");
    // Not overloaded: bare.
    def(&f, "Parser.unique");
    // Constructors are named like the class.
    assert!(f.definitions.iter().filter(|d| d.name == "Parser").count() == 3);
}

#[test]
fn fields_and_constants() {
    let f = facts(
        r#"class A {
    public static final int MAX_SIZE = 10, MIN_SIZE = 1;
    static final Logger log = Logger.get(A.class);
    private final java.util.List<String> names = new java.util.ArrayList<>();
    protected transient int count;
    int[] codes, more[];
}
"#,
    );
    assert_eq!(def(&f, "A.MAX_SIZE").label, Label::Constant);
    assert_eq!(def(&f, "A.MIN_SIZE").label, Label::Constant);
    assert_eq!(def(&f, "A.MAX_SIZE").signature.as_deref(), Some("public static final int MAX_SIZE"));
    // static final but not UPPER_CASE: a field.
    assert_eq!(def(&f, "A.log").label, Label::Field);
    assert!(!def(&f, "A.names").exported);
    assert!(def(&f, "A.count").exported);
    assert_eq!(def(&f, "A.more").signature.as_deref(), Some("int[][] more"));
    // Initializers belong to their field.
    let get = refs(&f, RefKind::Call, "get");
    assert_eq!(enclosing_qn(&f, get[0]), Some("A.log"));
    // `A.class` inside `A` is a type reference like any other.
    has_ref(&f, RefKind::Type, None, "A");
    has_ref(&f, RefKind::Type, None, "Logger");
}

#[test]
fn generics_type_parameters_are_not_references() {
    let f = facts(
        r#"class Box<T extends Comparable<T>> {
    private T value;
    <R extends Number> R map(java.util.function.Function<? super T, ? extends R> fn, Key<T> key) {
        T local = value;
        local.compareTo(value);
        return fn.apply(local);
    }
}
"#,
    );
    lacks_ref(&f, RefKind::Type, "T");
    lacks_ref(&f, RefKind::Type, "R");
    has_ref(&f, RefKind::Type, None, "Comparable");
    has_ref(&f, RefKind::Type, None, "Number");
    has_ref(&f, RefKind::Type, None, "Function");
    has_ref(&f, RefKind::Type, None, "Key");
    // A receiver typed by a type parameter stays untyped.
    has_ref(&f, RefKind::Call, Some("local"), "compareTo");
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[test]
fn calls_receivers_and_arity() {
    let f = facts(
        r#"package app;
import app.model.Repo;
class Service extends BaseService {
    private Repo repo;
    private final Cache cache = new Cache();
    void handle(Request req, String... args) {
        req.validate();
        repo.save(req, 1);
        this.repo.flush();
        this.audit();
        audit();
        super.handle(req);
        var builder = new Builder();
        builder.build();
        new Builder().reset();
        ((Repo) lookup()).close();
        Util.trim("x");
        Outer.Inner.make();
        java.util.Objects.hash(1, 2, 3);
        "abc".length();
        lookup().chain();
        cache.get("k");
        BaseService other = null;
        other.handle(req);
        Service self = this;
        self.audit();
    }
    void audit() {}
    Object lookup() { return null; }
}
"#,
    );
    has_ref(&f, RefKind::Call, Some("$type:Request"), "validate");
    has_ref(&f, RefKind::Call, Some("$type:Repo"), "save");
    assert_eq!(refs(&f, RefKind::Call, "save")[0].arity, Some(2));
    has_ref(&f, RefKind::Call, Some("$type:Repo"), "flush");
    has_ref(&f, RefKind::Call, Some("this"), "audit");
    has_ref(&f, RefKind::Call, None, "audit");
    has_ref(&f, RefKind::Call, Some("super"), "handle");
    has_ref(&f, RefKind::Call, Some("$type:Builder"), "build");
    has_ref(&f, RefKind::Call, Some("$type:Builder"), "reset");
    has_ref(&f, RefKind::Call, Some("$type:Repo"), "close");
    has_ref(&f, RefKind::Call, Some("Util"), "trim");
    has_ref(&f, RefKind::Call, Some("Outer.Inner"), "make");
    // A fully qualified class receiver is bound by a synthetic import.
    has_ref(&f, RefKind::Call, Some("Objects"), "hash");
    assert_eq!(refs(&f, RefKind::Call, "hash")[0].arity, Some(3));
    has_ref(&f, RefKind::Call, Some("$type:String"), "length");
    // A call on the result of this file's method: its declared return type.
    has_ref(&f, RefKind::Call, Some("$type:Object"), "chain");
    has_ref(&f, RefKind::Call, Some("$type:Cache"), "get");
    // Types are written as in the source, the superclass and this file's
    // own class included (the resolver never takes a type name for a
    // constructor).
    has_ref(&f, RefKind::Call, Some("$type:BaseService"), "handle");
    has_ref(&f, RefKind::Call, Some("$type:Service"), "audit");
    // Reads of fields used as receivers.
    has_ref(&f, RefKind::Read, None, "repo");
    has_ref(&f, RefKind::Read, Some("this"), "repo");
    // Parameters and locals are never references.
    lacks_ref(&f, RefKind::Read, "req");
    lacks_ref(&f, RefKind::Read, "builder");
    // Calls are attributed to the method, with the name's span.
    let v = refs(&f, RefKind::Call, "validate")[0];
    assert_eq!(enclosing_qn(&f, v), Some("Service.handle"));
    assert_eq!(v.span.start_line, 6);
}

#[test]
fn constructors_new_throw_and_types() {
    let f = facts(
        r#"class A extends B {
    A() { super(1); }
    A(int x) throws java.io.IOException, ParseException { this(); }
    Object make(Object o) throws Failure {
        try {
            if (o instanceof Widget w) return w;
            Gadget g = (Gadget) o;
            Object[] arr = new Part[3];
            Class<?> c = Thing.class;
            throw new Failure("x", 2);
        } catch (IOException | Oops e) {
            return new Outer.Nested(o) { };
        }
    }
}
"#,
    );
    has_ref(&f, RefKind::Call, Some("super"), "B");
    has_ref(&f, RefKind::Call, Some("this"), "A");
    assert_eq!(refs(&f, RefKind::Call, "B")[0].arity, Some(1));
    // `throws` clauses and `throw new` are Throw references.
    has_ref(&f, RefKind::Throw, None, "IOException");
    has_ref(&f, RefKind::Throw, None, "ParseException");
    has_ref(&f, RefKind::Throw, None, "Failure");
    let new_failure = refs(&f, RefKind::New, "Failure");
    assert_eq!(new_failure[0].arity, Some(2));
    for t in ["Widget", "Gadget", "Part", "Thing", "Oops"] {
        has_ref(&f, RefKind::Type, None, t);
    }
    // An anonymous class names its base as a type.
    has_ref(&f, RefKind::Type, Some("Outer"), "Nested");
    lacks_ref(&f, RefKind::New, "Nested");
    // An anonymous class is a definition named after its base.
    let anon = def(&f, "A.make.Nested$1");
    assert_eq!(anon.label, Label::Class);
    assert_eq!(anon.bases, vec!["Outer.Nested"]);
}

#[test]
fn annotations_are_decorators_and_parameter_annotations_are_types() {
    let f = facts(
        r#"import org.springframework.web.bind.annotation.GetMapping;
@RestController("x")
class C {
    @Autowired private Service service;
    @GetMapping(value = "/x", produces = MediaType.JSON)
    @com.acme.Audit
    String get(@PathVariable("id") String id, @RequestParam int page) { return id; }
}
"#,
    );
    let get = def(&f, "C.get");
    assert_eq!(get.decorators, vec!["GetMapping(value = \"/x\", produces = MediaType.JSON)", "com.acme.Audit"]);
    has_ref(&f, RefKind::Decorator, None, "RestController");
    has_ref(&f, RefKind::Decorator, None, "Autowired");
    has_ref(&f, RefKind::Decorator, None, "GetMapping");
    // A fully qualified annotation: synthetic import, simple name.
    has_ref(&f, RefKind::Decorator, None, "Audit");
    let d = refs(&f, RefKind::Decorator, "GetMapping")[0];
    assert_eq!(enclosing_qn(&f, d), Some("C.get"));
    // Annotation arguments are visited.
    has_ref(&f, RefKind::Read, Some("MediaType"), "JSON");
    // Parameter annotations are types, not decorators of the method.
    has_ref(&f, RefKind::Type, None, "PathVariable");
    has_ref(&f, RefKind::Type, None, "RequestParam");
    lacks_ref(&f, RefKind::Decorator, "PathVariable");
}

#[test]
fn method_references_lambdas_and_anonymous_classes() {
    let f = facts(
        r#"class A {
    private Helper helper;
    void run(java.util.List<Item> items) {
        items.forEach(Item::check);
        items.stream().map(Item::new).forEach(this::accept);
        items.forEach(helper::take);
        items.forEach(x -> x.toString());
        items.forEach((Item y) -> y.check());
        Runnable r = new Runnable() {
            int count;
            @Override public void run() { this.tick(); inner(); }
            void inner() {}
        };
    }
    void accept(Item i) {}
}
"#,
    );
    has_ref(&f, RefKind::Call, Some("Item"), "check");
    has_ref(&f, RefKind::New, None, "Item");
    has_ref(&f, RefKind::Call, Some("this"), "accept");
    has_ref(&f, RefKind::Call, Some("$type:Helper"), "take");
    // Lambda parameters are locals: untyped, or typed when declared.
    has_ref(&f, RefKind::Call, Some("x"), "toString");
    has_ref(&f, RefKind::Call, Some("$type:Item"), "check");
    // Anonymous classes are classes named after their base (`Runnable$1`,
    // child of the enclosing method): their members are definitions and
    // `this` inside them is the anonymous class.
    let anon = def(&f, "A.run.Runnable$1");
    assert_eq!((anon.label, anon.bases.clone()), (Label::Class, vec!["Runnable".to_string()]));
    assert_eq!(anon.signature.as_deref(), Some("new Runnable()"));
    assert_eq!(def(&f, "A.run.Runnable$1.run").label, Label::Method);
    assert_eq!(def(&f, "A.run.Runnable$1.inner").label, Label::Method);
    assert_eq!(def(&f, "A.run.Runnable$1.count").label, Label::Field);
    let tick = refs(&f, RefKind::Call, "tick")[0];
    assert_eq!(tick.receiver.as_deref(), Some("this"));
    assert_eq!(enclosing_qn(&f, tick), Some("A.run.Runnable$1.run"));
    has_ref(&f, RefKind::Type, None, "Runnable");
}

#[test]
fn reads_and_writes_of_fields_and_constants() {
    let f = facts(
        r#"class Counter {
    static final int LIMIT = 3;
    private int count;
    private Node head;
    void inc(int count) {
        this.count = count;
        total = count + LIMIT;
        Other.TOTAL = 1;
        head.next = null;
        if (count > Config.MAX) { reset(); }
        switch (mode) { case FAST -> {} default -> {} }
    }
}
"#,
    );
    has_ref(&f, RefKind::Write, Some("this"), "count");
    // A parameter shadows the field.
    lacks_ref(&f, RefKind::Read, "count");
    has_ref(&f, RefKind::Read, None, "LIMIT");
    has_ref(&f, RefKind::Read, Some("Config"), "MAX");
    has_ref(&f, RefKind::Read, None, "head");
    has_ref(&f, RefKind::Write, Some("$type:Node"), "next");
    has_ref(&f, RefKind::Read, None, "FAST");
    // Unknown lower-case names (inherited fields, packages) are not reported.
    lacks_ref(&f, RefKind::Write, "total");
    lacks_ref(&f, RefKind::Read, "mode");
}

#[test]
fn imports_single_on_demand_static_and_fully_qualified() {
    let f = facts(
        r#"package com.acme.app;

import java.util.List;
import com.acme.core.*;
import com.acme.core.Parser.Options;
import static com.acme.core.Util.max;
import static org.junit.jupiter.api.Assertions.*;
import static com.acme.core.Strings.*;

class App {
    void run() {
        max(1, 2);
        assertEquals(1, 2);
        trim("x");
        local();
        int v = DEFAULT_SIZE;
    }
    void local() {}
    com.acme.other.Thing thing;
}
"#,
    );
    let imp: Vec<(String, Vec<String>)> =
        f.imports.iter().map(|i| (i.module.clone(), i.names.iter().map(|n| n.name.clone()).collect())).collect();
    let has = |m: &str, n: &str| imp.iter().any(|(mm, nn)| mm == m && nn.iter().any(|x| x == n));
    assert_eq!(imp[0], ("com.acme.app".to_string(), vec![]));
    assert!(has("java.util", "List"));
    assert!(has("com.acme.core", "*"));
    assert!(has("com.acme.core", "Parser.Options"));
    assert!(has("com.acme.core", "Util.max"));
    assert!(has("org.junit.jupiter.api", "Assertions.*"));
    assert!(has("com.acme.core", "Strings.*"));
    // Static on demand: bare names not declared here and not imported by a
    // single static import are bound through every on-demand class.
    for n in ["assertEquals", "trim", "DEFAULT_SIZE"] {
        assert!(has("org.junit.jupiter.api", &format!("Assertions.{n}")), "{n}");
        assert!(has("com.acme.core", &format!("Strings.{n}")), "{n}");
    }
    assert!(!has("com.acme.core", "Strings.local"));
    assert!(!has("com.acme.core", "Strings.max"));
    // A fully qualified type: synthetic import.
    assert!(has("com.acme.other", "Thing"));
    has_ref(&f, RefKind::Type, None, "Thing");
}

#[test]
fn tests_junit_testng_and_naming() {
    let f = facts(
        r#"import org.junit.jupiter.api.*;
class ParserTest {
    @BeforeEach void setUp() {}
    @Test void parses() {}
    @ParameterizedTest @ValueSource(ints = {1}) void many(int x) {}
    void helper() {}
    @Nested class Inner { @Test void deep() {} }
}
class LegacyTest extends junit.framework.TestCase {
    public void testOld() {}
    public void setUp() {}
    public void util() {}
}
@org.testng.annotations.Test
class NgSuite {
    public void runs() {}
    private void hidden() {}
}
class Helpers { @Test void looksLikeATest() {} }
class Plain { void x() {} }
class WidgetIT {}
"#,
    );
    let t = |qn: &str| def(&f, qn).is_test;
    assert!(t("ParserTest") && t("ParserTest.parses") && t("ParserTest.many") && t("ParserTest.setUp"));
    assert!(!t("ParserTest.helper"));
    assert!(t("ParserTest.Inner") && t("ParserTest.Inner.deep"));
    assert!(t("LegacyTest") && t("LegacyTest.testOld") && t("LegacyTest.setUp") && !t("LegacyTest.util"));
    assert!(t("NgSuite") && t("NgSuite.runs") && !t("NgSuite.hidden"));
    // A class with test methods is a test class.
    assert!(t("Helpers") && t("Helpers.looksLikeATest"));
    assert!(!t("Plain") && !t("Plain.x"));
    assert!(t("WidgetIT"));
}

#[test]
fn spring_values_env_vars_and_environment() {
    let f = facts(
        r##"import org.springframework.beans.factory.annotation.Value;
import org.springframework.core.env.Environment;
class Config {
    @Value("${server.port:8080}") int port;
    @Value("http://${app.host}:${app.port:80}") String url;
    @Value("#{systemProperties['x']}") String spel;
    private Environment env;
    Config(@Value("${db.url}") String url) {}
    void load() {
        String home = System.getenv("APP_HOME");
        String dynamic = System.getenv(name());
        String u = env.getProperty("spring.datasource.url");
        String v = env.getProperty("a.b", "default");
    }
    String name() { return "x"; }
}
"##,
    );
    let mut keys: Vec<(&str, Option<&str>, Option<&str>)> = f
        .references
        .iter()
        .filter(|r| r.kind == RefKind::ConfigKey)
        .map(|r| (r.name.as_str(), r.receiver.as_deref(), enclosing_qn(&f, r)))
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            ("a.b", Some("spring"), Some("Config.load")),
            ("app.host", Some("spring"), Some("Config.url")),
            ("app.port", Some("spring"), Some("Config.url")),
            ("db.url", Some("spring"), Some("Config.Config")),
            ("server.port", Some("spring"), Some("Config.port")),
            ("spring.datasource.url", Some("spring"), Some("Config.load")),
        ]
    );
    let envs: Vec<&str> = f.references.iter().filter(|r| r.kind == RefKind::EnvVar).map(|r| r.name.as_str()).collect();
    assert_eq!(envs, vec!["APP_HOME"]);
    // A dynamic name stays an ordinary call.
    has_ref(&f, RefKind::Call, Some("System"), "getenv");
}

#[test]
fn bridge_needs_file_local_evidence() {
    let f = facts(
        r#"import com.acme.Value;
class System { static String getenv(String s) { return s; } }
class C {
    @Value("${not.spring}") String x;
    void m(Environment env) {
        System.getenv("NOT_AN_ENV_VAR");
        env.getProperty("not.spring.either");
    }
}
"#,
    );
    assert!(
        !f.references.iter().any(|r| matches!(r.kind, RefKind::EnvVar | RefKind::ConfigKey)),
        "{:#?}",
        ref_triples(&f)
    );
}

#[test]
fn types_are_written_as_in_the_source() {
    let f = facts(
        r#"package p;
class Outer extends Base {
    Outer() {}
    Outer copy(Outer other) { Base b = other; b.run(); return other.self(); }
    static class Node {
        Node() {}
        Node next(Node n, Outer o) { return n.next(n, o); }
    }
}
"#,
    );
    // Self-references and the superclass are ordinary type references (the
    // resolver looks type names up as types, never as constructors).
    has_ref(&f, RefKind::Type, None, "Outer");
    has_ref(&f, RefKind::Type, None, "Base");
    has_ref(&f, RefKind::Type, None, "Node");
    has_ref(&f, RefKind::Call, Some("$type:Outer"), "self");
    has_ref(&f, RefKind::Call, Some("$type:Base"), "run");
    has_ref(&f, RefKind::Call, Some("$type:Node"), "next");
}

#[test]
fn fields_and_methods_with_one_name_are_both_reported() {
    let f = facts(
        r#"class Color {
    private final String code;
    Color(String code) { this.code = code; }
    public String code() { return code; }
    String show() { return code() + this.code; }
}
"#,
    );
    // Members in source order: the field first.
    let order: Vec<(Label, &str)> =
        f.definitions.iter().skip(1).take(3).map(|d| (d.label, d.qualified_name.as_str())).collect();
    assert_eq!(
        order,
        vec![(Label::Field, "Color.code"), (Label::Method, "Color.Color"), (Label::Method, "Color.code")]
    );
    // Reads and writes of the field are reported although a method shares
    // its name (the resolver keeps them apart).
    has_ref(&f, RefKind::Write, Some("this"), "code");
    has_ref(&f, RefKind::Read, Some("this"), "code");
    has_ref(&f, RefKind::Read, None, "code");
    has_ref(&f, RefKind::Call, None, "code");
}

#[test]
fn calls_carry_static_argument_types() {
    let f = facts(
        r#"class W {
    private boolean flag;
    private String[] names;
    void w(JsonWriter out, long n, int[] xs, String... rest) throws IOException {
        out.value(n);
        out.value(flag);
        out.value("x" + n);
        out.value(1.5f);
        out.value(compute());
        out.value(this.names);
        out.value(rest);
        out.value(null, 'c', 2L, xs, new Item(), (Number) null, W.class, n > 1);
        out.flush();
        throw new JsonSyntaxException(new IOException("x"));
    }
    double compute() { return 0; }
}
"#,
    );
    let names: Vec<&str> = f.references.iter().filter(|r| bare(r) == "value").map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "value$args:long",
            "value$args:boolean",
            "value$args:String",
            "value$args:float",
            "value$args:double",
            "value$args:String[]",
            "value$args:String[]",
            "value$args:?,char,long,int[],Item,Number,Class,boolean",
        ]
    );
    // No argument: the bare name.
    has_ref(&f, RefKind::Call, Some("$type:JsonWriter"), "flush");
    assert!(f.references.iter().any(|r| r.name == "flush"));
    assert!(f.references.iter().any(|r| r.kind == RefKind::New && r.name == "JsonSyntaxException$args:IOException"));
}

#[test]
fn calls_on_own_method_results_are_typed() {
    let f = facts(
        r#"class JsonArray {
    private JsonElement single() { return null; }
    private <T> T any() { return null; }
    int count() { return 0; }
    boolean asBoolean() { return single().getAsBoolean() && this.single().isNull() && any().x(); }
}
"#,
    );
    has_ref(&f, RefKind::Call, Some("$type:JsonElement"), "getAsBoolean");
    has_ref(&f, RefKind::Call, Some("$type:JsonElement"), "isNull");
    // The method's own type parameter is not a receiver type.
    has_ref(&f, RefKind::Call, Some("any()"), "x");
}

// ---------------------------------------------------------------------------
// Robustness and determinism
// ---------------------------------------------------------------------------

#[test]
fn robustness() {
    for src in [
        "",
        "package",
        "class {",
        "class A { void m( { int x = ; } }",
        "@interface",
        "enum E { A(, B }",
        "record R(int x { }",
        "import static ;",
        "class A { void m() { new } }",
        "\u{0}\u{1}\u{2}",
    ] {
        let f = facts(src);
        for d in &f.definitions {
            if let Some(p) = d.parent {
                assert!((p as usize) < f.definitions.len());
            }
        }
        for r in &f.references {
            if let Some(e) = r.enclosing {
                assert!((e as usize) < f.definitions.len());
            }
        }
    }
    let broken = facts("class A { void m( { } }\nclass B { void ok() {} }");
    assert!(broken.parse_errors > 0);
    // Invalid UTF-8 is read lossily.
    let bytes = b"class A { String s = \"\xff\xfe\"; void m() {} }";
    let f = JavaPack::default().extract("A.java", Language::Java, bytes, &Budget::default()).unwrap();
    assert!(f.definitions.iter().any(|d| d.qualified_name == "A.m"));
}

#[test]
fn deep_nesting_is_bounded() {
    // Deep parentheses: the walk stops at the depth budget without
    // overflowing the stack.
    let depth = 3000;
    let src = format!("class A {{ int x = {}B.C{}; }}", "(".repeat(depth), ")".repeat(depth));
    let f = facts(&src);
    def(&f, "A.x");
    lacks_ref(&f, RefKind::Read, "C");
    // Call chains and `+` chains nest to the left; they are walked
    // iteratively, so every link is kept whatever the depth budget.
    let chain = format!("class A {{ void m() {{ a{} ; }} }}", ".b()".repeat(2000));
    let budget = Budget { max_depth: 16, ..Budget::default() };
    let f = JavaPack::default().extract("A.java", Language::Java, chain.as_bytes(), &budget).unwrap();
    assert_eq!(refs(&f, RefKind::Call, "b").len(), 2000);
    let sum: Vec<String> = (0..3000).map(|i| format!("K{i}")).collect();
    let f = facts(&format!("class A {{ int m() {{ return {}; }} }}", sum.join(" + ")));
    assert_eq!(f.references.iter().filter(|r| r.kind == RefKind::Read).count(), 3000);
    // Constructs handled out of line (lambdas, casts, conditionals, new)
    // nested to the default budget fit a 2 MiB thread stack (debug build).
    let nested = format!(
        "class A {{ Object m() {{ return {}f(){}; }} }}",
        "() -> (Object) (c ? new X(".repeat(600),
        ") : null)".repeat(600)
    );
    let f = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || facts(&nested))
        .unwrap()
        .join()
        .expect("no stack overflow");
    def(&f, "A.m");
    // Other nesting is cut at the budget.
    let lambdas = format!("class A {{ Object m() {{ return {}f(){}; }} }}", "() -> ".repeat(200), "");
    let f = JavaPack::default().extract("A.java", Language::Java, lambdas.as_bytes(), &budget).unwrap();
    lacks_ref(&f, RefKind::Call, "f");
}

#[test]
fn reference_cap() {
    let body: String = (0..(MAX_REFS + 100)).map(|i| format!("f{i}();")).collect();
    let f = facts(&format!("class A {{ void m() {{ {body} }} }}"));
    assert_eq!(f.references.len(), MAX_REFS);
}

#[test]
fn extraction_is_deterministic() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/maven/app/src/main/java/com/acme/app/App.java"
    ))
    .unwrap();
    let a = facts(&src);
    for _ in 0..3 {
        assert_eq!(facts(&src), a);
    }
}

#[test]
fn module_names_and_registry_hooks() {
    let pack = JavaPack::default();
    let project = ProjectFiles {
        paths: ["core/src/main/java/com/x/A.java", "Script.java"].into_iter().map(String::from).collect(),
        configs: Default::default(),
    };
    // Declared package wins; the default package is empty; without facts,
    // the path under a source root.
    let f = facts("package com.declared;\nclass A {}");
    assert_eq!(pack.module_name_for("core/src/main/java/com/x/A.java", Some(&f), &project), "com.declared");
    assert_eq!(pack.module_name_for("Script.java", Some(&facts("class S {}")), &project), "");
    assert_eq!(pack.module_name_for("core/src/main/java/com/x/A.java", None, &project), "com.x");
    assert_eq!(pack.module_scope(Language::Java), ModuleScope::Package);
    assert!(pack.implicit_self() && pack.overloads() && pack.overrides());
    assert_eq!(pack.self_receivers(), &["this"]);
    assert!(pack.is_builtin("String") && pack.is_builtin("java") && pack.is_builtin("toString"));
    assert!(!pack.is_builtin("Parser"));
    assert!(matches!(
        pack.extract("a.py", Language::Python, b"", &Budget::default()),
        Err(ExtractError::Unsupported(Language::Python))
    ));
}
