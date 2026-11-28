(line_comment) @comment
[
  (string_literal)
  (raw_string_literal)
  (byte_string_literal)
  (raw_byte_string_literal)
] @string
[(character_literal) (byte_literal)] @character
(integer_literal) @number
(float_literal) @number.float
(boolean_literal) @boolean

(data_document
  module: (identifier) @type
  record: (identifier) @type)
(named_argument name: (identifier) @property)
(call_expression function: (identifier) @constructor)
((arguments (identifier) @constructor) (#match? @constructor "^[A-Z]"))
((named_argument value: (identifier) @constructor) (#match? @constructor "^[A-Z]"))
((array_expression (identifier) @constructor) (#match? @constructor "^[A-Z]"))
((tuple_expression (identifier) @constructor) (#match? @constructor "^[A-Z]"))

["(" ")" "[" "]"] @punctuation.bracket
["." ":" ","] @punctuation.delimiter
