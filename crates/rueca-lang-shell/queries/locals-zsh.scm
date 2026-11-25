; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local scopes and bindings for zsh scripts (tree-sitter-zsh 0.64.3): the
; patterns of `locals.scm` (adapted from nvim-treesitter's bash query; see
; that file's header and `README.md` for the source and the adaptation)
; written against tree-sitter-zsh's node names.
;
; Differences from `locals.scm`:
; - loop variables are `simple_variable_name` nodes, and zsh's other loop
;   forms bind names too (`foreach x (…) … end`, `for x (…) cmd`);
; - names a declaration command declares without a value (`local x`,
;   `typeset -a xs`) are `word` arguments in this grammar: they are read in
;   `src/locals.rs` (`declaration_words`), not here.

; Scopes
(function_definition) @local.scope

; Definitions
(variable_assignment
  name: (variable_name) @local.definition.var)

(variable_assignment
  name: (subscript
    name: (variable_name) @local.definition.var))

(for_statement
  variable: (simple_variable_name) @local.definition.var)

(foreach_statement
  variable: (simple_variable_name) @local.definition.var)

(terse_for_statement
  variable: (simple_variable_name) @local.definition.var)
