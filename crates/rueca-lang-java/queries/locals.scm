; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
; SPDX-FileCopyrightText: BloopAI (bloop contributors)
;
; Local-variable scopes and bindings for Java (tree-sitter-java 0.23.5).
;
; Source (base):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/java/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
; Source (additions: switch-case and try-with-resources scopes, resources,
; `instanceof` and type-pattern bindings, spread parameters):
;   https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/java/scopes.scm
;   commit 431e9e82c5a293c40f22aadb3615c3aa387af82e, license Apache-2.0.
;
; Adapted for Rueca:
; - Only local variables are kept: parameters, local variables, `for` and
;   enhanced-`for` variables, `catch` parameters, lambda parameters,
;   try-with-resources variables and pattern variables. nvim's
;   `@local.definition.type` / `.enum` / `.method` / `.field` /
;   `.namespace` / `.import` captures and every `@local.reference` capture
;   are removed: classes, members, fields, packages and imports are
;   extracted by Rueca code, and the extractor looks identifiers up in the
;   scope table directly instead of matching every identifier.
; - Removed Neovim-only predicates (`#has-ancestor?`).
; - Class, record and enum bodies are not scopes (fields are not recorded
;   here). `if` statements and their branches are not scopes either, so a
;   pattern variable bound in a condition (`if (!(o instanceof Foo f))
;   return; f.m();`) stays visible in the enclosing block, which
;   approximates Java's flow scoping; nvim's `if_statement` scopes are
;   dropped.
; - Added (bloop): `switch_block_statement_group`, `switch_rule` and
;   `try_with_resources_statement` scopes; `resource`, `instanceof_expression`
;   `name:` and `type_pattern` bindings. Added (Rueca):
;   `compact_constructor_declaration` scope, `spread_parameter` and
;   `record_pattern_component` bindings, field names of tree-sitter-java
;   0.23.

; SCOPES

(program) @local.scope

(block) @local.scope

(lambda_expression) @local.scope

(method_declaration) @local.scope

(constructor_declaration) @local.scope

(compact_constructor_declaration) @local.scope

(for_statement) @local.scope

(enhanced_for_statement) @local.scope

(catch_clause) @local.scope

(try_with_resources_statement) @local.scope

(switch_block_statement_group) @local.scope

(switch_rule) @local.scope

; DEFINITIONS

(local_variable_declaration
  declarator: (variable_declarator
    name: (identifier) @local.definition.var))

(enhanced_for_statement
  name: (identifier) @local.definition.var)

(formal_parameter
  name: (identifier) @local.definition.parameter)

(spread_parameter
  (variable_declarator
    name: (identifier) @local.definition.parameter))

(catch_formal_parameter
  name: (identifier) @local.definition.parameter)

(inferred_parameters
  (identifier) @local.definition.parameter)

(lambda_expression
  parameters: (identifier) @local.definition.parameter)

(resource
  name: (identifier) @local.definition.var)

(instanceof_expression
  name: (identifier) @local.definition.var)

(type_pattern
  (identifier) @local.definition.var)

(record_pattern_component
  (identifier) @local.definition.var)
