; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Additional local scopes and bindings for C++ (tree-sitter-cpp 0.23).
; Rueca compiles `c-locals.scm` followed by this file against the C++
; grammar (nvim-treesitter: `; inherits: c`).
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/cpp/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
;
; Adapted for Rueca:
; - Kept: parameter and variable bindings (variadic, optional and reference
;   declarators), template type parameters (`@local.definition.type`, so a
;   template's `T` is never reported as a project type), and the scopes of
;   templates, lambdas, `try` bodies, `catch` clauses and `requires`
;   expressions.
; - Removed: class, struct, alias, concept, namespace, function and method
;   definitions and the class and namespace scopes (Rueca code extracts
;   them; class- and namespace-level names are not locals), and the
;   `@local.reference` captures with their Neovim-only `#set!` predicates.
; - Rueca additions: the `for_range_loop` scope (`for (auto& x : xs)`),
;   structured bindings (`auto [a, b] = …`), and the defaulted and variadic
;   template type parameters (`typename T = int`, `typename... Ts`).

; ---------------------------------------------------------------------------
; Scopes
; ---------------------------------------------------------------------------

(template_declaration) @local.scope

(lambda_expression) @local.scope

(try_statement
  body: (_) @local.scope)

(catch_clause) @local.scope

(requires_expression) @local.scope

(for_range_loop) @local.scope

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

(variadic_parameter_declaration
  declarator: (variadic_declarator
    (identifier) @local.definition.parameter))

(optional_parameter_declaration
  declarator: (identifier) @local.definition.parameter)

(reference_declarator
  (identifier) @local.definition.var)

(variadic_declarator
  (identifier) @local.definition.var)

(structured_binding_declarator
  (identifier) @local.definition.var)

(type_parameter_declaration
  (type_identifier) @local.definition.type)

(optional_type_parameter_declaration
  name: (type_identifier) @local.definition.type)

(variadic_type_parameter_declaration
  (type_identifier) @local.definition.type)
