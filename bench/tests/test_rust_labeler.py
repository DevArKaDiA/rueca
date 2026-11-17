"""Rust: the labeler's qualified names (the Rust pack's convention), test
code left out, call sites (methods, paths, macros, calls in macro
arguments) and gold derivation from a rust-analyzer-shaped SCIP index."""

import pytest

from rueca_bench.common import is_test_path
from rueca_bench.scip import Doc, Occ, derive_call_graph
from rueca_bench.treesitter import extract_functions, is_call_site, parse

SRC = """fn free() {}
mod m {
    pub fn f() {}
    pub struct Foo;
    impl Foo {
        pub fn method(&self) {}
    }
    pub mod inner {
        pub fn g() {}
    }
}
fn outer() {
    fn inner() {}
    struct Local;
    impl Local {
        fn value(&self) {}
    }
}
struct Foo<T>(T);
impl<'a, T: Clone> Foo<T> {
    fn new() {}
}
trait Tr {
    fn sig(&self);
    fn default_body(&self) {}
}
impl<'a> Tr for &'a mut crate::m::Foo {
    fn sig(&self) {}
}
impl Tr for dyn std::fmt::Debug {
    fn sig(&self) {}
}
impl<T> Tr for [T] {
    fn sig(&self) {}
}
extern "C" {
    fn ext();
}
macro_rules! mac {
    () => {};
}
#[inline]
/// docs
pub(crate) async fn attributed() {}
"""


def _names(src: str) -> dict[str, tuple[str, int, int]]:
    return {f.qualname: (f.label, f.def_line, f.outer_start) for f in extract_functions("src/lib.rs", src.encode(), "rust")}


def test_qualified_names_follow_the_pack_convention():
    names = _names(SRC)
    assert sorted(names) == sorted(
        [
            "free",
            "m.f",
            "m.Foo.method",
            "m.inner.g",
            "outer",
            "outer.inner",
            "outer.Local.value",
            "Foo.new",
            "Tr.sig",
            "Tr.default_body",
            "Foo.sig",  # impl Tr for &'a mut crate::m::Foo
            "Debug.sig",  # impl Tr for dyn std::fmt::Debug
            "[T].sig",  # no path: the type's text
            "mac",
            "attributed",
        ]
    )
    assert names["m.Foo.method"][0] == "Method"
    assert names["Tr.sig"][0] == "Method"  # a trait signature is a callable
    assert names["outer.inner"][0] == "Function"
    assert names["mac"][0] == "Function"
    assert "ext" not in names  # extern block declarations are not callables


def test_attributes_give_the_outer_start():
    label, def_line, outer_start = _names(SRC)["attributed"]
    assert (def_line, outer_start) == (44, 42)


def test_test_code_is_not_callable():
    src = """fn real() {}
#[test]
fn unit() { real(); }
#[tokio::test(flavor = "multi_thread")]
async fn async_unit() {}
#[cfg(test)]
mod tests {
    fn fixture() {}
    #[test]
    fn t() {}
}
#[cfg(all(test, feature = "x"))]
mod more {
    fn helper() {}
}
#[cfg(not(test))]
fn not_test() {}
"""
    assert sorted(_names(src)) == ["not_test", "real"]


@pytest.mark.parametrize(
    ("path", "test"),
    [("src/tests.rs", True), ("src/a/test.rs", True), ("src/parse_tests.rs", True), ("tests/it.rs", True), ("benches/b.rs", True), ("src/testing_utils.rs", False), ("src/attest.rs", False)],
)
def test_test_paths(path, test):
    assert is_test_path(path) is test


CALLS = """fn f() {
    a.method();
    Type::assoc();
    generic::<u8>();
    a.b::<T>();
    mine!(1);
    crate::m::mine!(2);
    println!("{} {}", foo(1), x.bar());
    vec![Type::new(), mine!(3)];
    let s = Shape { x: 1 };
    let v = value;
    // fake() in a comment
    let t = "fake()";
}
"""


def _site(line0: int, word: str, nth: int = 0) -> bool:
    tree = parse("src/lib.rs", CALLS.encode(), "rust")
    text = CALLS.split("\n")[line0]
    col = -1
    for _ in range(nth + 1):
        col = text.index(word, col + 1)
    return is_call_site(tree, "rust", line0, col, col + len(word))


@pytest.mark.parametrize(
    ("line0", "word", "nth", "call"),
    [
        (1, "method", 0, True),  # field_expression.field of a call
        (2, "assoc", 0, True),  # scoped_identifier.name of a call
        (2, "Type", 0, False),  # the path is not called
        (3, "generic", 0, True),  # turbofish
        (4, "b", 0, True),  # method with turbofish
        (5, "mine", 0, True),  # macro invocation
        (6, "mine", 0, True),  # scoped macro path
        (7, "foo", 0, True),  # call written in macro arguments
        (7, "bar", 0, True),  # method call in macro arguments
        (7, "x", 0, False),  # receiver in macro arguments
        (8, "new", 0, True),  # Type::new() in macro arguments
        (8, "mine", 0, True),  # macro invoked in macro arguments
        (9, "Shape", 0, False),  # struct literal
        (10, "value", 0, False),  # a reference, not a call
        (11, "fake", 0, False),  # comment
        (12, "fake", 0, False),  # string literal
    ],
)
def test_call_sites(line0, word, nth, call):
    assert _site(line0, word, nth) is call


GOLD = """trait Shape {
    fn area(&self) -> f64;
}
struct Sq;
impl Shape for Sq {
    fn area(&self) -> f64 {
        1.0
    }
}
fn total<T: Shape>(t: &T) -> f64 {
    t.area()
}
fn main() {
    println!("{}", Sq.area());
    let c = |x: f64| total(&Sq) + x;
    c(1.0);
}
#[cfg(unix)]
fn plat() {}
#[cfg(windows)]
fn plat() {}
#[cfg(test)]
mod tests {
    #[test]
    fn t() { super::total(&super::Sq); }
}
"""


def _occ(symbol: str, line0: int, sc: int, ec: int, definition: bool = False, enclosing=None) -> Occ:
    return Occ(symbol, 1 if definition else 0, (line0, sc, line0, ec), enclosing)


def test_gold_from_rust_analyzer_occurrences(tmp_path):
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "main.rs").write_text(GOLD)
    doc = Doc(
        path="src/main.rs",
        position_encoding=1,
        occurrences=[
            _occ("Shape#area().", 1, 7, 11, True),
            _occ("impl#[Sq][Shape]area().", 5, 7, 11, True),
            _occ("total().", 9, 3, 8, True),
            _occ("main().", 12, 3, 7, True, (10, 0, 16, 1)),  # rust-analyzer: from the doc comments / attributes above
            _occ("plat().", 18, 3, 7, True),  # #[cfg(windows)] plat is not compiled
            _occ("Shape#area().", 10, 6, 10),  # generic receiver: the trait method
            _occ("impl#[Sq][Shape]area().", 13, 22, 26),  # concrete receiver, in macro arguments
            _occ("total().", 14, 21, 26),  # in a closure: belongs to main
            _occ("local 1", 15, 4, 5),  # the closure is a local
            _occ("total().", 24, 20, 25),  # test code: no caller
        ],
    )
    g = derive_call_graph(tmp_path, [doc], [""], "rust")
    assert g["callees"] == {
        "src/main.rs#main@13": ["src/main.rs#Sq.area@6", "src/main.rs#total@10"],
        "src/main.rs#total@10": ["src/main.rs#Shape.area@2"],
    }
    assert g["classes"] == [["src/main.rs#plat@19", "src/main.rs#plat@21"]]  # cfg alternatives in one file
    assert g["stats"]["call_sites_module_level"] == 1
    main = g["callables"]["src/main.rs#main@13"]
    assert (main["start"], main["end"]) == (13, 17)  # tree-sitter start kept, enclosing end
