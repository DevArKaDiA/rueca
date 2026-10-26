"""Tree-sitter helpers: function/method extraction and call-site classification.

Used for (a) S1 gold (diff hunk -> enclosing function at the base commit),
(b) the grep baseline's function documents, and (c) S3 gold (is a SCIP
reference occurrence a call site? which callable encloses it?).
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
from functools import lru_cache

import tree_sitter_c
import tree_sitter_cpp
import tree_sitter_go
import tree_sitter_java
import tree_sitter_javascript
import tree_sitter_php
import tree_sitter_python
import tree_sitter_typescript
from tree_sitter import Language, Node, Parser

from .common import load_manifest

# Python grammar bindings the manifest may name in `[grammars.<name>] binding =
# "<module>:<function>"`. Adding a grammar = a pinned dependency in
# pyproject.toml, one import above, one entry here and the manifest data.
_BINDINGS = {
    "tree_sitter_c": tree_sitter_c,
    "tree_sitter_cpp": tree_sitter_cpp,
    "tree_sitter_go": tree_sitter_go,
    "tree_sitter_java": tree_sitter_java,
    "tree_sitter_javascript": tree_sitter_javascript,
    "tree_sitter_php": tree_sitter_php,
    "tree_sitter_python": tree_sitter_python,
    "tree_sitter_typescript": tree_sitter_typescript,
}


@lru_cache(maxsize=None)
def _grammars() -> dict[str, dict]:
    return dict(load_manifest().get("grammars", {}))


@lru_cache(maxsize=None)
def _ext_map() -> list[tuple[str, str]]:
    """(extension, grammar), longest extension first; grammars sharing an
    extension (C and C++ `.h`) keep manifest order."""
    pairs = [(ext, name) for name, g in _grammars().items() for ext in g.get("extensions", [])]
    return sorted(pairs, key=lambda p: (-len(p[0]), p[0]))


@lru_cache(maxsize=None)
def _language(kind: str) -> Language:
    spec = _grammars().get(kind)
    if spec is None:
        raise ValueError(kind)
    mod, _, fn = spec["binding"].partition(":")
    return Language(getattr(_BINDINGS[mod], fn)())


def grammar_for(path: str, language: str | None = None) -> str | None:
    """Grammar of a path. When several grammars claim the extension (`.h`:
    C and C++), the one whose `languages` lists the instance `language` wins;
    without a hint, the first declared in the manifest."""
    first = None
    for ext, name in _ext_map():
        if path.endswith(ext):
            if first is None:
                first = (ext, name)
            elif ext != first[0]:
                break
            if language is None or language in _grammars()[name].get("languages", [name]):
                return name
    return first[1] if first else None


def parse(path: str, source: bytes, language: str | None = None):
    kind = grammar_for(path, language)
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


_ECMA_CLASS = {"class_declaration", "class", "abstract_class_declaration"}
_ECMA_FUNC_VALUES = {"arrow_function", "function_expression", "function", "generator_function"}


def _text(node: Node | None) -> str:
    if node is None or node.text is None:
        return ""
    return node.text.decode("utf-8", errors="replace")


def extract_functions(path: str, source: bytes, language: str | None = None) -> list[Func]:
    kind = grammar_for(path, language)
    if kind is None:
        return []
    tree = parse(path, source, language)
    spec = _grammars()[kind]
    labeler = spec.get("labeler", "nesting")
    if labeler == "ecma":
        return _ecma_functions(tree.root_node)
    if labeler == "go":
        return _go_functions(tree.root_node)
    if labeler == "c":
        return _c_functions(tree.root_node, spec)
    return _nesting_functions(tree.root_node, spec)


def _walk(node: Node):
    stack = [node]
    while stack:
        n = stack.pop()
        yield n
        stack.extend(reversed(n.children))


def _nesting_functions(root: Node, spec: dict) -> list[Func]:
    """Data-driven labeler for grammars whose callables are named declaration
    nodes nested in named containers (manifest `[grammars.<name>]`):

    - `callables`: node type -> label (`Function` / `Method`); a `Function`
      whose nearest named ancestor (container or callable) is a container is
      a `Method` (Python `def` in a class body);
    - `containers`: class-like node types contributing to the qualified name;
    - `separator`: qualified-name separator (`.` Python, `::` PHP); the
      qualified name joins every enclosing container and callable name
      (`Class.method`, `outer.inner`, `Class::method`), unless `flat_functions`
      lists callable types whose name is global (PHP nested `function`);
    - `wrappers`: node types that wrap a declaration and carry its outer start
      (Python `decorated_definition`).
    """
    callables: dict[str, str] = spec["callables"]
    containers = set(spec.get("containers", []))
    named = containers | set(callables)
    sep = spec.get("separator", ".")
    flat = set(spec.get("flat_functions", []))
    wrappers = set(spec.get("wrappers", []))
    out: list[Func] = []
    for node in _walk(root):
        label = callables.get(node.type)
        if label is None:
            continue
        name_node = node.child_by_field_name("name")
        name = _text(name_node)
        if not name:
            continue
        parts = [name]
        anc = node.parent
        first = True
        while anc is not None:
            if anc.type in named:
                if node.type not in flat:
                    parts.append(_text(anc.child_by_field_name("name")) or "class@anonymous")
                if first and anc.type in containers and label == "Function":
                    label = "Method"
                first = False
            anc = anc.parent
        outer = node.parent if node.parent is not None and node.parent.type in wrappers else node
        out.append(
            Func(
                name=name,
                qualname=sep.join(reversed(parts)),
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


def _func(name: str, qualname: str, label: str, decl: Node, name_node: Node, outer: Node | None = None) -> Func:
    outer = outer or decl
    return Func(
        name=name,
        qualname=qualname,
        label=label,
        start=decl.start_point[0] + 1,
        end=decl.end_point[0] + 1,
        def_line=name_node.start_point[0] + 1,
        outer_start=outer.start_point[0] + 1,
        indent=decl.start_point[1],
    )


def _go_receiver_type(recv: Node | None) -> str:
    """`func (s *Server[T]) M()` -> `Server`."""
    if recv is None:
        return ""
    for p in recv.named_children:
        if p.type != "parameter_declaration":
            continue
        t = p.child_by_field_name("type")
        while t is not None and t.type in ("pointer_type", "generic_type", "parenthesized_type"):
            nxt = t.child_by_field_name("type") if t.type == "generic_type" else None
            t = nxt or (t.named_children[0] if t.named_children else None)
        return _text(t) if t is not None else ""
    return ""


def _go_functions(root: Node) -> list[Func]:
    """Go: `func F()` -> Function `F`; `func (s *T) M()` -> Method `T.M`
    (receiver type name, pointer and type arguments stripped). Function
    literals are not definitions (their calls belong to the enclosing
    function)."""
    out: list[Func] = []
    for node in _walk(root):
        if node.type not in ("function_declaration", "method_declaration"):
            continue
        nn = node.child_by_field_name("name")
        name = _text(nn)
        if not name:
            continue
        if node.type == "method_declaration":
            recv = _go_receiver_type(node.child_by_field_name("receiver"))
            out.append(_func(name, f"{recv}.{name}" if recv else name, "Method", node, nn))
        else:
            out.append(_func(name, name, "Function", node, nn))
    return out


_C_DECLARATOR_WRAPPERS = {
    "pointer_declarator",
    "reference_declarator",
    "parenthesized_declarator",
    "attributed_declarator",
}
_C_SCOPES = {"namespace_definition", "class_specifier", "struct_specifier", "union_specifier"}


def _c_declarator_name(decl: Node | None) -> tuple[list[str], Node | None]:
    """Function definition declarator -> (qualified parts, name node).
    `void A::b()` -> (["A", "b"], b); `~A()` -> (["~A"], ...);
    `operator==` -> (["operator=="], ...)."""
    d = decl
    while d is not None and d.type in _C_DECLARATOR_WRAPPERS:
        d = d.child_by_field_name("declarator") or (d.named_children[0] if d.named_children else None)
    if d is None or d.type != "function_declarator":
        return [], None
    n = d.child_by_field_name("declarator")
    parts: list[str] = []
    while n is not None and n.type == "qualified_identifier":
        scope = n.child_by_field_name("scope")
        if scope is not None:
            parts.append(_text(scope.child_by_field_name("name") or scope) if scope.type == "template_type" else _text(scope))
        n = n.child_by_field_name("name")
    if n is None:
        return [], None
    if n.type in ("template_function", "template_method"):
        n = n.child_by_field_name("name") or n
    parts.append(_text(n).replace(" ", ""))
    return parts, n


def _c_functions(root: Node, spec: dict) -> list[Func]:
    """C / C++: every `function_definition` (a body, not a prototype) is a
    callable; its qualified name joins enclosing named namespaces and
    classes / structs and the declarator's own scope with `::`
    (`fmt::v11::detail::write`, out-of-line `Foo::bar`). Anonymous namespaces
    add nothing. Inside a class body or with a declarator scope it is a
    `Method`, else a `Function`. Macros are not callables."""
    sep = spec.get("separator", "::")
    out: list[Func] = []
    for node in _walk(root):
        if node.type != "function_definition":
            continue
        parts, nn = _c_declarator_name(node.child_by_field_name("declarator"))
        if nn is None or not parts[-1]:
            continue
        scopes: list[str] = []
        in_class = False
        anc = node.parent
        while anc is not None:
            if anc.type in _C_SCOPES:
                n = anc.child_by_field_name("name")
                if n is not None:
                    scopes.append(_text(n))
                if anc.type != "namespace_definition":
                    in_class = True
            anc = anc.parent
        label = "Method" if in_class or len(parts) > 1 else "Function"
        qual = sep.join([*reversed(scopes), *parts])
        outer = node.parent if node.parent is not None and node.parent.type == "template_declaration" else node
        out.append(_func(parts[-1], qual, label, node, nn, outer))
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


def _field_child(node: Node, field: str) -> Node | None:
    """`field` child of a node; `@0` / `@-1` = first / last named child
    (children the grammar leaves unnamed: Java `generic_type`,
    `scoped_type_identifier`)."""
    if field in ("@0", "@-1"):
        kids = node.named_children
        return (kids[0] if field == "@0" else kids[-1]) if kids else None
    return node.child_by_field_name(field)


def _is_callee(node: Node, calls: dict[str, str], members: dict[str, str], depth: int = 0) -> bool:
    """Data-driven call-site test (manifest `[grammars.<name>]` `calls` /
    `members`): `node` is the callee when it is the `calls[parent.type]`
    child of its parent (`call_expression.function`, Java
    `method_invocation.name`), or the `members[parent.type]` child of a
    parent that is itself a callee (`s.M` in `s.M()`, `ns::f` in `ns::f()`,
    `F[int]` in `F[int]()`)."""
    parent = node.parent
    if parent is None or depth > 8:
        return False
    f = calls.get(parent.type)
    if f is not None and _field_child(parent, f) == node:
        return True
    f = members.get(parent.type)
    if f is not None and _field_child(parent, f) == node:
        return _is_callee(parent, calls, members, depth + 1)
    return False


# statements a local declaration can be a direct child of
_BODY_PARENTS = {"compound_statement", "case_statement", "for_statement", "condition_clause"}


def _is_construction(node: Node) -> bool:
    """C++ construction by declaration (manifest `constructions = true`): the
    name a declaration in a function body declares — `Token token(a, b);`
    (`init_declarator` with arguments), `Parser parser(input);` (the most
    vexing parse: a `function_declarator`), `T x{a};`, `T x;`. scip-clang
    records the constructor it calls on that name; the occurrence still has
    to reference a bound callable, so a plain variable never counts."""
    parent = node.parent
    if node.type != "identifier" or parent is None:
        return False
    decl = parent
    if parent.type in ("init_declarator", "function_declarator"):
        if parent.child_by_field_name("declarator") != node:
            return False
        decl = parent.parent
    elif parent.type == "declaration":
        if not any(c == node for c in parent.children_by_field_name("declarator")):
            return False
    else:
        return False
    return decl is not None and decl.type == "declaration" and decl.parent is not None and decl.parent.type in _BODY_PARENTS


def is_call_site(tree, grammar: str, line0: int, byte_col: int, end_byte_col: int) -> bool:
    node = tree.root_node.named_descendant_for_point_range((line0, byte_col), (line0, max(byte_col, end_byte_col - 1)))
    if node is None:
        return False
    parent = node.parent
    if parent is None:
        return False
    spec = _grammars().get(grammar, {})
    if "calls" in spec:
        return _is_callee(node, spec["calls"], spec.get("members", {})) or (bool(spec.get("constructions")) and _is_construction(node))
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
