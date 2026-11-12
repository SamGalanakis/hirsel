# Core front end

The new language's single check-and-lower traversal. `Program::declare` resolves every
item, then publishes one summary per type declaration (OWN-2: kind, declared bounds,
holds set, layout recipe) in dependency order and checks every signature once. It reads
no body. Inline containment cycles are refused once, at the continuing field of the
member with the least identity; alias chains that reach themselves are refused the same
way (LANG-9). Declared bounds are read, never re-derived (OWN-4).

Constructs this crate does not build yet are recorded as `Gap`s, never accepted silently
and never reported as refusals of the program.

Cases live in `core/front/cases` with hand-derived `// expect:` lines; `core/front/core`
is a fixture of the core library until the spec's `core/core/` signature files reach the
tools. See `core/front/README.md`.
