# CoreIR ownership soundness

A Lean 4 model of the CoreIR ownership pass (`crates/hirsel-core-ir/src/wf/own.rs`,
`own_flow.rs` and `own_names.rs`) and a proof that a body the pass accepts drops no place
twice (R1), uses no place after its drop (R2), drops no `view` parameter's place (R3), and
neither uses nor drops a view after its anchor is dropped or rewritten (OWN-20), on any
path.

The model has joins: a pointer block parameter that stands for a different place on each
incoming edge, as a `match` lowers to. The theorem has no hypothesis besides acceptance.

Line numbers are those of the three files, and of
`crates/hirsel-core-ir/src/tests/own.rs`, at commit `12b0f4b4`.

## Running

```sh
cd proofs/core-ir && lake build
```

The `proofs` job of `.github/workflows/linux-ci.yml` runs this build on every push to
`main` and every pull request.

Lean `v4.35.0` (`lean-toolchain`), core library only. The build fails on any error,
`sorry`, or change in the axioms a theorem rests on (`#guard_msgs` over `#print axioms`
in `OwnSoundness.lean`). No file declares an axiom or uses `native_decide`.

| Module | Holds |
|---|---|
| `OwnSoundness.lean` | What is proved, and the axioms each theorem rests on. |
| `OwnSoundness/Model.lean` | Places, the tables `check` builds, the analysis, the machine. |
| `OwnSoundness/Lemmas.lean`, `Abs.lean`, `Addr.lean` | The invariant `Abs` (an analysis state describes a store) and what an address denotes under it. |
| `OwnSoundness/Drop.lean`, `Write.lean`, `Acts.lean` | Each act keeps `Abs` (`act_sound`). |
| `OwnSoundness/Edge.lean` | Taking an edge keeps `Abs` (`enter_sound`, `refine_sound`). |
| `OwnSoundness/Sound.lean` | The block invariant, `preservation`, `sound` and its corollaries. |
| `OwnSoundness/Exec.lean` | An executable run of the machine, proved to be one of its runs. |
| `OwnSoundness/Examples.lean`, `Witness.lean` | The bodies of `tests/own.rs`, accepted and rejected. |

## What is proved

A body is `Accepted` when its tables pass `Prog.wf` and some assignment of start states
to blocks is a fixed point of the analysis that passes every check. The start states
`solve` computes (own_flow.rs:7-193) are meant to be such an assignment; that is read
off the Rust, not proved (see below). `Reachable P c` says the machine gets from the
entry of the body to the configuration `c`.

| Theorem | Statement |
|---|---|
| `sound` | An `Accepted` body reaches no fault. |
| `no_double_drop` | R1: no reachable `drop` is of a place with a dropped part. |
| `no_use_after_drop` | R2: no reachable use is of a place with a dropped part. |
| `no_view_drop` | R3: no reachable `drop` is of a `view` parameter's place. |
| `no_dangling_view` | OWN-20: no reachable use or drop is of a view whose anchor was dropped or rewritten. |
| `preservation` | A step from a configuration the accepted start states describe (`Inv`) reaches one they describe; they describe no fault. |
| `act_sound` | One act: where `apply`'s checks pass on a state that describes the store, the act does not fault and `apply`'s state describes the store it leaves. |
| `enter_sound` | One edge: `transfer`'s state describes the store after the edge binds its parameters. |
| `refine_sound` | A branch on a flag read that is not stale: the state kept for the edge taken describes the store. |
| `reach_of_exec` | A body that faults when `exec` runs it along a list of moves reaches that fault in the machine. |

All rest on `propext`, `Classical.choice` and `Quot.sound` at most.

## The four hypotheses of the join-free proof

The first version of this proof covered bodies with no joins and assumed four things the
pass did not check. The pass checks each now, the model has each, and `sound` assumes
none. For each, a body that breaks only that one faults in the machine and is not
`Accepted`.

| Was assumed | The pass | The model | Rejected body |
|---|---|---|---|
| `noEntryEdge`: no edge targets the entry block. | `solve` merges what an edge carries into `b0` like any other block (`send`, own_flow.rs:55-82), and `names` takes no edge that gives an entry pointer parameter anything but itself (own_names.rs:212-223). | `validate` asks that every state a block sends is in its target's start, the entry block's included. `wfArg` asks that a parameter that is no join and names one place is given a pointer that names a place. | `loopBack_rejected`, `EntryEdge.rebound_rejected`, `EntryEdge.rebound_not_wf` |
| `flagFresh`: a branch on a flag read sees the flag's value at the branch. | A read that feeds a branch is `Act::Fresh` (own.rs:446-448); a store to its flag makes it stale (own.rs:436-445); only a read that is not stale refines the edges (own_flow.rs:107-121). | `Act.fresh`, `AState.stale`. The machine's branch goes by the value the read saw (`Store.read`, `Step.branch`), whatever the flag holds by then. | `stale_rejected` |
| `flagsStored`: no write but a whole `store` names a flag's slot. | A write that may touch a flag slot leaves the flag unknown and its reads stale (own.rs:363-378). | `havoc`. The machine's write may leave any value in the flag slot it is rooted at (`ActStep.write`). | `copied_rejected` |
| `writesBegin`: a write begins every tracked place it overlaps. | A write begins only the tracked places it covers (`covers`, own_names.rs:38-91; own.rs:390-398). | `Prog.covers`, `applyWrite`. The machine's write revives only the places inside a place it covers (`Prog.revives`, `Store.revive`). | `partial_rejected`; `sole_accepted` is the write of a record's one field, which covers the record. |

## Scope and idealizations

What the proof says is about the model. These are the places where the model is not the
Rust, or says less than a reader may expect.

**The model starts at the pass's acts and tables.** A `Prog` holds what `Own::acts`
(own.rs:191-269) yields for each instruction and the tables `names` and `check` build:
`under` (`n.vals`), `names` (`n.addrs`), `base` and `joins` (what `j.addrs` and `routed`
are computed from), `tracked`, `cells` (`pairs`, `guards` and `binds`), `borrows` and
`reads`. Reading a CoreIR body into a `Prog` is not modelled: the bodies of
`Examples.lean` are transcribed by hand. `Prog.wf` states what the proof needs of the
tables, each a property `names` or `check` establishes at the cited lines. The kernel
checks `wf` for every accepted body here; that the Rust tables satisfy it for every body
is read off the Rust, not proved.

**`Accepted` is any passing fixed point.** `validate` checks a given `start`; it does not
run the worklist. That `solve` ends at such a `start` is not proved.

**What the machine tracks.** For each place: live, dropped, or a dangling view. For each
flag slot its value, and for each flag read the value it saw. For each pointer parameter
the place it points at, or nothing.

- **Places, not values.** A move leaves its source alone, so a use after a move is no
  fault; `check_moves` (own_names.rs:385-426) refuses a `move` of an operand that may
  own, and is not modelled. A write to a place that is not live is no fault.
- **A use may always go through.** `ActStep.use_ok` has no condition; a use of a place
  with a dropped or dangling part may also fault. `sound` says the fault is not among
  the outcomes, which is the stronger claim.
- **Pointers from elsewhere name nothing.** A pointer from an `alloc`, a load or a
  call's result (`Src.none`) binds a parameter to no place. A drop or write through it
  is a step that changes nothing, and a use of it never faults. The heap is not
  modelled.
- **Places under a run-time offset** (`cut`). A drop or write through one changes
  nothing in the machine. A use through one faults if any place under its static steps
  is dropped, as the pass assumes. A pointer to one binds a parameter to no place
  (`Src.eval`).
- **No `DEEP` bound.** `step` (own_names.rs:18-29) cuts a place at 16 static steps; the
  model's places have any number.
- **One join per address.** An address is based on at most one pointer parameter
  (`base`), so it reaches at most one join place. The `deref` read of an address
  (`n.derefs`, `deref_hits`) is an address of its own, with its own `use`.
- **Join places are not places of the machine.** A join's root is the pass's name for
  where its parameter points; only places below the join roots are dropped or left
  dangling (`Dangles`).
- **Entry.** Nothing is dropped and no borrow is in force. A join's parameter points at
  nothing until an edge binds it. Any other parameter points only at a place it names,
  and one that names a single place points at it (`Initial`).
- **`dead`** drops the places inside the slot's tracked places, and `live` begins every
  place of the slot. Either leaves the views anchored on the slot dangling.
- **Borrows.** A borrow with one dest place ends when that place gets a new value
  (`end`, own.rs:485-491), and `wf` asks that place to be below the join roots. A
  borrow with several dest places never ends, in the pass or the machine.
- **Flag slots.** A flag changes by a `store` of it, or by a write through an address
  that names a place in its slot. A flag slot whose pointer escapes guards nothing in
  the pass (own.rs:771-802); the model has no write to a flag through a pointer that
  names nothing, so its `cells` must hold only the guards `check` keeps.
- **Control.** A branch on a flag read goes by the value read. Any other terminator
  may take any of its edges, and an instruction with a failure edge may fail.
- **Failure edges.** An instruction that fails leaves the store as it was; a failing
  `drop` has done its acts (own_flow.rs:94-104).
- **`sole` is given.** Whether a place is a single-field record that is no resource
  comes from the module's types (own_names.rs:53-90); the model takes it as a table.

**Checks of the pass that are not modelled.** `Act::CopyToDest` and the `dest_escaped`
bit (own.rs:401-411, 432-434), the checks at a `return` (own_flow.rs:131-153),
`check_moves`, `check_call` (own_names.rs:339-382), `local_owned_slots`
(own_names.rs:299-336) and the scratch budget. They guard against what the machine
cannot express: a value that changes owner through a `move`, two `mut` or dest
arguments of a call that are one place, and a pointer or view of a local that outlives
the body. The theorems say nothing about those faults. The checks change no state the
four modelled faults depend on, which is why the model can leave them out.

**Shape of the definitions.** The walk of `invalidate` carries fuel in Lean; `dfs_spec`
shows `tracked.length + 1` is never exhausted. `renew` sets all places of a write or a
`live` at once where the pass goes place by place (own.rs:390-398, 420-428); the two
agree because `begun` and `dropped` keep which halves of a state are present.

## Test bodies

`Examples.lean` transcribes the bodies of `crates/hirsel-core-ir/src/tests/own.rs` that
the modelled checks decide. For each body the test expects accepted, the start states
`solve` computes pass `validate` (`_accepted`), so it reaches no fault (`_safe`). For
each body the test expects rejected, `exec` runs it to a fault, the kernel checks the
run, and so the body is not `Accepted` (`_rejected`). The verdicts are the tests', not
the model's.

| Test (`tests/own.rs`) | Lean | Accepted | Rejected: the fault reached |
|---|---|---|---|
| `ownership_rejects_a_drop_repeated_on_its_failure_edge` (8) | `FailEdge` | `once` | `again`: double drop |
| `ownership_follows_a_joined_pointer_along_its_edge` (43) | `Joined` | `loop` | `twice`: double drop; `after`: use after drop |
| `ownership_rejects_a_view_used_after_its_anchor_is_dropped` (109) | `ViewAnchor` | `owned` | `view`: dangling view |
| `ownership_drops_a_joined_place_only_where_the_join_is_bound` (186) | `Bound` | `cleared` | `kept`: double drop |
| `ownership_rejects_entry_backedge_stale_flag_nonstore_flag_and_partial_write` (260) | `loopBack`, `flagged`, `partialWrite` | `sole` | `loopBack`, `stale`, `copied`, `partial`: double drop |
| `ownership_rejects_dead_anchor_and_escaping_local_returns` (396), its first body | `DeadAnchor` | | `dead`: dangling view |
| `storage_dead_leaves_a_dropped_place_dropped` (620) | `Dead` | `reuse` | `again`: double drop |
| `rewriting_an_anchor_ends_the_view_at_the_write` (654) | `Rewrite` | `before` | `after`: dangling view |
| `a_parameter_that_may_be_a_slot_or_a_heap_pointer_is_a_join` (695) | `SlotOrHeap` | `built` | `revived`: double drop |
| `an_edge_into_the_entry_block_keeps_each_pointer_parameter` (750) | `EntryEdge` | `same` | `rebound`: double drop, and `wf` fails |
| `a_join_keeps_its_places_through_pointer_and_through_another_join` (800) | `Through` | | `laundered`: double drop; `forwarded`: use after drop |
| `a_view_reached_through_a_join_ends_with_its_anchor` (865) | `ViewJoin` | `before` | `after`: dangling view |
| `a_drop_through_a_join_follows_the_flag_of_the_join_moved_into_it` (927) | `Moved` | `moved` | `kept`: double drop |

Not transcribed, because the checks they exercise are not modelled: the second and
third bodies of `ownership_rejects_dead_anchor_and_escaping_local_returns` (the checks
at a `return`), `wf_module_with_budget_bounds_scratch_and_deadline` (496),
`memmove_requires_plain_operands_and_preserves_plain_sources` (556) and
`mutable_call_checks_the_places_carried_by_block_parameters` (585).

## Traceability

| Lean | Rust | Rule |
|---|---|---|
| `Pl`, `PStep`, `Path` | own_names.rs:4-13 | |
| `overlap` | own_names.rs:31-33 | R1, R2 |
| `Prog.covers`, `soleChain`, `Prog.sole` | own_names.rs:38-91 | |
| `Bits`, `Bits.Z` | own.rs:52-59 | |
| `Bits.half` | own.rs:61-63 (`halves`) | |
| `Bits.drops` | own.rs:64-68 (`dropped`), with `mark` own.rs:494-502 | R1 |
| `Bits.begun` | own.rs:69-72 | |
| `Bits.flag` | own.rs:73-81 | |
| `Bits.keep` | own_flow.rs:113-116 | |
| `Act` | own.rs:84-100 | |
| `Inst`, `Block` | own.rs:191-269 (`acts`), own_flow.rs:91-105 | |
| `Src`, `Edge` | own_names.rs:429-440 (`ptr_args`), own_flow.rs:246-262 | |
| `Term` | own_flow.rs:106-180 | |
| `Prog.under`, `Prog.names` | own_names.rs:95-99 (`Names`), 103-185 (`spread`), 199-224 | |
| `Prog.joins`, `joinOf`, `jroot`, `named`, `wfArg` | own_names.rs:225-278 (the join rule), 212-223 (edges into `b0`) | |
| `Prog.base`, `jname`, `places` | own_names.rs:279-293 (`j`), own.rs:536-540 (`routed`) | |
| `Prog.isView` | own.rs:520-533, 306-313 | R3 |
| `Prog.tracked`, `wfAct` | own.rs:541-559 | |
| `Prog.expand`, `wfAddr` | own.rs:589-613 | |
| `wfArg`, a join given to a join | own.rs:614-647 | |
| `Prog.hits` | own.rs:656-680 | R1, R2 |
| `Prog.shadow` | own.rs:681-697 | |
| `Borrow`, `Prog.borrows`, `wfBorrow` | own.rs:563-585 | OWN-20 |
| `Prog.lent`, `rewrites`, `anchored` | own.rs:699-770 (`lent`, `rewrites`, `anchor_slots`) | OWN-20 |
| `Cell`, `Prog.cells`, `owner`, `guard`, `pairsOf` | own.rs:803-807 (`pairs`), 881-931 (`guards`) | |
| `Cell.bind`, `Prog.bindCell`, `wfCells` | own.rs:808-810, 932-947 (`binds`) | |
| `Prog.reads`, `Act.fresh` | own.rs:844-880 (`flag_of`, `read_idx`, `reads_of_flag`) | |
| `AState`, `AState.init`, `AState.le` | own_flow.rs:11-29, 55-82 (`send`) | |
| `mayDrop` | own.rs:270-272 | R1, R2 |
| `Prog.seeds`, `Prog.succs`, `dfs`, `reach`, `invalidate` | own.rs:454-478 (`invalidate`, `enqueue`) | OWN-20 |
| `mark` | own.rs:494-502 | |
| `dropPlace` | own.rs:479-482 (`drop`) | R1, OWN-20 |
| `boundDrop`, `bindD` | own_flow.rs:216-222 (`bound_drop`) | R1 |
| `dropBound` | own_flow.rs:196-212 (`drop_bound`) | R1 |
| `Prog.same`, `shadowDrop`, `dropShadow` | own_flow.rs:226-242 (`drop_shadow`) | R1 |
| `Prog.own`, `dropName`, `applyDrop` | own.rs:305-359 (`Act::Drop`) | R1 |
| `endAll`, `renew` | own.rs:485-491 (`end`), 390-398, 420-428 | |
| `touches`, `havoc` | own.rs:362-378 | |
| `applyWrite` | own.rs:361-400 (`Act::Write`) | |
| `applySlot` | own.rs:412-429 (`Act::Begin`, `Act::Dead`) | |
| `applyAct` | own.rs:280-451 (`apply`) | |
| `checkAct` | own.rs:289-304 (`Act::Use`), 305-323 (`Act::Drop`) | R1, R2, R3 |
| `failOuts` | own_flow.rs:94-104 | |
| `refine` | own_flow.rs:107-116 | |
| `Prog.srcs`, `Prog.argOf`, `joinBits`, `bindOne`, `bindBits`, `transfer` | own_flow.rs:245-323 (`transfer`) | R1, R2 |
| `termOuts`, `termOk` | own_flow.rs:106-180 | |
| `blockOuts`, `blockOk` | own_flow.rs:83-182 (`step_block`) | |
| `validate`, `Accepted` | own_flow.rs:7-193 (`solve`) | |
| `Store`, `PSt`, `Fault`, `Den`, `Hangs`, `ActStep`, `Step`, `Initial` | the machine; own.rs:1-42 describes it | |
| `Abs`, `Inv` | the proof's invariant | |
| `dfs_spec`, `hangs_reach` | own.rs:454-467 reaches every view an anchor leaves dangling | OWN-20 |
| `drop_sound` | own.rs:305-359, own_flow.rs:196-242 | R1, OWN-20 |
| `write_sound`, `begin_sound`, `dead_sound` | own.rs:361-400, 412-429 | |
| `act_sound` | own.rs:280-451, one act | R1, R2, R3, OWN-20 |
| `enter_sound` | own_flow.rs:245-323, one edge | R1, R2 |
| `preservation`, `sound` | own_flow.rs:7-193 | R1, R2, R3, OWN-20 |
