; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local scopes and bindings for Swift (tree-sitter-swift 0.7.4,
; alex-pinkus/tree-sitter-swift).
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/swift/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0. The grammar ships the same file as `queries/locals.scm`.
;
; Adapted for Rueca:
; - The upstream scopes are kept. Upstream has no binding captures for
;   variables: its `@local.definition.import` / `.function` captures are
;   removed (imports and functions are extracted by Rueca code).
; - `class_declaration`, `protocol_declaration`, `property_declaration`
;   and `guard_statement` are not scopes: a type's members are not
;   locals, a local `let` is visible after its declaration in the
;   enclosing block, and so is a `guard let` binding.
; - Added scopes: lambda literals, `catch` blocks, switch entries,
;   init / deinit / subscript / protocol-function bodies, and the
;   getter / setter / `willSet` / `didSet` blocks of computed properties.
; - Added bindings: function, init, subscript and closure parameters;
;   `let` / `var` declarations (at any depth of a tuple pattern); `for`,
;   `if let` / `guard let` / `while let`, `catch` and `case let` bindings.
;   Property declarations of types and files are members, not locals:
;   `src/locals.rs` drops them, and drops a parameter's external label.

; Scopes
[
  (statements)
  (for_statement)
  (while_statement)
  (repeat_while_statement)
  (do_statement)
  (if_statement)
  (switch_statement)
  (switch_entry)
  (function_declaration)
  (init_declaration)
  (deinit_declaration)
  (subscript_declaration)
  (protocol_function_declaration)
  (lambda_literal)
  (catch_block)
  (computed_getter)
  (computed_setter)
  (willset_clause)
  (didset_clause)
] @local.scope

; Parameters (external labels are dropped in Rust)
(parameter
  (simple_identifier) @local.definition.parameter)

(lambda_parameter
  name: (simple_identifier) @local.definition.parameter)

; `let` / `var`
(property_declaration
  name: (pattern
    bound_identifier: (simple_identifier) @local.definition.var))

(property_declaration
  name: (pattern
    (pattern
      (simple_identifier) @local.definition.var)))

; `for x in`, `for (a, b) in`
(for_statement
  item: (pattern
    bound_identifier: (simple_identifier) @local.definition.var))

(for_statement
  item: (pattern
    (pattern
      (simple_identifier) @local.definition.var)))

; `if let x`, `guard let x`, `while let x`
(if_statement
  bound_identifier: (simple_identifier) @local.definition.var)

(guard_statement
  bound_identifier: (simple_identifier) @local.definition.var)

(while_statement
  bound_identifier: (simple_identifier) @local.definition.var)

; `catch let e`, `case .x(let v)`
(pattern
  (value_binding_pattern)
  bound_identifier: (simple_identifier) @local.definition.var)
