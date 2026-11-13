# hirsel-syntax

Own lossless lexical data, indexed ASTs, item paths and text hashes.

Allowed dependencies: hirsel-span, hirsel-intern, sha2; no phase implementation crates.

Invariants and boundary:

Canonical DefPath segments are nonempty, contain no separators, and use the
manifest's canonical package/version spellings. The resolver rejects duplicate
paths rather than appending positional ordinals. Moving an item between files
under the same manifest module leaves identity unchanged.

Revision 1 adds Item.name_span (exact declared name), FieldSyntax.growable and
DropSyntax.noalloc. Bound::NoallocDrop is contextual; no token/keyword was added.
Type declaration bounds are Option<Vec<Bound>>: None is omission, Some([]) is the
explicit `: ()` promise. Field growability is never a type qualifier. SyntaxFile
owns has_syntax_errors, which includes recovery
errors lacking AST Error nodes. Formatters must honor it. SourceMap ownership
still belongs to the driver. The parser follow-up implements the four new forms;
this revision adds transport without changing the conformance pending list.

Additive declaration-origin helpers recover exact parameter names/labels,
generic binder names, outer uses paths and variant names from the token tape.
Call them with the containing node's Origin and its own parameter/type IDs.
Nested function-type clauses and payload names stay in their own scopes; closure
trailing uses is part of that closure. No AST field or existing signature changed.
Absent tape/anonymous parameters return no precise site, preserving manual fixture
origins as the consumer fallback.

Revision 7 adds contextual Bound::Serializable. The existing Attribute row retains @serializable without a new keyword; represented opaque ordinary/resource capabilities reuse TypeBody and explicit bounds/authority/drop rows. Syntax conveys written facts, never schema or seal proof.

F-strings (SEM-3) add `TokenKind::Format(FormatPiece)`, `ExprKind::Formatter(FormatStep)`,
which only ever appears as the callee of a desugared step, and
`SyntaxTree.formats`: each written f-string's piece spellings and hole IDs, keyed by
the block the parser desugared it to.

Conditional lets (LANG-13) have no node either: `SyntaxTree.conditions` records each
written `if let` by its desugared match and each `while let` by its `while true` body
block, for the formatter and H's refutability judgment. `SyntaxTree.groups` keeps every
parenthesized expression with its span; `ends_in_block` reads it to accept
`let P = (if ..) else { .. }`.

`FormatPiece::Spec` retains the raw colon/options token. `FormatStep::AppendSpec`
carries parsed options; `FormatStep::Mark` and `FormatStep::Pad(hole)` pad a hole
whose options are only fill, alignment and width; `FormatString.specs` retains each
written spec. `AttributeArgument::Format` carries an `@format(function)` path.

Loop statements carry optional label Symbols; Break/Continue expressions carry optional target labels. StmtKind::loop_label exposes the written declaration without treating it as a value binding. A `Block` with a label is a labelled block expression (LANG-11), and `Break.value` is the operand of `break 'l value`, which only a labelled block accepts.

`SyntaxTree.repeats` retains each repeat literal's written element/count and
its desugared block, sequence and comptime-count IDs. All execution edges remain
ordinary AST nodes; this row preserves surface formatting and multiplicity.
