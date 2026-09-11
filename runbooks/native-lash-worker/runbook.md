# Native Lash coding worker product runbook

Follow [`../RULES.md`](../RULES.md). This runbook is a bounded real-provider product check, not a unit-test substitute.

## Preconditions

- Use a fresh data directory and unused loopback port other than `3076`.
- Build and serve the production frontend and Host from the same reviewed checkout.
- Configure a private `openrouter` provider entry with a usable API key. Do not copy or print the key into evidence.
- Confirm `hello_ok` reports the intended Host configuration. The accepted worker execution must record provider `openrouter`, model `deepseek/deepseek-v4.1-flash`, variant `default`, and the isolated test checkout cwd.
- Prepare a tiny repository with one focused failing test and no valuable state.

## Bounded scenario

Spend at most two worker model turns: one initial delegation and one follow-up. Do not automatically retry a failed or timed-out model call.

1. Ask the coordinator to delegate one child Task with `agent: "lash"`. The brief tells the worker to inspect the fixture, run the focused failing test, make the smallest repair, rerun it, and summarize changed files plus checks.
2. While the child is running, verify the parent UI remains responsive without sending another prompt.
3. In the child timeline, verify the callable catalog contains exactly `read`, `edit`, `write`, and `exec_command`; no coordinator, delegation, browser, process-control, or plugin tools appear.
4. Verify chronological reasoning, tool start, tool result, and assistant output rows. The first test must visibly fail and the later focused test must pass. Reconcile DOM, `open_thread`, captured frames, and SQLite IDs.
5. Send one follow-up to the same child asking it to identify the earlier changed file and test result without rereading the whole repository. Verify the same Task retains context and produces exactly one new terminal report to the parent.
6. Confirm neither successful turn marks the Task done.

## Deterministic failure probes

Run these without extra model calls where possible:

- Queue a native turn, then retarget or remove its accepted provider route before admission. It must fail clearly without a provider request or fallback.
- Cancel a queued turn and a running long `exec_command`; verify terminal state, process-group cleanup, and no duplicate parent report.
- Stop the isolated Host during an admitted shell mutation and restart it. The old running turn must become interrupted and the shell effect must not be blindly replayed or reported successful.
- Reset isolated history during an owned command. Verify tool shutdown and rejection of the old execution binding.

## Evidence and teardown

Retain sanitized screenshots, DOM extracts, frames, `open_thread` snapshots, relevant SQLite rows, exact commit/model/provider identifiers, command exit statuses, and the isolated Host log. Record `OBJECTIVE_PASS` only when deterministic predicates pass; the reviewing Agent separately judges the product scorecard. Stop only the process group started for this run.
