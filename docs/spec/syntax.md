# Syntax

### LANG-1: Source text

Source is UTF-8 with no byte order mark; invalid UTF-8 is refused before any position is
assigned. LF and CRLF are newlines; a bare CR is refused. The `Bidi_Control` characters
(U+061C, U+200E–U+200F, U+202A–U+202E, U+2066–U+2069) are refused everywhere, comments
included; a string escape can denote them. Identifiers are ASCII. Positions are byte
offsets; columns count Unicode scalars, a tab counting one.

```ebnf
letter        = "A".."Z" | "a".."z" | "_" ;
digit         = "0".."9" ;
identifier    = letter, { letter | digit } ;
NL            = LF | CR, LF ;
line-comment  = "//", { non-newline-scalar } ;
block-comment = "/*", { block-comment | non-delimiter-scalar }, "*/" ;
```

A `non-newline-scalar` is any scalar other than LF and CR; a `non-delimiter-scalar` is any
scalar that does not begin `/*` or `*/`. Tokens are formed by longest match, except that
no float literal begins directly after a `.` token, so `p.0.1` is `p`, `.`, `0`, `.`, `1`.
Comments nest. `///` and `/** */` are documentation comments and otherwise comments. A
block comment's newlines count as newlines for LANG-4. An unterminated comment is one
invalid token.

### LANG-2: Keywords

The reserved keywords are exactly:

```text
fn let mut match if else loop while for in break continue return
take copy view type stable resource scoped region bound
pub opaque const where true false bytes
```

A keyword cannot be declared or used as a name anywhere, after a `.` included. These names
are contextual and ordinary elsewhere: `drop` after a resource declaration; `plain`, `shareable`, `send`, `key`, `ordered`, `integer`, `float`, `packed` and
`streamable` in a bound list; `own` before `fn` in a type; `waits` after a function or callable header. Library types and functions, including `Region`,
`View`, `Rc`, `Arc` and `Capacity`, are ordinary names. `_` is the discard pattern.

```hirsel
fn view_count(n: u64) -> u64 { n }                        // `view_count` is one identifier
```
```hirsel
fn view(n: u64) -> u64 { n }                              // refused: LANG-2, keyword as a name
```

### LANG-3: Literals

```ebnf
digits(D)    = D, { [ "_" ], D } ;
hex-digit    = digit | "a".."f" | "A".."F" ;
integer      = ( digits(digit) | "0x", digits(hex-digit) | "0o", digits("0".."7")
               | "0b", digits("0" | "1") ), [ int-suffix ] ;
int-suffix   = "u8" | "u16" | "u32" | "u64" | "i8" | "i16" | "i32" | "i64" ;
float        = digits(digit), ".", digits(digit), [ exponent ], [ float-suffix ]
             | digits(digit), exponent, [ float-suffix ]
             | digits(digit), float-suffix ;
exponent     = ( "e" | "E" ), [ "+" | "-" ], digits(digit) ;
float-suffix = "f32" | "f64" ;
escape       = "\\", ( "\\" | '"' | "'" | "n" | "r" | "t" | "0" )
             | "\\x", hex-digit, hex-digit
             | "\\u{", hex-digit, { hex-digit }, "}" ;
string       = '"', { string-scalar | escape }, '"' ;
byte-string  = 'b"', { ascii-string-scalar | byte-escape }, '"' ;
raw-string   = "r", hashes, '"', raw-body, '"', hashes ;
raw-byte-string    = "br", hashes, '"', ascii-raw-body, '"', hashes ;
hashes       = { "#" } ;
character    = "'", ( character-scalar | escape ), "'" ;
byte         = "b'", ( ascii-character-scalar | byte-escape ), "'" ;
literal      = integer | float | character | byte | string | byte-string
             | raw-string | raw-byte-string | "true" | "false" ;
```

`string-scalar` excludes `"`, `\`, the controls U+0000–U+001F and U+007F, newlines and
`Bidi_Control`; `character-scalar` is the same with `'` in place of `"`; `raw-body` is any
scalars other than CR; `byte-escape` is `escape` without its `\u{..}` form; the `ascii-`
forms also exclude non-ASCII. In strings and characters `\xNN`
is at most 7F and `\u{..}` has one to six digits denoting a Unicode scalar value; in byte strings
and byte literals `\xNN` is any byte and `\u{..}` is absent. A character literal is exactly
one scalar. A raw literal closes at the first `"` followed by its opening number of
hashes, keeps every source byte including LF, and has no escapes; a CR inside it is
refused. Integer literals have no sign: `-128i8` is negation of a magnitude, and the
magnitude of `MIN` is accepted there. A malformed number or suffix is one invalid token. A
fractional point needs a digit on both sides, so `1..2` is a range. An unsuffixed number
takes its type from the expected type ([LANG-11](language.md#lang-11-typing-is-local)): an
`integer` token only an integer type and a `float` token only a float type, so `1` is never a
float (`1.0` and `1f64` are). A literal whose value does not fit its type is refused, at the
literal: `300` as a `u8`, `300u8`, and a magnitude above `MAX` that is not the operand of a
negation giving `MIN`. A literal operand ([LANG-21](language.md#lang-21-operators)) is
evaluated when checked; one whose evaluation fails in the type it takes (a value that does
not fit, a division by zero, a shift count out of range) is refused here, at the
literal operand, and never fails at run time.

```hirsel
const MASK: u32 = 0xff_ff
```
```hirsel
const BAD: u64 = 1.                                       // refused: LANG-3
```
```hirsel
fn f() -> u8 { 300 }                                      // refused: LANG-3, 300 does not fit u8
```
```hirsel
fn f(x: u8) -> bool { (255 + 1) < x }                     // refused: LANG-3, at 255 + 1: 256 is not a u8
```
```hirsel
fn f() -> f64 { 1 }                                       // refused: LANG-3, 1 is an integer
```

### LANG-4: Statements and newlines

No semicolons are inserted. These rules use syntax only:

1. Inside `( )` and `[ ]`, a newline is whitespace. A `{ }` block starts its own statement
   context, even inside parentheses. Record, variant and match braces are lists (rule 7).
2. While a statement or item is incomplete (an operand, delimiter, pattern, type,
   right-hand side or body is missing) a newline is whitespace: `let x =\n f()` and
   `a +\n b` continue.
3. At a complete statement or item a newline ends it. A leading operator, `.`, `(`, `[`
   or `?` on the next line never continues it. A bare `return`, `break` or `continue` is
   complete at a newline.
4. These continue across newlines and comments: `else` after an `if` block; `drop` after a
   resource declaration; `->`, `waits`, `where`, `in` and a body `{` after a
   function header that has no body yet.
5. `;` ends a statement explicitly. `}` and the end of file end a pending complete
   statement, so no END is needed before them. Two statements on one line are always
   separated by `;`.
6. A block's last expression is its value; a trailing `;` discards it. A final `let` or
   assignment gives unit.
7. In list braces a newline can replace a comma after a complete entry.

Indentation is never significant.

```hirsel
fn total(a: u64, b: u64) -> u64 {
  let sum = a +
    b
  sum
}
```
```hirsel
fn total(a: u64, b: u64) -> u64 {
  let sum = a
    + b                                                   // refused: LANG-4, `+ b` has no left operand
  sum
}
```

### LANG-5: Grammar

```ebnf
(* files and items *)
file        = { NL | item, ( END | EOF ) }, EOF ;
item        = { attribute }, [ "pub", [ "opaque" ] ], ( function | type-def | const-def ) ;
attribute   = "@", ( "test" | "compact", "(", ( "u16" | "u32" | "u64" ), ")"
                   | "twin", "(", type-path, ")" ) ;
const-def   = "const", name, ":", type, "=", expression ;
function    = "fn", name, fn-header, block ;
fn-header   = [ generics ], "(", list(parameter), ")", [ "->", type ],
              [ "waits" ], [ outlives ] ;
parameter   = [ "take" | "mut" | "bytes" ], name, ":", type ;
generics    = "[", nonempty(generic), "]" ;
generic     = name, [ ":", bounds ] | "in", name ;
bounds      = bound-name, { "+", bound-name } ;
bound-name  = "plain" | "shareable" | "send" | "key" | "ordered" | "integer" | "float"
            | "packed" | "streamable" ;
outlives    = "where", name, ":", name, { ",", name, ":", name } ;
type-def    = [ "stable" | "resource" | "scoped" ], "type", name, [ generics ],
              [ ":", bounds ], [ "grant", "bytes", integer ],
              ( fields | "=", type-rhs | empty ), [ drop ] ;
fields      = "{", [ field, { entry-sep, field }, [ entry-sep ] ], "}" ;
field       = name, ":", type ;
type-rhs    = variant, "|", variant, { "|", variant }
            | name, "(", list(payload), ")"
            | type ;
variant     = name, [ "(", list(payload), ")" ] ;
payload     = [ name, ":" ], type ;
drop        = "drop", "(", name, ")", block ;

(* types *)
type        = type-atom, [ anchor ] ;
anchor      = "in", place ;
place       = name, { ".", name } ;
type-atom   = type-path, [ "[", nonempty(type-arg), "]" ]
            | "(", ")" | "(", type, ")" | "(", type, ",", list(type), ")"
            | [ "scoped" | "own", [ "[", bounds, "]" ] ], "fn", "(", list(fn-param), ")",
              [ "->", type ], [ "waits" ] ;
type-arg    = type | "in", place | integer ;
fn-param    = [ "take" | "mut" ], [ name, ":" ], type ;
type-path   = name, { ".", name } ;

(* statements and expressions *)
block       = "{", { statement, END }, [ statement ], "}" ;
statement   = "let", [ "mut" ], pattern, [ ":", type ], [ "=", expression, [ "else", block ] ]
            | expression, assign-op, expression
            | expression ;
assign-op   = "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "<<=" | ">>=" | "&=" | "|=" | "^=" ;
cond        = expression | "let", pattern, "=", expression ;
label       = "'", name, ":" ;
expression  = range ;
range       = or-expr, [ ( ".." | "..=" ), or-expr ] ;
or-expr     = and-expr, { "||", and-expr } ;
and-expr    = compare, { "&&", compare } ;
compare     = bit-or, [ ( "==" | "!=" | "<" | "<=" | ">" | ">=" ), bit-or ] ;
bit-or      = bit-xor, { "|", bit-xor } ;
bit-xor     = bit-and, { "^", bit-and } ;
bit-and     = shift, { "&", shift } ;
shift       = additive, { ( "<<" | ">>" ), additive } ;
additive    = product, { ( "+" | "-" ), product } ;
product     = prefix, { ( "*" | "/" | "%" ), prefix } ;
prefix      = ( "-" | "!" | "~" | "take" | "mut" | "copy" | "view" ), prefix | placed ;
placed      = postfix, [ "in", place ] ;
postfix     = primary, { "(", list(argument), ")" | "[", expression, "]"
                       | ".", ( name | digits(digit) ) | "?" } ;
argument    = [ name, ":" ], expression | "bytes", expression ;
primary     = literal | type-path | "(", ")" | "(", expression, ")"
            | "(", expression, ",", list(expression), ")"
            | "[", list(expression), "]" | "[", expression, ";", integer, "]"
            | [ label ], ( block | loop-expr ) | if-expr | match-expr | closure
            | "region", name, block | "bound", "bytes", expression, block
            | "return", [ expression ] | "break", [ "'", name ], [ expression ]
            | "continue", [ "'", name ] ;
loop-expr   = "loop", block | "while", cond, block
            | "for", [ "take" ], pattern, "in", expression, block ;
if-expr     = "if", cond, block, [ "else", ( block | if-expr ) ] ;
match-expr  = "match", expression, "{", [ arm, { entry-sep, arm }, [ entry-sep ] ], "}" ;
arm         = pattern, [ "if", expression ], "=>", expression ;
closure     = [ "scoped" ], "fn", "(", list(fn-param), ")", [ "->", type ], [ "waits" ],
              block ;

(* patterns *)
pattern     = "_" | [ "take" ], name | scalar, [ "..=", scalar ]
            | "(", ")" | "(", pattern, ",", list(pattern), ")"
            | type-path, "(", [ pattern-field, { ",", pattern-field } ], [ ",", ".." ], ")" ;
pattern-field = [ name, ":" ], pattern ;
scalar      = literal | "-", integer ;

list(X)     = [ X, { ",", X }, [ "," ] ] ;
nonempty(X) = X, { ",", X }, [ "," ] ;
END         = NL, { NL } | ";" ;
entry-sep   = "," , { NL } | NL, { NL } ;
```

Precedence, strongest first:

| operators | associativity |
|---|---|
| call, index, `.`, `?` | left, postfix |
| `in` (placement) | postfix, once |
| `- ! ~ take mut copy view` | prefix |
| `* / %` | left |
| `+ -` | left |
| `<< >>` | left |
| `&`, then `^`, then `\|` | left at each level |
| `== != < <= > >=` | none: comparisons do not chain |
| `&&`, then `\|\|` | left, short-circuit |
| `.. ..=` | none |
| `return e`, `break e`, `break 'l e` | take a whole expression |
| assignment | statement only, no chaining |

The grammar is context-free. Parsing needs no names or types: types appear only after
`:` and `->`, inside `[ ]` after a type path and on the right of a type definition; expressions never carry type
arguments, so postfix `[ ]` always indexes; constructors, variants and calls share call
syntax and are told apart by name resolution; braces after a condition begin a block,
because records are built with parentheses. On the right of a type definition, two or more
`|`-separated variants or one name followed by `(` declare variants; anything else is an
alias. A `let` without `=` declares an uninitialized slot and must have a name pattern and
a type ([OWN-5](ownership.md#own-5-availability)); `let x` alone is refused here, at
`let`. A `let else` initializer and an `if`, `while`
or `match` subject cannot end in an unparenthesized block. `for` patterns, `let` patterns
without `else`, and `let`-condition patterns are checked for refutability after parsing
([LANG-17](language.md#lang-17-match)). An attribute applies only to its own target:
`@test` to a function, `@compact` to a `stable type`, `@twin` to a type; a repeated
attribute, an attribute on another item, a representation keyword on an alias,
and `drop` on a declaration that is not `resource` are refused. A parameter marked `bytes`
is a byte count of type `u64`, and its argument is written `bytes e`; the mark is part of the
signature, as `take` is. In a core declaration file ([core](core.md)) a function may end at
its header, with no body, and a type with no body is built in; everywhere else a function
needs a body. Malformed syntax with no
more specific rule is LANG-5.

```hirsel
fn f(xs: List[u64]) -> u64 { if list.len(xs) > 0 { xs[0] } else { 0 } }
```
```hirsel
fn f(a: u64, b: u64, c: u64) -> bool { a < b < c }        // refused: LANG-5, comparisons do not chain
```
