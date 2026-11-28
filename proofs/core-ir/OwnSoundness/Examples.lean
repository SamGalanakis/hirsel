import OwnSoundness.Model

/-!
# Bodies of `crates/hirsel-core-ir/src/tests/own.rs`, as `check` sees them

Instructions with no act and no failure edge are left out. A `copy to, from` is
`[use from, write to]`, a `store` of a constant to a flag slot `[write a, flag f c]`, a
`live s` `[begin s]` with `flag s none` when `s` is a flag slot, and a `read` that feeds a
branch `[use a, fresh ri]`.
-/

namespace Hirsel.CoreIR.Own

/-- A pairing state from its bits: 1 is `ZT`, 2 `ZF`, 4 `DT` and 8 `DF`. -/
def Bits.ofNat (n : Nat) : Bits := ⟨n.testBit 0, n.testBit 1, n.testBit 2, n.testBit 3⟩

/-- A block's start state: the cells, the borrows in force and the stale flag reads. -/
abbrev Row := List Nat × List Bool × List Bool

def Row.state (r : Row) : AState :=
  ⟨fun x => .ofNat (r.1.getD x 0), fun k => r.2.1.getD k false, fun ri => r.2.2.getD ri false⟩

/-- The start states of the blocks `solve` reaches. -/
def table (rows : List (Option Row)) : Nat → Option AState := fun b =>
  (rows.getD b none).map Row.state

/-- A whole place. -/
def pl (root : Nat) : Pl := ⟨root, [], false⟩

/-- The path of an address that is its pointer's place. -/
def here : Path := ⟨[], false⟩

/-- An instruction that cannot fail. -/
def acts (l : List Act) : Inst := ⟨l, none, false⟩

/-- A `drop` with no failure edge. -/
def dropAt (a : Nat) : Inst := ⟨[.drop a], none, true⟩

def jump (target : Nat) (args : List (Nat × Src) := []) : Term := .other [] [⟨target, args⟩]

/-- A branch on a value that is not a read of a flag slot. -/
def branch (yes no : Nat) : Term := .other [] [⟨yes, []⟩, ⟨no, []⟩]

def ret : Term := .other [] []

/-! ### `ownership_follows_a_joined_pointer_along_its_edge` (tests/own.rs)

`b2` moves the parameter's resource into `s0` and `b3` into `s1`; each gives `b4` a
pointer to its slot. Slots `s0`, `s1`; entry values `v0`, `v1`; `v4` is join 0, rooted
at 4. Addresses: 0 is `s0`, 1 is `v1`'s place, 2 is `s1` and 3 is `v4`'s place. -/

namespace Joined

inductive Kind where
  | loop
  | twice
  | after
deriving DecidableEq

def body (k : Kind) : Prog where
  slots := 2
  roots := 4
  views := [false, false]
  joins := [4]
  under := fun
    | 1 => [pl 3]
    | 4 => [pl 0, pl 1]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 3]
    | 2 => [pl 1]
    | 3 => [pl 0, pl 1]
    | _ => []
  base := fun
    | 1 => some (1, here)
    | 3 => some (4, here)
    | _ => none
  sole := fun _ => false
  tracked := if k = .after then [pl 0, pl 4] else [pl 0, pl 1, pl 4]
  cells :=
    if k = .after then [.pair (pl 0) none, .pair (pl 4) none, .bind (pl 0) (pl 4) 0 none]
    else [.pair (pl 0) none, .pair (pl 1) none, .pair (pl 4) none,
      .bind (pl 0) (pl 4) 0 none, .bind (pl 1) (pl 4) 1 none]
  borrows := []
  reads := []
  blocks := [
    ⟨if k = .after then [acts [.begin 0], acts [.use 1, .write 0]] else [], jump 1⟩,
    ⟨[], branch 2 3⟩,
    ⟨[acts [.begin 0], acts [.use 1, .write 0]], jump 4 [(4, .at (pl 0))]⟩,
    ⟨[acts [.begin 1], acts [.use 1, .write 2]], jump 4 [(4, .at (pl 1))]⟩,
    match k with
    | .loop => ⟨[dropAt 3], branch 1 5⟩
    | .twice => ⟨[dropAt 3, dropAt 3], branch 1 5⟩
    | .after => ⟨[dropAt 0, acts [.use 3, .write 1]], jump 5⟩,
    ⟨[], ret⟩]

end Joined

/-! ### `ownership_drops_a_joined_place_only_where_the_join_is_bound` (tests/own.rs)

`b0` fills `s1` and sets the flag `s2`; `b1` fills `s0` and gives `b3` a pointer to it;
`b2` gives `b3` a pointer to `s1`, clearing the flag when `cleared`. `b3` drops through
the join and `b4`, under the flag, drops `s1`. Slots `s0`, `s1`, `s2`; `v4` is join 0,
rooted at 5. Addresses: 0 is `s0`, 1 is `v1`'s place, 2 is `s1`, 3 is `v4`'s place and 4 is
`s2`. -/

namespace Bound

def body (cleared : Bool) : Prog where
  slots := 3
  roots := 5
  views := [false, false]
  joins := [4]
  under := fun
    | 1 => [pl 4]
    | 4 => [pl 0, pl 1]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 4]
    | 2 => [pl 1]
    | 3 => [pl 0, pl 1]
    | 4 => [pl 2]
    | _ => []
  base := fun
    | 1 => some (1, here)
    | 3 => some (4, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 0, pl 1, pl 5]
  cells := [.pair (pl 0) none, .pair (pl 1) none, .pair (pl 5) none, .pair (pl 1) (some 2),
    .bind (pl 0) (pl 5) 0 none, .bind (pl 1) (pl 5) 1 none, .bind (pl 1) (pl 5) 3 (some 2)]
  borrows := []
  reads := [2]
  blocks := [
    ⟨[acts [.begin 1], acts [.use 1, .write 2], acts [.begin 2, .flag 2 none],
      acts [.write 4, .flag 2 (some true)]], branch 1 2⟩,
    ⟨[acts [.begin 0], acts [.use 1, .write 0]], jump 3 [(4, .at (pl 0))]⟩,
    ⟨if cleared then [acts [.write 4, .flag 2 (some false)]] else [],
      jump 3 [(4, .at (pl 1))]⟩,
    ⟨[dropAt 3, acts [.use 4, .fresh 0]], .flagBranch 0 ⟨4, []⟩ ⟨5, []⟩⟩,
    ⟨[dropAt 2], jump 5⟩,
    ⟨[], ret⟩]

end Bound

/-! ### `storage_dead_leaves_a_dropped_place_dropped` (tests/own.rs)

`b1` fills `s0`, drops it, ends its storage, and loops; `again` drops it once more after
the `dead`. Slot `s0`; entry values `v0`, `v1`. Address 0 is `s0` and 1 is `v0`'s place. -/

namespace Dead

def body (again : Bool) : Prog where
  slots := 1
  roots := 3
  views := [false, false]
  joins := []
  under := fun
    | 0 => [pl 1]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 1]
    | _ => []
  base := fun
    | 1 => some (0, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 0]
  cells := [.pair (pl 0) none]
  borrows := []
  reads := []
  blocks := [
    ⟨[], jump 1⟩,
    ⟨[acts [.begin 0], acts [.use 1, .write 0], dropAt 0, acts [.dead 0]] ++
      (if again then [dropAt 0] else []), branch 1 2⟩,
    ⟨[], ret⟩]

end Dead

/-! ### `rewriting_an_anchor_ends_the_view_at_the_write` (tests/own.rs)

A call makes `s0` a view of `v0`'s place; a store to a field of that place ends the view.
`after` reads the view after the store, and otherwise before it. Slot `s0`; entry value
`v0`. Address 0 is `v0`'s place, 1 is `s0` and 2 is field 0 of `v0`'s place. -/

namespace Rewrite

def body (after : Bool) : Prog where
  slots := 1
  roots := 2
  views := [false]
  joins := []
  under := fun
    | 0 => [pl 1]
    | _ => []
  names := fun
    | 0 => [pl 1]
    | 1 => [pl 0]
    | 2 => [⟨1, [(0, 0, 0)], false⟩]
    | _ => []
  base := fun
    | 0 => some (0, here)
    | 2 => some (0, ⟨[(0, 0, 0)], false⟩)
    | _ => none
  sole := fun _ => false
  tracked := [pl 0]
  cells := [.pair (pl 0) none]
  borrows := [⟨[pl 0], [pl 1]⟩]
  reads := []
  blocks := [
    ⟨[acts [.begin 0], acts [.write 1, .use 0, .lend 0]] ++
      (if after then [acts [.write 2], acts [.use 1]] else [acts [.use 1], acts [.write 2]]),
      ret⟩]

end Rewrite

/-! ### `a_parameter_that_may_be_a_slot_or_a_heap_pointer_is_a_join` (tests/own.rs)

`b0` fills `s0` and drops it. `b1` gives `b3` a pointer to `s0` and `b2` a pointer a call
returned. `built` begins `s0` again in `b1`, and `b3` stores the two fields through the
parameter and drops through it. `revived` writes through the parameter and drops `s0`.
Slot `s0`; entry values `v0`, `v1`; `v5` is join 0, rooted at 3. Addresses: 0 is `s0`, 1
is `v0`'s place, 2 is `v5`'s place, and 3 and 4 are its fields 0 and 1. -/

namespace SlotOrHeap

def f0 : Path := ⟨[(0, 0, 0)], false⟩
def f1 : Path := ⟨[(0, 1, 0)], false⟩

def body (built : Bool) : Prog where
  slots := 1
  roots := 3
  views := [false, false]
  joins := [5]
  under := fun
    | 0 => [pl 1]
    | 5 => [pl 0]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 1]
    | 2 => [pl 0]
    | 3 => [(pl 0).app f0]
    | 4 => [(pl 0).app f1]
    | _ => []
  base := fun
    | 1 => some (0, here)
    | 2 => some (5, here)
    | 3 => some (5, f0)
    | 4 => some (5, f1)
    | _ => none
  sole := fun _ => false
  tracked := [pl 0, pl 3, (pl 3).app f0, (pl 3).app f1]
  cells := [.pair (pl 0) none, .pair (pl 3) none, .pair ((pl 3).app f0) none,
    .pair ((pl 3).app f1) none, .bind (pl 0) (pl 3) 0 none,
    .bind (pl 0) ((pl 3).app f0) 0 none, .bind (pl 0) ((pl 3).app f1) 0 none]
  borrows := []
  reads := []
  blocks := [
    ⟨[acts [.begin 0], acts [.use 1, .write 0], dropAt 0], branch 1 2⟩,
    ⟨if built then [acts [.begin 0]] else [], jump 3 [(5, .at (pl 0))]⟩,
    ⟨[], jump 3 [(5, .none)]⟩,
    ⟨if built then [acts [.write 3], acts [.write 4], dropAt 2]
      else [acts [.use 1, .write 2], dropAt 0], ret⟩]

end SlotOrHeap

/-! ### `an_edge_into_the_entry_block_keeps_each_pointer_parameter` (tests/own.rs)

`b0` moves `v3`'s resource into `v1`'s place; `b1` drops it and jumps back to `b0`; `b2`
drops `v2`'s place. `rebound` gives `v1` a pointer a call returned and `v2` the old `v1`
on the edge back. No slots; entry values `v0` to `v3`. Address 0 is `v1`'s place, 1 is
`v2`'s and 2 is `v3`'s. -/

namespace EntryEdge

def body (rebound : Bool) : Prog where
  slots := 0
  roots := 4
  views := [false, false, false, false]
  joins := []
  under := fun
    | 1 => [pl 1]
    | 2 => [pl 2]
    | 3 => [pl 3]
    | _ => []
  names := fun
    | 0 => [pl 1]
    | 1 => [pl 2]
    | 2 => [pl 3]
    | _ => []
  base := fun
    | 0 => some (1, here)
    | 1 => some (2, here)
    | 2 => some (3, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 1, pl 2]
  cells := [.pair (pl 1) none, .pair (pl 2) none]
  borrows := []
  reads := []
  blocks := [
    ⟨[acts [.use 2, .write 0]], branch 1 2⟩,
    ⟨[dropAt 0], jump 0
      (if rebound then [(1, .none), (2, .param 1), (3, .param 3)]
        else [(1, .param 1), (2, .param 2), (3, .param 3)])⟩,
    ⟨[dropAt 1], ret⟩]

end EntryEdge

/-! ### `a_join_keeps_its_places_through_pointer_and_through_another_join` (tests/own.rs)

`b0` fills `s0` and `s1` and drops `s1`. `b1` gives `b3` a pointer to `s1`; `b2` gives it
a pointer read from memory (`laundered`) or a pointer to `s0` (`forwarded`). `b3` gives
`b4` a `pointer` remade from its parameter (`laundered`) or the parameter itself.
`laundered` then writes through `b4`'s parameter and drops `s1`; `forwarded` reads through
it. Slots `s0`, `s1`; entry values `v0`, `v1`; `v4` is join 0, rooted at 4, and `v6` join
1, rooted at 5. Addresses: 0 is `s0`, 1 is `v1`'s place, 2 is `s1`, 3 is `v4`'s place, 4 is
`v6`'s place and 5 is `s0`. -/

namespace Through

def body (laundered : Bool) : Prog where
  slots := 2
  roots := 4
  views := [false, false]
  joins := [4, 6]
  under := fun
    | 1 => [pl 3]
    | 4 => if laundered then [pl 1] else [pl 1, pl 0]
    | 6 => if laundered then [pl 1] else [pl 1, pl 0]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 3]
    | 2 => [pl 1]
    | 3 => if laundered then [pl 1] else [pl 0]
    | 4 => if laundered then [pl 1] else [pl 1, pl 0]
    | 5 => [pl 0]
    | _ => []
  base := fun
    | 1 => some (1, here)
    | 3 => if laundered then some (4, here) else none
    | 4 => some (6, here)
    | _ => none
  sole := fun _ => false
  tracked := if laundered then [pl 1, pl 4, pl 5] else [pl 1, pl 5, pl 4]
  cells :=
    if laundered then [.pair (pl 1) none, .pair (pl 4) none, .pair (pl 5) none,
      .bind (pl 1) (pl 4) 0 none, .bind (pl 1) (pl 5) 0 none]
    else [.pair (pl 1) none, .pair (pl 5) none, .pair (pl 4) none,
      .bind (pl 1) (pl 5) 0 none, .bind (pl 1) (pl 4) 0 none]
  borrows := []
  reads := []
  blocks := [
    ⟨[acts [.begin 0], acts [.use 1, .write 0], acts [.begin 1], acts [.use 1, .write 2],
      dropAt 2], branch 1 2⟩,
    ⟨[], jump 3 [(4, .at (pl 1))]⟩,
    ⟨if laundered then [acts [.use 5]] else [],
      jump 3 [(4, if laundered then .none else .at (pl 0))]⟩,
    ⟨[], jump 4 [(6, if laundered then .ptr 4 here else .param 4)]⟩,
    ⟨if laundered then [acts [.use 1, .write 4], dropAt 2] else [acts [.use 4, .write 1]],
      ret⟩]

end Through

/-! ### `a_view_reached_through_a_join_ends_with_its_anchor` (tests/own.rs)

`b0` fills `s0`, makes `s1` a view of it by a call, and fills `s2`; `b3`'s parameter
points at `s1` or at `s2`. `b3` drops `s0` and reads through its parameter, after the
drop (`after`) or before it. Slots `s0`, `s1`, `s2`; entry values `v0`, `v1`; `v4` is join
0, rooted at 5. Addresses: 0 is `s0`, 1 is `v1`'s place, 2 is `s1`, 3 is `s2` and 4 is
`v4`'s place. -/

namespace ViewJoin

def body (after : Bool) : Prog where
  slots := 3
  roots := 5
  views := [false, false]
  joins := [4]
  under := fun
    | 1 => [pl 4]
    | 4 => [pl 1, pl 2]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 4]
    | 2 => [pl 1]
    | 3 => [pl 2]
    | 4 => [pl 1, pl 2]
    | _ => []
  base := fun
    | 1 => some (1, here)
    | 4 => some (4, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 0, pl 1, pl 5]
  cells := [.pair (pl 0) none, .pair (pl 1) none, .pair (pl 5) none,
    .bind (pl 1) (pl 5) 1 none]
  borrows := [⟨[pl 1], [pl 0]⟩]
  reads := []
  blocks := [
    ⟨[acts [.begin 0], acts [.use 1, .write 0], acts [.begin 1],
      acts [.write 2, .use 0, .lend 0], acts [.begin 2], acts [.use 1, .write 3]],
      branch 1 2⟩,
    ⟨[], jump 3 [(4, .at (pl 1))]⟩,
    ⟨[], jump 3 [(4, .at (pl 2))]⟩,
    ⟨if after then [dropAt 0, acts [.use 4, .write 1]]
      else [acts [.use 4, .write 1], dropAt 0], ret⟩]

end ViewJoin

/-! ### `a_drop_through_a_join_follows_the_flag_of_the_join_moved_into_it` (tests/own.rs)

`b3`'s parameter is `s0` or `s1`, filled on its edge; `b3` sets the flag `s3`. `b4` gives
`b6` that parameter, clearing the flag when `moved`; `b5` fills `s2` and gives `b6` a
pointer to it. `b6` drops through its parameter and `b7`, under the flag, through `b3`'s.
Slots `s0` to `s3`; entry values `v0`, `v1`; `v4` is join 0, rooted at 6, and `v6` join 1,
rooted at 7. Addresses: 0 is `s0`, 1 is `v1`'s place, 2 is `s1`, 3 is `s2`, 4 is `v4`'s
place, 5 is `v6`'s place and 6 is `s3`. -/

namespace Moved

def body (moved : Bool) : Prog where
  slots := 4
  roots := 6
  views := [false, false]
  joins := [4, 6]
  under := fun
    | 1 => [pl 5]
    | 4 => [pl 0, pl 1]
    | 6 => [pl 0, pl 1, pl 2]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 5]
    | 2 => [pl 1]
    | 3 => [pl 2]
    | 4 => [pl 0, pl 1]
    | 5 => [pl 0, pl 1, pl 2]
    | 6 => [pl 3]
    | _ => []
  base := fun
    | 1 => some (1, here)
    | 4 => some (4, here)
    | 5 => some (6, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 0, pl 1, pl 2, pl 7, pl 6]
  cells := [.pair (pl 0) none, .pair (pl 1) none, .pair (pl 2) none, .pair (pl 7) none,
    .pair (pl 6) none, .pair (pl 6) (some 3), .pair (pl 0) (some 3), .pair (pl 1) (some 3),
    .bind (pl 0) (pl 7) 0 none, .bind (pl 0) (pl 7) 6 (some 3),
    .bind (pl 0) (pl 6) 0 none, .bind (pl 0) (pl 6) 6 (some 3),
    .bind (pl 1) (pl 7) 1 none, .bind (pl 1) (pl 7) 7 (some 3),
    .bind (pl 1) (pl 6) 1 none, .bind (pl 1) (pl 6) 7 (some 3),
    .bind (pl 2) (pl 7) 2 none]
  borrows := []
  reads := [3]
  blocks := [
    ⟨[], branch 1 2⟩,
    ⟨[acts [.begin 0], acts [.use 1, .write 0]], jump 3 [(4, .at (pl 0))]⟩,
    ⟨[acts [.begin 1], acts [.use 1, .write 2]], jump 3 [(4, .at (pl 1))]⟩,
    ⟨[acts [.begin 3, .flag 3 none], acts [.write 6, .flag 3 (some true)]], branch 4 5⟩,
    ⟨if moved then [acts [.write 6, .flag 3 (some false)]] else [], jump 6 [(6, .param 4)]⟩,
    ⟨[acts [.begin 2], acts [.use 1, .write 3]], jump 6 [(6, .at (pl 2))]⟩,
    ⟨[dropAt 5, acts [.use 6, .fresh 0]], .flagBranch 0 ⟨7, []⟩ ⟨8, []⟩⟩,
    ⟨[dropAt 4], jump 8⟩,
    ⟨[], ret⟩]

end Moved

/-! ### `ownership_rejects_a_drop_repeated_on_its_failure_edge` (tests/own.rs)

`b0` drops `v1`'s place with failure edge `b1`; `b1` drops it again when `again`. -/

namespace FailEdge

def body (again : Bool) : Prog where
  slots := 0
  roots := 2
  views := [false, false]
  joins := []
  under := fun
    | 1 => [pl 1]
    | _ => []
  names := fun
    | 0 => [pl 1]
    | _ => []
  base := fun
    | 0 => some (1, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 1]
  cells := [.pair (pl 1) none]
  borrows := []
  reads := []
  blocks := [
    ⟨[⟨[.drop 0], some 1, true⟩], ret⟩,
    ⟨if again then [dropAt 0] else [], ret⟩]

end FailEdge

/-! ### `ownership_rejects_a_view_used_after_its_anchor_is_dropped` (tests/own.rs)

`b1` fills `s0`, calls with dest `s1` and argument `s0`, and drops `s0`; `b5` then uses
`s1`. When the call's dest views its argument (`view`), the call lends and the use is of a
dangling view. -/

namespace ViewAnchor

def body (view : Bool) : Prog where
  slots := 2
  roots := 2
  views := []
  joins := []
  under := fun _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 1]
    | 2 => [pl 0]
    | 3 => [pl 0]
    | 4 => [pl 0]
    | 5 => [pl 1]
    | _ => []
  base := fun _ => none
  sole := fun _ => false
  tracked := if view then [pl 0, pl 1] else [pl 0]
  cells := if view then [.pair (pl 0) none, .pair (pl 1) none] else [.pair (pl 0) none]
  borrows := if view then [⟨[pl 1], [pl 0]⟩] else []
  reads := []
  blocks := [
    ⟨[], jump 1⟩,
    ⟨[acts [.begin 0], acts [.write 0], ⟨[], some 2, false⟩, acts [.begin 1],
      ⟨[.write 1, .use 2] ++ (if view then [.lend 0] else []), some 2, false⟩,
      ⟨[.drop 3], some 4, true⟩], jump 5⟩,
    ⟨[⟨[.drop 4], some 3, true⟩], jump 3⟩,
    ⟨[], ret⟩,
    ⟨[], ret⟩,
    ⟨[acts [.use 5]], ret⟩]

end ViewAnchor

/-! ### `ownership_rejects_dead_anchor_and_escaping_local_returns` (tests/own.rs), its first body

`b0` fills `s0`, makes `s1` a view of it by a call, ends the storage of `s0`, and gives
`b1` a pointer to `s1`; `b1` reads through it. Slots `s0`, `s1`; no entry values; `v3` is
`b1`'s parameter. Addresses: 0 and 2 are `s0`, 1 is `s1` and 3 is `v3`'s place. -/

namespace DeadAnchor

def body : Prog where
  slots := 2
  roots := 2
  views := []
  joins := []
  under := fun
    | 3 => [pl 1]
    | _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 1]
    | 2 => [pl 0]
    | 3 => [pl 1]
    | _ => []
  base := fun
    | 3 => some (3, here)
    | _ => none
  sole := fun _ => false
  tracked := [pl 1]
  cells := [.pair (pl 1) none]
  borrows := [⟨[pl 1], [pl 0]⟩]
  reads := []
  blocks := [
    ⟨[acts [.begin 0], acts [.write 0], acts [.begin 1], acts [.write 1, .use 2, .lend 0],
      acts [.dead 0]], jump 1 [(3, .at (pl 1))]⟩,
    ⟨[acts [.use 3]], ret⟩]

end DeadAnchor

/-! ### `ownership_rejects_entry_backedge_stale_flag_nonstore_flag_and_partial_write` (tests/own.rs)

Four bodies that drop `s0` twice: by an edge into the entry block, a branch on a stale
flag read, a `copy` into a flag's slot, and a write of one field. The join-free proof
assumed each away by a hypothesis; the model now rejects each. -/

/-- `b0` drops `s0` and jumps to `b0`: the edge into the entry block carries the drop. -/
def loopBack : Prog where
  slots := 1
  roots := 1
  views := []
  joins := []
  under := fun _ => []
  names := fun
    | 0 => [pl 0]
    | _ => []
  base := fun _ => none
  sole := fun _ => false
  tracked := [pl 0]
  cells := [.pair (pl 0) none]
  borrows := []
  reads := []
  blocks := [⟨[dropAt 0], jump 0⟩]

/-- `s0` drops under the flag `s1`. `b0` reads the flag, drops `s0`, then stores false to
the flag (`stale`) or writes the flag's slot by a `copy` (otherwise, after a store of
false before the drop and with the read last), and branches on the read. -/
def flagged (stale : Bool) : Prog where
  slots := 2
  roots := 2
  views := []
  joins := []
  under := fun _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [pl 1]
    | _ => []
  base := fun _ => none
  sole := fun _ => false
  tracked := [pl 0]
  cells := [.pair (pl 0) none, .pair (pl 0) (some 1)]
  borrows := []
  reads := [1]
  blocks := [
    ⟨if stale then
        [acts [.use 1, .fresh 0], dropAt 0, acts [.write 1, .flag 1 (some false)]]
      else
        [acts [.write 1, .flag 1 (some false)], dropAt 0, acts [.write 1],
          acts [.use 1, .fresh 0]],
      .flagBranch 0 ⟨1, []⟩ ⟨2, []⟩⟩,
    ⟨[dropAt 0], ret⟩,
    ⟨[], ret⟩]

/-- `b0` drops `s0`, writes its field 0, and drops `s0` again. The write is of all of `s0`
only when `s0` is a record of that one field and no resource (`sole`). -/
def partialWrite (sole : Bool) : Prog where
  slots := 1
  roots := 1
  views := []
  joins := []
  under := fun _ => []
  names := fun
    | 0 => [pl 0]
    | 1 => [⟨0, [(0, 0, 0)], false⟩]
    | _ => []
  base := fun _ => none
  sole := fun t => sole && t == pl 0
  tracked := [pl 0]
  cells := [.pair (pl 0) none]
  borrows := []
  reads := []
  blocks := [⟨[dropAt 0, acts [.write 1], dropAt 0], ret⟩]

end Hirsel.CoreIR.Own
