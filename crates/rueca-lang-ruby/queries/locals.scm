; SPDX-License-Identifier: MIT
; SPDX-FileCopyrightText: 2016 Rob Rix; nvim-treesitter contributors
;
; Local scopes and bindings for Ruby (tree-sitter-ruby 0.23.1).
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/ruby/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   MIT (the file's own header, Copyright (c) 2016 Rob Rix, reproduced in
;   NOTICE).
;
; Adapted for Rueca:
; - Only scopes and bindings are kept. The `@local.reference` patterns
;   (every identifier, constant and instance variable) are removed: the
;   extractor visits every identifier itself and asks the scope table
;   whether it is bound locally. The module, class and method name
;   definitions are removed too: they are definitions, extracted by Rust
;   code, never local bindings.
; - Scopes are split by kind (`queries/README.md`): `@local.scope.hard`
;   for the nodes that start a new local table in Ruby (`def`, `def
;   self.x`, `class`, `module`, `class << x`) and `@local.scope` for the
;   ones that see the enclosing locals (blocks and lambdas). `lambda` is
;   added.
; - Bindings added: `operator_assignment` (`x ||= …`), `exception_variable`
;   (`rescue E => e`), the `for` loop variable, nested `left_assignment_list`
;   targets and the parameters of nested `destructured_parameter`s.
; - Capture names follow Rueca's contract: `@local.definition.parameter`
;   for parameters, `@local.definition.var` for every other binding.

; SCOPES

[
  (method)
  (singleton_method)
  (class)
  (module)
  (singleton_class)
] @local.scope.hard

[
  (block)
  (do_block)
  (lambda)
] @local.scope

; PARAMETERS

(method_parameters
  (identifier) @local.definition.parameter)

(lambda_parameters
  (identifier) @local.definition.parameter)

(block_parameters
  (identifier) @local.definition.parameter)

(splat_parameter
  name: (identifier) @local.definition.parameter)

(hash_splat_parameter
  name: (identifier) @local.definition.parameter)

(optional_parameter
  name: (identifier) @local.definition.parameter)

(destructured_parameter
  (identifier) @local.definition.parameter)

(block_parameter
  name: (identifier) @local.definition.parameter)

(keyword_parameter
  name: (identifier) @local.definition.parameter)

; ASSIGNMENTS

(assignment
  left: (identifier) @local.definition.var)

(operator_assignment
  left: (identifier) @local.definition.var)

(left_assignment_list
  (identifier) @local.definition.var)

(rest_assignment
  (identifier) @local.definition.var)

(destructured_left_assignment
  (identifier) @local.definition.var)

(exception_variable
  (identifier) @local.definition.var)

(for
  pattern: (identifier) @local.definition.var)
