# Syntax

### LANG-1: Source text

Source is UTF-8 with no byte order mark. The parser refuses invalid UTF-8 at its first
invalid byte span, before tokenization ([PROG-7](diagnostics.md#prog-7-one-rule-one-site)).
LF and CRLF are newlines. The parser refuses a bare CR. The `Bidi_Control` characters
(U+061C, U+200E–U+200F, U+202A–U+202E, U+2066–U+2069) are refused everywhere, comments
included; a string escape can denote them. Identifiers are ASCII. Positions are byte
offsets; columns count Unicode scalars, a tab counting one.

```ebnf
letter        = "A".."Z" | "a".."z" | "_" ;
digit         = "0".."9" ;
identifier    = letter, { letter | digit } ;
NL            = LF | CR, LF ;
line-comment  = "//", { non-newline-scalar } ;
```

A `non-newline-scalar` is any scalar other than LF and CR. Tokens are formed by longest
match, except that no float literal begins directly after a `.` token, so `p.0.1` is `p`,
`.`, `0`, `.`, `1`. `///` is a documentation comment and otherwise a comment.

No token spans a newline, so every line tokenizes on its own. `//` is the only comment:
there are no block comments, and `/*` is refused as one invalid token running to the end
of its line. A raw literal ends on the line it opens (LANG-3); a newline inside it is
refused here, and the invalid token ends before that newline.

### LANG-2: Keywords

The reserved keywords are exactly:

```text
fn let mut match if else loop while for in break continue return
take copy view type resource scoped region bound
pub opaque const where true false bytes
```

A keyword cannot be declared or used as a name anywhere, after a `.` included. These names
are contextual and ordinary elsewhere: `drop` after a resource declaration; `plain`, `shareable`, `send`, `cross`, `key`, `ordered`, `integer`, `float`, `packed`,
`streamable`, `codec` and `inert` in a bound list; `via` after a bound list, and `to` and `from` in
its parentheses; `removed` before a field or variant name; `own` before `fn` in a type; `waits` after a function or callable header; `steps` after `bound`, as a parameter mark and as an argument mark; `clone` at the start of an item; `cold` before `fn` at the start of a function item; `grant` after a type's bounds. Library types and functions, including `Region`,
`View`, `Rc`, `Arc` and `Capacity`, are ordinary names. `_` is the discard pattern, and after `const` it names an assertion
([LANG-20](language.md#lang-20-constants)); it is not a name.

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
scalars other than LF and CR; `byte-escape` is `escape` without its `\u{..}` form; the `ascii-`
forms also exclude non-ASCII. In strings and characters `\xNN`
is at most 7F and `\u{..}` has one to six digits denoting a Unicode scalar value; in byte strings
and byte literals `\xNN` is any byte and `\u{..}` is absent. A character literal is exactly
one scalar. A raw literal closes at the first `"` followed by its opening number of
hashes on the line it opens, keeps every source byte, and has no escapes; a newline inside
it is refused under LANG-1. Integer literals have no sign: `-128i8` is negation of a magnitude, and the
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
3. At a complete statement or item a newline ends it, unless the next token after it,
   past comments and blank lines, is a continuation token that continues the expression,
   assignment or variant list being parsed. The continuation tokens are a closed set: `&&` `||` `==`
   `!=` `<` `<=` `>` `>=` `+` `*` `/` `%` `&` `|` `^` `<<` `>>` `..` `..=`, `.`, `?` and `?=>`,
   and, at an assignment position, `=` and every compound assignment operator. A token is
   classified whole, so `!=` and `-=` are continuation tokens though `!` and `-` are not,
   and `?=>` written without spaces is always the one token: a guard ending in `?` is
   written `c? =>`.
   `-`, `!`, `~`, `(`, `[` and `{` never continue a line: each starts the next statement.
   A continued statement is exactly the statement its tokens make on one line, with the
   same precedence and the same refusals, so `a < b\n < c` is refused as a chained
   comparison. The rule is the parser's, not the lexer's:
   - Only an expression, an assignment or a type definition's variant list continues. A
     line that starts with `|` after a variant continues the list, so `type T = A\n | B`
     declares two variants; a `|` before the first variant, as in `type T =\n | A`, is
     still refused. A type, pattern or item header does not continue; a function item
     ends at its closing `}`, while an expression block such as `{ a }\n + b` continues as
     `{ a } + b` does.
   - An assignment position follows a complete expression statement, or a `let` with no
     `=` yet: `x\n += 1` and `let y: u64\n = 2` continue.
   - A bare `return`, `break` or `continue` is complete at a newline and no token continues
     it: `return\n + b` is refused. A valued exit continues its value: `return a\n + b`
     returns `a + b`.
   - An `if`, `while` or `match` subject continues: `if a\n && b {` and `match x\n .kind {`
     are one subject. An unparenthesized subject that ends in a block is still refused,
     however its lines break.
   - `;` always ends a statement, and a match arm's expression continues as a statement's
     does (rule 7).
4. These continue across newlines and comments: `else` after an `if` block; `drop` after a
   resource declaration; `->`, `waits`, `where`, `in` and a body `{` after a
   function header that has no body yet.
5. `;` ends a statement explicitly. `}` and the end of file end a pending complete
   statement, so no END is needed before them. Two statements on one line are always
   separated by `;`.
6. A block's last expression is its value; a trailing `;` discards it. A final `let` or
   assignment gives unit.
7. In list braces a newline can replace a comma after a complete entry. A match arm's
   expression continues under rule 3; a field or variant entry is not an expression and
   does not. Since `-` never continues, a next arm `-1 => ...` is an arm, not a subtraction.

Indentation is never significant. The formatter keeps every statement's tree: its output
parses to the tree of its input. Where it breaks a line inside an expression outside
brackets, it breaks before a continuation token, which starts the next line one level
deeper, and after a `-`, which ends its line.

A trailing comma in a parenthesized or bracketed list puts each entry on its own
line, indented one level from the surrounding block or list. The closing
delimiter returns to the surrounding indentation. This applies to arguments,
parameters, generic parameters, type arguments, array literals, tuples, and
constructor payloads and patterns. Lists without a trailing comma stay inline
where their contents allow it. The formatter keeps the commas as written.

```hirsel
fn total(a: u64, b: u64) -> u64 {
  let sum = a +
    b
  sum
}
```
```hirsel
fn total(a: u64, b: u64, c: u64) -> u64 {
  let sum = a
    + b * c                                               // one statement: a + b * c
  if sum > a
    && sum > b {
    sum
  } else {
    a
  }
}
```
```hirsel
type Shape = Dot
  | Line(u64)
  | Box(u64, u64)                                         // one definition with three variants
```
```hirsel
fn total(a: i64, b: i64) -> i64 {
  let sum = a
  -b                                                      // a second statement: the block's value is -b
}
```
```hirsel
fn first(a: u64) -> u64 {
  return
    + a                                                   // refused: LANG-4, a bare return is complete at its newline
}
```

### LANG-5: Grammar

```ebnf
(* files and items *)
file        = { NL | item, ( END | EOF ) }, EOF ;
item        = { attribute }, [ "pub", [ "opaque" ] ], ( [ "cold" ], function | type-def | const-def )
            | "clone", fn-header, block ;
attribute   = "@", ( "test" | "compact", "(", ( "u16" | "u32" | "u64" ), ")" ) ;
const-def   = "const", ( name | "_" ), ":", type, "=", expression ;
function    = "fn", name, fn-header, block ;
fn-header   = [ generics ], "(", list(parameter), ")", [ "->", type ],
              [ "waits" ], [ outlives ] ;
parameter   = [ "take" | "mut" | "bytes" | "steps" ], name, ":", type ;
generics    = "[", nonempty(generic), "]" ;
generic     = name, [ ":", bounds ] | "in", name ;
bounds      = bound-name, { "+", bound-name } ;
bound-name  = "plain" | "shareable" | "send" | "cross" | "key" | "ordered" | "integer" | "float"
            | "packed" | "streamable" | "codec" | "inert" ;
outlives    = "where", place, ":", place, { ",", place, ":", place } ;
type-def    = [ "resource" | "scoped" ], "type", name, [ generics ],
              [ ":", bounds, [ via ] ], [ "grant", "bytes", integer ],
              ( fields | "=", type-rhs | empty ), [ drop ] ;
fields      = "{", [ field, { entry-sep, field }, [ entry-sep ] ], "}" ;
via         = "via", type, "(", "to", ":", name, ",", "from", ":", name, ")" ;
field       = name, ":", type, [ "=", scalar ] | "removed", name, ":", type ;
type-rhs    = variant, "|", variant, { "|", variant }
            | name, "(", list(payload), ")"
            | type ;
variant     = [ "removed" ], name, [ "(", list(payload), ")" ] ;
payload     = [ name, ":" ], type ;
drop        = "drop", "(", name, ")", block ;

(* types *)
type        = type-atom, [ "in", place ] ;
slot-type   = type-atom, [ anchor ] ;
anchor      = "in", place, { ",", place } ;
place       = name, { ".", name } ;
type-atom   = type-path, [ "[", nonempty(type-arg), "]" ]
            | "(", ")" | "(", type, ")" | "(", type, ",", list(type), ")"
            | [ ( "scoped" | "own" ), [ "[", callable-bound, { "+", callable-bound }, "]" ] ],
              "fn", "(", list(fn-param), ")",
              [ "->", type ], [ "waits" ] ;
type-arg    = type | "in", place | integer ;
callable-bound = "view" | bound-name ;
fn-param    = [ "take" | "mut" | "bytes" | "steps" ], [ name, ":" ], type ;
type-path   = name, { ".", name } ;

(* statements and expressions *)
block       = "{", { statement, END }, [ statement ], "}" ;
statement   = "let", [ "mut" ], pattern, [ ":", slot-type ], [ "=", expression, [ "else", block ] ]
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
prefix      = ( "-" | "!" | "~" | "take" | "mut" | "copy" | "view" ), prefix | postfix ;
postfix     = primary, { "(", list(argument), ")" | "[", expression, "]"
                       | ".", ( name | digits(digit) ) | "?" }, [ "?=>", mapper ] ;
mapper      = type-path | "|", pattern, "|", prefix ;
argument    = [ name, ":" ], expression | ( "bytes" | "steps" ), expression ;
primary     = literal | type-path | "(", ")" | "(", expression, ")"
            | "bytes", "[", type, "]"
            | "(", expression, ":", type, ")"
            | "(", expression, ",", list(expression), ")"
            | "[", list(expression), "]" | "[", expression, ";", integer, "]"
            | [ label ], ( block | loop-expr ) | if-expr | match-expr | closure
            | "region", name, block | "bound", ( "bytes" | "steps" ), expression, block
            | "return", [ expression ] | "break", [ "'", name ], [ expression ]
            | "continue", [ "'", name ] ;
loop-expr   = "loop", block | "while", cond, block
            | "for", [ "take" ], pattern, "in", expression, block ;
if-expr     = "if", cond, block, [ "else", ( block | if-expr ) ] ;
match-expr  = "match", expression, "{", [ arm, { entry-sep, arm }, [ entry-sep ] ], "}" ;
arm         = pattern, [ "if", expression ], "=>", expression ;
closure     = [ "scoped" ], "fn", "(", list(closure-param), ")", [ "->", type ], [ "waits" ],
              block ;
closure-param = [ "take" | "mut" | "bytes" ], name, [ ":", type ] ;

(* patterns *)
pattern     = "_" | [ "take" | "mut" ], name | scalar, [ "..=", scalar ]
            | "(", ")" | "(", pattern, ",", list(pattern), ")"
            | type-path, "(", { pattern-field, "," }, [ pattern-field | "..", [ "," ] ], ")"
            | name, ".", type-path ;
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
| call, index, `.`, `?`, then `?=>` and its mapper | left, postfix; `?=>` ends the chain |
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
`:` and `->`, an **ascription** `(e : T)` included, inside `[ ]` after a type path or after the keyword `bytes` ([MEM-33](budgets.md#mem-33-reading-a-logical-charge); as an argument, `bytes [` always begins that form, so a marked argument that starts with an array literal, such as `[a, b][i]`, is parenthesized) and on the right of a type definition; expressions never carry type
arguments, so postfix `[ ]` always indexes; a `?=>` ends its postfix chain, so a call, index, field
or `?` after its mapper is refused, at that token, and is written `(e ?=> m).f`; constructors, variants and calls share call
syntax and are told apart by name resolution; braces after a condition begin a block,
because records are built with parentheses. On the right of a type definition, two or more
`|`-separated variants or one name followed by `(` declare variants; anything else is an
alias. A `let` without `=` declares an uninitialized slot and must have a name pattern and
a type ([OWN-5](ownership.md#own-5-availability)); `let x` alone is refused here, at
`let`. A `let else` initializer and an `if`, `while`
or `match` subject cannot end in an unparenthesized block. `for` patterns, `let` patterns
without `else`, and `let`-condition patterns are checked for refutability after parsing
([LANG-17](language.md#lang-17-match)). An attribute applies only to its own target:
`@test` to a function, `@compact` to a `type` that is neither `resource` nor `scoped`; a repeated
attribute is refused by [LANG-7](language.md#lang-7-names), and an attribute on another item, a representation keyword on an alias,
and `drop` on a declaration that is not `resource` are refused. A parameter marked `bytes`
is a byte count of type `u64`, and its argument is written `bytes e`; the mark is part of the
signature, as `take` is, and an argument whose mark does not match its parameter is
refused by [OWN-7](ownership.md#own-7-parameter-modes), at the argument. A parameter marked
`steps` is a `u64` as well, and its argument is written `steps e`; `steps` is a mark only before
a name in a parameter and before a name, an integer, `if`, `match`, `true` or `false` in an
argument. An item that begins `clone` is a clone body ([MEM-17](memory.md#mem-17-clone)). An
expression has no postfix `in`: `e in r` is refused by [MEM-2](memory.md#mem-2-region-data). In a file of the package `core` ([core](core.md)) a function may end at
its header, with no body, and a type with no body is built in. A function of a foreign
module ([FFI-2](foreign.md#ffi-2-foreign-modules)) ends at its header too. In any other place a
function without a body is refused here, at the end of its header. Malformed syntax with no
more specific rule is LANG-5.

```hirsel
fn f(xs: List[u64]) -> u64 { if list.len(xs) > 0 { xs[0] } else { 0 } }
```
```hirsel
cold fn grow(x: u64) -> u64 { x * 2 }                     // a cold function (LANG-8)
```
```hirsel
cold type T { x: u64 }                                    // refused: LANG-5, cold stands only before fn
```
```hirsel
fn f(a: u64, b: u64, c: u64) -> bool { a < b < c }        // refused: LANG-5, comparisons do not chain
```
```hirsel
fn width() -> u64                                         // refused: LANG-5, a function outside core needs a body
```
```hirsel
fn spend(steps k: u64) -> u64 { k }
fn ten() -> u64 { spend(steps 10) }
```
```hirsel
type Source = Text(String) | Count(u64)
fn bump(mut src: Source) { match src { Count(mut n) => { n += 1 }, _ => {} } }
```
```hirsel
fn run(f: scoped[view] fn() -> u64) -> u64 { f() }
```
```hirsel core
type Bag[T] { items: List[T] }
clone[T](b: Bag[T]) { Bag(items: clone(b.items)) }
```
```hirsel
type Own { n: u64 }
fn placed(r: Region) { let _ = Own(n: 1) in r }           // refused: MEM-2, no postfix in
```
