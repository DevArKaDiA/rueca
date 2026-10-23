; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
; SPDX-FileCopyrightText: BloopAI (bloop contributors)
;
; Local scopes and bindings for Python (tree-sitter-python 0.25).
;
; Source (base):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/python/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`; the file
;   was last changed upstream in 692b051b09935653befdb8f7ba8afdb640adf17b),
;   license Apache-2.0.
; Source (additions: lambda / generator-expression scopes, lambda parameters,
; `global`):
;   https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/python/scopes.scm
;   commit 431e9e82c5a293c40f22aadb3615c3aa387af82e, license Apache-2.0.
;
; Adapted for Telar:
; - Removed Neovim-only predicates (`#set! definition.*.scope "parent"`).
;   Their meaning is kept in Rust: `@local.definition.function` and
;   `@local.definition.type` bind in the scope *enclosing* the def/class.
; - Removed nvim's `@local.scope` on import statements (an import is not a
;   Python scope) and its `self.x` / class-body `@local.definition.field`
;   patterns (fields are extracted by Telar code; class-body assignments are
;   covered by the generic assignment pattern).
; - Parameter patterns use field names (`name:`) so default values and
;   annotations are not captured as parameters; `*args: T` / `**kw: T`
;   (typed splats) and lambda parameters are covered.
; - Target captures may be a whole pattern node (`a, (b, *c)`,
;   `as_pattern_target`); Telar binds every identifier inside it except
;   attribute / subscript sub-expressions. This replaces nvim's fixed-depth
;   `pattern_list` / `tuple_pattern` variants.
; - Definition kinds `iter` (for / comprehension targets), `alias`
;   (with / except `as`), `walrus` (`:=`, binds outside comprehensions per
;   PEP 572), `case` (match captures) and `type_parameter` (PEP 695) split
;   nvim's `var`, because module-level scope rules differ per kind.
; - Added `augmented_assignment`, `generator_expression` scope, match-case
;   captures, PEP 695 type parameters, and the Telar-specific captures
;   `@local.global` / `@local.nonlocal` for `global` / `nonlocal` names.
; - Imports bind only the first segment of `import a.b.c`.

; ---------------------------------------------------------------------------
; Scopes
; ---------------------------------------------------------------------------

(module) @local.scope
(class_definition) @local.scope
(function_definition) @local.scope
(lambda) @local.scope
(list_comprehension) @local.scope
(set_comprehension) @local.scope
(dictionary_comprehension) @local.scope
(generator_expression) @local.scope

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

; def / class names bind in the parent scope (nvim: #set! … "parent").
(function_definition
  name: (identifier) @local.definition.function)

(class_definition
  name: (identifier) @local.definition.type)

; PEP 695: def f[T](…), class C[T]: …  (`type_parameter` also wraps the
; arguments of generic annotations such as `dict[str, X]`, hence the field.)
(function_definition
  type_parameters: (type_parameter
    (type
      (identifier) @local.definition.type_parameter)))

(class_definition
  type_parameters: (type_parameter
    (type
      (identifier) @local.definition.type_parameter)))

; Parameters: def f(a, b=1, c: T, d: T = 1, *args, **kw, *a: T, **k: T)
(parameters
  [
    (identifier)
    (list_splat_pattern)
    (dictionary_splat_pattern)
    (tuple_pattern)
  ] @local.definition.parameter)

(parameters
  (default_parameter
    name: (_) @local.definition.parameter))

(parameters
  (typed_parameter
    .
    (_) @local.definition.parameter))

(parameters
  (typed_default_parameter
    name: (_) @local.definition.parameter))

; lambda a, b=1, *r, **k: …
(lambda_parameters
  [
    (identifier)
    (list_splat_pattern)
    (dictionary_splat_pattern)
    (tuple_pattern)
  ] @local.definition.parameter)

(lambda_parameters
  (default_parameter
    name: (_) @local.definition.parameter))

; Assignments: x = …, a, (b, *c) = …, x: T = …, x: T, x += 1
(assignment
  left: (_) @local.definition.var)

(augmented_assignment
  left: (identifier) @local.definition.var)

; Loops and comprehensions (a `for` statement is not a scope).
(for_statement
  left: (_) @local.definition.iter)

(for_in_clause
  left: (_) @local.definition.iter)

; with a as b / except E as e
(as_pattern
  alias: (as_pattern_target) @local.definition.alias)

; Walrus operator  x := 1
(named_expression
  name: (identifier) @local.definition.walrus)

; match / case captures
(case_pattern
  (dotted_name
    .
    (identifier) @local.definition.case
    .))

(keyword_pattern
  (dotted_name
    .
    (identifier) @local.definition.case
    .))

(splat_pattern
  (identifier) @local.definition.case)

(as_pattern
  (case_pattern)
  (identifier) @local.definition.case)

; Imports
(import_statement
  name: (dotted_name
    .
    (identifier) @local.definition.import))

(aliased_import
  alias: (identifier) @local.definition.import)

(import_from_statement
  name: (dotted_name
    .
    (identifier) @local.definition.import))

; global / nonlocal declarations (Telar)
(global_statement
  (identifier) @local.global)

(nonlocal_statement
  (identifier) @local.nonlocal)

; ---------------------------------------------------------------------------
; References
; ---------------------------------------------------------------------------

(identifier) @local.reference
