# Core syntax

`parse(text, ParseMode::Source)` parses complete source. `Signatures` additionally permits
function headers without bodies, for the seed library. Both retain every input byte,
including invalid tokens and recovered items. `ResourceRefusal` is an incomplete compiler
outcome, separate from source diagnostics. Callers must check it before publishing syntax.

Each item owns its text, token tape, node arena and child-index pool. `NodeId` and
`TokenId` address that item alone. Origins contain byte spans and half-open token ranges
relative to its owned text; `file_offset` rebinds them to the current file. Nodes are
append-only and nonrecursive to drop. `Tree::children` preserves written order.

The scanner, trivia, literal and decoding algorithms are extracted from hirsel-lexer
and hirsel-syntax. Cursor, brace recovery, precedence climbing and per-item origins adapt
hirsel-parser. Tokens and grammar nodes are new. `grammar.rs` holds operator precedence
and terminal sets. Grammar visitors live in parser.rs. Zig's flat node/edge tables and
Rowan's lossless token coverage inform the representation, with existing Hirsel item
arenas retained. Detailed source references are in the lane's PRIOR-ART.md.

Tests use source rules and hand-derived precedence, diagnostic, origin and token
expectations. They catch dropped trivia, item relocation, newline continuation, chained
comparisons, old or-pattern syntax, hidden subsequent items and literal decoding errors.
