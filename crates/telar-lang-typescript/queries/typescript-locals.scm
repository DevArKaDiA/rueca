; SPDX-License-Identifier: Apache-2.0
;
; Local bindings specific to the TypeScript and TSX grammars.
;
; Source (Apache-2.0, Copyright the nvim-treesitter contributors):
;   https://github.com/nvim-treesitter/nvim-treesitter/blob/29212d826054bb2948fa9d3a99eca9bf6a050eb2/runtime/queries/typescript/locals.scm
;
; Upstream starts with `; inherits: ecma`. Telar's loader resolves it by
; prepending ecma-locals.scm before compiling (src/locals.rs).
;
; Adapted for Telar (see queries/README.md): the upstream patterns are kept
; unchanged; the section marked "Telar:" at the end is new.

(required_parameter
  (identifier) @local.definition)

(optional_parameter
  (identifier) @local.definition)

; x => x
(arrow_function
  parameter: (identifier) @local.definition.parameter)

; ({ a }) => null
(required_parameter
  (object_pattern
    (shorthand_property_identifier_pattern) @local.definition.parameter))

; ({ a: b }) => null
(required_parameter
  (object_pattern
    (pair_pattern
      value: (identifier) @local.definition.parameter)))

; ([ a ]) => null
(required_parameter
  (array_pattern
    (identifier) @local.definition.parameter))

(required_parameter
  (rest_pattern
    (identifier) @local.definition.parameter))

; Telar: body-less callables own their parameters, so `(x: T) => void` or an
; overload signature does not leak `x` into the enclosing function.
(function_signature) @local.scope

(method_signature) @local.scope

(abstract_method_signature) @local.scope

(function_type) @local.scope

(construct_signature) @local.scope

(call_signature) @local.scope

; Telar: abstract classes declared inside a function are local bindings.
(abstract_class_declaration
  name: (_) @local.definition.type)

; Telar: a namespace body (`namespace N { ... }`, `declare module "m" { ... }`)
; is not a local scope; its declarations are namespace members.
(internal_module
  body: (_) @telar.namespace_body)

(module
  body: (_) @telar.namespace_body)
