; Vendored from tree-sitter-c-sharp 0.23.5 (queries/tags.scm), with one fix: the
; upstream file ends with a stray `@module` capture (a duplicate of the line
; above it) that `tree-sitter-tags` rejects as an unknown capture, so the crate's
; `TAGS_QUERY` const does not compile here. That final line is dropped.
; MIT-licensed, © the tree-sitter-c-sharp authors.

(class_declaration name: (identifier) @name) @definition.class

(class_declaration (base_list (_) @name)) @reference.class

(interface_declaration name: (identifier) @name) @definition.interface

(interface_declaration (base_list (_) @name)) @reference.interface

(method_declaration name: (identifier) @name) @definition.method

(object_creation_expression type: (identifier) @name) @reference.class

(type_parameter_constraints_clause (identifier) @name) @reference.class

(type_parameter_constraint (type type: (identifier) @name)) @reference.class

(variable_declaration type: (identifier) @name) @reference.class

(invocation_expression function: (member_access_expression name: (identifier) @name)) @reference.send

(namespace_declaration name: (identifier) @name) @definition.module
