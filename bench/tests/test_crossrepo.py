"""Cross-repository edges (crossrepo.py): the independent labelers and the
precision / recall scorer."""

from pathlib import Path

from rueca_bench.crossrepo import (
    PhpLib,
    PyLib,
    RustLib,
    _php_decls,
    _php_trait_uses,
    _php_uses,
    _rust_scan,
    _rust_strip,
    _rust_use_tree,
    compatible,
    label_php,
    label_python,
    label_rust,
    pick_release,
    satisfies,
    score_group,
    summarize,
)


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


# The PHP statement scanners replaced one-regex forms with nested / overlapping
# quantifiers (super-linear backtracking); the expectations below are what
# those regexes matched on the same text, quirks included.
PHP = (
    "<?php\nnamespace App;\n\nuse Foo\\Bar;\nuse  function strlen;\nuse function strlen;\nuse const X;\n"
    "use A\\B as C, D;\n\nabstract\nfinal class Pay extends Base implements I, J\n{\n    use T1, T2;\n  use T3;\n"
    "    $x->use T4;\n    $o = new class Anon {};\n}\n$y = \"class Fake {\";\nenum Suit: string {}\nreadonly classy Z {}\n"
)


def test_php_declarations_first_on_their_line_after_modifiers():
    decls = [(d.kind, d.name, d.header, PHP[d.end - 1]) for d in _php_decls(PHP)]
    # modifiers may sit on the previous line; `new class`, a string and `classy` are not declarations
    assert decls == [("class", "Pay", " extends Base implements I, J\n", "{"), ("enum", "Suit", ": string ", "{")]
    assert [d.end for d in _php_decls(PHP)] == [164, 277]


def test_php_top_level_uses_skip_functions_and_constants():
    aliases, lines = _php_uses(PHP)
    # `use  function` (two spaces) was accepted by the former lookahead; kept as is
    assert aliases == {"Bar": "Foo\\Bar", "function strlen": "function strlen", "C": "A\\B", "D": "D"}
    assert lines == {4, 5, 8}


def test_php_trait_uses_first_on_their_line():
    body = PHP[PHP.index("{") + 1 :]
    assert _php_trait_uses(body) == ["T1", "T2", "T3"], "`$x->use T4;` is not a statement"


# --- Rust ----------------------------------------------------------------------------------

KIT_LIB = """//! Kit. `pub mod hidden;` in a doc comment declares nothing.
#[macro_use]
mod macros;
cfg_net! {
    pub mod net;
}
pub mod io;
mod task;
#[path = "sys_unix.rs"]
mod sys;
pub mod prelude;
pub mod time {
    pub fn sleep(_ms: u64) {}
}
pub use task::spawn;
pub use kit_macros::main;
"""

KIT_FILES = {
    "Cargo.toml": '[workspace]\nmembers = ["kit", "kit-macros"]\n\n[workspace.package]\nversion = "1.4.0"\n',
    "kit/Cargo.toml": '[package]\nname = "kit"\nversion.workspace = true\n',
    "kit/src/lib.rs": KIT_LIB,
    "kit/src/macros.rs": '#[macro_export]\nmacro_rules! count {\n    ($x:expr) => { { let s = "}"; $x } };\n}\nmacro_rules! cfg_net {\n    ($($i:item)*) => { $($i)* };\n}\n',
    "kit/src/net/mod.rs": "mod tcp;\ncfg_net! {\n    pub use self::tcp::listener::TcpListener;\n}\npub fn lookup(_h: &str) {}\n",
    "kit/src/net/tcp/mod.rs": "pub(crate) mod listener;\n",
    "kit/src/net/tcp/listener.rs": "pub struct TcpListener;\nimpl TcpListener {\n    pub fn bind(_a: &str) -> Self { TcpListener }\n}\nimpl crate::io::ReadExt for TcpListener {}\n",
    "kit/src/io.rs": "mod read_ext;\npub use read_ext::ReadExt;\n",
    "kit/src/io/read_ext.rs": "pub trait ReadExt {\n    fn read_all(&mut self) -> usize { 0 }\n    fn peek_byte(&self);\n}\n",
    "kit/src/task.rs": "mod spawn;\npub use spawn::spawn;\n",
    "kit/src/task/spawn.rs": "pub fn spawn<F>(_f: F) {}\n",
    "kit/src/sys_unix.rs": "pub struct Fd(i32);\n",
    "kit/src/prelude.rs": "pub use crate::io::ReadExt;\n",
    "kit-macros/Cargo.toml": '[package]\nname = "kit-macros"\nversion = "1.4.0"\n\n[lib]\nproc-macro = true\n',
    "kit-macros/src/lib.rs": "pub fn main() {}\n",
}

APP_MAIN = """use kit::{net::{self, TcpListener}, spawn as go};
use kit::io::ReadExt as _;
use kit::prelude::*;
use kit_old::net::TcpListener as Old;
// use kit::time::sleep;

#[kit::main]
async fn main() {
    let mut l = TcpListener::bind("addr");
    go(async {});
    kit::time::sleep(1);
    net::lookup("host");
    let _s = "kit::sys::Fd";
    let _c = '"';
    l.read_all();
    kit::count!(1);
}
"""

APP_FILES = {
    "Cargo.toml": '[package]\nname = "app"\nversion = "0.1.0"\n\n[dependencies]\nkit = "1.2"\nkit_old = { package = "kit", version = "0.9" }\n\n[dev-dependencies]\nkit-macros = { version = "1" }\n',
    "src/main.rs": APP_MAIN,
    "src/lib.rs": "use kit::net::TcpListener;\nmod tests;\npub fn serve(_l: TcpListener) {}\n",
    "src/tests.rs": "use super::*;\nfn t() { let _ = TcpListener::bind(\"a\"); }\n",
}


def test_rust_strip_comments_strings_chars_not_lifetimes():
    src = 'let a = "x // y"; // gone\n/* a /* nested */ still */ fn f<\'a>(s: &\'a str) -> char { \'{\' }\nlet r = r#"raw " }"#; b"b";'
    out = _rust_strip(src)
    assert len(out) == len(src) and out.count("\n") == src.count("\n")
    assert "gone" not in out and "nested" not in out and "still" not in out and "{'" not in out
    assert "<'a>" in out and "&'a str" in out, "lifetimes are kept"
    assert "raw" not in out and out.count("{") == 1 and out.count("}") == 1


def test_rust_use_tree_groups_renames_self_and_globs():
    toks = ["kit", "::", "{", "net", "::", "{", "self", ",", "TcpListener", "as", "L", "}", ",", "io", "::", "*", ",", "spawn", "}"]
    assert _rust_use_tree(toks) == [(("kit", "net"), "net"), (("kit", "net", "TcpListener"), "L"), (("kit", "io"), None), (("kit", "spawn"), "spawn")]


def test_rust_scan_sees_through_macro_bodies_not_into_blocks():
    f = _rust_scan(KIT_LIB + "fn body() { mod not_a_module; struct Local; }\nmacro_rules! m { () => { pub mod nope; } }\n")
    assert ((), "net") in f.mods and ((), "sys") in f.mods and f.mods[((), "sys")]["path"] == "sys_unix.rs"
    assert ((), "hidden") not in f.mods and ((), "not_a_module") not in f.mods and ((), "nope") not in f.mods
    assert f.mods[((), "time")]["inline"] and f.items[(("time",), "sleep")] == "fn"
    assert ((), "Local") not in f.items and f.items[((), "m")] == "macro_rules"


def test_rust_library_module_tree_reexports_and_names(tmp_path):
    for rel, text in KIT_FILES.items():
        write(tmp_path / "kit", rel, text)
    lib = RustLib(tmp_path / "kit")
    assert {c: (v["package"], v["version"]) for c, v in lib.crates.items()} == {"kit": ("kit", "1.4.0"), "kit_macros": ("kit-macros", "1.4.0")}
    res, _ = lib.resolve_extern("kit", ("net", "TcpListener", "bind"))
    assert lib.qn(res[1], res[2]) == "kit::net::tcp::listener::TcpListener" and lib.file_of(res) == "kit/src/net/tcp/listener.rs"
    res, _ = lib.resolve_extern("kit", ("spawn",))
    assert lib.qn(res[1], res[2]) == "kit::task::spawn::spawn", "re-export followed, the function wins over the module"
    assert lib.qn(*lib.resolve_extern("kit", ("main",))[0][1:]) == "kit_macros::main", "into another crate of the repository"
    assert lib.qn(*lib.resolve_extern("kit", ("count",))[0][1:]) == "kit::macros::count", "#[macro_export] at the crate root"
    assert lib.qn(*lib.resolve_extern("kit", ("time", "sleep"))[0][1:]) == "kit::time.sleep", "inline module: local name m.f"
    assert lib.path_of("kit::sys_unix::Fd") == "kit/src/sys_unix.rs", "#[path] module"
    assert lib.ancestors("kit::net::tcp::listener::TcpListener") == {"kit::io::read_ext::ReadExt"}


def test_rust_labeler_imports_uses_and_dependency_ranges(tmp_path):
    for rel, text in KIT_FILES.items():
        write(tmp_path / "kit", rel, text)
    for rel, text in APP_FILES.items():
        write(tmp_path / "app", rel, text)
    imports, uses, ancestors, _ = label_rust(tmp_path / "app", [("acme/kit", RustLib(tmp_path / "kit"))])
    main = {x[2] for x in imports if x[0] == "src/main.rs"}
    # `kit_old` asks kit 0.9: outside 1.4's range, not mapped; the commented `use` is no import
    assert main == {"kit/src/net/mod.rs", "kit/src/net/tcp/listener.rs", "kit/src/task/spawn.rs", "kit/src/io/read_ext.rs", "kit/src/prelude.rs"}
    used = {x[2] for x in uses if x[0] == "src/main.rs"}
    assert used == {
        "kit::net::tcp::listener::TcpListener",  # imported by name, used
        "kit::task::spawn::spawn",  # renamed `go`
        "kit::io::read_ext::ReadExt",  # `as _`: used through its method `read_all`
        "kit_macros::main",  # attribute path
        "kit::time.sleep",  # path-qualified
        "kit::net::lookup",  # through the imported module `net`
        "kit::macros::count",  # exported macro
    }, "`kit::sys::Fd` inside a string is no use"
    assert ("src/tests.rs", "acme/kit", "kit::net::tcp::listener::TcpListener") in uses, "`use super::*` brings the parent's imports"
    assert ancestors[("acme/kit", "kit::net::tcp::listener::TcpListener")] == {"kit::io::read_ext::ReadExt"}


def test_rust_scoring_owner_separator():
    gold = (set(), {("a.rs", "lib", "kit::net::TcpListener")}, {}, {})
    e = {"consumer": "app", "src_repo": "app", "dst_repo": "lib", "src_label": "Function", "dst_label": "Method", "src_path": "a.rs", "dst_path": "net.rs", "kind": "CALLS", "src": "app::main", "dst": "kit::net::TcpListener.bind"}
    s = score_group([e, {**e, "dst": "kit::net::TcpListenerX.bind"}], "app", {"lib": "lib"}, gold, "rust")
    assert s["uses"] == {"predicted": 2, "gold": 1, "correct": 1, "found": 1}


# --- release pins --------------------------------------------------------------------------


def test_requirements_cargo_and_pep440():
    assert satisfies("v0.14.27", "0.14.24") and not satisfies("v0.15.0", "0.14.24") and not satisfies("0.14.23", "0.14.24")
    assert satisfies("1.39.2", "1.25.0") and not satisfies("2.0.0", "1") and satisfies("0.0.3", "^0.0.3") and not satisfies("0.0.4", "^0.0.3")
    assert satisfies("1.2.9", "~1.2.3") and not satisfies("1.3.0", "~1.2.3") and satisfies("1.0.0-rc.4", "=1.0.0-rc.4")
    assert satisfies("1.5.0", ">=1.2, <2") and not satisfies("2.0.0", ">=1.2, <2")
    assert satisfies("3.1.5", ">=3.1.0", "pypi") and satisfies("3.1.9", "~=3.1", "pypi") and not satisfies("4.0", "~=3.1", "pypi")
    assert satisfies("3.0.2", "==3.0.*", "pypi") and not satisfies("3.1.0", "==3.0.*", "pypi") and not satisfies("3.0.1", "!=3.0.1", "pypi")
    # compatibility range only: the pinned version may sit below the minimum
    assert compatible("0.3.1", "0.3.2") and not compatible("0.14.27", "=1.0.0-rc.4") and not compatible("0.4.0", "0.3")


def test_pick_release_highest_meeting_requirement_before_date():
    tags = [
        ("v0.14.26", "a", "2023-04-13T20:09:57Z"),
        ("v0.14.27", "b", "2023-06-26T16:09:27Z"),
        ("v0.14.28", "c", "2023-12-18T14:48:21Z"),
        ("v1.0.0-rc.4", "d", "2023-07-10T00:00:00Z"),
        ("v1.0.0", "e", "2023-11-15T00:00:00Z"),
    ]
    assert pick_release(tags, "0.14.24", "2023-07-16T01:40:31+02:00") == tags[1]
    assert pick_release(tags, "=1.0.0-rc.4", "2023-07-16T01:40:31+02:00") == tags[3]
    assert pick_release(tags, "2", "2030-01-01T00:00:00Z") is None
