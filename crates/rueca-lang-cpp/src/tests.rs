//! One test per row of the construct tables in the crate docs.

use super::*;
use rueca_model::{Definition, Label, RefKind, Reference};

fn facts(src: &str) -> FileFacts {
    CppPack::default().extract("x.cpp", Language::Cpp, src.as_bytes(), &Budget::default()).unwrap()
}

fn c_facts(src: &str) -> FileFacts {
    CppPack::default().extract("x.c", Language::C, src.as_bytes(), &Budget::default()).unwrap()
}

fn def<'f>(f: &'f FileFacts, qn: &str) -> &'f Definition {
    let v: Vec<&Definition> = f.definitions.iter().filter(|d| d.qualified_name == qn).collect();
    assert_eq!(
        v.len(),
        1,
        "expected one {qn} in {:#?}",
        f.definitions.iter().map(|d| &d.qualified_name).collect::<Vec<_>>()
    );
    v[0]
}

fn qns(f: &FileFacts) -> Vec<String> {
    f.definitions.iter().map(|d| format!("{:?} {}", d.label, d.qualified_name)).collect()
}

/// `(kind, name, receiver, enclosing qualified name, arity)` of a reference.
type Ref = (RefKind, String, Option<String>, Option<String>, Option<u32>);

/// Every reference of `f`.
fn refs(f: &FileFacts) -> Vec<Ref> {
    f.references
        .iter()
        .map(|r: &Reference| {
            let enc = r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone());
            (r.kind, r.name.clone(), r.receiver.clone(), enc, r.arity)
        })
        .collect()
}

fn has_ref(f: &FileFacts, kind: RefKind, name: &str, receiver: Option<&str>, enc: Option<&str>) -> bool {
    refs(f).iter().any(|(k, n, r, e, _)| *k == kind && n == name && r.as_deref() == receiver && e.as_deref() == enc)
}

#[test]
fn builtins_are_sorted() {
    assert!(builtins::BUILTINS.windows(2).all(|w| w[0] < w[1]));
    let pack = CppPack::default();
    for name in ["printf", "malloc", "std", "vector", "size_t", "static_cast", "EXPECT_EQ"] {
        assert!(pack.is_builtin(name), "{name}");
    }
    assert!(!pack.is_builtin("format"));
}

#[test]
fn pack_contract() {
    let pack = CppPack::default();
    assert_eq!(pack.languages(), &[Language::Cpp, Language::C]);
    assert_eq!(pack.module_name("src/a.cpp", &ProjectFiles::default()), "");
    assert_eq!(pack.self_receivers(), &["this"]);
    assert!(pack.implicit_self() && pack.overloads() && pack.overrides());
    assert!(matches!(
        pack.extract("a.py", Language::Python, b"", &Budget::default()),
        Err(ExtractError::Unsupported(_))
    ));
}

#[test]
fn functions_static_inline_and_prototypes() {
    let f = facts(
        "int add(int a, int b) { return a + b; }\nstatic int helper() { return 1; }\ninline int twice(int x) { return 2 * x; }\nvoid later(int, char*);\n",
    );
    let add = def(&f, "add");
    assert_eq!((add.label, add.exported, add.is_declaration), (Label::Function, true, false));
    assert_eq!(add.signature.as_deref(), Some("int add(int a, int b)"));
    assert!(!def(&f, "helper").exported, "static is file-local");
    assert!(def(&f, "twice").exported);
    let later = def(&f, "later");
    assert!(later.is_declaration);
    assert_eq!(later.signature.as_deref(), Some("void later(int, char*)"));
}

#[test]
fn namespaces_nested_inline_and_anonymous() {
    let f = facts(
        "namespace a { namespace b { void f(); } }\nnamespace a::c { int g; }\ninline namespace v1 { struct S {}; }\nnamespace { void hidden() {} }\nnamespace a { namespace { int secret; } }\n",
    );
    assert_eq!(
        qns(&f),
        ["Function a::b::f", "Variable a::c::g", "Struct v1::S", "Function hidden", "Variable a::secret"]
    );
    assert!(!def(&f, "hidden").exported);
    assert!(!def(&f, "a::secret").exported);
    assert!(def(&f, "a::c::g").exported);
}

#[test]
fn classes_structs_unions_and_members() {
    let src = "class Widget : public Base, private ns::Mixin<int>, public std::enable_shared_from_this<Widget> {\npublic:\n  Widget();\n  explicit Widget(int n);\n  ~Widget();\n  void draw() const;\n  static Widget* make();\n  int size_ = 0;\n  static constexpr int kMax = 4;\nprivate:\n  void secret() {}\n  struct Inner { int v; };\n  union { int i; float f; };\n};\nstruct P { int x, y; };\nunion U { int a; };\n";
    let f = facts(src);
    let w = def(&f, "Widget");
    assert_eq!(w.label, Label::Class);
    assert_eq!(w.bases, ["Base", "Mixin", "std::enable_shared_from_this"]);
    assert_eq!(
        w.signature.as_deref(),
        Some("class Widget : public Base, private ns::Mixin<int>, public std::enable_shared_from_this<Widget>")
    );
    let ctors: Vec<&Definition> =
        f.definitions.iter().filter(|d| d.name == "Widget" && d.label == Label::Method).collect();
    assert_eq!(ctors.len(), 2);
    assert_eq!(ctors[0].qualified_name, "Widget::Widget()");
    assert_eq!(ctors[1].qualified_name, "Widget::Widget(int)");
    assert!(ctors.iter().all(|c| c.is_declaration && c.parent == Some(0)));
    assert_eq!(def(&f, "Widget::~Widget").name, "~Widget");
    assert!(def(&f, "Widget::draw").is_declaration);
    assert_eq!(def(&f, "Widget::size_").label, Label::Field);
    assert_eq!(def(&f, "Widget::kMax").label, Label::Constant);
    let secret = def(&f, "Widget::secret");
    assert!(!secret.exported && !secret.is_declaration, "private, defined in class");
    assert_eq!(def(&f, "Widget::Inner").label, Label::Struct);
    assert_eq!(def(&f, "Widget::Inner::v").parent.map(|p| f.definitions[p as usize].name.as_str()), Some("Inner"));
    // Anonymous union members are members of the class.
    assert_eq!(def(&f, "Widget::i").parent, Some(0));
    assert_eq!(def(&f, "P::x").label, Label::Field);
    assert_eq!(def(&f, "P::y").label, Label::Field);
    assert_eq!(def(&f, "U").label, Label::Struct);
    // Forward declarations are not definitions.
    assert!(facts("class Fwd;\nstruct S2;\n").definitions.is_empty());
}

#[test]
fn out_of_line_bodies() {
    let src = "namespace ns {\nvoid Foo::bar() { baz(); }\nFoo::Foo(int x) : x_(x) {}\nFoo::~Foo() {}\n}\nvoid ns::Foo::qux() const {}\ntemplate <typename T> T Box<T>::get() { return v; }\nvoid ns::free_fn() {}\nint Foo::count = 0;\n";
    let f = facts(src);
    let bar = def(&f, "ns::Foo::bar");
    assert_eq!((bar.label, bar.parent, bar.exported, bar.is_declaration), (Label::Method, None, false, false));
    assert_eq!(def(&f, "ns::Foo::Foo").name, "Foo");
    assert_eq!(def(&f, "ns::Foo::~Foo").name, "~Foo");
    assert_eq!(def(&f, "ns::Foo::qux").label, Label::Method);
    assert_eq!(def(&f, "Box::get").label, Label::Method);
    // `ns` is a namespace of this file: a free function defined qualified.
    let free = def(&f, "ns::free_fn");
    assert_eq!((free.label, free.exported), (Label::Function, true));
    assert_eq!(def(&f, "Foo::count").label, Label::Field);
    // The receiver-less call in an out-of-line body is left to implicit self.
    assert!(has_ref(&f, RefKind::Call, "baz", None, Some("ns::Foo::bar")));
}

#[test]
fn overload_suffixes() {
    let src = "struct P {\n  void put(int v);\n  void put(const std::string& s, int n = 0);\n  void put(int v);\n  void solo(double);\n};\nvoid P::put(int value) {}\nint f(void);\nint f(char *argv[], ...);\n";
    let f = facts(src);
    let puts: Vec<&str> = f.definitions.iter().filter(|d| d.name == "put").map(|d| d.qualified_name.as_str()).collect();
    // The duplicate declaration and the body share the suffix.
    assert_eq!(puts, ["P::put(int)", "P::put(const std::string&, int)", "P::put(int)", "P::put(int)"]);
    assert_eq!(def(&f, "P::solo").name, "solo", "not overloaded: bare");
    let fs: Vec<&str> = f.definitions.iter().filter(|d| d.name == "f").map(|d| d.qualified_name.as_str()).collect();
    assert_eq!(fs, ["f()", "f(char*[], ...)"]);
}

#[test]
fn templates_and_specializations() {
    let f = facts(
        "template <typename T> class Vec { T* data_; };\ntemplate <> class Vec<bool> { int bits_; };\ntemplate <typename T, int N = 3> T first(const T (&a)[N]) { return a[0]; }\ntemplate <typename T> using Ptr = T*;\n",
    );
    let vecs: Vec<&Definition> = f.definitions.iter().filter(|d| d.qualified_name == "Vec").collect();
    assert_eq!(vecs.len(), 2, "primary and specialization share the name");
    assert!(f.definitions.iter().any(|d| d.qualified_name == "first" && d.label == Label::Function));
    // The template parameter is not a type reference, nor an alias base.
    assert!(!refs(&f).iter().any(|r| r.1 == "T"));
    assert!(def(&f, "Ptr").bases.is_empty());
}

#[test]
fn enums_typedefs_and_aliases() {
    let src = "enum class Color : int { Red, Green = 2 };\nenum { kA, kB };\ntypedef struct node { int v; } node_t, *node_p;\ntypedef struct { int w; } anon_t;\ntypedef struct same same;\ntypedef unsigned long ulong;\nusing Name = std::string;\nusing Handle = Widget*;\ntypedef int (*cmp_fn)(const void*, const void*);\n";
    let f = facts(src);
    assert_eq!(def(&f, "Color").label, Label::Enum);
    assert_eq!(def(&f, "Color::Red").label, Label::Constant);
    assert_eq!(def(&f, "Color::Green").parent, Some(0));
    assert_eq!(def(&f, "kA").parent, None, "anonymous enum: enumerators at namespace level");
    assert_eq!(def(&f, "node").label, Label::Struct);
    assert_eq!(def(&f, "node_t").bases, ["node"]);
    assert_eq!(def(&f, "node_p").bases, ["node"]);
    assert_eq!(def(&f, "anon_t").label, Label::Struct, "an anonymous struct takes the typedef name");
    assert!(f.definitions.iter().all(|d| d.qualified_name != "same"), "typedef struct same same");
    assert!(def(&f, "ulong").bases.is_empty());
    assert_eq!(def(&f, "Name").bases, ["std::string"]);
    assert_eq!(def(&f, "Handle").bases, ["Widget"]);
    assert_eq!(def(&f, "cmp_fn").label, Label::TypeAlias);
}

#[test]
fn variables_and_constants() {
    let f = facts(
        "int counter = 0;\nconst int kLimit = 10;\nconstexpr double kPi = 3.14;\nconst char* name = \"x\";\nstatic int file_local;\nextern int shared;\nint (*handler)(int);\nvoid f() { int local = 1; }\n",
    );
    assert_eq!(def(&f, "counter").label, Label::Variable);
    assert_eq!(def(&f, "kLimit").label, Label::Constant);
    assert_eq!(def(&f, "kPi").label, Label::Constant);
    assert_eq!(def(&f, "name").label, Label::Variable, "pointer to const");
    assert!(!def(&f, "file_local").exported);
    assert!(def(&f, "shared").is_declaration);
    assert_eq!(def(&f, "handler").label, Label::Variable, "function pointer variable");
    assert!(f.definitions.iter().all(|d| d.name != "local"), "locals are not definitions");
}

#[test]
fn macros() {
    let src = "#ifndef X_H\n#define X_H\n/// Maximum size.\n#define MAX_SIZE 64\n#define SQUARE(x) ((x) * (x))\n#define FEATURE\nnamespace ns {\n#define INNER 1\n}\n#endif\n";
    let f = facts(src);
    let max = def(&f, "MAX_SIZE");
    assert_eq!((max.label, max.parent, max.exported), (Label::Constant, None, true));
    assert_eq!(max.doc.as_deref(), Some("Maximum size."));
    assert_eq!(max.signature.as_deref(), Some("#define MAX_SIZE 64"));
    assert_eq!(max.span.start_line, max.span.end_line, "the span stops at the end of the line");
    let sq = def(&f, "SQUARE");
    assert_eq!(sq.label, Label::Function);
    assert_eq!(sq.signature.as_deref(), Some("#define SQUARE(x) ((x) * (x))"));
    assert!(f.definitions.iter().all(|d| d.name != "X_H"), "include guard");
    assert!(f.definitions.iter().any(|d| d.qualified_name == "FEATURE"), "a flag without a guard name");
    assert_eq!(def(&f, "INNER").qualified_name, "INNER", "macros ignore namespaces");
    // Invocations are calls (function-like) and reads (object-like).
    let g = facts("int area(int r) { return SQUARE(r) + MAX_SIZE; }\n");
    assert!(has_ref(&g, RefKind::Call, "SQUARE", None, Some("area")));
    assert!(has_ref(&g, RefKind::Read, "MAX_SIZE", None, Some("area")));
}

#[test]
fn operators_and_conversions() {
    let f = facts(
        "struct V {\n  V operator+(const V& o) const;\n  bool operator==(const V&) const = default;\n  V& operator=(V&&) noexcept;\n  int operator()(int) const;\n  explicit operator bool() const;\n  void* operator new(size_t n);\n};\nV operator-(const V& a, const V& b);\n",
    );
    for qn in [
        "V::operator+",
        "V::operator==",
        "V::operator=",
        "V::operator()",
        "V::operator bool",
        "V::operator new",
        "operator-",
    ] {
        def(&f, qn);
    }
}

#[test]
fn doc_comments() {
    let src = "/// Line doc\n/// continues.\nint a();\n\n/** Block doc.\n * Second line.\n */\nint b();\n\n// Plain comment block\n// still doc.\nint c();\n\n// Detached.\n\nint d();\nint e(); // trailing\nint g();\nstruct S {\n  int x; ///< member doc of x\n  int y;\n};\n";
    let f = facts(src);
    assert_eq!(def(&f, "a").doc.as_deref(), Some("Line doc\ncontinues."));
    assert_eq!(def(&f, "b").doc.as_deref(), Some("Block doc.\nSecond line."));
    assert_eq!(def(&f, "c").doc.as_deref(), Some("Plain comment block\nstill doc."));
    assert_eq!(def(&f, "d").doc, None, "blank line in between");
    assert_eq!(def(&f, "g").doc, None, "trailing comment of the previous line");
    assert_eq!(def(&f, "S::y").doc, None);
}

#[test]
fn tests_googletest_and_catch2() {
    let src = "TEST(MathTest, Adds) { EXPECT_EQ(add(1, 2), 3); }\nTEST_F(Fixture, Uses) { helper(); }\nTEST_P(Param, Works) {}\nTEST_CASE(\"vectors grow\", \"[vector]\") {\n  REQUIRE(grow() > 0);\n}\nTEST_CASE_METHOD(DbFixture, \"queries\") { query(); }\nnamespace suite {\nTEST(Inner, Case) {}\n}\n";
    let f = facts(src);
    assert_eq!(f.parse_errors, 0, "the Catch2 head's missing `;` is not counted");
    let adds = def(&f, "MathTest::Adds");
    assert_eq!((adds.label, adds.name.as_str(), adds.is_test, adds.exported), (Label::Test, "Adds", true, false));
    assert_eq!(adds.signature.as_deref(), Some("TEST(MathTest, Adds)"));
    assert!(has_ref(&f, RefKind::Call, "add", None, Some("MathTest::Adds")));
    assert!(has_ref(&f, RefKind::Call, "helper", None, Some("Fixture::Uses")));
    def(&f, "Param::Works");
    let grow = def(&f, "vectors grow");
    assert_eq!(grow.label, Label::Test);
    assert!(has_ref(&f, RefKind::Call, "grow", None, Some("vectors grow")));
    def(&f, "DbFixture::queries");
    def(&f, "suite::Inner::Case");
    // No parameter types of the macro are reported.
    assert!(!refs(&f).iter().any(|r| r.1 == "MathTest" || r.1 == "Adds"));
}

#[test]
fn calls_receivers_and_arity() {
    let src = "struct Repo { void save(int); Repo* next; };\nstruct Svc {\n  Repo repo_;\n  Repo* ptr_;\n  void run(Repo& r, Repo* p) {\n    r.save(1);\n    p->save(2);\n    repo_.save(3);\n    this->ptr_->save(4);\n    ptr_->next->save(5);\n    tick();\n    this->tick();\n    Repo local;\n    local.save(6);\n    auto made = new Repo();\n    made->save(7);\n    unknown.save(8);\n    make().save(9);\n    ns::util(1, 2);\n    Repo::create();\n    std::move(r);\n    (*p).save(10);\n  }\n  void tick();\n};\n";
    let f = facts(src);
    let enc = Some("Svc::run");
    assert!(has_ref(&f, RefKind::Call, "save", Some("$type:Repo"), enc));
    let saves: Vec<Option<String>> =
        refs(&f).into_iter().filter(|r| r.0 == RefKind::Call && r.1 == "save").map(|r| r.2).collect();
    assert_eq!(saves.iter().filter(|r| r.as_deref() == Some("$type:Repo")).count(), 8, "{saves:?}");
    assert!(saves.contains(&Some("unknown".into())));
    assert!(saves.contains(&Some("make()".into())));
    assert!(has_ref(&f, RefKind::Call, "tick", None, enc));
    assert!(has_ref(&f, RefKind::Call, "tick", Some("this"), enc));
    assert!(has_ref(&f, RefKind::Call, "util", None, enc), "namespace call: no receiver");
    assert!(has_ref(&f, RefKind::Call, "create", Some("$type:Repo"), enc));
    assert!(has_ref(&f, RefKind::Call, "move", Some("std"), enc));
    assert!(has_ref(&f, RefKind::New, "Repo", None, enc));
    let arity = refs(&f).into_iter().find(|r| r.1 == "util").unwrap().4;
    assert_eq!(arity, Some(2));
    assert!(has_ref(&f, RefKind::Read, "ptr_", Some("this"), enc));
}

#[test]
fn namespace_calls_inside_methods_keep_their_qualifier_when_shadowed() {
    let src = "namespace detail { void write(int); }\nstruct W {\n  void write(int x) { detail::write(x); }\n  void put(int x) { detail::write(x); }\n};\n";
    let f = facts(src);
    // A same-named member would win through implicit self: keep `detail`.
    assert!(has_ref(&f, RefKind::Call, "write", Some("detail"), Some("W::write")));
    assert!(has_ref(&f, RefKind::Call, "write", Some("detail"), Some("W::put")));
    let g = facts("namespace detail { void emit(int); }\nstruct W { void put(int x) { detail::emit(x); } };\n");
    assert!(has_ref(&g, RefKind::Call, "emit", None, Some("W::put")));
}

#[test]
fn qualified_types_are_prefixed_with_the_enclosing_namespace() {
    let src = "namespace fmt { namespace v11 {\nvoid f() { detail::buffer b; b.push(1); v11::detail::buffer c; c.push(2); }\n} }\n";
    let f = facts(src);
    let recvs: Vec<String> = refs(&f).into_iter().filter(|r| r.1 == "push").filter_map(|r| r.2).collect();
    assert_eq!(recvs, ["$type:fmt::v11::detail::buffer", "$type:fmt::v11::detail::buffer"]);
}

#[test]
fn types_new_throw_and_writes() {
    let src = "struct Err {};\nstruct Box { int n; void set(int v) { n = v; this->n = v; ++n; } };\nint g;\nWidget* build(const Config& cfg, std::vector<Item> items) {\n  g = 1;\n  if (!cfg.ok()) throw Err();\n  throw std::runtime_error(\"x\");\n  auto w = new ns::Widget(cfg, 2);\n  Point p{1, 2};\n  Shape s(3);\n  return static_cast<Widget*>(w);\n}\n";
    let f = facts(src);
    let b = Some("build");
    assert!(has_ref(&f, RefKind::Type, "Widget", None, b));
    assert!(has_ref(&f, RefKind::Type, "Config", None, b));
    assert!(has_ref(&f, RefKind::Type, "vector", Some("std"), b));
    assert!(has_ref(&f, RefKind::Type, "Item", None, b), "template arguments");
    assert!(has_ref(&f, RefKind::Throw, "Err", None, b));
    assert!(has_ref(&f, RefKind::Throw, "runtime_error", Some("std"), b));
    let new = refs(&f).into_iter().find(|r| r.0 == RefKind::New).unwrap();
    assert_eq!((new.1.as_str(), new.2, new.4), ("Widget", None, Some(2)));
    assert!(has_ref(&f, RefKind::Call, "Point", None, b), "brace construction");
    assert!(has_ref(&f, RefKind::Call, "Shape", None, b), "constructor call of a declared object");
    assert!(has_ref(&f, RefKind::Call, "ok", Some("$type:Config"), b));
    assert!(!refs(&f).iter().any(|r| r.1 == "static_cast"));
    assert!(has_ref(&f, RefKind::Write, "g", None, b));
    let set = Some("Box::set");
    assert!(has_ref(&f, RefKind::Write, "n", None, set));
    assert!(has_ref(&f, RefKind::Write, "n", Some("this"), set));
    // Parameters and locals are never references.
    assert!(!refs(&f).iter().any(|r| r.1 == "cfg" || r.1 == "items" || r.1 == "v" || r.1 == "w"));
}

#[test]
fn includes_are_star_imports() {
    let f = facts(
        "#include \"util/a.h\"\n#include <vector>\n  #  include \"b.h\" // note\n#include MACRO_HEADER\n#if 0\n#include \"c.h\"\n#endif\n",
    );
    let mods: Vec<&str> = f.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(mods, ["util/a.h", "<vector>", "b.h", "c.h"]);
    assert!(f.imports.iter().all(|i| i.names.len() == 1 && i.names[0].name == "*" && i.names[0].alias.is_none()));
    assert_eq!((f.imports[2].span.start_line, f.imports[2].span.end_line), (2, 2));
}

#[test]
fn env_var_reads() {
    let f = facts(
        "const char* home() { return getenv(\"HOME\"); }\nconst char* p() { return std::getenv(\"PATH\"); }\nconst char* q(const char* k) { return getenv(k); }\n",
    );
    let envs: Vec<String> = refs(&f).into_iter().filter(|r| r.0 == RefKind::EnvVar).map(|r| r.1).collect();
    assert_eq!(envs, ["HOME", "PATH"]);
}

#[test]
fn c_files() {
    let src = "#include \"list.h\"\nstruct node { int v; struct node *next; };\ntypedef void (*visit)(struct node *);\nstatic int count(struct node *n) { return n->v + n->next->v; }\nint walk(struct node *head, visit fn) { fn(head); return count(head); }\nint old(a) int a; { return a; }\n";
    let f = c_facts(src);
    assert_eq!(f.parse_errors, 0);
    assert_eq!(def(&f, "node").label, Label::Struct);
    assert!(!def(&f, "count").exported);
    assert!(has_ref(&f, RefKind::Read, "v", Some("$type:node"), Some("count")));
    assert!(has_ref(&f, RefKind::Call, "count", None, Some("walk")));
    assert!(!refs(&f).iter().any(|r| r.0 == RefKind::Call && r.1 == "fn"), "calls through a parameter are local");
}

#[test]
fn masked_macros_keep_definitions() {
    let src = "FMT_BEGIN_NAMESPACE\nclass FMT_API format_error : public std::runtime_error {\n public:\n  FMT_CONSTEXPR explicit format_error(const char* m);\n};\nFMT_API void vprint(int fd);\ntemplate <typename T, FMT_ENABLE_IF(is_integral<T>::value)>\nFMT_CONSTEXPR auto to_unsigned(T v) -> int { return v; }\nFMT_END_NAMESPACE\n";
    let f = facts(src);
    assert_eq!(f.parse_errors, 0, "{:?}", qns(&f));
    let e = def(&f, "format_error");
    assert_eq!(
        e.signature.as_deref(),
        Some("class FMT_API format_error : public std::runtime_error"),
        "signatures keep the original text"
    );
    def(&f, "format_error::format_error");
    def(&f, "vprint");
    def(&f, "to_unsigned");
}

#[test]
fn robustness_never_panics() {
    let nasty = [
        "#define X(a, b) a ## b\nX(int, x) = 1;\n#if\n#endif\n#endif\n}}}}{{{",
        "template <typename... Ts> auto f(Ts&&... ts) -> decltype((ts + ...)) { return (ts + ...); }",
        "class { public: int x; } anon;\nstruct S { struct { struct { int deep; }; }; };",
        "int main() { [&](auto x) mutable -> int { return x; }(1); co_return; }",
        "MACRO(\nMACRO2(a, b) {\n#ifdef Q\n} else {\n#endif\n",
        "\u{feff}int x = R\"xx(raw \" string ) with )xx\";\nint y = 'a';\n",
        "operator",
        "::",
        "#include",
        "struct A : virtual public B<C<D<E>>> { A() try : B() {} catch (...) {} };",
        "",
    ];
    for src in nasty {
        let _ = facts(src);
        let _ = c_facts(src);
    }
    // Deep nesting is bounded (and fits the test thread's 2 MiB stack).
    let n = 3000;
    for deep in [
        format!("void f() {{ {} }}", "{".repeat(n) + &"}".repeat(n)),
        format!("int x = {}1{};", "f(".repeat(n), ")".repeat(n)),
        format!("int x = {}1{};", "(".repeat(n), ")".repeat(n)),
        format!("{}{}", "struct A { ".repeat(n), "};".repeat(n)),
        format!("struct A {{ {} }};", "struct { ".repeat(n) + &"};".repeat(n)),
        format!("{}{}", "namespace a { ".repeat(n), "}".repeat(n)),
        format!("{}{}", "#ifdef X\nint a;\n".repeat(n), "#endif\n".repeat(n)),
    ] {
        let _ = facts(&deep);
    }
    // A macro-heavy body: many statement macros, no panic, references bounded.
    let mut heavy = String::from("void g() {\n");
    for i in 0..6_000 {
        heavy.push_str(&format!("  CHECK_{i}(x) LOG(y)\n  call_{i}();\n"));
    }
    heavy.push_str("}\n");
    let f = facts(&heavy);
    assert!(f.references.len() <= MAX_REFS);
}

#[test]
fn deterministic_and_too_large() {
    let src = "namespace a { struct S { void m(); }; void S::m() { other(); } }\n";
    assert_eq!(facts(src), facts(src));
    let budget = Budget { max_bytes: 4, ..Budget::default() };
    let err = CppPack::default().extract("x.cpp", Language::Cpp, b"int x;", &budget).unwrap_err();
    assert!(matches!(err, ExtractError::TooLarge { .. }));
}
