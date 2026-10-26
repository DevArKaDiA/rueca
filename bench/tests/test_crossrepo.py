"""Cross-repository edges (crossrepo.py): the independent labelers and the
precision / recall scorer."""

from pathlib import Path

from rueca_bench.crossrepo import PhpLib, PyLib, _php_uses, label_php, label_python, score_group, summarize


def write(root: Path, rel: str, text: str) -> None:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)


def test_python_labeler_follows_reexports_and_module_attributes(tmp_path):
    lib, app = tmp_path / "lib", tmp_path / "app"
    write(lib, "src/kit/__init__.py", "from .core import Gateway, charge\n")
    write(lib, "src/kit/core.py", "class Base:\n    pass\n\nclass Gateway(Base):\n    pass\n\ndef charge(x):\n    return x\n")
    write(app, "shop/pay.py", "import kit\nfrom kit import Gateway\nfrom kit.core import charge as c\n\nclass Retry(Gateway):\n    pass\n\ndef pay():\n    return kit.charge(1) + c(2)\n")
    write(app, "shop/unused.py", "from kit import Gateway\n")
    libs = [("acme/kit", PyLib(lib, ["kit"]))]
    assert libs[0][1].resolve("kit", "Gateway") == "kit.core.Gateway"
    imports, uses, ancestors, inherited = label_python(app, libs)
    assert ("shop/pay.py", "acme/kit", "src/kit/__init__.py") in imports
    assert ("shop/pay.py", "acme/kit", "src/kit/core.py") in imports
    assert {u[2] for u in uses if u[0] == "shop/pay.py"} == {"kit.core.Gateway", "kit.core.charge"}
    assert not any(u[0] == "shop/unused.py" for u in uses), "imported but unused"
    assert ancestors[("acme/kit", "kit.core.Gateway")] == {"kit.core.Base"}
    assert "kit.core.Gateway" in inherited[("shop/pay.py", "acme/kit")]


def test_php_labeler_uses_psr4_and_traits(tmp_path):
    lib, app = tmp_path / "lib", tmp_path / "app"
    write(lib, "composer.json", '{"autoload": {"psr-4": {"Acme\\\\Money\\\\": "src/"}}}')
    write(lib, "src/Money.php", "<?php\nnamespace Acme\\Money;\n\nclass Money extends Base\n{\n    use Arith;\n}\n")
    write(lib, "src/Base.php", "<?php\nnamespace Acme\\Money;\n\nabstract class Base\n{\n}\n")
    write(lib, "src/Arith.php", "<?php\nnamespace Acme\\Money;\n\ntrait Arith\n{\n}\n")
    write(app, "src/Pay.php", "<?php\nnamespace Shop;\n\nuse Acme\\Money\\{Money, Base as B};\n\nclass Pay\n{\n    use Helpers;\n    function f() { return Money::of(1); }\n}\n")
    aliases, lines = _php_uses((app / "src/Pay.php").read_text())
    assert aliases == {"Money": "Acme\\Money\\Money", "B": "Acme\\Money\\Base"} and lines == {4}
    php = PhpLib(lib)
    assert php.ancestors("Acme\\Money\\Money") == {"Acme\\Money\\Base", "Acme\\Money\\Arith"}
    imports, uses, ancestors, _ = label_php(app, [("acme/money", php)])
    assert ("src/Pay.php", "acme/money", "src/Money.php") in imports and ("src/Pay.php", "acme/money", "src/Base.php") in imports
    assert uses == {("src/Pay.php", "acme/money", "Acme\\Money\\Money")}, "`B` is imported, not used"


def test_score_group_precision_recall_and_reversed_edges():
    gold = (
        {("a.py", "lib", "lib/x.py")},
        {("a.py", "lib", "x.Thing"), ("a.py", "lib", "x.deco")},
        {("lib", "x.Thing"): {"x.Base"}},
        {},
    )
    e = lambda **kw: {"consumer": "app", "src_repo": "app", "dst_repo": "lib", "src_label": "Function", "dst_label": "Method", **kw}
    edges = [
        e(src_path="a.py", dst_path="lib/x.py", src_label="File", dst_label="File", kind="IMPORTS", src="a.py", dst="lib/x.py"),
        e(src_path="a.py", dst_path="lib/x.py", kind="CALLS", src="a.f", dst="x.Base.run"),  # inherited member: correct
        e(src_path="a.py", dst_path="lib/y.py", kind="CALLS", src="a.f", dst="y.other"),  # not imported: wrong
        # a library decorator on a consumer function points into the consumer
        {"consumer": "app", "src_repo": "lib", "dst_repo": "app", "src_label": "Function", "dst_label": "Function", "src_path": "lib/x.py", "dst_path": "a.py", "kind": "DECORATES", "src": "x.deco", "dst": "a.f"},
        # observed in the library (it consumes the app): not this pair's
        {"consumer": "lib", "src_repo": "lib", "dst_repo": "app", "src_label": "Function", "dst_label": "Function", "src_path": "lib/x.py", "dst_path": "a.py", "kind": "CALLS", "src": "x.g", "dst": "a.f"},
    ]
    s = score_group(edges, "app", {"lib": "lib"}, gold, "python")
    assert s["imports"] == {"predicted": 1, "gold": 1, "correct": 1, "found": 1}
    assert s["uses"] == {"predicted": 3, "gold": 2, "correct": 2, "found": 2}
    total = summarize([{"status": "ok", "libraries": [{"score": s}]}])
    assert total["all"]["precision"] == 3 / 4 and total["all"]["recall"] == 1.0
