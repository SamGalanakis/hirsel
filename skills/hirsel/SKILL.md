---
name: hirsel
description: Write, check, test and fix Hirsel programs with the installed hirsel binary. Use when writing or editing .hirsel files or a hirsel.toml package, or when a hirsel check refusal cites a rule ID such as OWN-7.
---

# Hirsel

Read `references/PRIMER.md` before writing Hirsel; it is the language in one page. Hirsel has
no imports, methods, traits or turbofish. Call core modules by path (`list.push(mut xs, 1)`),
write every top-level signature in full, and repeat each `mut` and `take` mode at the call.

## The loop

1. Write the code. Start a package with `hirsel new app` (`--lib` for a library).
2. Run `hirsel check` in the package directory. Each refusal is one line naming one
   rule and one site:

   ```text
   ./main.hirsel:2:32-34: OWN-7 a moving place is passed as `take xs`
   ```

3. Read the rule ID and the span first, then the message. Fix only that site.
4. Run `hirsel explain OWN-7` for the rule's exact statement and a refused example.
   Fix to the rule, not to the message's wording.
5. Repeat from step 2 until it prints `accepted`.
6. Run `hirsel test`. It runs every `@test` in its own process under the `[tests]`
   limit in `hirsel.toml`, and prints one row per test.
7. Run `hirsel run --grant Stdout` (one `--grant KIND` or `--dir PATH` per
   capability parameter of `main`). A missing grant exits 103 before the program starts.

## Finding an operation

Never guess a function name. `hirsel inspect --std --interface` lists every public std
declaration with its full signature. Read the signature's modes: an unmarked parameter is a view, `mut` is lent and `take` is moved.

## Habits that avoid refusals

- Give every `let` of a literal or a generic call a type: `let n: u64 = 0`.
- Never reuse a name: no shadowing, and no local named like a function.
- Bind or `?` every `Result`; discard one only with `let _ = e`.
- Pass capabilities (`Stdout`, `Dir`, `Clock`) as parameters; mark a function other
  than `main` `waits` exactly when it calls a `waits` function.
- `hirsel primer` prints the same primer as `references/PRIMER.md`.
