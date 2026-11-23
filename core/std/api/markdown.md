# Markdown

[API overview](../API.md). [Operations and contracts](../markdown.hirsel).

`std.markdown.parse` reads CommonMark 0.31.2. Pass `CommonMark` for the
standard grammar or `Wiki` to also recognize `[[target]]` and
`[[target|label]]` in inline content. The result is a flat tree of `Node`
records in document order. Node zero is the document. Follow `first` to
its first child, then `next` to each sibling. A zero child or sibling ends
that chain.

Each node's `span` is a half-open byte range in the original UTF-8 source.
Ranges include the node's Markdown delimiters. A range that covers several
lines can also include the intervening container markers and line endings.
A reference link spans its use. Its definition can be elsewhere in the
document, so parsing the use by itself may produce text.

A heading's `section` starts at the heading and ends at the next heading of
the same or a higher rank, or at the end of the source. For a wikilink,
`target` and `label` cover only the bytes between the delimiters. Without a
pipe the displayed label equals the target and the label span is `None`.
The target must be nonempty. A wikilink stays on one line, and its target and
label are literal strings.

The caller sets `ParseBudget.source_bytes`, `work_items` and `nesting`.
Nesting counts the document at depth one. The parser caps it at 128. The result is
`Error(SourceBytes)`, `Error(WorkItems)` or `Error(Nesting)` if parsing
exceeds a budget. Allocations also follow the caller's memory limit.

Raw HTML blocks and inline tags are retained as source text in the AST.
Consumers that render outside a trusted context must handle that text and
link destinations according to their output format.
