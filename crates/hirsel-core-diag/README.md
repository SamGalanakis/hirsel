# Current-spec diagnostics

`rules()` returns the ID-sorted static catalogue. `rule(id)` returns an exact match
or `None`. Each serializable `Rule` has `id`, `title`, `spec_file`, `anchor` and
`statement`. The statement is the first prose paragraph of the rule, with Markdown
retained and source line wrapping removed. It is an excerpt, not separately authored
repair advice. `spec_file#anchor` identifies the complete rule.

The build reads the rule headings and prose directly from every Markdown file in
`docs/spec`. Generated Rust stays in `OUT_DIR`; no catalogue copy is tracked. Cargo
and Buck declare the spec inputs. The land gate runs this crate's tests: compare the
embedded catalogue with the current spec, and walk production Rust modules (including
macro literals, excluding test modules) in syntax, resolve and front to ensure each
literal rule ID exists. Unknown or formatted rule IDs are not explanations.

The design adapts rustc's embedded Markdown explanations: immutable compiled data,
but IDs/titles/anchors also derive from the spec rather than a hand-maintained registry.
Goose's structured diagnostic/related-location design remains separate from rendering;
Hirsel adds the rule's declared/required pair and scalar columns to its byte sites.
No old guidance or related-rule table is read.
