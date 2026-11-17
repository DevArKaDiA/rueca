"""TypeScript / JavaScript HTTP client labeler (tree-sitter).

A call is an HTTP call when its callee's last name is a verb wrapper
(`get`, `post`, `put`, `patch`, `delete` / `del`, `head`, `options`),
`fetch` (GET unless an object argument says `method: '…'`) or `request`
(method from `method: '…'`, else `*`), and one of its arguments renders to
a path (`'/…'`, `http(s)://…`, `` `${base}/…` ``, `'/a/' + id`, or a
`const` of the enclosing function bound to one of these).

One hop across functions: when the rendered path interpolates a parameter
of the enclosing function (`` `${host}/api/${endpoint}` ``), every call of
that function in the same file passing a string literal at that position
(`this.processStreamableRequest('generate', …)`) is a call site too, at the
caller, with the literal substituted.
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

from tree_sitter import Node

from ..treesitter import parse
from .keys import HOLE, canonical_http
from .model import CALL, Site, clip, files, read
from .pyclient import looks_like_path

TS_EXTS = (".ts", ".mts", ".cts", ".tsx", ".js", ".mjs", ".cjs", ".jsx")
VERBS = {"get": "GET", "post": "POST", "put": "PUT", "patch": "PATCH", "delete": "DELETE", "del": "DELETE", "head": "HEAD", "options": "OPTIONS"}
_FUNCS = {"method_definition", "function_declaration", "function_expression", "arrow_function", "generator_function_declaration", "function"}

Part = str | tuple[str, str | None]  # literal text, or ("hole", parameter name or None)


@dataclass
class _Fn:
    name: str
    params: list[str]
    local: dict[str, Node]


def _text(n: Node | None) -> str:
    return n.text.decode("utf-8", errors="replace") if n is not None and n.text is not None else ""


def _walk(node: Node) -> Iterator[Node]:
    stack = [node]
    while stack:
        n = stack.pop()
        yield n
        stack.extend(reversed(n.children))


def _fn_name(fn: Node) -> str:
    name = fn.child_by_field_name("name")
    if name is not None:
        return _text(name)
    parent = fn.parent
    if parent is not None and parent.type in ("variable_declarator", "pair", "assignment_expression", "public_field_definition"):
        key = parent.child_by_field_name("name") or parent.child_by_field_name("key") or parent.child_by_field_name("left")
        return _text(key).rsplit(".", 1)[-1]
    return ""


def _params(fn: Node) -> list[str]:
    ps = fn.child_by_field_name("parameters")
    if ps is None:
        p = fn.child_by_field_name("parameter")
        return [_text(p)] if p is not None else []
    out = []
    for p in ps.named_children:
        pat = p.child_by_field_name("pattern") if p.type in ("required_parameter", "optional_parameter") else p
        out.append(_text(pat) if pat is not None and pat.type == "identifier" else "")
    return out


def _locals(fn: Node) -> dict[str, Node]:
    seen: dict[str, list[Node]] = {}
    body = fn.child_by_field_name("body")
    for n in _walk(body) if body is not None else ():
        if n.type == "variable_declarator":
            name, value = n.child_by_field_name("name"), n.child_by_field_name("value")
            if name is not None and value is not None and name.type == "identifier":
                seen.setdefault(_text(name), []).append(value)
    return {k: v[0] for k, v in seen.items() if len(v) == 1}


def _enclosing(node: Node) -> Node | None:
    p = node.parent
    while p is not None and p.type not in _FUNCS:
        p = p.parent
    return p


def _qualname(fn: Node | None) -> str:
    parts = []
    while fn is not None:
        if fn.type in _FUNCS and _fn_name(fn):
            parts.append(_fn_name(fn))
        elif fn.type in ("class_declaration", "class", "abstract_class_declaration") and fn.child_by_field_name("name") is not None:
            parts.append(_text(fn.child_by_field_name("name")))
        fn = fn.parent
    return ".".join(reversed(parts))


def _string(node: Node) -> str:
    t = _text(node)
    return t[1:-1] if len(t) >= 2 else ""


def render(node: Node | None, fn: _Fn | None, depth: int = 0) -> list[Part] | None:
    """The parts a string expression builds (see the module docs)."""
    if node is None or depth > 4:
        return None
    t = node.type
    if t == "string":
        return [_string(node)]
    if t == "template_string":
        return [_template_part(c, fn) for c in node.named_children]
    if t == "binary_expression":
        return _render_concat(node, fn, depth)
    if t == "parenthesized_expression" and node.named_children:
        return render(node.named_children[0], fn, depth + 1)
    if t == "identifier" and fn is not None:
        return _render_name(_text(node), fn, depth)
    return None


def _render_concat(node: Node, fn: _Fn | None, depth: int) -> list[Part] | None:
    if _text(node.child_by_field_name("operator")) != "+":
        return None
    left = render(node.child_by_field_name("left"), fn, depth + 1)
    right = render(node.child_by_field_name("right"), fn, depth + 1)
    if left is None and right is None:
        return None
    return (left or [("hole", None)]) + (right or [("hole", None)])


def _render_name(name: str, fn: _Fn, depth: int) -> list[Part] | None:
    if name in fn.local:
        return render(fn.local[name], fn, depth + 1)
    if name in fn.params:
        return [("hole", name)]
    return None


def _template_part(child: Node, fn: _Fn | None) -> Part:
    if child.type != "template_substitution":
        return _text(child)
    inner = child.named_children[0] if child.named_children else None
    if inner is not None and inner.type == "identifier" and fn is not None and _text(inner) in fn.params and _text(inner) not in fn.local:
        return ("hole", _text(inner))
    return ("hole", None)


def joined(parts: list[Part], subst: dict[str, str] | None = None) -> str:
    out = []
    for p in parts:
        if isinstance(p, str):
            out.append(p)
        else:
            out.append((subst or {}).get(p[1] or "", HOLE))
    return "".join(out)


def _object_method(args: Node) -> str | None:
    for a in args.named_children:
        if a.type != "object":
            continue
        for pair in a.named_children:
            if pair.type == "pair" and _text(pair.child_by_field_name("key")).strip("'\"") == "method":
                v = pair.child_by_field_name("value")
                if v is not None and v.type == "string":
                    return _string(v).upper()
    return None


def _callee(call: Node) -> str:
    f = call.child_by_field_name("function")
    if f is None:
        return ""
    if f.type == "member_expression":
        return _text(f.child_by_field_name("property"))
    return _text(f)


def call_method(call: Node) -> str | None:
    """HTTP method of a client call by its callee (None: not an HTTP call)."""
    name = _callee(call)
    args = call.child_by_field_name("arguments")
    if name in VERBS:
        return VERBS[name]
    if name in ("fetch", "request") and args is not None:
        return _object_method(args) or ("GET" if name == "fetch" else "*")
    return None


@dataclass
class _Template:
    fn: str
    params: list[str]
    parts: list[Part]
    method: str


class _File:
    def __init__(self, repo: str, rel: str, text: str) -> None:
        self.repo, self.rel = repo, rel
        self.lines = text.splitlines()
        self.tree = parse(rel, text.encode())
        self.sites: list[Site] = []
        self.templates: list[_Template] = []

    def _fn(self, node: Node) -> _Fn | None:
        fn = _enclosing(node)
        return _Fn(_fn_name(fn), _params(fn), _locals(fn)) if fn is not None else None

    def _site(self, node: Node, key: str, framework: str) -> Site:
        line = node.start_point[0] + 1
        sym = _qualname(_enclosing(node))
        return Site("http", CALL, key, self.repo, self.rel, sym, line, clip(self.lines[line - 1]), framework, (sym.rsplit(".", 1)[-1], ""))

    def http_calls(self) -> None:
        for call in (n for n in _walk(self.tree.root_node) if n.type == "call_expression"):
            method = call_method(call)
            args = call.child_by_field_name("arguments")
            if method is None or args is None:
                continue
            fn = self._fn(call)
            parts = _path_argument(args, fn)
            if parts is None:
                continue
            self.sites.append(self._site(call, canonical_http(method, joined(parts)), "ts-http"))
            if fn is not None and fn.name and any(isinstance(p, tuple) and p[1] for p in parts):
                self.templates.append(_Template(fn.name, fn.params, parts, method))

    def hops(self) -> None:
        """Callers of a function whose path interpolates its parameters."""
        by_name = {t.fn: t for t in self.templates}
        for call in (n for n in _walk(self.tree.root_node) if n.type == "call_expression"):
            t = by_name.get(_callee(call))
            args = call.child_by_field_name("arguments")
            if t is None or args is None:
                continue
            subst = _literal_args(args, t.params)
            if subst:
                self.sites.append(self._site(call, canonical_http(t.method, joined(t.parts, subst)), f"ts-http via {t.fn}"))


def _path_argument(args: Node, fn: _Fn | None) -> list[Part] | None:
    """The parts of the first argument that renders to a path."""
    for a in args.named_children:
        parts = render(a, fn)
        if parts is not None and looks_like_path(joined(parts)):
            return parts
    return None


def _literal_args(args: Node, params: list[str]) -> dict[str, str]:
    out = {}
    for i, a in enumerate(args.named_children):
        if i < len(params) and params[i] and a.type == "string":
            out[params[i]] = _string(a)
    return out


def ts_http_calls(repo: str, root: Path) -> list[Site]:
    out: list[Site] = []
    for rel in files(root, TS_EXTS):
        if rel.endswith(".d.ts"):
            continue
        f = _File(repo, rel, read(root, rel))
        if f.tree is None:
            continue
        f.http_calls()
        f.hops()
        out += f.sites
    return out
