# Replay a service

Every effect a Hirsel run performs is recorded, so a whole system of runs can be
replayed from its tape. This guide walks through
[examples/ledger-service](../examples/ledger-service/main.hir). It is a small ledger whose
store lives at one place, a `db@1` place whose head holds the store open and serves
transaction entries, and whose client calls those entries through a runner.

The rules are in [runners](spec/runners.md) (`db@1`) and [storage](spec/storage.md)
(STORE-12).

## The service

`ledger.hir` holds the ledger's rules: `open`, `transfer` and `balance` are functions over
a `Tx` and a typed table `balances: Table[u64, u64]`. They hold only a `Tx` and data, so a
store can run them again.

`main.hir` wraps them as entries the place serves. A transaction entry takes the `Tx`,
the call's `CallInfo` and its argument:

```hirsel
fn moves(mut tx: store.Tx, info: store.CallInfo, req: Transfer) -> Result[
  ledger.Receipt,
  ledger.Refusal
] {
  ledger.transfer(mut tx, req.from, req.to, req.cents)
}
```

The place's head opens the store once, over the place's `ledger` authority, and serves
the entries until its host stops:

```hirsel
pub fn head(rt: Io, a: ()) -> Result[(), store.StoreError] {
  let Ok(ledger) = io.authority(rt, "ledger") else {
    return Error(Corrupt("ledger"))
  }
  let Ok(inbox) = io.inbox(rt) else {
    return Error(Corrupt("inbox"))
  }
  let mut s = store.open(ledger, store.default_config())?
  store.serve(
    mut s,
    take inbox,
    [
      store.served_transact(opens),
      store.served_transact(moves),
      store.served_query(balance),
    ]
  )
}
```

The client calls an entry with `runner.transact(ledger, moves, Transfer(...))` and reads
with `runner.query(ledger, balance, account)`. A transact call takes the next position of
the client's call stream. The store decides each position once, inside the commit
(STORE-12): when a call's provider sends it again after an in-doubt end, the store returns
the outcome it recorded for that position instead of running the entry twice. A query
uses position 0, which no stream decides, and reads the latest commit.

The head checks what the host leaves to it: an entry it does not serve ends
`NotRun(Denied())`, a call naming other code `NotRun(VersionMismatch())`, and a level the
place does not declare `Failed(Storage(NoLevel()))`, each before anything runs.

## Run it under a simulated place

The `ledger` runner is a `sim@1` node that models `db@1` and names `head` as the place's
head entry. The first call to the node starts the head as a run of the same execution,
under the runner's grants, holding the place's inbox. Copy the grants template to a
scratch directory, because relative `host` paths resolve beside the template:

```sh
mkdir -p /tmp/ledger/db && cp examples/ledger-service/grants.json /tmp/ledger/
hirsel run examples/ledger-service/hirsel.json --grants /tmp/ledger/grants.json --record /tmp/ledger/T
```

```text
opened 1 with 500
opened 2 with 100
moved 200 from 1 to 2, fee 0
opened 3 with 0
moved 50 from 2 to 3, fee 0
moved 250 from 2 to 1, fee 0
insufficient: 550
account 1: 550
account 2: 0
account 3: 50
account 4: no account 4
```

The last transfer is refused with its sender's balance, and nothing it touched changes.

## Replay the whole system

The head is a run of the client's execution, and its place, store and directory are part
of the same deterministic simulation. So `--record T` writes one tape for the whole
system, and it replays alone, with no directory and no place:

```sh
hirsel run examples/ledger-service/hirsel.json --replay /tmp/ledger/T
```

The replay prints the same eleven lines.

## What is not here yet

- A `sim@1` place's head runs in the simulated world, and its `ledger` grant is an
  `authority.memory@1` authority, so its store lives in memory for the run. The runner's `db` grant must name an existing directory but is not written,
  and a second run starts from an empty store.
- `sim@1` produces no faults at a `db@1` place, so the crash and lost-reply paths are not
  exercised here.
- The head does not yet record each take and end on a tape of its own, so
  `hirsel tapes check` finds no links and `hirsel tapes trial` re-executes no transact or
  query calls (STORE-15).
- `hirsel serve` can host a real place (`place` in its host configuration), but a
  caller's `db@1` runner over `remote.tls@1` does not send transact or query calls yet, and
  native runs do not reach places.
