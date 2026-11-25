"""Swift: the labeler's qualified names (members of extensions, `init` /
`deinit` / `subscript` / operators, protocol requirements, local
functions), call sites (calls, member calls, constructions, `.init`, calls
the grammar attaches to a whole `+` / `!` expression), the scoped `Tests/`
directory, and gold derivation from an index-store dump."""

import json

from rueca_bench.common import is_test_path
from rueca_bench.indexstore import read_indexstore
from rueca_bench.scip import derive_call_graph
from rueca_bench.treesitter import extract_functions, is_call_site, parse

SRC = """protocol Shape {
    func area() -> Double
}
struct Point: Shape {
    var x: Double
    init(x: Double) { self.x = x }
    deinit {}
    func area() -> Double { helper() + other() }
    func helper() -> Double {
        func local() -> Double { 1 }
        return local()
    }
    func other() -> Double { 0 }
    subscript(i: Int) -> Double { x }
    static func + (a: Point, b: Point) -> Point { a }
}
extension Point {
    func scaled() -> Point {
        let p: Point = .init(x: 2)
        return Box<Int>(1).wrap(Point(x: x))
    }
}
func top() {
    if !Point(x: 1).helper().isZero {}
}
"""


def _funcs() -> dict[str, str]:
    return {f"{f.qualname}@{f.def_line}": f.label for f in extract_functions("Sources/A/a.swift", SRC.encode(), "swift")}


def test_qualified_names():
    got = _funcs()
    assert sorted(got) == sorted(
        [
            "Shape.area@2",
            "Point.init@6",
            "Point.deinit@7",
            "Point.area@8",
            "Point.helper@9",
            "Point.helper.local@10",
            "Point.other@13",
            "Point.subscript@14",
            "Point.+@15",
            "Point.scaled@18",
            "top@23",
        ]
    )
    assert got["Point.helper.local@10"] == "Function"
    assert got["Point.scaled@18"] == "Method"
    assert got["top@23"] == "Function"


def _site(line: int, needle: str) -> bool:
    tree = parse("a.swift", SRC.encode(), "swift")
    text = SRC.split("\n")[line - 1]
    col = text.index(needle)
    return is_call_site(tree, "swift", line - 1, col, col + len(needle))


def test_call_sites():
    assert _site(8, "helper")  # `helper() + other()`: the left operand
    assert _site(8, "other")  # the grammar's `(helper() + other)()`
    assert _site(11, "local")
    assert _site(19, "init")  # `.init(x: 2)`
    assert _site(20, "Box")  # `Box<Int>(1)`
    assert _site(20, "wrap")
    assert _site(20, "Point")
    assert _site(24, "helper")  # `!Point(x: 1).helper()`
    assert not _site(24, "isZero")
    assert not _site(5, "x")


def test_tests_directory_is_scoped_to_swift():
    assert is_test_path("Tests/CoreTests/PointTests.swift")
    assert is_test_path("Tests/CoreTests/Helpers.swift")
    assert not is_test_path("Tests/fixtures/data.json")
    assert not is_test_path("Sources/Core/Point.swift")


def test_gold_from_an_index_store_dump(tmp_path):
    root = tmp_path / "pkg"
    (root / "Sources/A").mkdir(parents=True)
    (root / "Sources/A/a.swift").write_text(SRC)
    lines = SRC.split("\n")

    def occ(usr: str, line: int, needle: str, roles: int) -> dict:
        col = lines[line - 1].index(needle)
        return {"usr": usr, "name": needle, "kind": 0, "subkind": 0, "props": 0, "roles": roles, "line": line, "col": col + 1, "tok": [col, col + len(needle)], "rel": []}

    DEF, REF_CALL, IMPLICIT = 2, 4 | 32, 256
    dump = {
        "files": {
            "Sources/A/a.swift": [
                occ("s:area", 8, "area", DEF),
                occ("s:helper", 9, "helper", DEF),
                occ("s:other", 13, "other", DEF),
                occ("s:init", 6, "init", DEF),
                occ("s:helper", 8, "helper", REF_CALL),
                occ("s:other", 8, "other", REF_CALL),
                occ("s:init", 20, "Point", REF_CALL),
                occ("s:helper", 24, "helper", REF_CALL),
                # a getter the compiler calls: implicit, not a call written by name
                occ("s:other", 5, "x", REF_CALL | IMPLICIT),
            ]
        }
    }
    path = tmp_path / "index.json"
    path.write_text(json.dumps(dump))
    graph = derive_call_graph(root, read_indexstore(path), ["Sources/"], "swift")
    by_name = {f["qualname"]: k for k, f in graph["callables"].items()}
    assert sorted(graph["callees"][by_name["Point.area"]]) == sorted([by_name["Point.helper"], by_name["Point.other"]])
    assert graph["callees"][by_name["Point.scaled"]] == [by_name["Point.init"]]
    assert graph["callees"][by_name["top"]] == [by_name["Point.helper"]]
