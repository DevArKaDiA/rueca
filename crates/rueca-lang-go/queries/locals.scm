; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
; SPDX-FileCopyrightText: BloopAI (bloop contributors)
;
; Local scopes and bindings for Go (tree-sitter-go 0.25.0).
;
; Source (base):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/go/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
; Source (additions: function-literal parameters, type parameters, type
; switch aliases, `select` receive bindings, case-clause scopes):
;   https://github.com/BloopAI/bloop/blob/431e9e82c5a293c40f22aadb3615c3aa387af82e/server/bleep/src/intelligence/language/go/scopes.scm
;   commit 431e9e82c5a293c40f22aadb3615c3aa387af82e, license Apache-2.0.
;
; Adapted for Rueca:
; - Only scopes and bindings are kept. nvim's and bloop's `@local.reference`
;   patterns (identifiers, call targets, type references) are removed: the
;   extractor visits every identifier itself and asks the scope table whether
;   it is bound locally. The `package_clause` / `import_spec` namespace
;   definitions are removed too: imports are extracted by Rust code and are
;   never local.
; - Removed Neovim-only predicates (`#set! reference.kind …`) and the
;   `; @function` highlight comments.
; - Capture names follow Rueca's contract (`queries/README.md`):
;   `@local.definition.<kind>` with kinds `parameter`, `type_parameter`,
;   `var`, `const`, `range`, `type_switch`, `receive`, `type`, `function`.
; - Grammar 0.25 node names: type parameters are `type_parameter_declaration`
;   (bloop's `type_parameter_list (parameter_declaration (type_identifier))`
;   predates it); grouped `var ( … )` specs sit in `var_spec_list`; the
;   `name:` field is used where the grammar has it.
; - Scopes: nvim's `source_file`, function, method, function literal,
;   `block`, `if`, `for` and expression `switch` scopes, plus bloop's type
;   `switch` and `select` case scopes; every case clause (`expression_case`,
;   `type_case`, `default_case`) is an implicit block in Go and a scope
;   here (Rueca addition). bloop's `field_declaration_list` and
;   `type_declaration` scopes are dropped (fields and types are definitions
;   extracted by Rust code, not local bindings).
; - Rueca addition: a method's receiver type arguments (`func (l *List[T])`)
;   declare type parameters, so `T` in the method is never a type
;   reference (two patterns, value and pointer receivers).

; Scopes
(source_file) @local.scope
(function_declaration) @local.scope
(method_declaration) @local.scope
(func_literal) @local.scope
(block) @local.scope
(if_statement) @local.scope
(for_statement) @local.scope
(expression_switch_statement) @local.scope
(type_switch_statement) @local.scope
(expression_case) @local.scope
(type_case) @local.scope
(default_case) @local.scope
(communication_case) @local.scope

; Package-level and local declarations. Bindings directly in `source_file`
; are package-level names, not locals (see `src/locals.rs`).
(function_declaration
  name: (identifier) @local.definition.function)

(type_declaration
  (type_spec
    name: (type_identifier) @local.definition.type))

(type_declaration
  (type_alias
    name: (type_identifier) @local.definition.type))

; Parameters (receiver, parameters and named results) of functions,
; methods and function literals.
(parameter_declaration
  name: (identifier) @local.definition.parameter)

(variadic_parameter_declaration
  name: (identifier) @local.definition.parameter)

; Type parameters: `func Map[T, U any]`, `type List[T any]`.
(type_parameter_declaration
  name: (identifier) @local.definition.type_parameter)

; Type parameters of a method, declared by the receiver's type arguments:
; `func (l *List[T]) Push(v T)` (Rueca addition).
(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: (generic_type
        type_arguments: (type_arguments
          (type_elem
            (type_identifier) @local.definition.type_parameter))))))

(method_declaration
  receiver: (parameter_list
    (parameter_declaration
      type: (pointer_type
        (generic_type
          type_arguments: (type_arguments
            (type_elem
              (type_identifier) @local.definition.type_parameter)))))))

; x := …
(short_var_declaration
  left: (expression_list
    (identifier) @local.definition.var))

; var x T = …
(var_spec
  name: (identifier) @local.definition.var)

; const x = …
(const_spec
  name: (identifier) @local.definition.const)

; for k, v := range …
(for_statement
  (range_clause
    left: (expression_list
      (identifier) @local.definition.range)))

; switch v := x.(type)
(type_switch_statement
  alias: (expression_list
    (identifier) @local.definition.type_switch))

; select { case v := <-ch: }
(receive_statement
  left: (expression_list
    (identifier) @local.definition.receive))
