"""Ruby: the labeler's qualified names (the Ruby pack's convention:
constant paths, `#` instance and `.` singleton methods), call sites (calls
with and without parentheses, bare identifiers, `super`) and gold
derivation from a scip-ruby-shaped SCIP index."""

from rueca_bench.common import is_test_path
from rueca_bench.scip import Doc, Occ, derive_call_graph
from rueca_bench.treesitter import extract_functions, is_call_site, parse

SRC = """def top; end
module A
  class B::C < Base
    def inst; end
    def self.sing; end
    class << self
      def in_block; end
    end
    private def priv
      1
    end
  end
  class ::Top
    def m; end
  end
end
def Util.helper; end
Pair = Struct.new(:a) do
  def sum; end
end
"""


def _names(src: str) -> dict[str, tuple[str, int]]:
    return {f.qualname: (f.label, f.def_line) for f in extract_functions("lib/a.rb", src.encode(), "ruby")}


def test_ruby_qualified_names():
    assert _names(SRC) == {
        "top": ("Function", 1),
        "A::B::C#inst": ("Method", 4),
        "A::B::C.sing": ("Method", 5),
        "A::B::C.in_block": ("Method", 7),
        "A::B::C#priv": ("Method", 9),
        "Top#m": ("Method", 14),
        "Util.helper": ("Method", 17),
        "Pair#sum": ("Method", 19),
    }


CALLS = """class K
  def run(x)
    helper
    x.go(1)
    items.map { |i| i }
    y = 1
    y
    super
    Foo.new
  end
end
"""


def _call_at(line0: int, word: str) -> bool:
    tree = parse("lib/k.rb", CALLS.encode(), "ruby")
    col = CALLS.split("\n")[line0].index(word)
    return is_call_site(tree, "ruby", line0, col, col + len(word))


def test_ruby_call_sites():
    assert _call_at(2, "helper")
    assert _call_at(3, "go")
    assert _call_at(4, "items")
    assert _call_at(4, "map")
    assert _call_at(7, "super")
    assert _call_at(8, "new")
    assert not _call_at(1, "run")  # a definition name
    assert not _call_at(8, "Foo")  # a constant


def test_ruby_test_paths():
    for p in ["spec/models/user_spec.rb", "lib/foo_test.rb", "test/helper.rb"]:
        assert is_test_path(p), p
    assert not is_test_path("lib/rack/utils.rb")


GOLD = """class Base
  def save(x)
    x
  end
end

class Repo < Base
  def store(x)
    save(x)
    log
  end

  def log
    1
  end
end
"""


def _occ(symbol: str, line0: int, sc: int, ec: int, definition: bool = False) -> Occ:
    return Occ(symbol, 1 if definition else 0, (line0, sc, line0, ec), None)


def test_gold_from_scip_ruby_occurrences(tmp_path):
    (tmp_path / "lib").mkdir()
    (tmp_path / "lib" / "repo.rb").write_text(GOLD)
    doc = Doc(
        path="lib/repo.rb",
        position_encoding=1,
        occurrences=[
            _occ("Base#save().", 1, 6, 10, True),
            _occ("Repo#store().", 7, 6, 11, True),
            _occ("Repo#log().", 12, 6, 9, True),
            _occ("Base#save().", 8, 4, 8),  # `save(x)`
            _occ("Repo#log().", 9, 4, 7),  # bare `log`
            _occ("local 1", 8, 9, 10),
        ],
    )
    g = derive_call_graph(tmp_path, [doc], [""], "ruby")
    assert g["callees"] == {"lib/repo.rb#Repo#store@8": ["lib/repo.rb#Base#save@2", "lib/repo.rb#Repo#log@13"]}
