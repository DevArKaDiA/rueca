; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
; SPDX-FileCopyrightText: BloopAI (bloop contributors)
;
; Variable scopes and bindings for PHP (tree-sitter-php 0.25, `LANGUAGE_PHP`).
;
; Source (base):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/php_only/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`;
;   `runtime/queries/php/locals.scm` is `; inherits: php_only`), license
;   Apache-2.0.
; Source (additions: arrow-function scope, variadic parameters, assignment,
; list destructuring, `static` and `global` bindings):
;   https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/php/scopes.scm
;   commit 431e9e82c5a293c40f22aadb3615c3aa387af82e, license Apache-2.0.
;
; Adapted for Rueca:
; - Only `$variable` scoping is kept. PHP functions, classes and constants
;   are never local (they are global once declared), so nvim's
;   `@local.definition.type` / `.method` / `.function` captures, its
;   `property_declaration` fields and `namespace_use_clause` imports, and its
;   type / method / field / function `@local.reference` patterns are removed:
;   definitions, imports and name references are extracted by Rueca code.
; - Removed Neovim-only predicates (`#set! definition.*.scope "parent"`,
;   `#set! reference.kind …`).
; - Scopes are the variable scopes of PHP: the file (`program`, Rueca
;   addition), functions, methods, closures (nvim) and arrow functions
;   (bloop). Block statements are not scopes (PHP variables are
;   function-scoped), so bloop's block scopes are dropped.
; - bloop's `anonymous_function_creation_expression` is `anonymous_function`
;   in tree-sitter-php 0.25.
; - Definition captures may be a whole target node (`list_literal`, `pair`,
;   `by_ref`); Rueca binds every `$variable` inside it except under member,
;   property or subscript accesses. This replaces bloop's single-level
;   `list_literal` pattern and nvim's two-variable `foreach` pattern, which
;   missed `foreach ($this->items as $item)` and nested destructuring.
; - Definition kinds: `parameter` (simple, variadic and — Rueca addition —
;   promoted constructor parameters), `var` (assignments, `foreach` targets,
;   `static`), `catch` (Rueca addition: `catch (X $e)`), `global` (bloop
;   records `global $x` as a reference; it binds the name in PHP) and
;   `capture` (closure `use ($x)`, nvim's `@local.definition.var` on
;   `anonymous_function_use_clause`, renamed because it refers to the
;   enclosing scope's variable).
; - Every `$variable` use is a `@local.reference` (nvim `variable_name`).

; ---------------------------------------------------------------------------
; Scopes
; ---------------------------------------------------------------------------

(program) @local.scope
(function_definition) @local.scope
(method_declaration) @local.scope
(anonymous_function) @local.scope
(arrow_function) @local.scope

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

(simple_parameter
  name: (variable_name) @local.definition.parameter)

(variadic_parameter
  name: (variable_name) @local.definition.parameter)

(property_promotion_parameter
  name: [(variable_name) (by_ref)] @local.definition.parameter)

(anonymous_function_use_clause
  [(variable_name) (by_ref)] @local.definition.capture)

(assignment_expression
  left: [(variable_name) (list_literal)] @local.definition.var)

(reference_assignment_expression
  left: [(variable_name) (list_literal)] @local.definition.var)

(foreach_statement
  .
  (_)
  .
  [(variable_name) (pair) (list_literal) (by_ref)] @local.definition.var)

(static_variable_declaration
  name: (variable_name) @local.definition.var)

(catch_clause
  name: (variable_name) @local.definition.catch)

(global_declaration
  (variable_name) @local.definition.global)

; ---------------------------------------------------------------------------
; References
; ---------------------------------------------------------------------------

(variable_name) @local.reference
