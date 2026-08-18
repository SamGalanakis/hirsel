# Push Discipline Runbook

## Purpose

Prove the judgment-only push invariant and token lifecycle end to end. Registering the same token is
an idempotent refresh; one open `requires_response` judgment pushes exactly once; ordinary Chat,
summary awareness, read, and resolve transitions add no pushes; unregister is itself idempotent and
prevents later judgments from delivering.

Fully mechanical: `HIRSEL_AGENT=scripted`, `HIRSEL_DRIVER=fake`, disposable state under
`/tmp/hirsel-e2e-push-discipline`, and a port selected from 3320. Judgments come only from the
scripted delegation producer.

## Execute

From a neutral working directory:

```bash
/workspace/code/hirsel-rbcov/e2e/protocol-compatibility/push-discipline/run.sh
```

## Gate 1: registration refreshes one logical row

Register `e2e-push-discipline-token` as Android, then register the same token as Web. The second
response must preserve `created_ts`, update `platform`, and carry a non-decreasing `last_seen_ts`.
The next gate's recorded recipient list must contain that token exactly once, proving the upsert did
not leave two live rows.

## Gate 2: a judgment pushes exactly once

The scripted delegation path creates one open judgment with `requires_response:true`. Poll
`/debug/pushes` until exactly one row names its Event id, then require that the row's `tokens` is
exactly the one-element registered-token array.

## Gate 3: non-interrupting activity is quiet

Drive each path without resetting the push recorder:

- normal Owner Chat and its scripted reply;
- a test-actor-triggered digest summary;
- `read-ping` on the judgment;
- `resolve-ping` on the judgment.

The total recorded-push count must remain one for a polling window, and the summary Event id must
never appear in a payload. The runbook fabricates only the one digest invocation under test; it does
not register or assert any standing schedule.

## Gate 4: unregister is idempotent and stops delivery

The first `/debug/unregister-push-token` response must return `removed:true`; the second must return
`removed:false`. Create another judgment through scripted delegation and prove, across a polling
window, that no recorded push names it.

## Success Gates

- Gate 1: one token refreshes in place with stable creation time and refreshed metadata.
- Gate 2: one `requires_response` judgment → exactly one push to exactly one token.
- Gate 3: Chat, summary awareness, read, and resolve → zero additional pushes.
- Gate 4: unregister is idempotent and stops later judgment delivery.

## Report

Record both registration responses, both judgment ids, the summary id, each unregister response,
and the final `/debug/pushes` array. The run is void if the tester inserts an Event or edits push
tables directly.
