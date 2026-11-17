; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local scopes and bindings for Rust (tree-sitter-rust 0.24.2).
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/rust/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
;
; Adapted for Rueca:
; - Only scopes and bindings are kept. nvim's `@local.reference` patterns
;   (every identifier, type and field identifier) are removed: the
;   extractor visits every identifier itself and asks the scope table
;   whether it is bound locally. The import, function, type, field, macro
;   and module definitions are removed too: they are items, extracted by
;   Rust code, never local bindings.
; - Removed Neovim-only predicates (`#set! reference.kind …`).
; - Capture names follow Rueca's contract (`queries/README.md`):
;   `@local.definition.parameter` (function and closure parameters),
;   `@local.definition.var` (every identifier a pattern binds: `let`,
;   `for`, `match` arms, `if let` / `while let`, nested tuple, struct,
;   slice, reference, `ref` / `mut` and `x @ …` patterns; the Rust side
;   decides the binding site and drops the `type` of tuple-struct patterns
;   and upper-case names, which are paths, not bindings) and
;   `@local.definition.type_parameter` (type and const generic
;   parameters; Rueca addition, so `T` is never a type reference).
; - nvim's `const_item` variable is dropped (a `const` is an item even in a
;   block), and so are its `struct_item`, `enum_item` and `impl_item`
;   scopes (they bind no local); the item scopes that type parameters need
;   are computed on the Rust side.
; - Rueca additions: closure parameters with a type (`|a: u32|`),
;   `ref` / `mut` / `@` / slice / or / reference / struct-field patterns,
;   `while` scopes for `while let`.

; Scopes
[
  (block)
  (function_item)
  (closure_expression)
  (while_expression)
  (for_expression)
  (loop_expression)
  (if_expression)
  (match_expression)
  (match_arm)
] @local.scope

; Parameters
(parameter
  pattern: (identifier) @local.definition.parameter)

(closure_parameters
  (identifier) @local.definition.parameter)

; Pattern bindings
(let_declaration
  pattern: (identifier) @local.definition.var)

(tuple_pattern
  (identifier) @local.definition.var)

(let_condition
  pattern: (_
    (identifier) @local.definition.var))

(let_condition
  pattern: (identifier) @local.definition.var)

(tuple_struct_pattern
  (identifier) @local.definition.var)

(for_expression
  pattern: (identifier) @local.definition.var)

(match_pattern
  (identifier) @local.definition.var)

(field_pattern
  name: (shorthand_field_identifier) @local.definition.var)

(field_pattern
  pattern: (identifier) @local.definition.var)

(ref_pattern
  (identifier) @local.definition.var)

(mut_pattern
  (identifier) @local.definition.var)

(captured_pattern
  (identifier) @local.definition.var)

(slice_pattern
  (identifier) @local.definition.var)

(or_pattern
  (identifier) @local.definition.var)

(reference_pattern
  (identifier) @local.definition.var)

; Generic parameters
(type_parameter
  name: (type_identifier) @local.definition.type_parameter)

(const_parameter
  name: (identifier) @local.definition.type_parameter)
