# Diagnostics

A program that breaks a rule is refused before it runs. A refusal tells the author which
rule and where.

### PROG-7: One rule, one site

Each refusal names exactly one rule ID of this specification and one **primary site**: the
source span of the construct that the rule's statement is about (the moved use for
[OWN-5](ownership.md#own-5-availability), the conflicting loan for
[OWN-10](ownership.md#own-10-freezes), the missing arm's `match` for
[LANG-17](language.md#lang-17-match)). It may add secondary sites, such as the move or the
freeze that a use conflicts with. The rule named is the one whose statement the program
breaks; when a construct breaks a specific rule and the general rule it restates, the
specific rule is named. A refusal never names a rule the program does not break.

### PROG-8: No global order

An implementation reports at least one refusal for every refused program and may report
several, in any order. No first diagnostic is fixed across independent refusals, and a
refusal is never suppressed or reworded by another one, except that checks depending on a
construct that failed to parse, resolve or type are not made.
