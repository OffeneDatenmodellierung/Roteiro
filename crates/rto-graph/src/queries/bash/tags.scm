; Hand-authored for Roteiro: tree-sitter-bash ships no tags query, so this
; minimal one surfaces shell function definitions and command invocations.
; Node names verified against tree-sitter-bash 0.25.1 `src/node-types.json`.

; Definitions

(function_definition
  name: (word) @name) @definition.function

; References — a command invocation references a name (an internal function if
; one is defined with that name, otherwise an external command left unresolved).

(command
  name: (command_name (word) @name)) @reference.call
