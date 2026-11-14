# Core syntax

Create one `Symbols` table for the compilation package and its dependency sources, then
call `parse(text, mode, &mut symbols)` or `parse_bytes(bytes, mode, &mut symbols)`.
`Source` requires function bodies; `Signatures` permits bodiless declarations. Lexing
interns identifiers once into this table. Grammar terminals have prefilled symbol IDs;
parsing dispatches on integers. Symbols from unrelated tables must not be mixed.

A `SyntaxFile` owns one source string and one arena of tokens, nodes and child edges.
`items` contains ranges into those arrays; `item(n)` and `items()` yield Copy borrowed
`Item` views. Token and node IDs address the file. Node byte spans remain relative to
the containing item, while token byte spans and token/child indices address the file.
`Item::token_span` converts a token's span to an item-relative anchor. `SyntaxFile::text`
reconstructs the original bytes from token spans and source gaps, including invalid and recovered source. Whitespace has no token rows; comments stay indexed. Lexical tags are one byte and fixed terminals share their symbol numbers.
A compiler `ResourceRefusal` is separate from language diagnostics; incomplete syntax
must not be published as a successful parse.

`PARSER_NESTING_LIMIT` is 128 simultaneously active recursive productions, shared by
expressions, compound expressions, types, patterns and blocks. It is an implementation
resource ceiling, not a language limit. `ParserStack { at, limit }` identifies the next
token and the exhausted budget; its Display message includes both. Generic arguments
and callable headers recurse through types; attributes, paths and postfix chains are
iterative. The stack measurements and supported thread stacks are documented under
[core parser nesting](../../docs/building.md#core-parser-nesting).

The scanner, trivia/literal decoding, recovery, precedence climbing and relative source
anchors adapt the old front end. Grammar, nodes and token vocabulary belong to the new
language. Goose, Zig and rustc informed the flat arenas and symbols; the lane's
PRIOR-ART.md records choices and tradeoffs. There are no old semantic dependencies.

`format(&ParseOutput)` formats a successful complete parse, retaining every literal,
comment and semicolon. Blocks, fields, match arms and statements use four-space structural
layout; declarations have one blank line between them. Other lists stay inline. Tests
check token preservation, item-relative anchors, AST shape, recovery and idempotence.
No reparse or validation of our own output runs in the product.

`hirsel-core-syntax FILE [--signatures]` writes canonical text to stdout. Invalid source
prints located rule diagnostics and exits unsuccessfully without formatted output.

`format_package(root, mode, FormatAction::{Check, Write})` discovers all `.hirsel`
files recursively; symlink directories are not followed. `format_files` takes explicit
`(PathBuf, ParseMode)` inputs. Both return `FormatReport { files, differing, refused }`
in sorted path order. A `FormatRefusal` contains its path, syntax diagnostics and
optional resource refusal. Check never writes. Write prepares every input first and
writes nothing when any source refuses or a read fails. A write-time I/O failure is
returned as an error and may leave earlier files formatted.

The internal binary accepts `--check FILE...` or `--write FILE...`, optionally with
`--signatures`. Check prints differing paths and fails on differences or refusals;
write fails on refusals. The driver's user command calls the library directly.
The gate checks changed sources under core/core, core/std, core/front/cases,
core/programs/m1 and core/langarena. Explicit `// expect: refuse` or `// expect: gap`
fixtures are excluded, since their original coordinates are the diagnostic oracle.
Parser-input fixtures are not case/program sources and are not gate targets. Accepted
cases have no primary-site refusal oracle and receive canonical formatting normally.

Package preparation adapts Go's gofmt file-list reporting and Rust's cargo fmt check
mode. Unlike gofmt's independent writes, a parse refusal here prevents all writes in
one operation. Unlike cargo fmt's external process coordination, the driver uses one
library call. No second parse or checker runs in the formatter.
