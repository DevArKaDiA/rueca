"""Kotlin: the labeler's qualified names (constructors named like their
class, companion and object members, local functions), call sites (calls,
member calls, trailing lambdas, supertype constructor calls) and gold
derivation from a scip-java-shaped SCIP index (its Kotlin indexer)."""

import pytest

from rueca_bench.common import is_test_path
from rueca_bench.scip import Doc, Occ, derive_call_graph, kotlin_def_name, scip_last_name
from rueca_bench.treesitter import extract_functions, is_call_site, parse

SRC = """package a.b

open class Base(val z: Int) {
    open fun baz() {}
}
interface Iface {
    fun baz()
}
class Foo(val x: Int) : Base(x), Iface {
    constructor(s: String) : this(s.length)
    val y: Int
        get() = bar()
    fun bar(): Int {
        fun local() = 1
        return local()
    }
    companion object {
        fun make() = Foo(1)
    }
    override fun baz() {
        super.baz()
        listOf(this).map { it.bar() }
    }
}
class OnlySecondary : Base {
    constructor() : super(0)
}
object Single {
    fun go() {}
}
class `-Quoted`
fun String.ext(): Int = length
fun top() {
    Foo.make()
    Single.go()
    "s".ext()
    Foo.Companion.make()
}
"""


def _funcs() -> dict[str, tuple[str, int, int, int]]:
    return {f.qualname + f"@{f.def_line}": (f.label, f.start, f.end, f.def_line) for f in extract_functions("src/Foo.kt", SRC.encode())}


def test_qualified_names_and_constructors():
    got = _funcs()
    assert sorted(got) == sorted(
        [
            "Base.Base@3",
            "Base.baz@4",
            "Iface.baz@7",  # interfaces have no constructor
            "Foo.Foo@9",  # primary constructor on the class name
            "Foo.Foo@10",  # secondary constructor on its first line
            "Foo.bar@13",
            "Foo.bar.local@14",
            "Foo.Companion.make@18",
            "Foo.baz@20",
            "OnlySecondary.OnlySecondary@26",  # no implicit primary constructor
            "Single.go@29",
            "-Quoted.-Quoted@31",  # backquotes removed
            "ext@32",  # the receiver type is not part of the name
            "top@33",
        ]
    )
    assert got["Foo.Foo@9"] == ("Method", 9, 9, 9)  # extent = the class header
    assert got["Foo.bar.local@14"][0] == "Function"
    assert got["top@33"] == ("Function", 33, 38, 33)


@pytest.mark.parametrize(
    "path,test",
    [
        ("src/main/kotlin/a/Foo.kt", False),
        ("src/test/kotlin/a/FooTest.kt", True),
        ("lib/src/commonTest/kotlin/a/Foo.kt", True),
        ("lib/src/jvmTest/kotlin/a/Foo.kt", True),
        ("app/src/androidTest/java/a/Foo.kt", True),
        ("lib/src/main/kotlin/a/FooTests.kt", True),
        ("lib/src/main/kotlin/a/TypeSpec.kt", False),
    ],
)
def test_test_paths(path, test):
    assert is_test_path(path) is test


CALLS = """fun f() {
    a.b().c()
    g { x }
    h<Int>(1)
    q?.r()
    val k = ::top
    x.prop
}
class C : a.Base<Int>(1), I
"""


@pytest.mark.parametrize(
    "line0,word,nth,call",
    [
        (1, "a", 0, False),  # receiver
        (1, "b", 0, True),
        (1, "c", 0, True),
        (2, "g", 0, True),  # trailing lambda
        (3, "h", 0, True),  # type arguments
        (4, "r", 0, True),  # safe call
        (5, "top", 0, False),  # callable reference
        (6, "prop", 0, False),  # property read
        (8, "Base", 0, True),  # supertype constructor call
        (8, "a", 0, False),
        (8, "I", 0, False),  # interface: no call
    ],
)
def test_call_sites(line0, word, nth, call):
    tree = parse("x.kt", CALLS.encode())
    line = CALLS.split("\n")[line0]
    col = -1
    for _ in range(nth + 1):
        col = line.index(word, col + 1)
    assert is_call_site(tree, "kotlin", line0, col, col + len(word)) is call


def test_scip_symbol_names():
    assert scip_last_name("scip-java maven . . a/b/Foo#`<init>`(+1).") == "<init>"
    assert scip_last_name("scip-java maven . . a/b/Foo#") == "Foo"
    assert scip_last_name("scip-java maven . . a/b/Foo#Companion#make().") == "make"
    assert scip_last_name("scip-java maven . . a/b/Foo#`<init>`().(x)") == ""
    assert kotlin_def_name("scip-java maven . . a/b/Foo#`<init>`(+1).", "/** doc */") == "Foo"
    assert kotlin_def_name("scip-java maven . . a/b/E#values().", "E") is None  # synthetic member on the class name
    assert kotlin_def_name("scip-java maven . . a/b/`-Quoted`#", "`-Quoted`") == "-Quoted"
    assert kotlin_def_name("local 3", "local") == "local"


GOLD = """package a

open class Base(val z: Int)
class Foo(val x: Int) : Base(x) {
    /** Secondary. */
    constructor(s: String) : this(s.length)
    fun bar(): Int {
        fun local() = make()
        return local()
    }
    fun make() = Foo("s")
}
enum class E { A }
fun use() = E.values()
"""


def _occ(symbol, line0, sc, ec, definition=False, enclosing=None):
    full = symbol if symbol.startswith("local ") else f"scip-java maven . . {symbol}"
    return Occ(full, 1 if definition else 0, (line0, sc, line0, ec), enclosing)


def test_gold_from_scip_java_kotlin_occurrences(tmp_path):
    (tmp_path / "Foo.kt").write_text(GOLD)
    doc = Doc(
        path="Foo.kt",
        occurrences=[
            _occ("a/Base#", 2, 11, 15, True, (2, 0, 2, 27)),
            _occ("a/Base#`<init>`().", 2, 11, 15, True, (2, 11, 2, 27)),
            _occ("a/Foo#", 3, 6, 9, True, (3, 0, 11, 1)),
            _occ("a/Foo#`<init>`().", 3, 6, 9, True, (3, 6, 3, 21)),
            _occ("a/Base#", 3, 24, 28),  # `: Base(x)` references the class
            _occ("a/Foo#`<init>`(+1).", 4, 4, 18, True, (4, 4, 5, 43)),  # on the KDoc
            _occ("a/Foo#bar().", 6, 8, 11, True, (6, 4, 9, 5)),
            _occ("local 0", 7, 12, 17, True, (7, 8, 7, 28)),
            _occ("a/Foo#make().", 7, 22, 26),  # in the local function
            _occ("local 0", 8, 15, 20),
            _occ("a/Foo#make().", 10, 8, 12, True, (10, 4, 10, 25)),
            _occ("a/Foo#`<init>`(+1).", 10, 17, 20),
            _occ("a/E#", 12, 11, 12, True, (12, 0, 12, 18)),
            _occ("a/E#`<init>`().", 12, 11, 12, True, (12, 0, 12, 18)),
            _occ("a/E#values().", 12, 11, 12, True, (12, 0, 12, 18)),  # synthetic, on the class name
            _occ("a/use().", 13, 4, 7, True, (13, 0, 13, 22)),
            _occ("a/E#values().", 13, 12, 18),  # not E's constructor
        ],
    )
    g = derive_call_graph(tmp_path, [doc], [""], "kotlin")
    assert g["callees"] == {
        "Foo.kt#Foo.Foo@4": ["Foo.kt#Base.Base@3"],
        "Foo.kt#Foo.bar.local@8": ["Foo.kt#Foo.make@11"],
        "Foo.kt#Foo.bar@7": ["Foo.kt#Foo.bar.local@8"],
        "Foo.kt#Foo.make@11": ["Foo.kt#Foo.Foo@6"],
    }
    assert g["classes"] == []
    foo = g["callables"]["Foo.kt#Foo.Foo@4"]
    assert (foo["start"], foo["end"]) == (4, 4)  # the enclosing range (the whole class) is not used
