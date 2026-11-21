(function_definition
  name: (identifier) @name) @definition.function

(function_signature
  name: (identifier) @name) @definition.function

(type_definition
  name: (identifier) @name) @definition.type

(constant_definition
  name: (identifier) @name) @definition.constant

(call_expression
  function: (identifier) @name) @reference.call

(call_expression
  function: (field_expression
    field: (identifier) @name)) @reference.call
