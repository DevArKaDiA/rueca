; SPDX-License-Identifier: Apache-2.0 AND MIT
;
; Local bindings shared by JavaScript, TypeScript and TSX.
;
; Source (Apache-2.0, Copyright the nvim-treesitter contributors):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/ecma/locals.scm
; One pattern, `(pattern/identifier)`, comes from tree-sitter-javascript v0.25.0
; (MIT, Copyright (c) 2014 Max Brunsfeld):
;   https://github.com/tree-sitter/tree-sitter-javascript/blob/44c892e0be055ac465d5eeddae6d3e194424e7de/queries/locals.scm
;
; Adapted for Rueca (see queries/README.md):
;   - removed the `@local.reference` patterns: Rueca's walker emits references
;     itself (RFC 0002 §3.4), the query only finds bindings;
;   - removed the `method_definition` name definition: method names are
;     properties, not bindings in the enclosing scope;
;   - added (marked "Rueca:") generator scopes and names, function-expression
;     and class names, catch parameters, for-in/for-of targets and destructuring
;     bindings at any depth.
;
; Captures: `@local.scope`, `@local.definition*`. A `#set! definition.var.scope
; parent` property binds the definition in the scope enclosing the innermost
; one (a function's name belongs to the scope around the function).

; Scopes
;-------
(statement_block) @local.scope

(function_expression) @local.scope

(arrow_function) @local.scope

(function_declaration) @local.scope

(method_definition) @local.scope

(for_statement) @local.scope

(for_in_statement) @local.scope

(catch_clause) @local.scope

; Rueca: generators are function scopes too.
(generator_function) @local.scope

(generator_function_declaration) @local.scope

; Definitions
;------------
(variable_declarator
  name: (identifier) @local.definition.var)

(variable_declarator
  name: (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.var))

(variable_declarator
  (object_pattern
    (pair_pattern
      (identifier) @local.definition.var)))

(import_specifier
  (identifier) @local.definition.import)

(namespace_import
  (identifier) @local.definition.import)

(function_declaration
  (identifier) @local.definition.function
  (#set! definition.var.scope parent))

; Rueca: identifiers bound by a destructuring pattern at any depth
; (`const { a: { b }, c: [d, ...e] } = x`), from tree-sitter-javascript.
(pattern/identifier) @local.definition.var

(object_pattern
  (shorthand_property_identifier_pattern) @local.definition.var)

(object_assignment_pattern
  left: (shorthand_property_identifier_pattern) @local.definition.var)

(rest_pattern
  (identifier) @local.definition.var)

; Rueca: `catch (err)`, `for (const x of xs)`.
(catch_clause
  parameter: (identifier) @local.definition.var)

(for_in_statement
  left: (identifier) @local.definition.var)

; Rueca: names of nested functions and classes.
(generator_function_declaration
  name: (identifier) @local.definition.function
  (#set! definition.var.scope parent))

(function_expression
  name: (identifier) @local.definition.function)

(generator_function
  name: (identifier) @local.definition.function)

(class_declaration
  name: (_) @local.definition.type)
