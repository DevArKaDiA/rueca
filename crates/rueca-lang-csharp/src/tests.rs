//! Extraction tests: definitions, qualified names, namespaces, usings,
//! references and receivers, overloads, tests and the configuration bridge.

use super::*;
use rueca_model::{Definition, Label, RefKind, Reference};

fn facts_at(path: &str, src: &str) -> FileFacts {
    CSharpPack.extract(path, Language::CSharp, src.as_bytes(), &Budget::default()).expect("extract")
}

fn facts(src: &str) -> FileFacts {
    facts_at("src/App/Main.cs", src)
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| {
        let all: Vec<&str> = f.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
        panic!("no definition {qn}: {all:?}")
    })
}

fn refs(f: &FileFacts, kind: RefKind) -> Vec<&Reference> {
    f.references.iter().filter(|r| r.kind == kind).collect()
}

/// `(name, receiver)` of the references of `kind`.
fn pairs(f: &FileFacts, kind: RefKind) -> Vec<(String, Option<String>)> {
    refs(f, kind).into_iter().map(|r| (r.name.clone(), r.receiver.clone())).collect()
}

fn has(f: &FileFacts, kind: RefKind, name: &str, receiver: Option<&str>) -> bool {
    pairs(f, kind).iter().any(|(n, r)| n == name && r.as_deref() == receiver)
}

#[test]
fn definitions_labels_and_qualified_names() {
    let f = facts(
        r#"
namespace Acme.Shop
{
    /// <summary>A store of <see cref="T:Acme.Shop.Item"/>s.</summary>
    [Serializable]
    public partial class Store<T> : Base, IStore where T : class
    {
        private readonly IRepo _repo;
        public const int Max = 3;
        public string Name { get; set; }
        public event EventHandler Changed;
        public Store(IRepo repo) : base(repo) { _repo = repo; }
        ~Store() { }
        public void Add(T item) { int Local(int k) => k; Local(1); }
        public static Store<T> operator +(Store<T> a, Store<T> b) => a;
        public static implicit operator string(Store<T> s) => s.Name;
        public int this[int i] => i;
        void IStore.Explicit() { }
        public class Options { }
    }
    public record Person(string Name, int Age);
    public record struct Pair(int A, int B);
    public struct Point { public int X; }
    public interface IStore { void Explicit(); }
    public enum Color { Red, Green = 2 }
    public delegate void Handler(object sender);
}
"#,
    );
    use Label::*;
    let cases = [
        ("Store", Class),
        ("Store._repo", Field),
        ("Store.Max", Constant),
        ("Store.Name", Field),
        ("Store.Changed", Field),
        ("Store.Store", Method),
        ("Store.~Store", Method),
        ("Store.Add", Method),
        ("Store.Add.Local", Function),
        ("Store.operator+", Method),
        ("Store.implicit operator string", Method),
        ("Store.this[]", Field),
        ("Store.IStore.Explicit", Method),
        ("Store.Options", Class),
        ("Person", Class),
        ("Person.Name", Field),
        ("Pair", Struct),
        ("Point", Struct),
        ("Point.X", Field),
        ("IStore", Interface),
        ("IStore.Explicit", Method),
        ("Color", Enum),
        ("Color.Green", Constant),
        ("Handler", TypeAlias),
    ];
    for (qn, label) in cases {
        assert_eq!(def(&f, qn).label, label, "{qn}");
    }
    let store = def(&f, "Store");
    assert_eq!(store.bases, ["Base"]);
    assert_eq!(store.implements, ["IStore"]);
    assert_eq!(store.decorators, ["Serializable"]);
    assert_eq!(store.doc.as_deref(), Some("A store of Acme.Shop.Items."));
    assert_eq!(store.signature.as_deref(), Some("public partial class Store<T> : Base, IStore where T : class"));
    assert_eq!(declared_namespace(&f), Some("Acme.Shop"));
    // A local function is a child of its method; members of their type.
    let add = f.definitions.iter().position(|d| d.qualified_name == "Store.Add").unwrap() as u32;
    assert_eq!(def(&f, "Store.Add.Local").parent, Some(add));
    // Visibility: private field, interface members, top-level types.
    assert!(!def(&f, "Store._repo").exported);
    assert!(def(&f, "IStore.Explicit").exported);
    assert!(def(&f, "Point").exported);
    assert!(!def(&f, "Store.~Store").exported);
}

#[test]
fn file_scoped_nested_and_mixed_namespaces() {
    let f = facts("namespace A.B;\nclass C { void M() { } }\n");
    assert_eq!(declared_namespace(&f), Some("A.B"));
    def(&f, "C.M");
    let f = facts("namespace A { namespace B { class C { } } }");
    assert_eq!(declared_namespace(&f), Some("A.B"));
    def(&f, "C");
    let f = facts("namespace A.B { class C { } }\nnamespace A.D { class E { } }");
    assert_eq!(declared_namespace(&f), Some("A"));
    def(&f, "B.C");
    def(&f, "D.E");
    let f = facts("class Global { }");
    assert_eq!(declared_namespace(&f), Some(""));
    // Types under `#if` are walked.
    let f = facts("namespace N;\n#if NET8_0\nclass X { }\n#else\nclass Y { }\n#endif\n");
    assert_eq!(declared_namespace(&f), Some("N"));
    def(&f, "X");
    def(&f, "Y");
}

#[test]
fn usings_are_recorded_after_the_namespace() {
    let f = facts(
        "global using System.Text;\nusing Acme.Core;\nusing static Acme.Core.Util;\nusing Json = Newtonsoft.Json.JsonConvert;\nusing Core = Acme;\nnamespace Acme.App;\nclass P { void M() { Shout(); } }\n",
    );
    type Using<'a> = (&'a str, Vec<(&'a str, Option<&'a str>)>, Option<&'a str>, bool);
    let imps: Vec<Using<'_>> = f
        .imports
        .iter()
        .map(|i| {
            let names = i.names.iter().map(|n| (n.name.as_str(), n.alias.as_deref())).collect();
            (i.module.as_str(), names, i.module_alias.as_deref(), i.reexport)
        })
        .collect();
    assert_eq!(imps[0], ("Acme.App", vec![], None, false));
    assert_eq!(imps[1], ("System.Text", vec![("*", None)], None, true));
    assert_eq!(imps[2], ("Acme.Core", vec![("*", None)], None, false));
    assert_eq!(imps[3], ("Acme.Core", vec![("Util.*", None)], None, false));
    assert_eq!(imps[4], ("Newtonsoft.Json", vec![("JsonConvert", Some("Json"))], None, false));
    assert_eq!(imps[5], ("Acme", vec![], Some("Core"), false));
    // `using static`: a synthetic member import per bare call.
    assert!(imps.iter().any(|i| i.0 == "Acme.Core" && i.1 == vec![("Util.Shout", None)]));
}

#[test]
fn receivers_are_typed_from_declarations() {
    let f = facts(
        r#"
namespace N;
class Svc : BaseSvc
{
    private readonly IRepo _repo;
    public Cfg Settings { get; }
    Order Make() => null;
    void Run(Writer w, string s, object o)
    {
        var a = new Alpha();
        a.Go();
        Beta b = Factory();
        b.Go();
        w?.Write("x");
        _repo.Save(1);
        this._repo.Save(2);
        Settings.Load();
        Make().Submit();
        ((Gamma)o).Go();
        (o as Delta).Go();
        if (o is Eps e) e.Go();
        foreach (Item it in Items()) it.Go();
        s.Trim();
        Util.Static();
        Outer.Inner.Static();
        base.Run2();
        this.Local2();
        new Zeta().Go();
    }
}
"#,
    );
    use RefKind::Call;
    for (name, recv) in [
        ("Go", "$type:Alpha"),
        ("Go", "$type:Beta"),
        ("Write$args:string", "$type:Writer"),
        ("Save$args:int", "$type:IRepo"),
        ("Load", "$type:Cfg"),
        ("Submit", "$type:Order"),
        ("Go", "$type:Gamma"),
        ("Go", "$type:Delta"),
        ("Go", "$type:Eps"),
        ("Go", "$type:Item"),
        ("Trim", "$type:string"),
        ("Static", "Util"),
        ("Static", "Outer.Inner"),
        ("Run2", "base"),
        ("Local2", "this"),
        ("Go", "$type:Zeta"),
    ] {
        assert!(has(&f, Call, name, Some(recv)), "{name} on {recv}: {:?}", pairs(&f, Call));
    }
    assert!(has(&f, RefKind::New, "Alpha", None));
    assert!(has(&f, RefKind::Read, "_repo", None));
    assert!(has(&f, RefKind::Type, "IRepo", None));
}

#[test]
fn constructors_creations_and_initializers() {
    let f = facts(
        r#"
namespace N;
class Child : Parent
{
    public Child(int x) : base(x) { }
    public Child() : this(0) { }
    void M()
    {
        Opts o = new(3) { Retries = 2 };
        var p = new Point { X = 1 };
        throw new BadThing("x");
    }
}
class Svc(IRepo repo) : Base(repo) { }
"#,
    );
    use RefKind::*;
    assert!(has(&f, Call, "Parent$args:int", Some("base")), "{:?}", pairs(&f, Call));
    assert!(has(&f, Call, "Child$args:int", Some("this")));
    assert!(has(&f, New, "Opts$args:int", None));
    assert!(has(&f, Write, "Retries", Some("$type:Opts")));
    assert!(has(&f, Write, "X", Some("$type:Point")));
    assert!(has(&f, Throw, "BadThing", None));
    assert!(has(&f, Call, "Base$args:IRepo", Some("base")), "{:?}", pairs(&f, Call));
    let child = refs(&f, New).into_iter().find(|r| r.name == "Opts$args:int").unwrap();
    assert_eq!(child.arity, Some(1));
}

#[test]
fn overloads_get_parameter_suffixes() {
    let f = facts(
        r#"
class P
{
    public void Parse(string s) { }
    public void Parse(string s, int n = 0) { }
    public void Parse(List<Item> items, params object[] rest) { }
    public void Parse(ref int x, out string? y) { y = null; }
    public P() { }
    public P(int a) { }
    public void Single(int a) { }
}
"#,
    );
    for qn in [
        "P.Parse(string)",
        "P.Parse(string,int)",
        "P.Parse(List,object...)",
        "P.Parse(int,string)",
        "P.P()",
        "P.P(int)",
        "P.Single",
    ] {
        def(&f, qn);
    }
}

#[test]
fn locals_shadow_fields_and_skip_calls() {
    let f = facts(
        r#"
class C
{
    int count;
    void Step() { }
    void M(int count)
    {
        count++;
        Action Step = () => { };
        Step();
        var s = nameof(Step);
        Other(count);
    }
    void N() { count = 1; }
}
"#,
    );
    let calls: Vec<String> = refs(&f, RefKind::Call).iter().map(|r| r.name.clone()).collect();
    assert!(!calls.iter().any(|c| c.starts_with("Step") || c.starts_with("nameof")), "{calls:?}");
    assert!(calls.iter().any(|c| c == "Other$args:int"));
    let m = f.definitions.iter().position(|d| d.qualified_name == "C.M").unwrap() as u32;
    assert!(!refs(&f, RefKind::Read).iter().any(|r| r.name == "count" && r.enclosing == Some(m)));
    assert!(has(&f, RefKind::Write, "count", None));
}

#[test]
fn tests_are_detected_by_attributes_and_paths() {
    let src = r#"
namespace N.Tests;
public class StoreTests
{
    [Fact] public void Adds() { }
    [Theory, InlineData(1)] public void Many(int x) { }
    [SetUp] public void Init() { }
    public void Helper() { }
}
[TestFixture]
public class Fixture { [Test] public void Works() { } [SetUp] public void Init() { } }
[TestClass]
public class MsTests { [TestMethod] public void A() { } [DataTestMethod] public void B() { } }
public class Plain { public void Go() { } }
"#;
    let f = facts_at("src/N/Store.cs", src);
    for qn in [
        "StoreTests",
        "StoreTests.Adds",
        "StoreTests.Many",
        "StoreTests.Init",
        "Fixture",
        "Fixture.Works",
        "Fixture.Init",
        "MsTests.A",
        "MsTests.B",
    ] {
        assert!(def(&f, qn).is_test, "{qn}");
    }
    for qn in ["StoreTests.Helper", "Plain", "Plain.Go"] {
        assert!(!def(&f, qn).is_test, "{qn}");
    }
    let f = facts_at("tests/N.Tests/Support.cs", "class Support { void Go() { } }");
    assert!(def(&f, "Support").is_test && def(&f, "Support.Go").is_test);
    let f = facts_at("src/N.UnitTests/Support.cs", "class Support { }");
    assert!(def(&f, "Support").is_test);
}

#[test]
fn environment_and_configuration_reads() {
    let f = facts(
        r#"
using Microsoft.Extensions.Configuration;
class Startup
{
    private readonly IConfiguration _config;
    public IConfiguration Configuration { get; }
    void M(WebApplicationBuilder builder, IConfigurationRoot root, string key)
    {
        var a = Environment.GetEnvironmentVariable("DB_URL");
        var b = System.Environment.GetEnvironmentVariable("HOME");
        var c = _config["Shop:TaxRate"];
        var d = root.GetValue<int>("Shop:Retries");
        var e = Configuration.GetSection("Logging");
        var g = builder.Configuration.GetConnectionString("Default");
        var h = _config[key];
        var i = _config.GetValue<string>(key);
    }
}
"#,
    );
    let env: Vec<String> = refs(&f, RefKind::EnvVar).iter().map(|r| r.name.clone()).collect();
    assert_eq!(env, ["DB_URL", "HOME"]);
    let keys: Vec<(String, Option<String>)> = pairs(&f, RefKind::ConfigKey);
    let want = ["Shop:TaxRate", "Shop:Retries", "Logging", "ConnectionStrings:Default"];
    assert_eq!(keys.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), want);
    assert!(keys.iter().all(|(_, r)| r.as_deref() == Some("dotnet")));
    // A key that is not a literal is a plain call.
    assert!(has(&f, RefKind::Call, "GetValue$args:string", Some("$type:IConfiguration")));
}

#[test]
fn generic_arguments_attributes_and_types() {
    let f = facts(
        r#"
namespace N;
[ApiController]
class C<T> where T : IEntity
{
    [HttpGet("{id}")]
    public List<Item> Get(Dictionary<string, Order> map, T t)
    {
        var x = Map<Dto>(map);
        var y = typeof(Widget);
        if (map is Special s) { }
        return default(List<Item>);
    }
}
"#,
    );
    use RefKind::*;
    assert!(has(&f, Decorator, "ApiControllerAttribute", None));
    assert!(has(&f, Decorator, "HttpGetAttribute", None));
    for t in ["IEntity", "Item", "Order", "Dto", "Widget", "Special"] {
        assert!(has(&f, Type, t, None), "{t}: {:?}", pairs(&f, Type));
    }
    assert!(!has(&f, Type, "T", None));
    assert!(def(&f, "C.Get").decorators == ["HttpGet(\"{id}\")"]);
}

#[test]
fn parse_errors_are_counted() {
    let f = facts("class C { void M( { }");
    assert!(f.parse_errors > 0);
    let f = facts("namespace N; class C { }");
    assert_eq!(f.parse_errors, 0);
}
