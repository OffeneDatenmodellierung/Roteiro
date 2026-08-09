; Hand-authored for Roteiro: tree-sitter-sequel (0.3.11) ships no tags query.
; Node shapes verified against the grammar's parse tree — CREATE statements name
; their object via `(object_reference name: (identifier))`, and a function/table
; call is an `invocation`. Tables/views map to `Other(...)` kinds; functions to
; `Fn` so their body invocations are captured in `meta.calls`.

; Definitions

(create_table
  (object_reference name: (identifier) @name)) @definition.table

(create_view
  (object_reference name: (identifier) @name)) @definition.view

(create_materialized_view
  (object_reference name: (identifier) @name)) @definition.view

(create_function
  (object_reference name: (identifier) @name)) @definition.function

; References — a call/aggregate invocation references a name (an internal
; function if one is defined with it, otherwise a builtin left unresolved).

(invocation
  (object_reference name: (identifier) @name)) @reference.call
