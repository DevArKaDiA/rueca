; SPDX-License-Identifier: Apache-2.0
;
; Local bindings specific to the JavaScript grammar.
;
; Source (Apache-2.0, Copyright the nvim-treesitter contributors):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/javascript/locals.scm
;
; Upstream starts with `; inherits: ecma`. Telar's loader resolves it by
; prepending ecma-locals.scm before compiling (src/locals.rs).
;
; Adapted for Telar (see queries/README.md):
;   - removed `field_definition` and `this.x = ...` definitions (properties,
;     not bindings);
;   - removed the `method_definition` name definition (property, not binding);
;   - removed the `(member_expression object: (this))` reference pattern.

(formal_parameters
  (identifier) @local.definition.parameter)

; function(arg = []) {
(formal_parameters
  (assignment_pattern
    left: (identifier) @local.definition.parameter))

; x => x
(arrow_function
  parameter: (identifier) @local.definition.parameter)

; ({ a }) => null
(formal_parameters
  (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.parameter))

; ({ a: b }) => null
(formal_parameters
  (object_pattern
    (pair_pattern
      value: (identifier) @local.definition.parameter)))

; ([ a ]) => null
(formal_parameters
  (array_pattern
    (identifier) @local.definition.parameter))

(formal_parameters
  (rest_pattern
    (identifier) @local.definition.parameter))
