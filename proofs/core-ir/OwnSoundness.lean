import OwnSoundness.Witness

/-!
# Soundness of the CoreIR ownership pass, joins included

A Lean 4 model of `crates/hirsel-core-ir/src/wf/{own,own_flow,own_names}.rs` and a proof
that a body the pass accepts drops no place twice (R1), uses no place after its drop (R2),
drops no `view` parameter's place (R3), and neither uses nor drops a view after its anchor
is dropped or rewritten (OWN-20), on any path. The bodies may have joins: pointer block
parameters that stand for a different place on each incoming edge, as a `match` lowers to.

## The statement

`Accepted P` says some assignment of start states to blocks is a fixed point of
`own_flow.rs` `solve` that passes every check of `own.rs` `apply`, over tables that pass
the checks of `Prog.wf`. `Reachable P c` says the machine of `Model.lean` gets from the
entry of the body to `c`.

* `sound : Accepted P → ¬ Reachable P (.fault k)`
* `no_double_drop`, `no_use_after_drop`, `no_view_drop`, `no_dangling_view`: `sound` for
  each of the four faults.
* `preservation`: a step from a configuration the accepted start states describe reaches
  one they describe, and they describe no fault.

`sound` has no hypothesis besides `Accepted`. The four hypotheses of the join-free proof
are now part of the model: an edge into the entry block merges into its start state, a
branch on a flag read goes by the value read and refines only a read that is not stale, a
write that may touch a flag slot leaves the flag unknown, and a write begins only the
places it covers. `README.md` says where each is modelled, lists what the model leaves
out, and maps each definition to the Rust it models.

## Modules

* `Model`: places, the tables `check` builds, the analysis, the machine.
* `Lemmas`, `Abs`, `Addr`: the invariant `Abs` (an analysis state describes a store) and
  what an address denotes under it.
* `Drop`, `Write`, `Acts`: each act keeps `Abs` (`act_sound`).
* `Edge`: taking an edge keeps `Abs` (`enter_sound`, `refine_sound`).
* `Sound`: the block invariant, `preservation`, `sound` and its four corollaries.
* `Exec`: an executable run of the machine (`exec_steps`, `reach_of_exec`).
* `Examples`, `Witness`: the bodies of `crates/hirsel-core-ir/src/tests/own.rs`. Each one
  the pass accepts is `Accepted`; each one it rejects reaches a fault, so is not.

## Axioms

Each theorem rests on Lean's three standard axioms at most. `lake build` fails if one of
these messages changes.
-/

namespace Hirsel.CoreIR.Own

/-! ### The proof -/

/--
info: 'Hirsel.CoreIR.Own.act_sound' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms act_sound

/--
info: 'Hirsel.CoreIR.Own.enter_sound' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms enter_sound

/--
info: 'Hirsel.CoreIR.Own.refine_sound' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms refine_sound

/--
info: 'Hirsel.CoreIR.Own.preservation' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms preservation

/--
info: 'Hirsel.CoreIR.Own.sound' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms sound

/--
info: 'Hirsel.CoreIR.Own.no_double_drop' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms no_double_drop

/--
info: 'Hirsel.CoreIR.Own.no_use_after_drop' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms no_use_after_drop

/--
info: 'Hirsel.CoreIR.Own.no_view_drop' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms no_view_drop

/--
info: 'Hirsel.CoreIR.Own.no_dangling_view' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms no_dangling_view

/-! ### Runs of the machine -/

/--
info: 'Hirsel.CoreIR.Own.exec_steps' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms exec_steps

/--
info: 'Hirsel.CoreIR.Own.reach_of_exec' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms reach_of_exec

/--
info: 'Hirsel.CoreIR.Own.rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms rejected

/-! ### The test bodies -/

/--
info: 'Hirsel.CoreIR.Own.Joined.loop_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms Joined.loop_accepted

/--
info: 'Hirsel.CoreIR.Own.Joined.loop_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Joined.loop_safe

/--
info: 'Hirsel.CoreIR.Own.Joined.twice_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Joined.twice_double_drops

/--
info: 'Hirsel.CoreIR.Own.Joined.twice_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Joined.twice_rejected

/--
info: 'Hirsel.CoreIR.Own.Joined.after_uses_dropped' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Joined.after_uses_dropped

/--
info: 'Hirsel.CoreIR.Own.Joined.after_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Joined.after_rejected

/--
info: 'Hirsel.CoreIR.Own.Bound.cleared_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms Bound.cleared_accepted

/--
info: 'Hirsel.CoreIR.Own.Bound.cleared_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Bound.cleared_safe

/--
info: 'Hirsel.CoreIR.Own.Bound.kept_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Bound.kept_double_drops

/--
info: 'Hirsel.CoreIR.Own.Bound.kept_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Bound.kept_rejected

/--
info: 'Hirsel.CoreIR.Own.Dead.reuse_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms Dead.reuse_accepted

/--
info: 'Hirsel.CoreIR.Own.Dead.reuse_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Dead.reuse_safe

/--
info: 'Hirsel.CoreIR.Own.Dead.again_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Dead.again_double_drops

/--
info: 'Hirsel.CoreIR.Own.Dead.again_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Dead.again_rejected

/--
info: 'Hirsel.CoreIR.Own.Rewrite.before_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms Rewrite.before_accepted

/--
info: 'Hirsel.CoreIR.Own.Rewrite.before_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Rewrite.before_safe

/--
info: 'Hirsel.CoreIR.Own.Rewrite.after_dangles' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Rewrite.after_dangles

/--
info: 'Hirsel.CoreIR.Own.Rewrite.after_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Rewrite.after_rejected

/--
info: 'Hirsel.CoreIR.Own.SlotOrHeap.built_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms SlotOrHeap.built_accepted

/--
info: 'Hirsel.CoreIR.Own.SlotOrHeap.built_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms SlotOrHeap.built_safe

/--
info: 'Hirsel.CoreIR.Own.SlotOrHeap.revived_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms SlotOrHeap.revived_double_drops

/--
info: 'Hirsel.CoreIR.Own.SlotOrHeap.revived_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms SlotOrHeap.revived_rejected

/--
info: 'Hirsel.CoreIR.Own.EntryEdge.same_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms EntryEdge.same_accepted

/--
info: 'Hirsel.CoreIR.Own.EntryEdge.same_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms EntryEdge.same_safe

/--
info: 'Hirsel.CoreIR.Own.EntryEdge.rebound_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms EntryEdge.rebound_double_drops

/--
info: 'Hirsel.CoreIR.Own.EntryEdge.rebound_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms EntryEdge.rebound_rejected

/--
info: 'Hirsel.CoreIR.Own.EntryEdge.rebound_not_wf' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms EntryEdge.rebound_not_wf

/--
info: 'Hirsel.CoreIR.Own.Through.laundered_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Through.laundered_double_drops

/--
info: 'Hirsel.CoreIR.Own.Through.laundered_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Through.laundered_rejected

/--
info: 'Hirsel.CoreIR.Own.Through.forwarded_uses_dropped' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Through.forwarded_uses_dropped

/--
info: 'Hirsel.CoreIR.Own.Through.forwarded_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Through.forwarded_rejected

/--
info: 'Hirsel.CoreIR.Own.ViewJoin.before_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms ViewJoin.before_accepted

/--
info: 'Hirsel.CoreIR.Own.ViewJoin.before_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms ViewJoin.before_safe

/--
info: 'Hirsel.CoreIR.Own.ViewJoin.after_dangles' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms ViewJoin.after_dangles

/--
info: 'Hirsel.CoreIR.Own.ViewJoin.after_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms ViewJoin.after_rejected

/--
info: 'Hirsel.CoreIR.Own.Moved.moved_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms Moved.moved_accepted

/--
info: 'Hirsel.CoreIR.Own.Moved.moved_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Moved.moved_safe

/--
info: 'Hirsel.CoreIR.Own.Moved.kept_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms Moved.kept_double_drops

/--
info: 'Hirsel.CoreIR.Own.Moved.kept_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms Moved.kept_rejected

/--
info: 'Hirsel.CoreIR.Own.FailEdge.once_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms FailEdge.once_accepted

/--
info: 'Hirsel.CoreIR.Own.FailEdge.once_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms FailEdge.once_safe

/--
info: 'Hirsel.CoreIR.Own.FailEdge.again_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms FailEdge.again_double_drops

/--
info: 'Hirsel.CoreIR.Own.FailEdge.again_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms FailEdge.again_rejected

/--
info: 'Hirsel.CoreIR.Own.ViewAnchor.owned_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms ViewAnchor.owned_accepted

/--
info: 'Hirsel.CoreIR.Own.ViewAnchor.owned_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms ViewAnchor.owned_safe

/--
info: 'Hirsel.CoreIR.Own.ViewAnchor.view_dangles' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms ViewAnchor.view_dangles

/--
info: 'Hirsel.CoreIR.Own.ViewAnchor.view_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms ViewAnchor.view_rejected

/--
info: 'Hirsel.CoreIR.Own.DeadAnchor.dead_dangles' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms DeadAnchor.dead_dangles

/--
info: 'Hirsel.CoreIR.Own.DeadAnchor.dead_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms DeadAnchor.dead_rejected

/--
info: 'Hirsel.CoreIR.Own.loopBack_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms loopBack_double_drops

/--
info: 'Hirsel.CoreIR.Own.loopBack_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms loopBack_rejected

/--
info: 'Hirsel.CoreIR.Own.stale_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms stale_double_drops

/--
info: 'Hirsel.CoreIR.Own.stale_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms stale_rejected

/--
info: 'Hirsel.CoreIR.Own.copied_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms copied_double_drops

/--
info: 'Hirsel.CoreIR.Own.copied_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms copied_rejected

/--
info: 'Hirsel.CoreIR.Own.partial_double_drops' depends on axioms: [propext, Quot.sound]
-/
#guard_msgs in
#print axioms partial_double_drops

/--
info: 'Hirsel.CoreIR.Own.partial_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms partial_rejected

/--
info: 'Hirsel.CoreIR.Own.sole_accepted' depends on axioms: [propext]
-/
#guard_msgs in
#print axioms sole_accepted

/--
info: 'Hirsel.CoreIR.Own.sole_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms sole_safe

end Hirsel.CoreIR.Own
