"""Tree-sitter helpers: function/method extraction and call-site classification.

Used for (a) S1 gold (diff hunk -> enclosing function at the base commit),
(b) the grep baseline's function documents, and (c) S3 gold (is a SCIP
reference occurrence a call site? which callable encloses it?).
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from functools import lru_cache

import tree_sitter_javascript
import tree_sitter_python
import tree_sitter_typescript
from tree_sitter import Language, Node, Parser


@lru_cache(maxsize=None)
def _language(kind: str) -> Language:
    if kind == "python":
        return Language(tree_sitter_python.language())
    if kind == "typescript":
        return Language(tree_sitter_typescript.language_typescript())
    if kind == "tsx":
        return Language(tree_sitter_typescript.language_tsx())
    if kind == "javascript":
        return Language(tree_sitter_javascript.language())
    raise ValueError(kind)


def grammar_for(path: str) -> str | None:
    if path.endswith((".py", ".pyi")):
        return "python"
    if path.endswith(".tsx"):
        return "tsx"
    if path.endswith((".ts", ".mts", ".cts")):
        return "typescript"
    if path.endswith((".js", ".jsx", ".mjs", ".cjs")):
        return "javascript"
    return None


def parse(path: str, source: bytes):
    kind = grammar_for(path)
    if kind is None:
        return None
    parser = Parser(_language(kind))
    return parser.parse(source)


@dataclass(frozen=True)
class Func:
    name: str
    qualname: str  # local qualified name: Class.method / outer.inner
    label: str  # Function | Method
    start: int  # 1-based, declaration node start (after decorators)
    end: int  # 1-based inclusive
    def_line: int  # 1-based line of the name identifier
    outer_start: int  # 1-based, includes decorators
    indent: int  # column of the declaration start

    def as_dict(self) -> dict:
        return asdict(self)


_PY_CONTAINERS = {"class_definition", "function_definition"}
_ECMA_CLASS = {"class_declaration", "class", "abstract_class_declaration"}
_ECMA_FUNC_VALUES = {"arrow_function", "function_expression", "function", "generator_function"}


def _text(node: Node | None) -> str:
    if node is None or node.text is None:
        return ""
    return node.text.decode("utf-8", errors="replace")


def extract_functions(path: str, source: bytes) -> list[Func]:
    tree = parse(path, source)
    if tree is None:
        return []
    if grammar_for(path) == "python":
        return _py_functions(tree.root_node)
    return _ecma_functions(tree.root_node)


def _walk(node: Node):
    stack = [node]
    while stack:
        n = stack.pop()
        yield n
        stack.extend(reversed(n.children))


def _py_functions(root: Node) -> list[Func]:
    out: list[Func] = []
    for node in _walk(root):
        if node.type != "function_definition":
            continue
        name_node = node.child_by_field_name("name")
        name = _text(name_node)
        if not name:
            continue
        parts = [name]
        label = "Function"
        anc = node.parent
        first = True
        while anc is not None:
            if anc.type in _PY_CONTAINERS:
                parts.append(_text(anc.child_by_field_name("name")))
                if first and anc.type == "class_definition":
                    label = "Method"
                first = False
            anc = anc.parent
        outer = node.parent if node.parent is not None and node.parent.type == "decorated_definition" else node
        out.append(
            Func(
                name=name,
                qualname=".".join(reversed(parts)),
                label=label,
                start=node.start_point[0] + 1,
                end=node.end_point[0] + 1,
                def_line=name_node.start_point[0] + 1,
                outer_start=outer.start_point[0] + 1,
                indent=node.start_point[1],
            )
        )
    return out


def _ecma_name_and_label(node: Node) -> tuple[str, str, Node | None, Node] | None:
    """Returns (name, label, name_node, decl_node) for a callable definition node."""
    t = node.type
    if t in ("function_declaration", "generator_function_declaration"):
        nn = node.child_by_field_name("name")
        return (_text(nn), "Function", nn, node) if nn else None
    if t == "method_definition":
        nn = node.child_by_field_name("name")
        return (_text(nn).strip("'\"`"), "Method", nn, node) if nn else None
    if t in _ECMA_FUNC_VALUES:
        parent = node.parent
        if parent is None:
            return None
        if parent.type == "variable_declarator" and parent.child_by_field_name("value") == node:
            nn = parent.child_by_field_name("name")
            if nn is not None and nn.type == "identifier":
                decl = parent.parent if parent.parent is not None else parent
                return _text(nn), "Function", nn, decl
        if parent.type == "assignment_expression" and parent.child_by_field_name("right") == node:
            left = parent.child_by_field_name("left")
            if left is not None and left.type == "member_expression":
                nn = left.child_by_field_name("property")
                stmt = parent.parent if parent.parent is not None else parent
                return _text(nn), "Method", nn, stmt
            if left is not None and left.type == "identifier":
                stmt = parent.parent if parent.parent is not None else parent
                return _text(left), "Function", left, stmt
        if parent.type == "pair" and parent.child_by_field_name("value") == node:
            nn = parent.child_by_field_name("key")
            return _text(nn).strip("'\"`"), "Method", nn, parent
        if parent.type in ("public_field_definition", "field_definition") and parent.child_by_field_name("value") == node:
            nn = parent.child_by_field_name("name") or parent.child_by_field_name("property")
            return (_text(nn), "Method", nn, parent) if nn else None
        if t in ("function_expression", "function", "generator_function"):
            nn = node.child_by_field_name("name")
            if nn is not None:
                return _text(nn), "Function", nn, node
    return None


def _ecma_functions(root: Node) -> list[Func]:
    out: list[Func] = []
    for node in _walk(root):
        info = _ecma_name_and_label(node)
        if info is None:
            continue
        name, label, name_node, decl = info
        if not name or name_node is None:
            continue
        parts = [name]
        anc = decl.parent
        while anc is not None:
            if anc.type in _ECMA_CLASS:
                cn = anc.child_by_field_name("name")
                if cn is not None:
                    parts.append(_text(cn))
            else:
                inner = _ecma_name_and_label(anc)
                if inner is not None and inner[0]:
                    parts.append(inner[0])
            anc = anc.parent
        out.append(
            Func(
                name=name,
                qualname=".".join(reversed(parts)),
                label=label,
                start=decl.start_point[0] + 1,
                end=decl.end_point[0] + 1,
                def_line=name_node.start_point[0] + 1,
                outer_start=decl.start_point[0] + 1,
                indent=decl.start_point[1],
            )
        )
    return out


def innermost(funcs: list[Func], line: int) -> Func | None:
    best: Func | None = None
    for f in funcs:
        if f.start <= line <= f.end:
            if best is None or (f.end - f.start) < (best.end - best.start):
                best = f
    return best


def innermost_strict(funcs: list[Func], a: int, b: int) -> Func | None:
    """Innermost function containing both lines a and b."""
    best: Func | None = None
    for f in funcs:
        if f.start <= a and b <= f.end:
            if best is None or (f.end - f.start) < (best.end - best.start):
                best = f
    return best


# --- call-site classification -------------------------------------------------


def utf16_col_to_byte(line_text: str, col: int) -> int:
    units = 0
    for i, ch in enumerate(line_text):
        if units >= col:
            return len(line_text[:i].encode("utf-8"))
        units += 2 if ord(ch) > 0xFFFF else 1
    return len(line_text.encode("utf-8"))


def is_call_site(tree, grammar: str, line0: int, byte_col: int, end_byte_col: int) -> bool:
    node = tree.root_node.named_descendant_for_point_range((line0, byte_col), (line0, max(byte_col, end_byte_col - 1)))
    if node is None:
        return False
    parent = node.parent
    if parent is None:
        return False
    if grammar == "python":
        if parent.type == "call" and parent.child_by_field_name("function") == node:
            return True
        if parent.type == "attribute" and parent.child_by_field_name("attribute") == node:
            gp = parent.parent
            return gp is not None and gp.type == "call" and gp.child_by_field_name("function") == parent
        return False
    # ECMAScript
    if parent.type == "call_expression" and parent.child_by_field_name("function") == node:
        return True
    if parent.type == "new_expression" and parent.child_by_field_name("constructor") == node:
        return True
    if parent.type == "member_expression" and parent.child_by_field_name("property") == node:
        gp = parent.parent
        if gp is not None and gp.type in ("call_expression",) and gp.child_by_field_name("function") == parent:
            return True
        if gp is not None and gp.type == "new_expression" and gp.child_by_field_name("constructor") == parent:
            return True
    return False
