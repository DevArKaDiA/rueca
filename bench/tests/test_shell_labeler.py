"""Shell: the labeler's callables (the shell pack's names), the parser
repairs shared with the pack, test files, and the code-backed gold
(shellgold.py): sourced paths by literal tail, name lookup in the file, the
files it sources and the repository, ambiguity, call positions."""

from pathlib import Path

from rueca_bench.common import is_code_file, is_test_path, lang_family
from rueca_bench.scip import questions
from rueca_bench.shellgold import resolve_tail, shell_call_graph, source_tail
from rueca_bench.treesitter import extract_functions, grammar_for, shell_repair


def _names(src: str, path: str = "lib/x.sh") -> list[tuple[str, str, int, int, int]]:
    return [(f.qualname, f.label, f.def_line, f.start, f.end) for f in extract_functions(path, src.encode())]


def test_functions_are_flat_and_global():
    src = "f() { :; }\nfunction g {\n  inner() { :; }\n}\nfunction h() {\n  :\n}\nif x; then k() { :; }; fi\n"
    assert _names(src) == [
        ("f", "Function", 1, 1, 1),
        ("g", "Function", 2, 2, 4),
        ("inner", "Function", 3, 3, 3),
        ("h", "Function", 5, 5, 7),
        ("k", "Function", 8, 8, 8),
    ]


def test_grammar_files_and_tests():
    for p in ("a.sh", "a.bash", "a.zsh", "a.ksh", "t/a.bats"):
        assert grammar_for(p) == "bash", p
        assert is_code_file(p, "shell")
    assert grammar_for("bin/tool") is None
    assert lang_family("shell") == "shell"
    for p in ("test/a.sh", "x.bats", "scripts/test_a.sh", "scripts/a_test.bash", "spec/a_spec.sh"):
        assert is_test_path(p), p
    for p in ("scripts/latest.sh", "lib/testing.sh", "install.sh"):
        assert not is_test_path(p), p


def test_repairs_keep_lengths():
    cases = {
        b'x\n\\. "$D/a.sh"\n': b'x\n . "$D/a.sh"\n',
        b"c=${name:$i:1}": b"c=${name: i:1}",
        b"o=${1//&/\\&amp;}": b"o=${1//_/\\_amp;}",
        b"j=${(j:,:)parts}": b"j=${______parts}",
        b"echo \\$HOME && x": b"echo \\$HOME && x",
    }
    for src, want in cases.items():
        assert shell_repair(src) == want
        assert len(shell_repair(src)) == len(src)
    # nvm's alias bypass after a word: the function after it is still seen.
    src = 'source x/c.sh\n\\. "$NVM_DIR/nvm.sh"\nafter() {\n  :\n}\n'
    assert [n[0] for n in _names(src)] == ["after"]


def test_source_tails():
    assert source_tail('"$(dirname "$0")/../lib/b.sh"') == "lib/b.sh"
    assert source_tail("./lib/a.sh") == "lib/a.sh"
    assert source_tail('"${BASH_SOURCE%/*}/helpers.sh"') == "helpers.sh"
    assert source_tail('"$NVM_DIR/nvm.sh"') == "nvm.sh"
    assert source_tail("lib.sh") == "lib.sh"
    for dynamic in ('"$f"', '"$dir/$name.sh"', '"${name}.sh"', "$(find . -name x)", "~/x.sh", '"$plugin"'):
        assert source_tail(dynamic) is None, dynamic
    files = ["lib/a.sh", "lib/b.sh", "x/lib/b.sh", "nvm.sh"]
    assert resolve_tail("lib/a.sh", files) == "lib/a.sh"
    assert resolve_tail("lib/b.sh", files) is None  # two files end with it
    assert resolve_tail("x/lib/b.sh", files) == "x/lib/b.sh"
    assert resolve_tail("a.sh", files) == "lib/a.sh"


def _write(root: Path, files: dict[str, str]) -> Path:
    for rel, text in files.items():
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)
    return root


def _edges(graph: dict) -> set[tuple[str, str]]:
    return {(a.split("@")[0], b.split("@")[0]) for a, bs in graph["callees"].items() for b in bs}


def test_gold_name_lookup(tmp_path: Path):
    root = _write(
        tmp_path,
        {
            "main.sh": (
                "#!/bin/bash\n"
                'source "$(dirname "$0")/lib/a.sh"\n'
                ". ./lib/p1.sh\n"
                ". ./lib/p2.sh\n"
                "log() { echo main; }\n"
                "run() {\n"
                "  log x | from_a\n"
                "  x=$(from_b)\n"
                "  pick\n"
                "  ambient\n"
                "  command from_a\n"
                "  trap from_a EXIT\n"
                "  ./other.sh\n"
                "  $dyn\n"
                "  run\n"
                "}\n"
                "run\n"
            ),
            "lib/a.sh": 'source "${BASH_SOURCE%/*}/b.sh"\nfrom_a() {\n  from_b\n}\nlog() { echo a; }\n',
            "lib/b.sh": "from_b() {\n  :\n}\n",
            "lib/p1.sh": "pick() { echo 1; }\n",
            "lib/p2.sh": "pick() { echo 2; }\n",
            "lib/amb.sh": "ambient() {\n  :\n}\n",
            "test/helpers.sh": "ambient() { :; }\nhelper() {\n  from_a\n}\n",
        },
    )
    g = shell_call_graph(root, [""])
    assert _edges(g) == {
        ("main.sh#run", "main.sh#log"),  # own definition wins over the sourced one
        ("main.sh#run", "lib/a.sh#from_a"),
        ("main.sh#run", "lib/b.sh#from_b"),  # through a.sh, transitively
        ("main.sh#run", "lib/amb.sh#ambient"),  # unique outside tests
        ("main.sh#run", "main.sh#run"),  # recursion
        ("lib/a.sh#from_a", "lib/b.sh#from_b"),
    }
    # `pick` (two sourced files), `command`, `trap`, a script run and a
    # dynamic word give no edge; test files have no callables.
    assert not any(k.startswith("test/") for k in g["callables"])
    assert g["stats"]["targets_ambiguous"] == 1
    assert g["stats"]["sources_resolved"] == 4
    q = questions(g, 3)
    assert "main.sh#run@6" in q["callees"]


def test_gold_conditional_definitions_are_one_class(tmp_path: Path):
    root = _write(tmp_path, {"main.sh": "if a; then\n  emit() { echo 1; }\nelse\n  emit() { echo 2; }\nfi\nrun() {\n  emit\n}\n"})
    g = shell_call_graph(root, [""])
    assert g["classes"] == [["main.sh#emit@2", "main.sh#emit@4"]]
    assert sorted(g["callees"]["main.sh#run@6"]) == ["main.sh#emit@2", "main.sh#emit@4"]
