# Callable fitting at a generic argument

LANG-13 (language.md314-317,330-332) admits a plain callable in either scoped
kind with the same signature. The minimal program is `let n = invoke(seven); n` where
`invoke[R]` accepts a scoped[view] fn()->R and `seven` returns7. There are no
The let has no written type, so R is fixed by the function argument. There are no captures, allocations, store operations or capabilities in that call. Its answer
is7 and it retains0 admitted bytes. `mut` and `take` parameters are not captures
(LANG-13,323-325).
