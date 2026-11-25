/// <reference types="tree-sitter-cli/dsl" />

const keywords = [
  'fn', 'let', 'mut', 'match', 'if', 'else', 'loop', 'while', 'for', 'in',
  'break', 'continue', 'return', 'take', 'copy', 'view', 'type', 'resource',
  'scoped', 'region', 'bound', 'pub', 'opaque', 'const', 'where', 'true',
  'false', 'bytes',
];
const bounds = [
  'plain', 'shareable', 'send', 'cross', 'key', 'ordered', 'integer', 'float',
  'packed', 'streamable', 'codec', 'inert',
];
const digits = /[0-9](_?[0-9])*/;
const hex = /[0-9a-fA-F]/;
const simpleEscape = /[\\'"nrt0]/;
const asciiHexEscape = seq('x', /[0-7]/, hex);
const byteEscape = seq('\\', choice(simpleEscape, seq('x', hex, hex)));
const stringEscape = seq('\\', choice(simpleEscape, asciiHexEscape, /u\{[0-9a-fA-F]{1,6}\}/));

function nonempty(rule) {
  return seq(rule, repeat(seq(',', rule)), optional(','));
}

function commaList(rule) {
  return optional(nonempty(rule));
}

function separated(rule, separator) {
  return optional(seq(rule, repeat(seq(separator, rule)), optional(separator)));
}

function infix(left, operator, right) {
  return seq(field('left', left), field('operator', operator), field('right', right));
}

const binaryTiers = [
  ['range', 'logical_or', ['..', '..='], false],
  ['logical_or', 'logical_and', ['||']],
  ['logical_and', 'comparison', ['&&']],
  ['comparison', 'bit_or', ['==', '!=', '<', '<=', '>', '>='], false],
  ['bit_or', 'bit_xor', ['|']],
  ['bit_xor', 'bit_and', ['^']],
  ['bit_and', 'shift', ['&']],
  ['shift', 'sum', ['<<', '>>']],
  ['sum', 'product', ['+', '-']],
  ['product', 'prefix', ['*', '/', '%']],
];

const binaryRules = Object.fromEntries(binaryTiers.flatMap(([tier, operand, operators, leftAssociative = true]) => {
  const rule = `_${tier}`;
  const operation = `_${tier}_operation`;
  return [
    [rule, $ => choice($[`_${operand}`], alias($[operation], $.binary_expression))],
    [operation, $ => infix(
      $[leftAssociative ? rule : `_${operand}`],
      operators.length === 1 ? operators[0] : choice(...operators),
      $[`_${operand}`],
    )],
  ];
}));

module.exports = grammar({
  name: 'hirsel',

  word: $ => $.identifier,
  reserved: { global: _ => keywords },
  extras: $ => [/[ \t]/, $._trivia_newline, $.line_comment],

  externals: $ => [
    $.line_comment, $.raw_string_literal, $.raw_byte_string_literal,
    $._expression_newline, $._entry_newline, $._exit_newline, $._trivia_newline,
    $._item_newline, $.steps_mode, $._projection_dot, $._error_recovery, $._bytes_mode,
  ],

  conflicts: $ => [
    [$.let_statement, $._pattern],
    [$.binding_pattern],
  ],

  rules: {
    source_file: $ => seq(
      repeat(choice(seq($._item, choice(';', $._item_newline)), ';')), optional($._item),
    ),
    _item: $ => choice(
      $.function_definition, $.function_signature, $.type_definition,
      $.constant_definition, $.clone_definition,
    ),
    visibility: _ => seq('pub', optional('opaque')),
    attribute: _ => seq('@', choice('test', seq('compact', '(', choice('u16', 'u32', 'u64'), ')'))),
    function_definition: $ => seq(
      repeat($.attribute), optional($.visibility), optional('cold'), 'fn', field('name', $.identifier),
      $._function_header, field('body', $.block),
    ),
    function_signature: $ => prec.right(seq(
      repeat($.attribute), optional($.visibility), optional('cold'), 'fn', field('name', $.identifier),
      $._function_header,
    )),
    clone_definition: $ => seq(
      repeat($.attribute), optional($.visibility), 'clone', $._function_header, field('body', $.block),
    ),
    _function_header: $ => seq(
      optional($.generic_parameters), field('parameters', $.parameters),
      optional($._return_type), optional('waits'), optional($.where_clause),
    ),
    _return_type: $ => seq('->', field('return_type', $.type)),
    parameters: $ => seq('(', commaList($.parameter), ')'),
    parameter: $ => seq(optional($._mode), field('name', $.identifier), ':', field('type', $.type)),
    _mode: $ => choice('take', 'mut', 'bytes', $.steps_mode),
    where_clause: $ => seq('where', $.outlives, repeat(seq(',', $.outlives))),
    outlives: $ => seq($.type_path, ':', $.type_path),
    generic_parameters: $ => seq('[', nonempty($.generic_parameter), ']'),
    generic_parameter: $ => seq(optional('in'), field('name', $.identifier), optional(seq(':', $.bounds))),
    bounds: $ => prec.right(seq($._bound, repeat(seq('+', $._bound)))),
    _bound: _ => choice(...bounds),
    constant_definition: $ => seq(
      optional($.visibility), 'const', field('name', $.identifier),
      ':', field('type', $.type), '=', field('value', $._expression),
    ),
    type_definition: $ => prec.right(seq(
      repeat($.attribute), optional($.visibility), optional(choice('resource', 'scoped')),
      'type', field('name', $.identifier), optional($.generic_parameters),
      optional(seq(':', $.bounds, optional($.codec_via))), optional($.grant),
      optional(choice($.fields, seq('=', choice($.variants, field('alias', $.type))))),
      optional($.drop_definition),
    )),
    codec_via: $ => seq(
      'via', field('type', $.type), '(', 'to', ':', field('to', $.identifier),
      ',', 'from', ':', field('from', $.identifier), ')',
    ),
    grant: $ => seq('grant', 'bytes', $.integer_literal),
    fields: $ => seq('{', separated($.field_definition, choice(',', $._entry_newline)), '}'),
    field_definition: $ => seq(
      optional('removed'), field('name', $.identifier), ':', field('type', $.type),
      optional(seq('=', field('default', $._scalar))),
    ),
    variants: $ => choice(
      seq($.variant, repeat1(seq('|', $.variant))),
      alias($._single_variant, $.variant),
    ),
    _single_variant: $ => choice(
      seq(field('name', $.identifier), $.payload),
      seq('removed', field('name', $.identifier), optional($.payload)),
    ),
    variant: $ => seq(optional('removed'), field('name', $.identifier), optional($.payload)),
    payload: $ => seq('(', optional(choice(
      nonempty(alias($._positional_payload, $.payload_field)),
      nonempty(alias($._labelled_payload, $.payload_field)),
    )), ')'),
    _positional_payload: $ => field('type', $.type),
    _labelled_payload: $ => seq(field('name', $.identifier), ':', field('type', $.type)),
    drop_definition: $ => seq('drop', '(', field('name', $.identifier), ')', field('body', $.block)),

    type: $ => prec.right(seq($._type_atom, optional($.anchor))),
    _type_atom: $ => choice($.type_path, $.generic_type, $.tuple_type, $.callable_type),
    type_path: $ => prec.right(seq($.identifier, repeat(seq('.', $.identifier)))),
    generic_type: $ => seq(field('name', $.type_path), '[', nonempty($._type_argument), ']'),
    _type_argument: $ => choice($.type, $.anchor, $.integer_literal),
    tuple_type: $ => seq('(', commaList($.type), ')'),
    anchor: $ => seq('in', $.type_path),
    callable_type: $ => prec.right(seq(
      optional(seq(choice('scoped', 'own'), optional(seq('[', $.callable_bounds, ']')))),
      'fn', '(', commaList($.callable_parameter), ')', optional($._return_type), optional('waits'),
    )),
    callable_bounds: $ => seq(choice('view', $._bound), repeat(seq('+', choice('view', $._bound)))),
    callable_parameter: $ => seq(
      optional($._mode), optional(seq(field('name', $.identifier), ':')), field('type', $.type),
    ),

    block: $ => seq('{', repeat(choice(seq($._statement, choice(';', $._expression_newline)), ';')),
      optional($._statement), '}'),
    _statement: $ => choice($.let_statement, $.assignment, $._expression),
    let_statement: $ => prec.right(seq(
      'let', optional('mut'),
      choice(
        seq(field('pattern', $._pattern), optional(seq(':', field('type', $._slot_type))),
          '=', field('value', $._expression), optional(seq('else', field('alternative', $.block)))),
        seq(field('pattern', $.binding_pattern), ':', field('type', $._slot_type)),
      ),
    )),
    _slot_type: $ => prec.right(seq($._type_atom, optional(seq($.anchor, repeat(seq(',', $.type_path)))))),
    assignment: $ => infix($._range, choice(
      '=', '+=', '-=', '*=', '/=', '%=', '<<=', '>>=', '&=', '|=', '^=',
    ), $._expression),
    _condition: $ => choice($._expression, $.let_condition),
    let_condition: $ => seq('let', field('pattern', $._pattern), '=', field('value', $._expression)),

    _expression: $ => choice($._range, $.return_expression, $.break_expression, $.continue_expression),
    ...binaryRules,
    _prefix: $ => choice($.unary_expression, $._postfix, $.try_map_expression),
    unary_expression: $ => seq(
      field('operator', choice('-', '!', '~', 'take', 'mut', 'copy', 'view')),
      field('operand', $._prefix),
    ),
    _postfix: $ => choice(
      $._primary, $.call_expression, $.index_expression, $.field_expression, $.try_expression,
    ),
    call_expression: $ => prec.left(seq(field('function', $._postfix), $.arguments)),
    arguments: $ => seq('(', commaList($._argument), ')'),
    _argument: $ => choice($._expression, $.named_argument, $.marked_argument),
    named_argument: $ => seq(field('name', $.identifier), ':', field('value', $._expression)),
    marked_argument: $ => seq(
      field('mode', choice(alias($._bytes_mode, 'bytes'), $.steps_mode)), field('value', $._expression),
    ),
    index_expression: $ => prec.left(seq(
      field('object', $._postfix), '[', field('index', $._expression), ']',
    )),
    field_expression: $ => prec.left(seq(
      field('object', $._postfix), alias($._projection_dot, '.'),
      field('field', choice($.identifier, $.tuple_index)),
    )),
    tuple_index: _ => /[0-9]+/,
    try_expression: $ => prec.left(seq(field('value', $._postfix), '?')),
    try_map_expression: $ => seq(
      field('value', $._postfix), '?=>', field('mapper', choice($.mapper_path, $.mapper_arm)),
    ),
    mapper_path: $ => prec.right(seq($.identifier, repeat(seq('.', $.identifier)))),
    mapper_arm: $ => seq('|', field('pattern', $._pattern), '|', field('value', $._prefix)),

    _primary: $ => choice(
      $._literal, $.identifier, $.parenthesized_expression, $.ascription_expression, $.bytes_expression,
      $.tuple_expression, $.array_expression, $.repeat_array_expression,
      $.block, $.labelled_expression, $._loop, $.if_expression, $.match_expression,
      $.closure, $.region_expression, $.bound_expression,
    ),
    parenthesized_expression: $ => seq('(', $._expression, ')'),
    ascription_expression: $ => seq('(', field('value', $._expression), ':', field('type', $.type), ')'),
    bytes_expression: $ => seq('bytes', '[', field('type', $.type), ']'),
    tuple_expression: $ => seq('(', optional(seq($._expression, ',', commaList($._expression))), ')'),
    array_expression: $ => seq('[', commaList($._expression), ']'),
    repeat_array_expression: $ => seq('[', field('value', $._expression), ';',
      field('count', $.integer_literal), ']'),
    labelled_expression: $ => seq(field('label', $.label), ':', choice($.block, $._loop)),
    _loop: $ => choice($.loop_expression, $.while_expression, $.for_expression),
    loop_expression: $ => seq('loop', field('body', $.block)),
    while_expression: $ => seq('while', field('condition', $._condition), field('body', $.block)),
    for_expression: $ => seq(
      'for', optional('take'), field('pattern', $._pattern), 'in', field('value', $._expression),
      field('body', $.block),
    ),
    if_expression: $ => prec.right(seq(
      'if', field('condition', $._condition), field('consequence', $.block),
      optional(seq('else', field('alternative', choice($.block, $.if_expression)))),
    )),
    match_expression: $ => seq('match', field('value', $._expression), '{',
      separated($.match_arm, choice(',', $._expression_newline)), '}'),
    match_arm: $ => seq(
      field('pattern', $._pattern), optional(seq('if', field('guard', $._expression))),
      '=>', field('value', $._expression),
    ),
    closure: $ => seq(
      optional('scoped'), 'fn', field('parameters', $.closure_parameters),
      optional($._return_type), optional('waits'), field('body', $.block),
    ),
    closure_parameters: $ => seq('(', commaList($.closure_parameter), ')'),
    closure_parameter: $ => seq(
      optional(choice('take', 'mut', 'bytes')), field('name', $.identifier),
      optional(seq(':', field('type', $.type))),
    ),
    region_expression: $ => seq('region', field('name', $.identifier), field('body', $.block)),
    bound_expression: $ => seq(
      'bound', field('unit', choice('bytes', 'steps')), field('limit', $._expression),
      field('body', $.block),
    ),
    return_expression: $ => prec.right(seq('return', optional(choice($._exit_newline, field('value', $._expression))))),
    break_expression: $ => prec.right(seq('break', optional(choice($._exit_newline,
      seq(field('label', $.label), optional(choice($._exit_newline, field('value', $._expression)))),
      field('value', $._expression))))),
    continue_expression: $ => seq('continue', optional(choice($._exit_newline, field('label', $.label)))),

    _pattern: $ => choice(
      $.binding_pattern, $.mut_pattern, $.wildcard_pattern, $.literal_pattern, $.range_pattern,
      $.tuple_pattern, $.constructor_pattern,
    ),
    binding_pattern: $ => prec(1, seq(optional('take'), field('name', $.identifier))),
    mut_pattern: $ => seq('mut', field('name', $.identifier)),
    wildcard_pattern: _ => '_',
    literal_pattern: $ => $._scalar,
    range_pattern: $ => seq(field('start', $._scalar), '..=', field('end', $._scalar)),
    tuple_pattern: $ => seq('(', commaList($._pattern), ')'),
    constructor_pattern: $ => prec.right(choice(
      seq(field('name', $.type_path), '(', repeat(seq($.pattern_field, ',')),
        optional(choice($.pattern_field, seq($.rest_pattern, optional(',')))), ')'),
      field('name', $.qualified_constructor),
    )),
    qualified_constructor: $ => seq($.identifier, repeat1(seq('.', $.identifier))),
    pattern_field: $ => seq(optional(seq(field('name', $.identifier), ':')), $._pattern),
    rest_pattern: _ => '..',

    _scalar: $ => choice($._literal, $.negative_integer),
    negative_integer: $ => seq('-', $.integer_literal),
    _literal: $ => choice(
      $.integer_literal, $.float_literal, $.string_literal, $.raw_string_literal,
      $.byte_string_literal, $.raw_byte_string_literal, $.character_literal,
      $.byte_literal, $.boolean_literal,
    ),
    integer_literal: _ => token(seq(
      choice(digits, /0x[0-9a-fA-F](_?[0-9a-fA-F])*/, /0o[0-7](_?[0-7])*/, /0b[01](_?[01])*/),
      optional(/[ui](8|16|32|64)/),
    )),
    float_literal: _ => token(choice(
      seq(digits, '.', digits, optional(/[eE][+-]?[0-9](_?[0-9])*/), optional(/f(32|64)/)),
      seq(digits, /[eE][+-]?[0-9](_?[0-9])*/, optional(/f(32|64)/)),
      seq(digits, /f(32|64)/),
    )),
    string_literal: _ => token(seq('"', repeat(choice(
      /[^"\\\x00-\x1f\x7f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/, stringEscape,
    )), '"')),
    byte_string_literal: _ => token(seq('b"', repeat(choice(/[\x20-\x21\x23-\x5b\x5d-\x7e]/, byteEscape)), '"')),
    character_literal: _ => token(seq("'", choice(
      /[^'\\\x00-\x1f\x7f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/, stringEscape,
    ), "'")),
    byte_literal: _ => token(seq("b'", choice(/[\x20-\x26\x28-\x5b\x5d-\x7e]/, byteEscape), "'")),
    boolean_literal: _ => choice('true', 'false'),
    identifier: _ => /([a-zA-Z][a-zA-Z_0-9]*|_[a-zA-Z_0-9]+)/,
    label: _ => /'([a-zA-Z][a-zA-Z_0-9]*|_[a-zA-Z_0-9]+)/,
  },
});
