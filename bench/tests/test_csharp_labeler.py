"""C#: the labeler's qualified names (the C# pack's convention), call sites
(methods, generic and conditional calls, constructions), test projects,
and gold derivation from a scip-dotnet-shaped SCIP index: constructors
reached through the type of `new T(…)`, local functions bound per
document, and calls of generic overloads recorded with the first
overload's symbol."""

from rueca_bench.common import is_test_path
from rueca_bench.scip import Doc, Occ, derive_call_graph
from rueca_bench.treesitter import call_arity_at, csharp_signatures, extract_functions, is_call_site, parse

SRC = """namespace App.Core;

public class Store<T> : IStore
{
    public Store(int n) { }
    ~Store() { }
    public void Add(T item)
    {
        int Local(int k) => k;
        Local(1);
    }
    public static Store<T> operator +(Store<T> a, Store<T> b) => a;
    public static implicit operator string(Store<T> s) => "";
    void IStore.Run() { }
    public string Name { get { return Format(); } }
    private string Format() => "";
    public class Inner
    {
        public void Act() { }
    }
}
public interface IStore { void Run(); }
public record Person(string Name) { public string Greet() => Name; }
public struct Point { public int Length() => 0; }
"""


def _names(src: str) -> dict[str, tuple[str, int]]:
    return {f.qualname: (f.label, f.def_line) for f in extract_functions("src/Store.cs", src.encode(), "csharp")}


def test_labeler_names_follow_the_pack():
    names = _names(SRC)
    assert names == {
        "Store.Store": ("Method", 5),
        "Store.~Store": ("Method", 6),
        "Store.Add": ("Method", 7),
        "Store.Add.Local": ("Function", 9),
        "Store.operator+": ("Method", 12),
        "Store.implicit operator string": ("Method", 13),
        "Store.IStore.Run": ("Method", 14),
        "Store.Format": ("Method", 16),
        "Store.Inner.Act": ("Method", 19),
        "IStore.Run": ("Method", 22),
        "Person.Greet": ("Method", 23),
        "Point.Length": ("Method", 24),
    }


CALLS = """class C
{
    void M(Svc s, Opt o)
    {
        s.Run();
        Helper();
        s?.Go();
        Map<int>(1);
        s.Get<int>();
        var x = new Foo(1);
        var y = new A.B.Bar();
        var z = new Box<int>();
        var p = s.Name;
    }
}
"""


def _site(word: str, nth: int = 0) -> tuple[int, int, int]:
    lines = CALLS.split("\n")
    seen = 0
    for i, line in enumerate(lines):
        col = line.find(word)
        while col >= 0:
            if seen == nth:
                return i, col, col + len(word)
            seen += 1
            col = line.find(word, col + 1)
    raise AssertionError(word)


def test_call_sites():
    tree = parse("src/C.cs", CALLS.encode(), "csharp")
    call = lambda w, n=0: is_call_site(tree, "csharp", *_site(w, n))
    for w in ["Run", "Helper", "Go", "Map", "Get", "Foo", "Bar", "Box"]:
        assert call(w), w
    assert not call("Name")
    assert call_arity_at(tree, *_site("Map")) == 1
    assert call_arity_at(tree, *_site("Run")) == 0


def test_test_projects_are_test_paths():
    assert is_test_path("src/Shop.Tests/OrderSupport.cs")
    assert is_test_path("tests/Shop.UnitTests/A.cs")
    assert is_test_path("src/Shop/OrderTests.cs")
    assert not is_test_path("src/Shop/Order.cs")


GOLD_SRC = """class Order
{
    public Order(int id) { }
    public Order(string s) { }
    public void Write(int level, string t) { }
    public void Write<T>(int level, string t, T value) { }
    public void Log<T>(string t, T value)
    {
        Write(1, t, value);
        int Twice(int k) => k * 2;
        Twice(2);
    }
    public static Order Make() => new Order(1);
}
"""


def _occ(symbol: str, line: int, word: str, roles: int = 0, nth: int = 0) -> Occ:
    lines = GOLD_SRC.split("\n")
    col = -1
    for _ in range(nth + 1):
        col = lines[line].find(word, col + 1)
    return Occ(symbol, roles, (line, col, line, col + len(word)), None)


def test_scip_dotnet_gold_repairs(tmp_path):
    (tmp_path / "Order.cs").write_text(GOLD_SRC)
    p = "scip-dotnet nuget . . App/Order#"
    occs = [
        _occ(p, 0, "Order", 1),
        _occ(p + "`.ctor`().", 2, "Order", 1),
        _occ(p + "`.ctor`(+1).", 3, "Order", 1),
        _occ(p + "Write().", 4, "Write", 1),
        _occ(p + "Write(+1).", 5, "Write", 1),
        _occ(p + "Log().", 6, "Log", 1),
        # scip-dotnet: the generic overload's call carries the first symbol
        _occ(p + "Write().", 8, "Write"),
        _occ("local 3", 9, "Twice", 1),
        _occ("local 3", 10, "Twice"),
        _occ(p + "Make().", 12, "Make", 1),
        # `new Order(1)`: a reference to the type
        _occ(p, 12, "Order", nth=1),
    ]
    graph = derive_call_graph(tmp_path, [Doc(path="Order.cs", occurrences=occs)], [""], "csharp")
    callees = {k.split("#")[1]: v for k, v in graph["callees"].items()}
    assert [c.split("#")[1] for c in callees["Order.Log@7"]] == ["Order.Log.Twice@10", "Order.Write@6"]
    # the constructors of one type are one class; `new` reaches it
    assert [c.split("#")[1] for c in callees["Order.Make@13"]] == ["Order.Order@3"]
    assert any(sorted(x.split("#")[1] for x in c) == ["Order.Order@3", "Order.Order@4"] for c in graph["classes"])
    sigs = csharp_signatures(parse("Order.cs", GOLD_SRC.encode(), "csharp"))
    assert sigs[(6, "Write")] == (3, 3, True) and sigs[(5, "Write")] == (2, 2, False)
