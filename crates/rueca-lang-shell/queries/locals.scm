; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local scopes and bindings for shell scripts (tree-sitter-bash 0.25.1).
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/bash/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
;
; Adapted for Rueca:
; - nvim's `@local.reference` patterns (every `variable_name` and `word`)
;   are removed: the extractor visits every expansion itself and asks the
;   table whether the name is bound.
; - The function definition capture is removed: shell functions are global
;   once defined, so the extractor treats them as file-level definitions.
; - Added bindings nvim does not capture: array element assignments
;   (`a[i]=x`), names declared without a value (`local x`, `declare -a xs`,
;   `export X`, `readonly Y`) and `for` loop variables. `read`, `getopts`,
;   `mapfile` / `readarray`, `printf -v` and `select` bind names too; those
;   are commands, not syntax, and are recognized in `src/locals.rs`.

; Scopes
(function_definition) @local.scope

; Definitions
(variable_assignment
  name: (variable_name) @local.definition.var)

(variable_assignment
  name: (subscript
    name: (variable_name) @local.definition.var))

(declaration_command
  (variable_name) @local.definition.var)

(for_statement
  variable: (variable_name) @local.definition.var)
