; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local scopes and bindings for Kotlin (tree-sitter-kotlin-sg 0.4.1, the
; crates.io release of fwcd/tree-sitter-kotlin).
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/kotlin/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
;
; Adapted for Rueca:
; - Only local bindings are kept: function, constructor, anonymous-function
;   and lambda parameters, local `val` / `var` (also destructured), `for`
;   and `when` subject variables, `catch` parameters, setter parameters and
;   primary-constructor parameters. nvim's `.namespace` / `.import` /
;   `.function` / `.method` / `.type` captures are removed: packages,
;   imports, functions and types are extracted by Rueca code.
; - Removed the `#set!` directives (Neovim-only).
; - `primary_constructor` is not a scope: its parameters are visible in
;   the whole class (initializers and `init` blocks).
; - Added: secondary-constructor, anonymous-function and setter
;   parameters, `catch` parameters, and the `catch`, getter, setter,
;   secondary-constructor and anonymous-function scopes.
; - A `variable_declaration` of a class or file property and a
;   `val` / `var` primary-constructor parameter (a property) are members,
;   not locals: `src/locals.rs` drops them.

; Variables
(function_declaration
  (function_value_parameters
    (parameter
      (simple_identifier) @local.definition.parameter)))

(secondary_constructor
  (function_value_parameters
    (parameter
      (simple_identifier) @local.definition.parameter)))

(anonymous_function
  (function_value_parameters
    (parameter
      (simple_identifier) @local.definition.parameter)))

(setter
  (parameter_with_optional_type
    (simple_identifier) @local.definition.parameter))

(lambda_literal
  (lambda_parameters
    (variable_declaration
      (simple_identifier) @local.definition.parameter)))

(class_declaration
  (primary_constructor
    (class_parameter
      (simple_identifier) @local.definition.field)))

(catch_block
  (simple_identifier) @local.definition.var)

(variable_declaration
  (simple_identifier) @local.definition.var)

; Scopes
[
  (if_expression)
  (when_expression)
  (when_entry)
  (for_statement)
  (while_statement)
  (do_while_statement)
  (lambda_literal)
  (function_declaration)
  (anonymous_function)
  (secondary_constructor)
  (anonymous_initializer)
  (class_declaration)
  (enum_class_body)
  (enum_entry)
  (interpolated_expression)
  (catch_block)
  (getter)
  (setter)
] @local.scope
