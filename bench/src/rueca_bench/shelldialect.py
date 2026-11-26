"""Shell dialects for the bench labeler: the port of the shell pack's
dialect detection and same-length repairs (`rueca-lang-shell`
`dialect.rs`, `repair.rs`, `extract.rs: parse_file`), so the labeler reads
every shell file with the grammar, and the text, the pack reads.

- `detect(path, source)` → (`sh` | `bash` | `zsh` | `ksh` | `bats`, rule):
  the `.bats` extension, a ShellCheck `shell=` directive in the leading
  comments, the `#!` line, zsh's `#compdef` / `#autoload`, the file name,
  an editor modeline, a directory named after a shell, content signals,
  else `sh`.
- zsh files are parsed with tree-sitter-zsh, the others with
  tree-sitter-bash; when the dialect's grammar leaves errors and the other
  one reads the file without any, the other tree is used (never for bats).
- `repair(source, grammar)`: the rules of `repair.rs` for that grammar.

`bench/tests/test_shell_labeler.py` checks both against the cases of the
Rust unit tests.
"""

from __future__ import annotations

import re

HEAD_BYTES = 2048
CONTENT_BYTES = 256 * 1024
MODELINE_LINES = 5

_NAMES = {
    "sh": "sh", "dash": "sh", "ash": "sh", "posix": "sh", "posh": "sh", "yash": "sh", "busybox": "sh",
    "bash": "bash",
    "zsh": "zsh",
    "ksh": "ksh", "mksh": "ksh", "pdksh": "ksh", "oksh": "ksh", "loksh": "ksh",
    "bats": "bats",
}  # fmt: skip


def dialect_of_name(name: str) -> str | None:
    """`Dialect::parse`: a shell name with its version suffix dropped."""
    return _NAMES.get(name.rstrip("0123456789.").lower())


def shebang_interpreter(head: bytes) -> str | None:
    """`Language::shebang_interpreter`."""
    line = head.split(b"\n", 1)[0]
    if not line.startswith(b"#!"):
        return None
    try:
        words = line[2:].decode("utf-8").split()
    except UnicodeDecodeError:
        return None
    if not words:
        return None
    prog = words[0].rsplit("/", 1)[-1]
    if prog in ("env", "busybox"):
        rest = [w for w in words[1:] if not w.startswith("-") and "=" not in w]
        if not rest:
            return None
        prog = rest[0].rsplit("/", 1)[-1]
    return prog.rstrip("0123456789.") or prog


def _lines(text: str) -> list[str]:
    """Rust's `str::lines`: split at `\\n`, a trailing `\\r` dropped."""
    out = text.split("\n")
    if out and out[-1] == "":
        out.pop()
    return [line[:-1] if line.endswith("\r") else line for line in out]


def _directive(head: str) -> str | None:
    for line in _lines(head):
        line = line.strip()
        if not line:
            continue
        if not line.startswith("#"):
            return None
        rest = line[1:].lstrip()
        if not rest.startswith("shellcheck"):
            continue
        for word in rest[len("shellcheck") :].split():
            if word.startswith("shell="):
                d = dialect_of_name(word[len("shell=") :])
                if d:
                    return d
                break
    return None


_BY_BARE_NAME = {
    "zshrc": "zsh", "zshenv": "zsh", "zprofile": "zsh", "zlogin": "zsh", "zlogout": "zsh",
    "bashrc": "bash", "bash_profile": "bash", "bash_login": "bash", "bash_logout": "bash", "bash_aliases": "bash",
    "kshrc": "ksh", "mkshrc": "ksh",
    "profile": "sh",
}  # fmt: skip


def _by_file_name(name: str) -> str | None:
    bare = name.lstrip(".")
    ext = bare.rsplit(".", 1)[1] if "." in bare else ""
    if ext in ("zsh", "zsh-theme"):
        return "zsh"
    if ext == "bash":
        return "bash"
    if ext == "ksh":
        return "ksh"
    if ext == "sh":
        return None
    return _BY_BARE_NAME.get(bare)


def _modeline_of(line: str) -> str | None:
    line = line.strip()
    if not line.startswith("#"):
        return None

    def value_after(key: str) -> str | None:
        at = line.find(key)
        if at < 0:
            return None
        m = re.match(r"[A-Za-z0-9._-]*", line[at + len(key) :], re.ASCII)
        return dialect_of_name(m.group(0)) if m else None

    if "vim:" in line or "vi:" in line or "ex:" in line:
        for key in ("filetype=", "ft=", "syntax=", "syn="):
            d = value_after(key)
            if d:
                return d
        return None
    if "-*-" in line:
        return value_after("sh-shell: ") or value_after("sh-shell:")
    return None


def _modeline(source: bytes) -> str | None:
    head = source[:HEAD_BYTES].decode("utf-8", errors="replace")
    tail = source[max(0, len(source) - HEAD_BYTES) :].decode("utf-8", errors="replace")
    for line in _lines(head)[:MODELINE_LINES] + _lines(tail)[-MODELINE_LINES:]:
        d = _modeline_of(line)
        if d:
            return d
    return None


_DIR_WORDS = {"zsh": "zsh", "ohmyzsh": "zsh", "zshrc": "zsh", "zfunctions": "zsh", "bash": "bash", "bashrc": "bash", "ksh": "ksh", "ksh93": "ksh", "mksh": "ksh"}


def _by_directory(path: str) -> str | None:
    for d in reversed(path.split("/")[:-1]):
        here = None
        for word in re.split(r"[-_.]", d.lower()):
            w = _DIR_WORDS.get(word)
            if w is None:
                continue
            if here is not None and here != w:
                return None
            here = w
        if here is not None:
            return here
    return None


ZSH_COMMANDS = ("setopt", "unsetopt", "autoload", "zstyle", "zmodload", "compdef", "emulate", "zle", "bindkey", "zparseopts", "add-zsh-hook", "compinit", "zcompile")
BASH_COMMANDS = ("shopt", "mapfile", "readarray", "complete", "compgen", "compopt")
ZSH_TEXT = ("${(", "$+commands", "${+", "$+functions")
BASH_TEXT = ("BASH_SOURCE", "BASH_REMATCH", "COMPREPLY", "PROMPT_COMMAND", "declare -", "local -n ")
KSH_TEXT = ("set -A ", "${.sh.", "print -u", "KSH_VERSION")
EXTENDED_TEXT = ("[[ ", "<<<", "=(", "<(", "&>", "$'")


def signals(text: str) -> dict:
    """`dialect::signals`: each distinct signal once."""
    seen: set[str] = set()
    s = {"zsh": 0, "bash": 0, "ksh": 0, "extended": 0, "portable": False}

    def once(key: str) -> bool:
        if key in seen:
            return False
        seen.add(key)
        return True

    for line in _lines(text):
        line = line.lstrip()
        if not line or line.startswith("#"):
            continue
        if "ZSH_VERSION" in line or "BASH_VERSION" in line:
            s["portable"] = True
        first = re.split(r"[ \t\n\x0c\r;]", line, maxsplit=1)[0]
        if first in ZSH_COMMANDS and once(first):
            s["zsh"] += 2 if first in ("autoload", "bindkey") else 3
        if first in BASH_COMMANDS and once(first):
            s["bash"] += 2 if first == "complete" else 3
        if first == "function" and once("function"):
            s["extended"] += 1
        for key, field, weight in ((ZSH_TEXT, "zsh", 3), (BASH_TEXT, "bash", 3), (KSH_TEXT, "ksh", 3), (EXTENDED_TEXT, "extended", 1)):
            for k in key:
                if k in line and once(k):
                    s[field] += weight
    return s


def verdict(s: dict) -> str | None:
    beyond_posix = s["bash"] + s["zsh"] + s["ksh"] + s["extended"] > 0
    if s["portable"]:
        return "bash" if beyond_posix else None
    if s["zsh"] >= 3 and s["zsh"] > s["bash"] and s["zsh"] > s["ksh"]:
        return "zsh"
    if s["ksh"] >= 3 and s["ksh"] > s["bash"] and s["ksh"] > s["zsh"]:
        return "ksh"
    return "bash" if beyond_posix else None


def detect(path: str, source: bytes) -> tuple[str, str]:
    """`dialect::detect`: the dialect and the rule that chose it."""
    name = path.rsplit("/", 1)[-1]
    if name.endswith(".bats"):
        return "bats", "Extension"
    head_bytes = source[:HEAD_BYTES]
    head = head_bytes.decode("utf-8", errors="replace")
    d = _directive(head)
    if d:
        return d, "Directive"
    prog = shebang_interpreter(head.encode("utf-8"))
    d = dialect_of_name(prog) if prog else None
    if d:
        return d, "Shebang"
    lines = _lines(head)
    first = lines[0] if lines else ""
    if first.startswith(("#compdef", "#autoload")):
        return "zsh", "ZshHeader"
    d = _by_file_name(name)
    if d:
        return d, "FileName"
    d = _modeline(source)
    if d:
        return d, "Modeline"
    d = _by_directory(path)
    if d:
        return d, "Directory"
    d = verdict(signals(source[:CONTENT_BYTES].decode("utf-8", errors="replace")))
    return (d, "Content") if d else ("sh", "Default")


def grammar_of(dialect: str) -> str:
    """The grammar a dialect is read with first (`bash` | `zsh`)."""
    return "zsh" if dialect == "zsh" else "bash"


def _zsh_flags_end(source: bytes, open_: int) -> int | None:
    """The `)` closing zsh parameter flags opened at `open_` (`(j:,:)`)."""
    rest = source[open_ + 1 : open_ + 33]
    close = rest.find(b")")
    if close < 0 or any(c in b"\n(${}" for c in rest[:close]):
        return None
    return open_ + 1 + close


def _name_byte(b: int) -> bool:
    return b < 128 and (chr(b).isalnum() or b == 0x5F)


def _alpha(b: int) -> bool:
    return b < 128 and chr(b).isalpha()


def repair(source: bytes, grammar: str = "bash") -> bytes:
    """The same-length repairs of `repair.rs` for `grammar` (its module docs
    list the rules): byte for byte the pack's text."""
    bash = grammar == "bash"
    out = bytearray(source)
    n = len(source)

    def at(i: int) -> int:
        return source[i] if i < n else 0x20

    line_start, depth, redirected, arithmetic, i = True, 0, False, False, 0
    while i < n:
        b = source[i]
        nxt = at(i + 1)
        step = 1
        if b == 0x5C:  # backslash
            if not bash:
                step = 2
            elif line_start and (_alpha(nxt) or nxt in b"._"):
                out[i] = 0x20
            else:
                if depth and nxt in b"&<>;":
                    out[i + 1] = 0x5F
                step = 2
        elif b == 0x24 and nxt == 0x7B:  # ${
            step = 2
            if bash:
                depth += 1
                if at(i + 2) == 0x28:
                    close = _zsh_flags_end(source, i + 2)
                    if close is not None:
                        out[i + 2 : close + 1] = b"_" * (close - i - 1)
                        step = close + 1 - i
        elif b == 0x24 and nxt == 0x24:  # $$
            after = at(i + 2)
            if not bash and after not in b" \t\n\x0c\r\"'-:({":
                out[i + 1] = 0x21
            step = 2
        elif b == 0x7D and depth:
            depth -= 1
        elif b == 0x23 and not bash and line_start:  # '#NAME=' / '#NAME+='
            k = i + 1
            if _alpha(at(k)) or at(k) == 0x5F:
                while k < n and _name_byte(source[k]):
                    k += 1
                if at(k) == 0x3D or source.startswith(b"+=", k):
                    out[k] = 0x20
                step = k - i
        elif b == 0x25 and not bash and nxt == 0x7B and not (i > 0 and _name_byte(source[i - 1])):  # %{
            out[i + 1] = 0x5F
            step = 2
        elif b == 0x7D:
            if line_start and not bash and nxt == 0x20 and at(i + 2) in b'"$':
                out[i + 1] = 0x3B
                step = 2
            elif source.startswith(b"} 0<", i) and at(i + 4) not in b"<(":
                out[i + 2] = 0x20
        elif b == 0x3A and bash and depth and nxt == 0x24 and (_alpha(at(i + 2)) or at(i + 2) == 0x5F):
            out[i + 1] = 0x20
        elif bash and depth and b in b"&<>;":
            out[i] = 0x5F
        elif b == 0x3C and bash and redirected and source.startswith(b"<<<", i):
            out[i] = out[i + 1] = 0x20
            step = 3
        elif b == 0x3E:
            redirected = True
        elif b == 0x0A:
            redirected = arithmetic = False
        elif b in b";|":
            redirected = False
        elif b == 0x28 and nxt == 0x28:
            arithmetic = True
            step = 2
        elif b == 0x30 and arithmetic and source.startswith(b"0x$", i) and not (i > 0 and _name_byte(source[i - 1])):
            out[i] = out[i + 1] = 0x20
            step = 2
        elif b == 0x66 and depth == 0 and not (i > 0 and _name_byte(source[i - 1])) and source.startswith(b"for ", i):
            k = i + 4
            while k < n and _name_byte(source[k]):
                k += 1
            after_do = at(k + 3)
            if k > i + 4 and source.startswith(b" do", k) and (after_do in b" \t\n\x0c\r" or after_do == 0x3B):
                out[k] = 0x3B
                step = k + 3 - i
        line_start = b == 0x0A or (line_start and b in b" \t")
        i += step
    return bytes(out)
