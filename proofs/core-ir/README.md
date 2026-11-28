# CoreIR ownership soundness

`OwnSoundness.lean` is a Lean 4 model of the CoreIR ownership pass
`crates/hirsel-core-ir/src/wf/own.rs` and a proof that a body the pass accepts drops no
place twice (R1), uses no place after its drop (R2), drops no `view` parameter's place
(R3), and neither uses nor drops a dest view after a drop of its anchor (OWN-20), on any
path. It covers join-free bodies and holds under four named hypotheses, each of which the
pass relies on and does not check.

Line numbers are those of `own.rs` at commit `a6acb07f`.

## Running

```sh
cd proofs/core-ir && lake build
```

No repository gate runs this build; it is run by hand.

Lean `v4.35.0` (`lean-toolchain`), core library only. The build fails on any error,
`sorry`, or change in the axioms a theorem rests on (`#guard_msgs` over `#print axioms`
at the end of the file).

## What is proved

A body is `Accepted` when some assignment of start states to blocks is a post-fixpoint
of the analysis and passes every check. The fixpoint `check` computes (own.rs:875-966)
is such an assignment, so a body `wf::own` accepts is `Accepted`.

| Theorem | Statement |
|---|---|
| `preservation` | A step from a configuration the accepted analysis describes (`Inv`) reaches one it describes; a fault is never described. |
| `sound` | Under `Hyp`, an `Accepted` body reaches no `Fault`. |
| `no_double_drop` | R1: no reachable `drop` names a place overlapping one that is dropped. |
| `no_use_after_drop` | R2: no reachable use names a place overlapping one that is dropped. |
| `no_view_drop` | R3: no reachable `drop` names a `view` parameter's place. |
| `no_dangling_view` | OWN-20: no reachable use or drop names a dest view left dangling by a drop of its anchor. |
| `hyp_own`, `hyp_cells` | `Hyp` from checks of the body, on each of the two machines below. `flagFresh` is not among the checks: both machines branch on the flag's value at the branch. |

All rest on `propext`, `Classical.choice` and `Quot.sound` at most.

## Hypotheses

`Hyp S P` has five fields. `wf` says the tables are built as `check` builds them (a
drop's uncut names and every borrow's dest are tracked). The other four are assumptions
about the body or the machine. For each, a body that breaks only that one is accepted by
the analysis and drops a place twice in the model, and `wf::module` accepts a CoreIR
body of the same shape (below).

| Field | Says | Why the pass needs it | Lean witness |
|---|---|---|---|
| `noEntryEdge` | No edge or failure edge targets the entry block. | own.rs:962 discards what an edge carries into block 0, so the entry state never learns of a drop on a path back to it. | `EntryEdge.needed` |
| `flagFresh` | A branch on a read of a drop flag sees the flag's value at the branch. | own.rs:912-919 refines both edges by the flag's state at the branch; `flag_of` (own.rs:811-819) maps the value read, however early the read. | `StaleFlag.needed` |
| `flagsStored` | No write other than a whole `store` names a guarding flag's slot. | own.rs:339-345 turns only a whole `store` into `Act::Flag`; a `copy`, `move`, `set_case` or `mut` argument into the slot is an `Act::Write` of an untracked place. | `FlagWrite.needed` |
| `writesBegin` | A write through a single place begins every tracked place it overlaps. | own.rs:454-470 marks every tracked place overlapping the write begun, including a place the write covers only part of. | `PartialWrite.needed` |

The two machines differ in what a write begins. On `Sem.own`, the machine the header of
own.rs describes, a write begins every place it overlaps, and `writesBegin` holds of every
body. On `Sem.cells` a write begins only the places inside the written one, so a write of
one field leaves the rest of a dropped place dropped; there `writesBegin` is the check
`Prog.writesWhole`: each write through a single place covers every tracked place it
overlaps.

`flagFresh` is a hypothesis on the machine, not a check of the body. A `Prog` holds
acts, and no act is a read, so the model cannot say where a body reads a flag: `Sem.own`
and `Sem.cells` branch on the flag's value at the branch, and `hyp_own` and `hyp_cells`
get `flagFresh` from that alone. For a CoreIR body it stays an unchecked obligation: no
store to a drop flag between the `read` whose value a `branch` tests and that `branch`.
The `flagFresh` body below maps to a `Prog` that `hyp_own` and `sound` call safe; it is
the machine `StaleFlag.stale`, whose branch may see a value read earlier, that runs it
as CoreIR does.

The CoreIR bodies `wf::module` accepts at `a6acb07f`, of the same shape as the Lean
witnesses (a `resource` type `t0`, `t1 = { a: t0, b: t0 }`, `v1` a `ptr` parameter):

- `noEntryEdge`: `b0(v0, v1)` drops `a0 = v1 : t0`, branches to `b1`, and `b1` jumps
  to `b0(v0, v1)`. The same loop one block later is rejected.
- `flagFresh`: with `s0` filled and `a2` the `bool` slot `s1`: `store a2, true`,
  `v6 = read a2`, `drop a0`, `store a2, false`, `branch v6, b1, b2`, and `b1` drops `a0`.
- `flagsStored`: `store a2, false`, `store a3, true`, `copy a2, a3`, `drop a0`,
  `v6 = read a2`, `branch v6, b1, b2`, and `b1` drops `a0`.
- `writesBegin`: `drop a0` of all of `s0 : t1`, `copy` into `s0 field 0`, then `drop` of
  `s0 field 1`. Without the `copy` the second drop is rejected.

## Scope and idealizations

- **Joins are not modelled.** A pointer block parameter whose edges carry more than one
  place gets a root of its own (own.rs:162-199); `routed`, `shadow`, `binds`, `transfer`
  (own.rs:551-629) and `drop_bound` (own.rs:510-530) handle it. The model is own.rs with
  no joins, where all of these do nothing. Bodies with joins are outside the theorems.
- **The model starts at the pass's acts.** `Prog` holds what `Own::acts` (own.rs:329-381)
  yields for each instruction and the tables `check` builds. The reading of CoreIR
  instructions into acts, and of addresses into places (`spread`, own.rs:87-156), is not
  modelled: the machine assumes an address denotes one of the places `names` gives it,
  and that a pointer from an `alloc`, a load or a call's result names nothing tracked.
- **Places, not values.** As the pass, the machine records for each place whether it is
  dropped. A move leaves its source's state alone (own.rs:30-32), so a use after a move
  is not a fault here; neither is a write to a place that is not live, a write through
  two `mut` paths to one place, or a `return` that leaves a view of a local.
- **`live` and `dead` begin a place anew** without a value (`Act::Begin`,
  own.rs:472-482): after a drop, `dead` of the slot clears the pass's dropped bit, and
  the machine's place is live again. A second drop after it passes this pass and is no
  fault in the machine.
- **A view dangles only by a drop.** `dead` of the anchor's slot, or a new value written
  to the anchor, leaves the dest view usable in both the pass and the machine.
  `wf::module` accepts both.
- **Places under a run-time offset** (`cut`) are tracked by neither.
- **Failure edges.** A failing instruction leaves the store as it was; a failing `drop`
  has done its acts (own.rs:899-909).
- **The walk of `drop`** carries fuel in Lean; `dfs_spec` shows `tracked.length + 1` is
  never exhausted, so `dfs` is own.rs `drop` (own.rs:494-507).
- **Entry borrow bits.** `AState.init` sets every borrow's bit, as `vec![Z; width]` does
  (own.rs:878).

## Traceability

| Lean | own.rs | Rule |
|---|---|---|
| `Pl`, `PStep` | 46-55 | |
| `overlap` | 73-75 | R1, R2 |
| `Bits`, `Bits.Z` | 215-222 | |
| `Bits.half` | 224-226 (`halves`) | |
| `Bits.dropped` | 227-231, with `mark` 540-548 | R1 |
| `Bits.begun` | 232-235 | |
| `Bits.flag` | 236-244 | |
| `Bits.keep` | 915-919 | |
| `Act` | 247-255 | |
| `Inst`, `Block` | 329-381 (`acts`), 897-910 | |
| `Term` | 911-957 | |
| `Prog.names`, `Prog.derefs` | 79-84, 162-199 (`n.addrs`, `n.derefs`) | |
| `Prog.tracked`, `Prog.wf` | 656-672, 702-707 | |
| `Prog.pairing`, `owner`, `guard` | 803 (`pairs`), 826-859 (`guards`) | |
| `Borrow`, `Prog.borrows` | 674-709 | OWN-20 |
| `Prog.isView` | 409, 639-650 | R3 |
| `Prog.hits`, `Prog.derefHits` | 739-752 | R1, R2 |
| `Prog.succs` | 767-786 (`lent`), 498-504 | OWN-20 |
| `AState`, `AState.init`, `AState.le` | 877-888 | |
| `mayDrop` | 382-384 | R1, R2 |
| `dfs`, `dropAll`, `dropPlace` | 494-507 (`drop`), 540-548 (`mark`) | OWN-20 |
| `dropRoots` | 433-442 | R1 |
| `beginAll` | 533-537 (`end`), 463-469, 473-481 | |
| `applyAct` | 386-491 (`apply`) | |
| `checkAct` | 388-422 | R1, R2, R3 |
| `failOuts` | 899-909 | |
| `refine`, `termOuts`, `termOk` | 911-957 | |
| `blockOuts`, `blockOk` | 891-959 (`flow`) | |
| `validate`, `Accepted` | 875-972 | |
| `Store`, `PSt`, `Fault`, `ActStep`, `Step` | the machine; own.rs:1-42 describes it | |
| `Abs`, `Inv` | the proof's invariant | |
| `dfs_spec`, `hangs_dfs` | 494-507 reaches every view a drop leaves dangling | OWN-20 |
| `act_sound` | 386-491, one act | R1, R2, R3, OWN-20 |
| `preservation`, `sound` | 875-972 | R1, R2, R3, OWN-20 |
| `FailEdge` | `tests/own.rs:7-33` | R1 |
| `ViewAnchor` | `tests/own.rs:109-176` | OWN-20 |

`FailEdge` and `ViewAnchor` transcribe two tests of `crates/hirsel-core-ir/src/tests/own.rs`:
the variant the test expects accepted is `Accepted` and reaches no fault
(`once_safe`, `owned_safe`), and the variant it expects rejected reaches a fault in the
machine and so is not `Accepted` (`twice_rejected`, `view_rejected`).
