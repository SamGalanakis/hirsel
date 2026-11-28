import OwnSoundness.Examples
import OwnSoundness.Exec
import OwnSoundness.Sound

/-!
# The test bodies, accepted and rejected

For each body `wf::own` accepts in `crates/hirsel-core-ir/src/tests/own.rs`, the start
states `solve` computes pass `validate`, so the body reaches no fault. For each body it
rejects, a run that faults, so the model's `wf::own` rejects it too: an accepted body
would not fault.
-/

namespace Hirsel.CoreIR.Own

/-- A body that reaches a fault is not accepted. -/
theorem rejected {P : Prog} {k : Fault} (h : Reachable P (.fault k)) : ¬ Accepted P :=
  fun ha => sound ha k h

open Move

namespace Joined

set_option maxRecDepth 100000 in
theorem loop_accepted : Accepted (body .loop) :=
  ⟨table [some ([3, 3, 3, 3, 3], [], []), some ([15, 15, 15, 15, 15], [], []),
    some ([15, 15, 15, 15, 15], [], []), some ([15, 15, 15, 15, 15], [], []),
    some ([15, 15, 3, 15, 15], [], []), some ([15, 15, 12, 15, 15], [], [])],
    by decide +kernel⟩

theorem loop_safe (k : Fault) : ¬ Reachable (body .loop) (.fault k) := sound loop_accepted k

/-- `b2` binds the join to `s0`, and `b4` drops through it twice. -/
theorem twice_double_drops : Reachable (body .twice) (.fault .doubleDrop) :=
  reach_of_exec false [edge 0, edge 0, next, next, edge 0, next, next] (by decide +kernel)

theorem twice_rejected : ¬ Accepted (body .twice) := rejected twice_double_drops

/-- `b4` drops `s0` and then reads through the join `b2` bound to it. -/
theorem after_uses_dropped : Reachable (body .after) (.fault .useAfterDrop) :=
  reach_of_exec false [next, next, edge 0, edge 0, next, next, edge 0, next, next]
    (by decide +kernel)

theorem after_rejected : ¬ Accepted (body .after) := rejected after_uses_dropped

end Joined

namespace Bound

set_option maxRecDepth 100000 in
theorem cleared_accepted : Accepted (body true) :=
  ⟨table [some ([3, 3, 3, 3, 3, 3, 3], [], [true]), some ([3, 3, 3, 1, 3, 3, 1], [], [true]),
    some ([3, 3, 3, 1, 3, 3, 1], [], [true]), some ([3, 3, 3, 3, 15, 15, 9], [], [true]),
    some ([15, 15, 12, 1, 15, 15, 1], [], [false]),
    some ([15, 15, 12, 14, 15, 15, 9], [], [false])],
    by decide +kernel⟩

theorem cleared_safe (k : Fault) : ¬ Reachable (body true) (.fault k) :=
  sound cleared_accepted k

/-- `b2` binds the join to `s1` with the flag still set; `b3` drops through the join and
`b4`, under the flag, drops `s1`. -/
theorem kept_double_drops : Reachable (body false) (.fault .doubleDrop) :=
  reach_of_exec false [next, next, next, next, edge 1, edge 0, next, next, next, next]
    (by decide +kernel)

theorem kept_rejected : ¬ Accepted (body false) := rejected kept_double_drops

end Bound

namespace Dead

theorem reuse_accepted : Accepted (body false) :=
  ⟨table [some ([3], [], []), some ([15], [], []), some ([12], [], [])], by decide +kernel⟩

theorem reuse_safe (k : Fault) : ¬ Reachable (body false) (.fault k) := sound reuse_accepted k

theorem again_double_drops : Reachable (body true) (.fault .doubleDrop) :=
  reach_of_exec false [edge 0, next, next, next, next, next] (by decide +kernel)

theorem again_rejected : ¬ Accepted (body true) := rejected again_double_drops

end Dead

namespace Rewrite

theorem before_accepted : Accepted (body false) :=
  ⟨table [some ([3], [false], [])], by decide +kernel⟩

theorem before_safe (k : Fault) : ¬ Reachable (body false) (.fault k) :=
  sound before_accepted k

/-- The store to the anchor's field leaves the view dangling, and the read is of it. -/
theorem after_dangles : Reachable (body true) (.fault .danglingView) :=
  reach_of_exec false [next, next, next, next] (by decide +kernel)

theorem after_rejected : ¬ Accepted (body true) := rejected after_dangles

end Rewrite

namespace SlotOrHeap

set_option maxRecDepth 100000 in
theorem built_accepted : Accepted (body true) :=
  ⟨table [some ([3, 3, 3, 3, 3, 3, 3], [], []), some ([12, 15, 15, 15, 3, 3, 3], [], []),
    some ([12, 15, 15, 15, 3, 3, 3], [], []), some ([15, 3, 3, 3, 15, 15, 15], [], [])],
    by decide +kernel⟩

theorem built_safe (k : Fault) : ¬ Reachable (body true) (.fault k) := sound built_accepted k

/-- `b2` gives the parameter a pointer a call returned: the write through it begins
nothing of `s0`, which `b3` then drops a second time. -/
theorem revived_double_drops : Reachable (body false) (.fault .doubleDrop) :=
  reach_of_exec false [next, next, next, edge 1, edge 0, next, next] (by decide +kernel)

theorem revived_rejected : ¬ Accepted (body false) := rejected revived_double_drops

end SlotOrHeap

namespace EntryEdge

theorem same_accepted : Accepted (body false) :=
  ⟨table [some ([15, 3], [], []), some ([3, 3], [], []), some ([3, 3], [], [])],
    by decide +kernel⟩

theorem same_safe (k : Fault) : ¬ Reachable (body false) (.fault k) := sound same_accepted k

/-- The edge back gives `v2` the old `v1`, whose place `b1` dropped; `b2` drops it again. -/
theorem rebound_double_drops : Reachable (body true) (.fault .doubleDrop) :=
  reach_of_exec false [next, edge 0, next, edge 0, next, edge 1, next] (by decide +kernel)

theorem rebound_rejected : ¬ Accepted (body true) := rejected rebound_double_drops

/-- `own_names.rs` `names` rejects the body before `solve` runs: the edge into the entry
block gives `v2` a place it does not name. -/
theorem rebound_not_wf : (body true).wf = false := by decide +kernel

end EntryEdge

namespace Through

/-- The pointer read from memory binds the join to nothing: the write through it begins
nothing of `s1`, which `b4` then drops a second time. -/
theorem laundered_double_drops : Reachable (body true) (.fault .doubleDrop) :=
  reach_of_exec false
    [next, next, next, next, next, edge 1, next, edge 0, edge 0, next, next]
    (by decide +kernel)

theorem laundered_rejected : ¬ Accepted (body true) := rejected laundered_double_drops

/-- `b1` binds the first join to the dropped `s1`, `b3` forwards it, and `b4` reads
through the second. -/
theorem forwarded_uses_dropped : Reachable (body false) (.fault .useAfterDrop) :=
  reach_of_exec false [next, next, next, next, next, edge 0, edge 0, edge 0, next]
    (by decide +kernel)

theorem forwarded_rejected : ¬ Accepted (body false) := rejected forwarded_uses_dropped

end Through

namespace ViewJoin

set_option maxRecDepth 100000 in
theorem before_accepted : Accepted (body false) :=
  ⟨table [some ([3, 3, 3, 3], [false], []), some ([3, 3, 3, 3], [true], []),
    some ([3, 3, 3, 3], [true], []), some ([3, 3, 3, 15], [true], [])],
    by decide +kernel⟩

theorem before_safe (k : Fault) : ¬ Reachable (body false) (.fault k) :=
  sound before_accepted k

/-- `b1` binds the join to the view `s1`; `b3` drops its anchor and reads through the
join. -/
theorem after_dangles : Reachable (body true) (.fault .danglingView) :=
  reach_of_exec false [next, next, next, next, next, next, edge 0, edge 0, next, next]
    (by decide +kernel)

theorem after_rejected : ¬ Accepted (body true) := rejected after_dangles

end ViewJoin

namespace Moved

set_option maxRecDepth 100000 in
theorem moved_accepted : Accepted (body true) :=
  ⟨table [
    some ([3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3], [], [true]),
    some ([3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3], [], [true]),
    some ([3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3], [], [true]),
    some ([3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 15, 15, 3, 3, 15, 15, 3], [], [true]),
    some ([3, 3, 3, 3, 3, 1, 1, 1, 3, 1, 15, 5, 3, 1, 15, 5, 3], [], [true]),
    some ([3, 3, 3, 3, 3, 1, 1, 1, 3, 1, 15, 5, 3, 1, 15, 5, 3], [], [true]),
    some ([3, 3, 3, 3, 3, 3, 3, 3, 15, 11, 15, 15, 15, 11, 15, 15, 15], [], [true]),
    some ([15, 15, 15, 12, 15, 1, 1, 1, 15, 1, 15, 5, 15, 1, 15, 5, 15], [], [false]),
    some ([15, 15, 15, 12, 15, 14, 15, 15, 15, 11, 15, 15, 15, 11, 15, 15, 15], [],
      [false])],
    by decide +kernel⟩

theorem moved_safe (k : Fault) : ¬ Reachable (body true) (.fault k) := sound moved_accepted k

/-- `b4` gives `b6` the first join with the flag still set; `b6` drops through its
parameter and `b7`, under the flag, through the first join. -/
theorem kept_double_drops : Reachable (body false) (.fault .doubleDrop) :=
  reach_of_exec false
    [edge 0, next, next, edge 0, next, next, edge 0, edge 0, next, next, next, next]
    (by decide +kernel)

theorem kept_rejected : ¬ Accepted (body false) := rejected kept_double_drops

end Moved

namespace FailEdge

theorem once_accepted : Accepted (body false) :=
  ⟨table [some ([3], [], []), some ([12], [], [])], by decide +kernel⟩

theorem once_safe (k : Fault) : ¬ Reachable (body false) (.fault k) := sound once_accepted k

/-- A `drop`'s failure edge leaves after the drop. -/
theorem again_double_drops : Reachable (body true) (.fault .doubleDrop) :=
  reach_of_exec false [fail, next] (by decide +kernel)

theorem again_rejected : ¬ Accepted (body true) := rejected again_double_drops

end FailEdge

namespace ViewAnchor

theorem owned_accepted : Accepted (body false) :=
  ⟨table [some ([3], [], []), some ([3], [], []), some ([3], [], []), some ([12], [], []),
    some ([12], [], []), some ([12], [], [])],
    by decide +kernel⟩

theorem owned_safe (k : Fault) : ¬ Reachable (body false) (.fault k) := sound owned_accepted k

theorem view_dangles : Reachable (body true) (.fault .danglingView) :=
  reach_of_exec false [edge 0, next, next, next, next, next, next, edge 0, next]
    (by decide +kernel)

theorem view_rejected : ¬ Accepted (body true) := rejected view_dangles

end ViewAnchor

namespace DeadAnchor

/-- The `dead` of the anchor's slot leaves the view dangling, and `b1` reads it. -/
theorem dead_dangles : Reachable body (.fault .danglingView) :=
  reach_of_exec false [next, next, next, next, next, edge 0, next] (by decide +kernel)

theorem dead_rejected : ¬ Accepted body := rejected dead_dangles

end DeadAnchor

/-! ### `ownership_rejects_entry_backedge_stale_flag_nonstore_flag_and_partial_write` -/

/-- The drop goes round the edge into the entry block. -/
theorem loopBack_double_drops : Reachable loopBack (.fault .doubleDrop) :=
  reach_of_exec false [next, edge 0, next] (by decide +kernel)

theorem loopBack_rejected : ¬ Accepted loopBack := rejected loopBack_double_drops

/-- The branch goes by what the read saw, before the flag was cleared. -/
theorem stale_double_drops : Reachable (flagged true) (.fault .doubleDrop) :=
  reach_of_exec true [next, next, next, next, next] (by decide +kernel)

theorem stale_rejected : ¬ Accepted (flagged true) := rejected stale_double_drops

/-- The `copy` to the flag's slot sets the flag the store had cleared. -/
theorem copied_double_drops : Reachable (flagged false) (.fault .doubleDrop) :=
  reach_of_exec true [next, next, next, next, next, next] (by decide +kernel)

theorem copied_rejected : ¬ Accepted (flagged false) := rejected copied_double_drops

/-- The write of one field begins only that field. -/
theorem partial_double_drops : Reachable (partialWrite false) (.fault .doubleDrop) :=
  reach_of_exec false [next, next, next] (by decide +kernel)

theorem partial_rejected : ¬ Accepted (partialWrite false) := rejected partial_double_drops

theorem sole_accepted : Accepted (partialWrite true) :=
  ⟨table [some ([3], [], [])], by decide +kernel⟩

theorem sole_safe (k : Fault) : ¬ Reachable (partialWrite true) (.fault k) :=
  sound sole_accepted k

end Hirsel.CoreIR.Own
