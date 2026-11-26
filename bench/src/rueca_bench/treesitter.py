"""Tree-sitter helpers: function/method extraction and call-site classification.

Used for (a) S1 gold (diff hunk -> enclosing function at the base commit),
(b) the grep baseline's function documents, and (c) S3 gold (is a SCIP
reference occurrence a call site? which callable encloses it?).
"""

from __future__ import annotations

import re
from dataclasses import asdict, dataclass
from functools import lru_cache

import tree_sitter_bash
import tree_sitter_c
import tree_sitter_c_sharp
import tree_sitter_cpp
import tree_sitter_go
import tree_sitter_java
import tree_sitter_javascript
import tree_sitter_php
import tree_sitter_python
import tree_sitter_rust
import tree_sitter_typescript
from tree_sitter import Language, Node, Parser

from .common import load_manifest

# Python grammar bindings the manifest may name in `[grammars.<name>] binding =
# "<module>:<function>"`. Adding a grammar = a pinned dependency in
# pyproject.toml, one import above, one entry here and the manifest data.
_BINDINGS = {
    "tree_sitter_bash": tree_sitter_bash,
    "tree_sitter_c": tree_sitter_c,
    "tree_sitter_c_sharp": tree_sitter_c_sharp,
    "tree_sitter_cpp": tree_sitter_cpp,
    "tree_sitter_go": tree_sitter_go,
    "tree_sitter_java": tree_sitter_java,
    "tree_sitter_javascript": tree_sitter_javascript,
    "tree_sitter_php": tree_sitter_php,
    "tree_sitter_python": tree_sitter_python,
    "tree_sitter_rust": tree_sitter_rust,
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


def grammar_spec(name: str | None) -> dict:
    """The manifest entry of a grammar (`{}` for none)."""
    return _grammars().get(name or "", {})


def creation_type_at(tree, line0: int, byte_col: int, end_byte_col: int) -> bool:
    """Whether the node at a range is the type of an object creation
    (`Foo` in `new Foo(…)`, `new A.Foo(…)`, `new Foo<T>(…)`)."""
    node = tree.root_node.named_descendant_for_point_range((line0, byte_col), (line0, max(byte_col, end_byte_col - 1)))
    for _ in range(4):
        if node is None or node.parent is None:
            return False
        parent = node.parent
        if parent.type == "object_creation_expression":
            return parent.child_by_field_name("type") == node
        if parent.type not in ("qualified_name", "generic_name", "alias_qualified_name"):
            return False
        node = parent
    return False


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
    if _grammars()[kind].get("labeler") == "shell":
        source = shell_repair(source)
    return parser.parse(source)


def _zsh_flags_end(source: bytes, open_: int) -> int | None:
    """The `)` closing zsh parameter flags opened at `open_` (`(j:,:)`)."""
    rest = source[open_ + 1 : open_ + 33]
    close = rest.find(b")")
    if close < 0 or any(c in b"\n(${}" for c in rest[:close]):
        return None
    return open_ + 1 + close


def shell_repair(source: bytes) -> bytes:
    """The same-length repairs the shell pack applies before parsing
    (`rueca-lang-shell` `repair.rs`): constructs tree-sitter-bash 0.25
    rejects although the shell accepts them — a line-start backslash before
    a command name (`\\. "$NVM_DIR/nvm.sh"`) becomes a space; inside
    `${…}`, `:$name` (a substring offset) loses its `$`, `&` `<` `>` become
    `_`; zsh parameter flags `${(j:,:)xs}` become `_`."""
    out = bytearray(source)
    line_start, depth, i, n = True, 0, 0, len(source)
    while i < n:
        b = source[i]
        nxt = source[i + 1] if i + 1 < n else 32
        if b == 0x5C:  # backslash
            if line_start and (chr(nxt).isalpha() and nxt < 128 or nxt in b"._"):
                out[i] = 32
            else:
                if depth and nxt in b"&<>":
                    out[i + 1] = 0x5F
                i += 1
        elif b == 0x24 and nxt == 0x7B:  # ${
            depth += 1
            if i + 2 < n and source[i + 2] == 0x28:
                close = _zsh_flags_end(source, i + 2)
                if close is not None:
                    out[i + 2 : close + 1] = b"_" * (close - i - 1)
                    i = close
            else:
                i += 1
        elif b == 0x7D and depth:
            depth -= 1
        elif b == 0x3A and depth and nxt == 0x24 and i + 2 < n and (chr(source[i + 2]).isalpha() and source[i + 2] < 128 or source[i + 2] == 0x5F):
            out[i + 1] = 32
        elif depth and b in b"&<>":
            out[i] = 0x5F
        line_start = b == 0x0A or (line_start and b in b" \t")
        i += 1
    return bytes(out)


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
    if labeler == "rust":
        return _rust_functions(tree.root_node, spec)
    if labeler == "shell":
        return _shell_functions(tree.root_node)
    if labeler == "csharp":
        return _csharp_functions(tree.root_node, spec)
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


# Rust: the self type of an `impl` names its methods. Wrappers stripped down
# to the last path segment: `&mut a::Foo<'x, T>` -> `Foo`, `dyn Tr` -> `Tr`.
_RUST_TYPE_STEPS = {
    "reference_type": "type",
    "pointer_type": "type",
    "generic_type": "type",
    "scoped_type_identifier": "name",
    "dynamic_type": "trait",
}
# `#[test]`, `#[tokio::test]`, `#[bench]`, `#[cfg(test)]`, `#[cfg(all(test, ...))]`
# (whitespace removed); `#[cfg(not(test))]` is not test code.
_RUST_TEST_ATTR = re.compile(r"^#\[(?:(?:\w+::)*(?:test|bench)(?:\(.*\))?|cfg\(test\)|cfg\(all\(test[,)].*)\]$")
_RUST_ITEMS = {"function_item", "mod_item", "impl_item", "trait_item"}
_RUST_ATTACHED = {"attribute_item", "line_comment", "block_comment"}


def _rust_type_name(t: Node | None) -> str:
    """Last path segment of an impl's self type, generics and references
    stripped (`impl<T> Tr for &a::Foo<T>` -> `Foo`); other shapes (`[T]`,
    tuples) keep their text without whitespace."""
    while t is not None and t.type in _RUST_TYPE_STEPS:
        t = t.child_by_field_name(_RUST_TYPE_STEPS[t.type])
    return re.sub(r"\s+", "", _text(t))


def _rust_attributes(node: Node) -> list[Node]:
    """Attribute items written before an item (doc comments may sit between
    them), nearest first."""
    out: list[Node] = []
    sib = node.prev_named_sibling
    while sib is not None and sib.type in _RUST_ATTACHED:
        if sib.type == "attribute_item":
            out.append(sib)
        sib = sib.prev_named_sibling
    return out


def _rust_is_test_item(node: Node) -> bool:
    if node.type not in _RUST_ITEMS:
        return False
    return any(_RUST_TEST_ATTR.match(re.sub(r"\s+", "", _text(a))) for a in _rust_attributes(node))


def _rust_container_name(node: Node) -> str:
    if node.type == "impl_item":
        return _rust_type_name(node.child_by_field_name("type"))
    return _text(node.child_by_field_name("name"))


def _rust_scope(node: Node, containers: set[str]) -> tuple[list[str], str]:
    """(enclosing names outermost first, label): inline `mod`s, `impl` self
    types, traits and enclosing functions; `Method` when the nearest of them
    is an `impl` or a trait."""
    parts: list[str] = []
    label = ""
    anc = node.parent
    while anc is not None:
        if anc.type in containers or anc.type == "function_item":
            parts.append(_rust_container_name(anc))
            if not label:
                label = "Method" if anc.type in ("impl_item", "trait_item") else "Function"
        anc = anc.parent
    return list(reversed(parts)), label or "Function"


def _rust_callable(node: Node, containers: set[str]) -> Func | None:
    """A callable node -> Func; a bodiless `fn` signature is a callable only
    in a trait (an `extern` block declares foreign functions)."""
    if node.type == "function_signature_item":
        body = node.parent
        if body is None or body.parent is None or body.parent.type != "trait_item":
            return None
    nn = node.child_by_field_name("name")
    name = _text(nn)
    if nn is None or not name:
        return None
    parts, label = _rust_scope(node, containers)
    if node.type == "macro_definition":
        label = "Function"
    attrs = _rust_attributes(node)
    return _func(name, ".".join([*parts, name]), label, node, nn, attrs[-1] if attrs else None)


def _rust_functions(root: Node, spec: dict) -> list[Func]:
    """Rust (the Rust pack's qualified names): free `fn` -> `name`; in an
    inline `mod m` -> `m.name`; nested in a function -> `outer.inner`;
    methods and associated functions of `impl<T> Foo<T>` / `impl Tr for
    &Foo<'a>` -> `Foo.method` (last path segment of the self type); trait
    methods, signatures and default bodies -> `Tr.method`; `macro_rules!`
    definitions -> `name` (manifest `callables`). File modules are not part
    of the name. Test code — items marked `#[test]`, `#[tokio::test]`,
    `#[bench]` or `#[cfg(test)]` (the usual `mod tests`) — is skipped with
    everything inside it, as test files are elsewhere."""
    kinds = set(spec.get("callables", {}))
    containers = set(spec.get("containers", []))
    out: list[Func] = []
    stack = [root]
    while stack:
        node = stack.pop()
        if _rust_is_test_item(node):
            continue
        if node.type in kinds:
            f = _rust_callable(node, containers)
            if f is not None:
                out.append(f)
        stack.extend(reversed(node.children))
    return out


def _shell_functions(root: Node) -> list[Func]:
    """Shell (the shell pack's names): every `f() { … }` / `function f { … }`
    is a `Function` named `f` — shell functions are global once defined, so
    a function nested in another (or in an `if`) is not qualified by it."""
    out: list[Func] = []
    for node in _walk(root):
        if node.type != "function_definition":
            continue
        nn = node.child_by_field_name("name")
        name = _text(nn)
        if name:
            out.append(_func(name, name, "Function", node, nn))
    return out


def _csharp_operator_name(node: Node) -> tuple[str, Node]:
    """`operator+` / `operator true` / `implicit operator string` (the C#
    pack's names) and the node the name sits on."""
    if node.type == "conversion_operator_declaration":
        kind = "explicit" if any(c.type == "explicit" for c in node.children) else "implicit"
        ty = node.child_by_field_name("type")
        text = re.sub(r"<.*>", "", _text(ty)).replace(" ", "").rstrip("?")
        return f"{kind} operator {text.rsplit('.', 1)[-1]}", ty or node
    op = node.child_by_field_name("operator")
    t = _text(op)
    return (f"operator {t}" if t[:1].isalpha() else f"operator{t}"), op or node


def _csharp_name(node: Node, type_name: str) -> tuple[str, Node] | None:
    """Name and name node of a C# callable (the C# pack's names):
    constructors are named like their type, destructors `~Type`."""
    if node.type in ("operator_declaration", "conversion_operator_declaration"):
        return _csharp_operator_name(node)
    nn = node.child_by_field_name("name")
    if nn is None:
        return None
    if node.type == "destructor_declaration":
        return f"~{type_name}", nn
    return _text(nn), nn


def _csharp_functions(root: Node, spec: dict) -> list[Func]:
    """C# (the C# pack's qualified names): methods, constructors,
    destructors, operators and conversion operators of types are `Method`s
    named `Type.member` (nested types joined with `.`, namespaces left out;
    an explicit interface implementation `Type.IFace.member`); local
    functions are `Function`s under their enclosing callable
    (`Type.method.local`). Property and event accessors are not callables
    (their code belongs to no callable, as in the pack, where it belongs
    to the property)."""
    kinds: dict[str, str] = spec["callables"]
    containers = set(spec.get("containers", []))
    out: list[Func] = []
    for node in _walk(root):
        label = kinds.get(node.type)
        if label is None:
            continue
        parts: list[str] = []
        anc = node.parent
        while anc is not None:
            if anc.type in containers or anc.type in kinds:
                info = _csharp_name(anc, "") if anc.type in kinds else (_text(anc.child_by_field_name("name")), anc)
                parts.append(info[0] if info else "")
            anc = anc.parent
        parts.reverse()
        type_name = next((p for p in reversed(parts) if p), "")
        info = _csharp_name(node, type_name)
        if info is None or not info[0]:
            continue
        name, nn = info
        iface = next((c for c in node.named_children if c.type == "explicit_interface_specifier"), None)
        local = [_text(iface.named_children[0]).rsplit(".", 1)[-1], name] if iface and iface.named_children else [name]
        out.append(_func(name, ".".join([*parts, *local]), label, node, nn))
    return out


_CS_CALLABLES = ("method_declaration", "local_function_statement")


def _cs_arity(params: Node | None) -> tuple[int, int | None]:
    """(minimum, maximum; `None` = `params`) arguments of a C# parameter list."""
    if params is None:
        return 0, 0
    lo = hi = 0
    variadic = False
    for c in params.children:
        if c.type == "params":
            variadic = True
        elif c.type == "parameter":
            hi += 1
            fixed = (c.child_by_field_name("type"), c.child_by_field_name("name"))
            has_default = any(k.is_named and k not in fixed and k.type not in ("attribute_list", "modifier") for k in c.children)
            lo += 0 if has_default else 1
    return lo, (None if variadic else hi)


def csharp_signatures(tree) -> dict[tuple[int, str], tuple[int, int | None, bool]]:
    """C# methods by (1-based name line, name): (minimum arguments, maximum
    or `None` for `params`, generic). For the SCIP gold's generic-overload
    repair (scip.py)."""
    out: dict[tuple[int, str], tuple[int, int | None, bool]] = {}
    for node in _walk(tree.root_node):
        if node.type not in _CS_CALLABLES:
            continue
        nn = node.child_by_field_name("name")
        if nn is None:
            continue
        lo, hi = _cs_arity(node.child_by_field_name("parameters"))
        generic = node.child_by_field_name("type_parameters") is not None
        out[(nn.start_point[0] + 1, _text(nn))] = (lo, hi, generic)
    return out


def call_arity_at(tree, line0: int, byte_col: int, end_byte_col: int) -> int | None:
    """Argument count of the C# invocation whose callee is at a range."""
    node = tree.root_node.named_descendant_for_point_range((line0, byte_col), (line0, max(byte_col, end_byte_col - 1)))
    for _ in range(6):
        if node is None:
            return None
        if node.type == "invocation_expression":
            args = node.child_by_field_name("arguments")
            return sum(1 for c in args.named_children if c.type == "argument") if args is not None else 0
        node = node.parent
    return None


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


def _is_token_call(node: Node, token_tree: str) -> bool:
    """A call written in a macro's arguments (manifest `token_calls`: the
    grammar's unparsed token-tree node, Rust `token_tree`). Tree-sitter keeps
    `println!("{}", foo(x))`, `vec![a.len()]` and `assert_eq!(T::new(), b)` as
    flat tokens; an identifier directly followed by a `(`-delimited token tree
    is called there (`foo(`, `.len(`, `::new(`), and one followed by `!` and a
    token tree is a nested macro invocation. rust-analyzer resolves these
    tokens through the expansion."""
    parent = node.parent
    if parent is None or parent.type != token_tree or node.type != "identifier":
        return False
    nxt = node.next_sibling
    if nxt is not None and nxt.type == "!":
        nxt = nxt.next_sibling
        return nxt is not None and nxt.type == token_tree
    return nxt is not None and nxt.type == token_tree and nxt.child_count > 0 and nxt.children[0].type == "("


def is_call_site(tree, grammar: str, line0: int, byte_col: int, end_byte_col: int) -> bool:
    node = tree.root_node.named_descendant_for_point_range((line0, byte_col), (line0, max(byte_col, end_byte_col - 1)))
    if node is None:
        return False
    parent = node.parent
    if parent is None:
        return False
    spec = _grammars().get(grammar, {})
    if "calls" in spec:
        return (
            _is_callee(node, spec["calls"], spec.get("members", {}))
            or (bool(spec.get("constructions")) and _is_construction(node))
            or (bool(spec.get("token_calls")) and _is_token_call(node, spec["token_calls"]))
        )
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
