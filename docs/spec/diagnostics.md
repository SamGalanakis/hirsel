# Diagnostics

### PROG-7: One rule, one site

Each refusal, other than a resource refusal ([PROG-6](programs.md#prog-6-one-meaning)),
names exactly one rule ID of this specification and one **primary site**: the
source span of the construct that the rule's statement is about (the moved use for
[OWN-5](ownership.md#own-5-availability), the conflicting loan for
[OWN-10](ownership.md#own-10-freezes), the missing arm's `match` for
[LANG-17](language.md#lang-17-match)). It may add secondary sites, such as the move or the
freeze that a use conflicts with. An unclosed delimiter refusal points back to its
opening token. The rule named is the one whose statement the program
breaks; when a construct breaks a specific rule and the general rule it restates, the
specific rule is named: a maybe slot's use names OWN-16, not OWN-5; overlapping
arguments of one call name OWN-11, not OWN-9. A refusal never names a rule the program does not break.

Each causal site carries a role that identifies the earlier construct. The
primary site marks the operation that conflicts with it. Ownership refusals name
the full place, including its field or index path.

**Machine format.** Each refusal in the CLI's `--json` output has `rule`,
`message`, `primary`, `causal`, `declared`, `required` and `fixes`. The
declared and required facts are strings or null. Each is the fact compared at
the primary site, such as declared `view` and required `take`.

A site has `module`, `span: { start, end }`, `line`, `column` and `line_text`.
The module is the manifest-relative source path when available and the module
path otherwise. Spans are half-open and count bytes from the start of the
source. Invalid UTF-8 is reported at the span of its first invalid bytes.
Line and column use the one-based terminal convention below. The line text
excludes its line ending. The line, column and line text are null for a module
site without source. Incomplete and resource diagnostics use these same sites.

Each element of `causal` is `{ site, role }`. The role is one of
`declared_here`, `conflicts_here`, `moved_here`, `viewed_here`,
`lent_mut_here`, `reserved_here`, `opened_here` or `derived_here`.

Each element of `fixes` is `{ applicability, edits }`. Each edit is
`{ module, span, text }` and replaces that byte span with the text. An empty
span inserts text and an empty string deletes the span. Apply all edits within
one fix to the original input. Each fix is a separate suggestion. Applicability
is `safe` for a correction known from the diagnosed fact, `maybe` for a
candidate that needs review, and `placeholder` when the edit supplies code the
author needs to complete. The encoding and suggestions are tool output outside
the language. Rule explanations come from this specification.

An exhaustiveness refusal may also carry `missing_patterns`, an array of
uncovered pattern witnesses. It can accompany a placeholder arm.

**Terminal sites.** Without `--json`, the CLI prints each site of a file the request read
as `path:line:column`, adding `-column` or `-line:column` for a span end that differs. The
path is the file as the request named it. Lines and columns start at 1. A line ends at LF,
and a CR immediately before that LF is part of the line end, not a column. Columns count
Unicode scalar values, so a multi-byte character is one column; an invalid UTF-8 byte that
does not continue a sequence also counts as one. A site in a file the request did not read,
such as a library image's source, keeps its module path and byte span (`module start..end`).
The primary site leads the line, followed by the rule ID and message; each causal site
follows on its own line with the role and location.

For an unresolved qualified name, a LANG-7 edit can insert the `std.` prefix
when the requested public declaration exists there. The library selection lists
extensions with `extends_core: true` in `hirsel inspect --std --interface --json`.

### PROG-8: No global order

An implementation reports at least one refusal for every refused program and may report
several, in any order. No first diagnostic is fixed across independent refusals, and a
refusal is never suppressed or reworded by another one, except that checks depending on a
construct that failed to parse, resolve or type are not made.
