[
  "fn"
  "type"
  "let"
  "const"
  "pub"
  "opaque"
  "resource"
  "scoped"
  "region"
  "bound"
  "where"
  "in"
  "take"
  "mut"
  "copy"
  "view"
  "bytes"
  "clone"
  "drop"
  "grant"
  "own"
  "removed"
  "steps"
  "via"
  "waits"
  (steps_mode)
] @keyword

[
  "if"
  "else"
  "match"
] @keyword.conditional

[
  "for"
  "while"
  "loop"
] @keyword.repeat

[
  "break"
  "continue"
  "return"
] @keyword.return

(attribute) @attribute

((identifier) @type.builtin
  (#any-of? @type.builtin "u8" "u16" "u32" "u64" "i8" "i16" "i32" "i64" "f32" "f64" "bool"))

(bounds
  [
    "plain"
    "shareable"
    "send"
    "cross"
    "key"
    "ordered"
    "integer"
    "float"
    "packed"
    "streamable"
    "codec"
    "inert"
  ] @type.builtin)

(callable_bounds
  [
    "plain"
    "shareable"
    "send"
    "cross"
    "key"
    "ordered"
    "integer"
    "float"
    "packed"
    "streamable"
    "codec"
    "inert"
  ] @type.builtin)

(type_path
  (identifier) @type)

(type_definition
  name: (identifier) @type.definition)

(variant
  name: (identifier) @constructor)

(function_definition
  name: (identifier) @function)

(function_signature
  name: (identifier) @function)

(call_expression
  function: (identifier) @function.call)

(call_expression
  function: (field_expression
    field: (identifier) @function.call))

(parameter
  name: (identifier) @variable.parameter)

(closure_parameter
  name: (identifier) @variable.parameter)

(callable_parameter
  name: (identifier) @variable.parameter)

(field_definition
  name: (identifier) @property)

(field_expression
  field: [
    (identifier)
    (tuple_index)
  ] @property)

(named_argument
  name: (identifier) @property)

[
  (line_comment)
  (block_comment)
] @comment

[
  (string_literal)
  (raw_string_literal)
  (byte_string_literal)
  (raw_byte_string_literal)
] @string

[
  (character_literal)
  (byte_literal)
] @character

(integer_literal) @number

(float_literal) @number.float

(boolean_literal) @boolean

(label) @label

[
  "&&"
  "||"
  "=="
  "!="
  "<"
  "<="
  ">"
  ">="
  "+"
  "-"
  "*"
  "/"
  "%"
  "&"
  "|"
  "^"
  "~"
  "!"
  "<<"
  ">>"
  ".."
  "..="
  "?"
  "?=>"
  "="
  "+="
  "-="
  "*="
  "/="
  "%="
  "&="
  "|="
  "^="
  "<<="
  ">>="
  "=>"
  "->"
] @operator

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

[
  ","
  ";"
  ":"
  "."
] @punctuation.delimiter
