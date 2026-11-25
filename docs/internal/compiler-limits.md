# Compiler resource limits (internal)

Implementer notes for resource refusals ([PROG-6](../spec/programs.md)) and parser stack budgets.

## Core parser nesting

`hirsel_core_syntax::PARSER_NESTING_LIMIT` is **128 simultaneously active recursive
productions**. It is an implementation resource ceiling, not a restriction on Hirsel
programs. Exhaustion returns `ResourceRefusal::ParserStack` with that limit and the
file-relative byte span of the token where the next production would start. The CLI
and driver report it as a resource refusal (below), never a source-language refusal or
a successful parse. The formatter does not print an incomplete tree.

## Resource refusals

A resource refusal ([PROG-6](../spec/programs.md)) is the implementation declining a
program that passes one of its limits. It names no rule and is not a meaning of the
program, so the driver keeps it apart from both rule refusals and incomplete (unbuilt)
features: `CheckProduct.resources` and `Reachable.resources`, each a
`ResourceRefusal { primary, message }` with no rule field. Four sources feed it, each
with an exact primary site and a message carrying the measured amount and the limit:

| Source | Limit | Primary site |
| --- | --- | --- |
| parser | `PARSER_NESTING_LIMIT` productions; u32-indexed source bytes | the token where the next production starts; the file start |
| resolver | `RESOLVE_NESTING_LIMIT` expression depth | the expression past the limit |
| front layout | the engine's 64-bit native size range | the field whose placement passes it |
| instance collection | `NativeOptions.instance_limit` (65 536 in the CLI) | the definition of the instance past the limit |

`check --json` and `build --json` carry them in a `resources` array of
`{primary, message}`; stderr prints `resource <module> <start>..<end>: <message>`.
The command exits **2**, as for an incomplete outcome; a rule refusal exits 1.
"0 gaps" in census reports counts only unbuilt features: the instance limit is not a
gap stop.

Reuse: the parser, resolver and layout limits are compiler constants (layout recipes
are identical on every supported target), so a stored product, keyed by compiler
identity, is never served across a change to them. The instance limit belongs to the
build: instance collection runs per build from the stored product and its refusal is
never stored, so the same stored product is refused under a small limit and accepted
under a large one.

## Parser stack-probe measurements

One shared guard runs before recursive expressions, compound expressions, types,
patterns and blocks. Compound expressions include labels and `else if`; type recursion
includes generic arguments and callable parameter/result types. Several guarded
productions may be active for one pair of source delimiters. Attributes, qualified
paths, lists of declarations/parameters and postfix chains use loops; they have no
recursive nesting charge. The limit applies to mixed constructs through one counter.

On x86-64 with Rust 1.99, stack-address probes across those paths measured at most
9,120 bytes between guarded entries in an unoptimized build, 1,488 in the checked
build and 1,977 in release. The unoptimized main-thread and default Rust-worker
measurements agree. A conservative allowance of 12 KiB per entry gives 1.5 MiB at
128 entries, leaving 512 KiB for caller and leaf frames on the smallest supported
stack, a 2 MiB default Rust worker (also used by unit tests). The measured Linux main
thread has an 8 MiB stack, and the CLI parses on it. No stack size is enlarged
to accommodate parsing. The boundary and 100,000-depth cases cover each recursive
path, and long iterative paths remain accepted. Recheck the stack allowance when
changing parser frames, compiler profiles or supported platforms.
