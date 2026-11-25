"""Shell: the labeler's callables (the shell pack's names), the parser
repairs shared with the pack, test files, and the code-backed gold
(shellgold.py): sourced paths by literal tail, name lookup in the file, the
files it sources and the repository, ambiguity, call positions."""

from pathlib import Path

from rueca_bench.common import is_code_file, is_test_path, lang_family
from rueca_bench.scip import questions
from rueca_bench.shellgold import resolve_tail, shell_call_graph, source_tail
from rueca_bench.shelldialect import detect, repair, shebang_interpreter
from rueca_bench.treesitter import error_nodes, extract_functions, grammar_for, parse_shell, shell_repair


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
        b"o=${1//&/\\&amp;}": b"o=${1//_/\\_amp_}",
        b"j=${(j:,:)parts}": b"j=${______parts}",
        b"echo \\$HOME && x": b"echo \\$HOME && x",
    }
    for src, want in cases.items():
        assert shell_repair(src) == want
        assert len(shell_repair(src)) == len(src)
    # nvm's alias bypass after a word: the function after it is still seen.
    src = 'source x/c.sh\n\\. "$NVM_DIR/nvm.sh"\nafter() {\n  :\n}\n'
    assert [n[0] for n in _names(src)] == ["after"]


def test_dialects_like_the_pack():
    """The cases of `rueca-lang-shell` `dialect.rs` (its unit tests)."""
    cases = [
        ("test/x.bats", "#!/usr/bin/env zsh\n", ("bats", "Extension")),
        ("x.sh", "#!/bin/sh\n# shellcheck shell=bash\n", ("bash", "Directive")),
        ("x.sh", "# shellcheck shell=ksh disable=SC2034\nx=1\n", ("ksh", "Directive")),
        ("x.sh", "x=1\n# shellcheck shell=zsh\n", ("sh", "Default")),
        ("bin/tool", "#!/bin/dash\necho\n", ("sh", "Shebang")),
        ("bin/tool", "#!/bin/busybox sh\necho\n", ("sh", "Shebang")),
        ("bin/tool", "#!/usr/bin/env -S bash -eu\necho\n", ("bash", "Shebang")),
        ("bin/tool.zsh", "#!/bin/ksh93\necho\n", ("ksh", "Shebang")),
        ("x.zsh", "#!/usr/bin/env python3\n", ("zsh", "FileName")),
        ("completions/_git", "#compdef git\n_git() { :; }\n", ("zsh", "ZshHeader")),
        ("functions/x.sh", "#autoload\n", ("zsh", "ZshHeader")),
        ("themes/robbyrussell.zsh-theme", "x=1\n", ("zsh", "FileName")),
        ("home/.zlogout", "x=1\n", ("zsh", "FileName")),
        ("home/.bash_profile", "x=1\n", ("bash", "FileName")),
        ("home/.mkshrc", "x=1\n", ("ksh", "FileName")),
        ("home/.profile", "x=1\n", ("sh", "FileName")),
        ("x.sh", "x=1\n# vim: set ft=zsh sw=2:\n", ("zsh", "Modeline")),
        ("x.sh", "# -*- mode: sh; sh-shell: bash -*-\nx=1\n", ("bash", "Modeline")),
        ("x.sh", "echo 'vim: ft=zsh'\n", ("sh", "Default")),
        ("shell/zsh/init.sh", "x=1\n", ("zsh", "Directory")),
        ("etc/bash_completion.d/tool.sh", "x=1\n", ("bash", "Directory")),
        ("contrib/ksh93/x.sh", "x=1\n", ("ksh", "Directory")),
        ("bash-zsh/x.sh", "x=1\n", ("sh", "Default")),
        ("zsh/bash/x.sh", "x=1\n", ("bash", "Directory")),
        ("bashful/x.sh", "x=1\n", ("sh", "Default")),
        ("x.sh", "f() { echo hi; }\nf\n", ("sh", "Default")),
        ("x.sh", "if [[ -n $x ]]; then y=(a b); fi\n", ("bash", "Content")),
        ("x.sh", "dir=${BASH_SOURCE[0]%/*}\n", ("bash", "Content")),
        ("x.sh", "setopt extendedglob\nprint ${(j:,:)xs}\n", ("zsh", "Content")),
        ("x.sh", "autoload -U compinit\ncompinit\n", ("zsh", "Content")),
        ("x.sh", "bindkey -e\n", ("bash", "Content")),
        ("x.sh", "set -A xs a b\nprint -u2 x\n", ("ksh", "Content")),
        ("x.sh", "# setopt extendedglob\necho\n", ("sh", "Default")),
        ("nvm.sh", 'if [ -n "$ZSH_VERSION" ]; then\n  setopt local_options\n  autoload -U x\nfi\n', ("bash", "Content")),
        ("x.sh", '[ -n "$BASH_VERSION" ] && echo bash\n', ("sh", "Default")),
        ("x.sh", "", ("sh", "Default")),
    ]
    for path, src, want in cases:
        assert detect(path, src.encode()) == want, (path, src)
    assert detect("x.sh", b"\xff\xfe\n") == ("sh", "Default")
    assert shebang_interpreter(b"#!/usr/bin/env -S LC_ALL=C bash -e\nx") == "bash"
    assert shebang_interpreter(b"#!/usr/bin/python3.12") == "python"
    assert shebang_interpreter(b"echo") is None


def test_repairs_per_grammar_like_the_pack():
    """The cases of `repair.rs` (its unit tests), both rule sets."""
    bash = {
        'cat > f <<<"x"\n': 'cat > f   <"x"\n',
        'cat <<<"x" > f\nread a <<<"$b"': 'cat <<<"x" > f\nread a <<<"$b"',
        "a > f; b <<<x\na > f | b <<<x": "a > f; b <<<x\na > f | b <<<x",
        "t=$f.tmp.$$.zwc": "t=$f.tmp.$$.zwc",
        '} "$1"': '} "$1"',
        "#FOO=1": "#FOO=1",
        "C=%{$fg[red]%}": "C=%{$fg[red]%}",
    }
    zsh = {
        'cat > f <<<"x"': 'cat > f <<<"x"',
        't=$f.tmp.$$.zwc; pid=$$; echo $$ "$$" $$': 't=$f.tmp.$!.zwc; pid=$!; echo $$ "$$" $$',
        '() {\n  a\n} "$1" &|\n} $x\n': '() {\n  a\n};"$1" &|\n};$x\n',
        'f() {\n  a\n} > f\n  } "$1"': 'f() {\n  a\n} > f\n  };"$1"',
        '#FOO="x" bar\n  #B+=1\n#foo\n# A=1\nA=1 #B=2': '#FOO "x" bar\n  #B =1\n#foo\n# A=1\nA=1 #B=2',
        'C=%{$fg[red]%}"::%{$x%}" ${x%{*}': 'C=%_$fg[red]%}"::%_$x%}" ${x%{*}',
        "\\. x\nc=${s:$i:1} o=${1//&/x} j=${(j:,:)parts}": "\\. x\nc=${s:$i:1} o=${1//&/x} j=${(j:,:)parts}",
    }
    both = {
        "for arg do echo; done": "for arg;do echo; done",
        "  for a do\n": "  for a;do\n",
        "for a in x; do :; done; for a\ndo :; done": "for a in x; do :; done; for a\ndo :; done",
        "for a dog": "for a dog",
        "wait_for x do": "wait_for x do",
        "n=$((0x${h} * 2)) m=$(( 0x$a ))": "n=$((  ${h} * 2)) m=$((   $a ))",
        "echo 0x$a; n=$((0x10)); (( n = a0x$b ))": "echo 0x$a; n=$((0x10)); (( n = a0x$b ))",
        "{ a; } 0</dev/null": "{ a; }  </dev/null",
        "}\n} 0<&3\n": "}\n}  <&3\n",
        "{ a; } 0<<EOF": "{ a; } 0<<EOF",
    }
    for grammar, cases in (("bash", bash | both), ("zsh", zsh | both)):
        for src, want in cases.items():
            assert repair(src.encode(), grammar) == want.encode(), (grammar, src)
    for s in (b"", b"${", b"${(", b"x=${(a", b"\\", b"for ", b"for x", b"0x$", b"} 0<", b"$$", b"<<<", b"}", b"#", b"#a", b"#a+", b"%"):
        assert len(repair(s, "bash")) == len(s) and len(repair(s, "zsh")) == len(s)


def test_grammar_per_dialect():
    zsh = "for f ($ZSH/lib/*.zsh(N)) { source $f }\nzfn() {\n  print ${(j:,:)parts}\n}\n() { anon }\n"
    tree, dialect, grammar = parse_shell("lib/x.zsh", zsh.encode())
    assert (dialect, grammar, error_nodes(tree)) == ("zsh", "zsh", 0)
    assert [n[0] for n in _names(zsh, "lib/x.zsh")] == ["zfn"]
    # the dialect's grammar leaves errors, the other one none: the other tree
    src = "#!/bin/bash\nfor x (a b) { helper $x }\nhelper() { :; }\n"
    tree, dialect, grammar = parse_shell("x.sh", src.encode())
    assert (dialect, grammar, error_nodes(tree)) == ("bash", "zsh", 0)
    assert [n[0] for n in _names(src, "x.sh")] == ["helper"]
    # both leave errors: the dialect's tree; bats files are only read as bash
    tree, dialect, grammar = parse_shell("z.sh", b"#!/bin/sh\nf() {\n  if then fi\n")
    assert (dialect, grammar, error_nodes(tree) > 0) == ("sh", "bash", True)
    tree, dialect, grammar = parse_shell("t.bats", b"for x (a b) { helper $x }\n")
    assert (dialect, grammar, error_nodes(tree) > 0) == ("bats", "bash", True)


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
