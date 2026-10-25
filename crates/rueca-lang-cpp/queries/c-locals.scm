; SPDX-License-Identifier: Apache-2.0
; SPDX-FileCopyrightText: nvim-treesitter contributors
;
; Local scopes and bindings for C (tree-sitter-c 0.24); also the base of
; `cpp-locals.scm` (tree-sitter-cpp 0.23), which Rueca appends to this file
; for C++ exactly as nvim-treesitter's `; inherits: c` does.
;
; Source:
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/c/locals.scm
;   commit 29212d826054bb2948fa9d3a99eca9bf6a050eb2 (branch `main`), license
;   Apache-2.0.
;
; Adapted for Rueca:
; - Only bindings that can be local to a function body are kept: parameters
;   and variables (`@local.definition.parameter`, `@local.definition.var`).
;   nvim's function, macro, type, field and label definitions are removed:
;   Rueca code extracts definitions, macros and types from the syntax tree.
; - Scopes are block-like constructs only. `translation_unit` and
;   `struct_specifier` scopes are removed (file-, namespace- and class-level
;   names are definitions, not locals), `preproc_function_def` is not a
;   scope (macro bodies are opaque text), and `switch_statement`,
;   `do_statement` and `case_statement` scopes are Rueca additions (C99
;   declarations in their blocks).
; - `@local.reference` captures and the Neovim-only `#set! reference.kind`
;   predicates are removed: Rueca looks names up by position in its own
;   walk (see `src/locals.rs`).

; ---------------------------------------------------------------------------
; Scopes
; ---------------------------------------------------------------------------

[
  (for_statement)
  (if_statement)
  (while_statement)
  (do_statement)
  (switch_statement)
  (case_statement)
  (function_definition)
  (compound_statement)
] @local.scope

; ---------------------------------------------------------------------------
; Definitions
; ---------------------------------------------------------------------------

(parameter_declaration
  declarator: (identifier) @local.definition.parameter)

(pointer_declarator
  declarator: (identifier) @local.definition.var)

(init_declarator
  declarator: (identifier) @local.definition.var)

(array_declarator
  declarator: (identifier) @local.definition.var)

(declaration
  declarator: (identifier) @local.definition.var)
