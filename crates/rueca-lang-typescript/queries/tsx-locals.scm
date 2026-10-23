; SPDX-License-Identifier: Apache-2.0
;
; Local bindings specific to the TSX grammar.
;
; Source (Apache-2.0, Copyright the nvim-treesitter contributors):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/tsx/locals.scm
;
; Upstream is the single line `; inherits: typescript`. Rueca's loader resolves
; it by compiling ecma-locals.scm + typescript-locals.scm + this file
; (src/locals.rs). No TSX-only patterns.
