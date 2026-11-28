---
name: hirsel
description: Write, check, test and fix Hirsel programs with the installed hirsel binary. Use when writing or editing .hirsel files or a hirsel.hcfg package, or when hirsel check refuses code.
---

# Hirsel

**Authority rule:** Always run `hirsel explain primer` from your installed binary as the exact
authority for your installed version (`references/PRIMER.md` is the repository copy).
For task-shaped recipes, see [hirsel.ai/cookbook.md](https://hirsel.ai/cookbook.md) or
the single-file agent bundle at [hirsel.ai/llms-full.txt](https://hirsel.ai/llms-full.txt).

Hirsel has no imports, methods, traits or turbofish. Call core and std modules by path
(`list.push(mut xs, 1)`, `std.json.parse(...)`), write every top-level signature in full,
and repeat each `mut` and `take` mode at the call site.

## The loop

1. Write the code. Start a package with `hirsel new app` (`--lib` for a library).
2. Run `hirsel check` in the package directory. Each refusal starts with one line naming its
   rule code and primary site:

   ```text
   ./main.hirsel:2:32-34: <RULE> a moving place is passed as `take xs`
   ```

3. Read the rule code and the span first, then the message. For automation,
   `hirsel check --json` returns structured rule codes, byte spans, and causal sites (and on
   `main` / newer toolchains, `line`/`column`/`line_text` plus machine-applicable `fixes`;
   when `fixes` are present, apply a fix's edits together at their byte spans, review
   `maybe` fixes, and fill in bodies supplied by `placeholder` fixes).
4. When the compiler refuses code it prints a short rule code (e.g. `OWN-7`); run
   `hirsel explain OWN-7` (or with `--json`) for the rule's exact statement and a refused
   example, or `hirsel explain --list` to list all rules.
   Fix to the rule, not to the message's wording.
5. Repeat from step 2 until it prints `accepted`.
6. Run `hirsel test`. It runs every `@test` in its own process under the `[tests]`
   limit in `hirsel.hcfg`, and prints one row per test (use `--filter TEXT` to select
   tests, or `--sim --seeds N` for deterministic simulation runs).
7. Run `hirsel run --grant Stdout` (one `--grant KIND` or `--dir PATH` per
   capability parameter of `main`). A missing grant exits 103 before the program starts.

## Finding an operation

Never guess a function name. Inspect your installed toolchain directly:

- `hirsel inspect --core --interface` — every public core declaration and signature
- `hirsel inspect --std --interface` — every public std declaration and signature
- `hirsel env --json` — active compiler, root, target, and runtime info

Read every parameter mode in a signature: unmarked is a read-only view, `mut` is lent,
and `take` is moved.

## Making it fast

Measure before tuning, and never tune from `objdump` output:

```sh
# Choose a logical root limit in bytes for your workload. This example uses 64 MiB.
hirsel prof run . --release --limit 67108864 --grant Stdout --out run1.hprof -- ARGS
hirsel prof top run1.hprof                       # hottest functions and source lines
hirsel prof annotate run1.hprof main.hirsel:42   # one line's samples and compiler remarks
```

Order by two keys with one tuple comparison, `(k1, v1) < (k2, v2)`, not an `if` on the
first key: tuples compare lexicographically.

## Habits that avoid refusals

- Give every `let` of a literal or a generic call an explicit type: `let n: u64 = 0`,
  `let mut xs: List[u64] = list.new()`.
- Never reuse a name: no shadowing, and no local named like a function.
- Bind or `?` every `Result`; discard one only with `let _ = e`.
- Pass capabilities (`Stdout`, `Dir`, `Clock`, `Net`, `Tasks`, `Env`, `Args`, `Random`)
  as explicit parameters; mark a function other than `main` `waits` exactly when it calls
  a `waits` function.
- Always trust `hirsel explain primer`, `hirsel explain`, and `hirsel inspect` from your installed
  binary over any web copy.
