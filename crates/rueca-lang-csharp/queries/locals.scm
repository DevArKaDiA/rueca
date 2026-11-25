; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local-variable scopes and bindings for C# (tree-sitter-c-sharp 0.24.0).
;
; Source (base):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/c_sharp/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
;
; Adapted for Rueca:
; - Only local variables are kept: parameters, local variables (also in
;   `using`, `for` and `fixed` statements), `foreach` variables (and
;   their deconstructions), `out var` declarations, pattern variables,
;   `catch` variables, lambda parameters and query range variables. nvim's
;   `@local.definition.method` / `.type` / bare `@local.definition`
;   captures (methods, local functions, properties, type parameters,
;   classes) and every `@local.reference` capture are removed: types,
;   members and local functions are extracted by Rueca code, and the
;   extractor looks identifiers up in the scope table directly.
; - nvim's `variable_declarator` patterns match fields too; here a
;   declarator binds only under a local declaration, `using`, `for` or
;   `fixed` statement.
; - Scopes added (Rueca): every callable (methods, constructors,
;   destructors, operators, conversion operators, local functions,
;   accessors, lambdas, anonymous methods), `for`, `foreach`, `catch`,
;   `using`, `fixed`, switch sections and arms, and the declaration of a
;   class or struct with a primary constructor (its parameters are in
;   scope in the whole body). Record parameters are not bindings: they
;   are properties (members), recorded by the extractor.
; - Bindings added (Rueca): `params` parameters (the grammar puts their
;   name in the parameter list), `catch` declarations, declaration
;   (also `var x`) and recursive patterns, implicit lambda parameters,
;   `from` / `let` range variables.

; SCOPES

(block) @local.scope
(method_declaration) @local.scope
(constructor_declaration) @local.scope
(destructor_declaration) @local.scope
(operator_declaration) @local.scope
(conversion_operator_declaration) @local.scope
(local_function_statement) @local.scope
(accessor_declaration) @local.scope
(indexer_declaration) @local.scope
(lambda_expression) @local.scope
(anonymous_method_expression) @local.scope
(for_statement) @local.scope
(foreach_statement) @local.scope
(catch_clause) @local.scope
(using_statement) @local.scope
(fixed_statement) @local.scope
(switch_section) @local.scope
(switch_expression_arm) @local.scope
(query_expression) @local.scope
(class_declaration
  (parameter_list)) @local.scope
(struct_declaration
  (parameter_list)) @local.scope

; DEFINITIONS

(local_declaration_statement
  (variable_declaration
    (variable_declarator
      name: (identifier) @local.definition.var)))

(local_declaration_statement
  (variable_declaration
    (variable_declarator
      (tuple_pattern
        (identifier) @local.definition.var))))

(using_statement
  (variable_declaration
    (variable_declarator
      name: (identifier) @local.definition.var)))

(for_statement
  initializer: (variable_declaration
    (variable_declarator
      name: (identifier) @local.definition.var)))

(fixed_statement
  (variable_declaration
    (variable_declarator
      name: (identifier) @local.definition.var)))

(declaration_expression
  name: (identifier) @local.definition.var)

(foreach_statement
  left: (identifier) @local.definition.var)

(foreach_statement
  left: (tuple_pattern
    (identifier) @local.definition.var))

(parameter
  name: (identifier) @local.definition.parameter)

(parameter_list
  name: (identifier) @local.definition.parameter)

(implicit_parameter) @local.definition.parameter

(catch_declaration
  name: (identifier) @local.definition.var)

(declaration_pattern
  name: (identifier) @local.definition.var)

(recursive_pattern
  name: (identifier) @local.definition.var)

(from_clause
  name: (identifier) @local.definition.var)

(let_clause
  (identifier) @local.definition.var)
