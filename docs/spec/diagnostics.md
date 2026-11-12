# Diagnostics

### PROG-7: One rule, one site

Each refusal, other than a resource refusal ([PROG-6](programs.md#prog-6-one-meaning)),
names exactly one rule ID of this specification and one **primary site**: the
source span of the construct that the rule's statement is about (the moved use for
[OWN-5](ownership.md#own-5-availability), the conflicting loan for
[OWN-10](ownership.md#own-10-freezes), the missing arm's `match` for
[LANG-17](language.md#lang-17-match)). It may add secondary sites, such as the move or the
freeze that a use conflicts with. The rule named is the one whose statement the program
breaks; when a construct breaks a specific rule and the general rule it restates, the
specific rule is named: a maybe slot's use names OWN-16, not OWN-5; overlapping
arguments of one call name OWN-11, not OWN-9. A refusal never names a rule the program does not break.

A machine-readable refusal contains the rule ID; the primary site; each **causal site**,
the earlier construct that makes the primary site wrong (the move, the loan, the
declaration); and the facts compared, as the declared fact and the required one (declared
bounds `shareable` against required `send`; parameter mode view against required `take`).
Sites are a module path with a byte span; a source that is not valid UTF-8 is reported at
the span of its first invalid bytes. The encoding is a tool format outside the
language; the rule explanations shown with refusals are generated from this specification.

### PROG-8: No global order

An implementation reports at least one refusal for every refused program and may report
several, in any order. No first diagnostic is fixed across independent refusals, and a
refusal is never suppressed or reworded by another one, except that checks depending on a
construct that failed to parse, resolve or type are not made.
