/-!
# The model: places, the tables `check` builds, the analysis and the machine

`OwnSoundness.lean` says what is proved of this model, and `README.md` maps each
definition to the Rust it models and lists what the model leaves out.
-/

namespace Hirsel.CoreIR.Own

/-! ## Places (`own_names.rs`) -/

/-- One static step, as `own_names.rs` `Steps` holds it: `(0, field, 0)`,
`(1, case, field)` or `(2, seq word, 0)`. -/
abbrev PStep := Nat × Nat × Nat

/-- `own_names.rs` `Pl`: a root (a slot, an entry value's place at `slots + k`, or a join's
root at `roots + k`), its static steps, and `cut` when a run-time offset stands for some
part of the place. -/
structure Pl where
  root : Nat
  steps : List PStep
  cut : Bool
deriving DecidableEq, Repr

/-- `own_names.rs` `overlap`: one root, and one's steps a prefix of the other's. -/
def overlap (a b : Pl) : Bool :=
  a.root == b.root && (b.steps.isPrefixOf a.steps || a.steps.isPrefixOf b.steps)

/-- `p` is `r` or a part of it. -/
def inside (r p : Pl) : Bool :=
  r.root == p.root && r.steps.isPrefixOf p.steps

/-- The static steps of an address from the pointer it is based on, or of a join place
from its root; `cut` when a run-time offset follows them. -/
structure Path where
  steps : List PStep
  cut : Bool
deriving DecidableEq, Repr

/-- `x` with the steps of `r` after its own, as `expand` and `transfer` build a place. -/
def Pl.app (x : Pl) (r : Path) : Pl := ⟨x.root, x.steps ++ r.steps, x.cut || r.cut⟩

/-- `own_names.rs` `step` along `r`: a place under a run-time offset takes no more
steps. -/
def Pl.step (x : Pl) (r : Path) : Pl := if x.cut then x else x.app r

def Pl.path (t : Pl) : Path := ⟨t.steps, t.cut⟩

/-! ## Pairing states (`own.rs` `halves`, `dropped`, `begun`, `flag`)

A state is the set of reachable (dropped, flag) pairs: `zt` is `own.rs` `ZT` (not dropped,
flag true), `zf` is `ZF`, `dt` is `DT` and `df` is `DF`. A bind state reads `D` as "the
join may be bound to the place" and `Z` as "it may be bound elsewhere". -/

structure Bits where
  zt : Bool
  zf : Bool
  dt : Bool
  df : Bool
deriving DecidableEq, Repr

namespace Bits

/-- `own.rs` `Z`: not dropped, under either flag value. The entry state. -/
def Z : Bits := ⟨true, true, false, false⟩

def zero : Bits := ⟨false, false, false, false⟩

/-- `own.rs` `halves`: whether the half for flag value `b` is reachable. -/
def half (s : Bits) (b : Bool) : Bool := if b then s.zt || s.dt else s.zf || s.df

/-- The dropped bit of the half for flag value `b`. -/
def d (s : Bits) (b : Bool) : Bool := if b then s.dt else s.df

/-- `s & D != 0`. -/
def anyD (s : Bits) : Bool := s.dt || s.df

/-- `s & Z != 0`. -/
def anyZ (s : Bits) : Bool := s.zt || s.zf

def isZero (s : Bits) : Bool := !(s.zt || s.zf || s.dt || s.df)

def or (s t : Bits) : Bits := ⟨s.zt || t.zt, s.zf || t.zf, s.dt || t.dt, s.df || t.df⟩

/-- `dropped(s, true)`: dropped in each reachable half. -/
def drops (s : Bits) : Bits := ⟨false, false, s.half true, s.half false⟩

/-- `own.rs` `begun`: not dropped in each reachable half. -/
def begun (s : Bits) : Bits := ⟨s.half true, s.half false, false, false⟩

/-- `own.rs` `flag`: a store of the flag moves both of its states to the stored half. -/
def flag (s : Bits) (v : Option Bool) : Bits :=
  let z := s.zt || s.zf
  let dd := s.dt || s.df
  match v with
  | some true => ⟨z, false, dd, false⟩
  | some false => ⟨false, z, false, dd⟩
  | none => ⟨z, z, dd, dd⟩

/-- `s & T` or `s & F`: the pairs of one edge of a branch on the flag (`own_flow.rs`
`solve`). -/
def keep (s : Bits) (b : Bool) : Bits :=
  if b then ⟨s.zt, false, s.dt, false⟩ else ⟨false, s.zf, false, s.df⟩

def le (s t : Bits) : Bool :=
  (!s.zt || t.zt) && (!s.zf || t.zf) && (!s.dt || t.dt) && (!s.df || t.df)

end Bits

/-! ## Programs: the acts of a body and the tables `check` builds -/

/-- `own.rs` `Act`, by address. `CopyToDest` is left out: it feeds only the return checks,
which the model does not have. -/
inductive Act where
  | use (a : Nat)
  | drop (a : Nat)
  | write (a : Nat)
  | invalidate (a : Nat)
  | begin (slot : Nat)
  | dead (slot : Nat)
  | flag (f : Nat) (v : Option Bool)
  | fresh (ri : Nat)
  | lend (k : Nat)
deriving DecidableEq, Repr

/-- An instruction: its acts (`own.rs` `acts`), the failure block it names, and whether it
is a `drop` (whose failure edge leaves after its acts, `own_flow.rs` `solve`). -/
structure Inst where
  acts : List Act
  fail : Option Nat
  isDrop : Bool

/-- What an edge gives a pointer block parameter. -/
inductive Src where
  /-- A pointer block parameter or entry value, as it is. -/
  | param (u : Nat)
  /-- A `pointer` of an address `rel` from parameter `u`. -/
  | ptr (u : Nat) (rel : Path)
  /-- A `pointer` of an address into a slot. -/
  | at (x : Pl)
  /-- A pointer that names nothing: read from memory, a call's result. -/
  | none
deriving DecidableEq, Repr

/-- An edge: its target and, for each pointer parameter of the target, its argument
(`own_names.rs` `ptr_args`). A failure edge and a terminator's other successors have no
arguments. -/
structure Edge where
  target : Nat
  args : List (Nat × Src)

/-- A terminator, as `solve` reads it: a branch on flag read `ri`, or anything else, with
the acts it applies first (`raise fail a` uses `a`) and its edges. -/
inductive Term where
  | flagBranch (ri : Nat) (yes no : Edge)
  | other (pre : List Act) (edges : List Edge)

def Term.edges : Term → List Edge
  | .flagBranch _ yes no => [yes, no]
  | .other _ es => es

structure Block where
  insts : List Inst
  term : Term

/-- A call whose dest views its anchors (OWN-20): the dest's tracked places (`own.rs`
`borrows[k]`) and the anchor arguments' places. -/
structure Borrow where
  dest : List Pl
  anchor : List Pl

/-- One state index of `now` below `base`: a pairing of tracked place `q` (in `pairs[q]`,
and in `guards[g]` when `g` is a flag slot), or the bind state of `binds[q]`'s entry
`(t, p, _)`, which `check` puts in the guard list of its pairing `p`. -/
inductive Cell where
  | pair (q : Pl) (g : Option Nat)
  | bind (q t : Pl) (p : Nat) (g : Option Nat)
deriving DecidableEq, Repr

/-- A body as `check` sees it.

* `roots` is the first join root and `joins[k]` the parameter of join `k`.
* `under v` is `n.vals[v]` for a pointer block parameter or entry value `v`: the places
  it may point at.
* `names a` is `n.addrs[a]`. `base a` is the parameter `a` is based on, through any
  `pointer`s, and the path from it; `none` for an address into a slot. The `deref` read
  of an address is an address of its own here.
* `sole t` says the place `t` is a single-field record that is not a resource
  (`own_names.rs` `covers`).
* `cells` are the state indices of `now` below `base`; borrow `k`'s state is `lend k`, and
  flag read `ri`'s stale bit is `stale ri`, for flag slot `reads[ri]`. -/
structure Prog where
  slots : Nat
  roots : Nat
  views : List Bool
  joins : List Nat
  under : Nat → List Pl
  names : Nat → List Pl
  base : Nat → Option (Nat × Path)
  sole : Pl → Bool
  tracked : List Pl
  cells : List Cell
  borrows : List Borrow
  reads : List Nat
  blocks : List Block

/-- The one place a parameter names, not under a run-time offset. -/
def one : List Pl → Option Pl
  | x :: rest => if !x.cut && rest.all (· == x) then some x else none
  | [] => none

namespace Prog

/-- `own.rs` `Act::Drop`'s first check: the root is a `view` parameter's pointer. -/
def isView (P : Prog) (root : Nat) : Bool :=
  decide (P.slots ≤ root) && P.views.getD (root - P.slots) false

/-- `joins.iter().position(..)`. -/
def joinOf (P : Prog) (v : Nat) : Option Nat := P.joins.findIdx? (· == v)

/-- The parameter of the join a place is rooted at. -/
def paramOf (P : Prog) (t : Pl) : Option Nat :=
  if t.root < P.roots then none else P.joins[t.root - P.roots]?

/-- Join `k`'s root. -/
def jroot (P : Prog) (k : Nat) : Pl := ⟨P.roots + k, [], false⟩

/-- `j.addrs[a]`: the join place an address based on a join's parameter names. -/
def jname (P : Prog) (a : Nat) : Option Pl :=
  match P.base a with
  | some (v, rel) => (P.joinOf v).map fun k => (P.jroot k).app rel
  | none => none

/-- `j.addrs[a]` when `routed[a]`, else `n.addrs[a]`. -/
def places (P : Prog) (a : Nat) : List Pl :=
  match P.jname a with
  | some r => [r]
  | none => P.names a

/-- `own.rs` `hits[a]`: the tracked places an address overlaps. -/
def hits (P : Prog) (a : Nat) : List Pl :=
  P.tracked.filter fun q => (P.places a).any fun r => overlap r q

/-- `check`'s `expand`: the places a join place stands for. -/
def expand (P : Prog) (t : Pl) : List Pl :=
  match P.paramOf t with
  | some v => (P.under v).map fun x => x.app t.path
  | none => []

/-- `own.rs` `shadow[q]`: the tracked join places that may be `q`, in part. -/
def shadow (P : Prog) (q : Pl) : List Pl :=
  P.tracked.filter fun t => (P.expand t).any fun x => overlap x q

def owner (P : Prog) (x : Nat) : Option Pl :=
  match P.cells[x]? with
  | some (.pair q _) => some q
  | _ => none

def guard (P : Prog) (x : Nat) : Option Nat :=
  match P.cells[x]? with
  | some (.pair _ g) => g
  | some (.bind _ _ _ g) => g
  | none => none

/-- `pairs[q]`. -/
def pairsOf (P : Prog) (q : Pl) : List Nat :=
  (List.range P.cells.length).filter fun x => P.owner x == some q

/-- `k ∈ lent[q]`: borrow `b` is anchored on a place overlapping `q`. -/
def lent (P : Prog) (q : Pl) (b : Borrow) : Bool :=
  decide (q.root < P.roots) && b.anchor.any fun f => overlap f q

/-- `own.rs` `enqueue`: the dests of the borrows in force among those `seed` picks. -/
def seeds (P : Prog) (on : Nat → Bool) (seed : Borrow → Bool) : List Pl :=
  P.borrows.zipIdx.flatMap fun bk => if on bk.2 && seed bk.1 then bk.1.dest else []

/-- One step of `invalidate`'s walk from `q`: `enqueue(lent[q])`. -/
def succs (P : Prog) (on : Nat → Bool) (q : Pl) : List Pl := P.seeds on (P.lent q)

/-- The sole-field chain of `covers`: each further step is field 0 of a single-field
record that is not a resource. -/
def soleChain (P : Prog) (root : Nat) : List PStep → List PStep → Bool
  | _, [] => true
  | at_, st :: rest =>
    st == (0, 0, 0) && P.sole ⟨root, at_, false⟩ && soleChain P root (at_ ++ [st]) rest

/-- `own_names.rs` `covers`: writing `w` overwrites all of `t`. -/
def covers (P : Prog) (w t : Pl) : Bool :=
  !w.cut && w.root == t.root &&
    (w.steps.isPrefixOf t.steps ||
      (t.steps.isPrefixOf w.steps && decide (t.root < P.slots) &&
        P.soleChain t.root t.steps (w.steps.drop t.steps.length)))

/-- A write of `y` gives `p` a new value: `p` is in a place the write covers. -/
def revives (P : Prog) (y p : Pl) : Bool :=
  (List.range (y.steps.length + 1)).any fun n =>
    let q : Pl := ⟨y.root, y.steps.take n, false⟩
    P.covers y q && inside q p

/-- What an edge argument names: `n.vals` of the argument. -/
def srcNames (P : Prog) : Src → List Pl
  | .param u => P.under u
  | .ptr u rel => (P.under u).map fun x => x.step rel
  | .at x => [x]
  | .none => []

/-- `transfer`'s `srcs`: a join's parameter stands for its root, any other argument for
the places it names. -/
def srcs (P : Prog) : Src → List Pl
  | .param u =>
    match P.joinOf u with
    | some i => [P.jroot i]
    | none => P.under u
  | s => P.srcNames s

def allActs (P : Prog) : List Act :=
  P.blocks.flatMap fun b =>
    b.insts.flatMap (fun i => i.acts) ++
      match b.term with
      | .other pre _ => pre
      | .flagBranch .. => []

def allEdges (P : Prog) : List Edge := P.blocks.flatMap fun b => b.term.edges

/-- The addresses the body's acts go through. -/
def addrs (P : Prog) : List Nat :=
  P.allActs.filterMap fun
    | .use a | .drop a | .write a | .invalidate a => some a
    | _ => none

/-- An edge argument that names a place and is not made from a join: not `mixed` in
`own_names.rs` `names`. -/
def named (P : Prog) : Src → Bool
  | .param u | .ptr u _ => (P.joinOf u).isNone && !(P.under u).isEmpty
  | .at _ => true
  | .none => false

/-- A drop's names, by slot and by join, are tracked unless under a run-time offset
(`check`), and `live` and `dead` name slots. -/
def wfAct (P : Prog) : Act → Bool
  | .drop a => (P.names a ++ (P.jname a).toList).all fun r => r.cut || P.tracked.contains r
  | .begin s | .dead s => decide (s < P.slots)
  | _ => true

/-- An address names places below the join roots. One based on a parameter names that
path from each place the parameter may point at, and names one place only when the
parameter does (`own_names.rs` `spread`). The join place it reaches is tracked when a
place that stands for overlaps a tracked one (`check`). -/
def wfAddr (P : Prog) (a : Nat) : Bool :=
  ((P.names a).all fun r => decide (r.root < P.roots)) &&
  (match P.base a with
    | some (v, rel) =>
      ((P.under v).all fun x => x.cut || (P.names a).contains (x.app rel)) &&
      ((P.joinOf v).isSome ||
        match P.names a with
        | [r] => r.cut || (one (P.under v)).isSome
        | _ => true)
    | none => true) &&
  (match P.jname a with
    | some r =>
      !((P.expand r).any fun x =>
          P.tracked.any fun t => decide (t.root < P.roots) && overlap x t) ||
        P.tracked.contains { r with cut := false }
    | none => true)

/-- An edge's argument for parameter `w`.

* What it names, `w` names (`spread` along the edge).
* A join given to the join `w` tracks every place `w` does (`check`).
* The join rule of `own_names.rs` `names`: when `w` names one place and is not a join,
  the argument names a place and is not made from a join. `names` makes every other
  parameter of a later block a join, and takes no edge that gives an entry parameter
  anything but itself. -/
def wfArg (P : Prog) (w : Nat) (src : Src) : Bool :=
  ((P.srcNames src).all fun x => (P.under w).contains x) &&
  (match src with
    | .param u =>
      (match P.joinOf u with
        | some i => P.tracked.all fun t =>
            P.paramOf t != some w || P.tracked.contains ⟨P.roots + i, t.steps, t.cut⟩
        | none => true)
    | _ => true) &&
  (!((P.joinOf w).isNone && (one (P.under w)).isSome) || P.named src)

/-- A borrow's dests are tracked, and one with a single dest has it below the join
roots. -/
def wfBorrow (P : Prog) (b : Borrow) : Bool :=
  (b.dest.all fun q => P.tracked.contains q) &&
  match b.dest with
  | [d] => decide (d.root < P.roots)
  | _ => true

/-- Every pairing of a tracked place has a bind state for each join place in its shadow,
under that pairing's flag, and flags are slots. -/
def wfCells (P : Prog) : Bool :=
  (P.tracked.all fun q => (P.shadow q).all fun t => (P.pairsOf q).all fun x =>
    P.cells.contains (.bind q t x (P.guard x))) &&
  (P.cells.all fun
    | .bind q _ p g => P.owner p == some q && P.guard p == g
    | .pair .. => true) &&
  ((List.range P.cells.length).all fun x =>
    match P.guard x with
    | some g => decide (g < P.slots)
    | none => true) &&
  P.reads.all fun f => decide (f < P.slots)

/-- The tables are built as `check` and `names` build them. -/
def wf (P : Prog) : Bool :=
  decide (P.slots ≤ P.roots) &&
  (P.joins.all fun v => (P.under v).all fun x => decide (x.root < P.roots)) &&
  P.allActs.all P.wfAct &&
  P.addrs.all P.wfAddr &&
  P.borrows.all P.wfBorrow &&
  (P.allEdges.all fun e => e.args.all fun ws => P.wfArg ws.1 ws.2) &&
  P.wfCells

end Prog

/-! ## The analysis (`own.rs` `apply`, `own_flow.rs` `solve`, `transfer`) -/

/-- The analysis state: `bits x` is `now[x]` for cell `x`, `lend k` is `now[base + k] != 0`
and `stale ri` is `now[stale_base + ri] != 0`. -/
structure AState where
  bits : Nat → Bits
  lend : Nat → Bool
  stale : Nat → Bool

/-- `solve`'s `init`: every cell `Z`, no borrow in force, every flag read stale. -/
def AState.init : AState := ⟨fun _ => Bits.Z, fun _ => false, fun _ => true⟩

def AState.le (P : Prog) (s t : AState) : Bool :=
  ((List.range P.cells.length).all fun x => (s.bits x).le (t.bits x)) &&
  ((List.range P.borrows.length).all fun k => !s.lend k || t.lend k) &&
  (List.range P.reads.length).all fun ri => !s.stale ri || t.stale ri

/-- `own.rs` `may_drop`: every pairing of `q` has a dropped state. -/
def mayDrop (P : Prog) (s : AState) (q : Pl) : Bool :=
  (List.range P.cells.length).all fun x => P.owner x != some q || (s.bits x).anyD

/-- `invalidate`'s walk with its `seen` list: the places reached from `q` through the
borrows in force. `fuel` bounds the depth; `P.tracked.length + 1` is never exhausted
(`dfs_spec`). -/
def dfs (P : Prog) (on : Nat → Bool) : Nat → Pl → List Pl → List Pl
  | 0, _, seen => seen
  | fuel + 1, q, seen =>
    if seen.contains q then seen
    else (P.succs on q).foldl (fun acc d => dfs P on fuel d acc) (q :: seen)

/-- The dests `invalidate(loans)` reaches, `seed` saying which borrows `loans` holds. -/
def reach (P : Prog) (on : Nat → Bool) (seed : Borrow → Bool) : List Pl :=
  (P.seeds on seed).foldl (fun acc d => dfs P on (P.tracked.length + 1) d acc) []

/-- `own.rs` `mark(q, f, strong)` for every `q` in `Q`. -/
def mark (P : Prog) (s : AState) (Q : List Pl) (f : Bits → Bits) (strong : Bool) : AState :=
  { s with bits := fun x =>
      match P.owner x with
      | some q =>
        if Q.contains q then (if strong then f (s.bits x) else (s.bits x).or (f (s.bits x)))
        else s.bits x
      | none => s.bits x }

/-- `own.rs` `invalidate`: each dest reached, and each join place that may be it, may be
dropped. -/
def invalidate (P : Prog) (s : AState) (seed : Borrow → Bool) : AState :=
  let R := reach P s.lend seed
  mark P s (R ++ R.flatMap P.shadow) Bits.drops false

/-- `own.rs` `drop`. -/
def dropPlace (P : Prog) (s : AState) (q : Pl) (strong : Bool) : AState :=
  invalidate P (mark P s [q] Bits.drops strong) (P.lent q)

/-- Whether a join place of `own` may be bound to `q`, in the half for `b` of `q`'s
pairing `x`: the `D` bit of `bound` in `bound_drop`. -/
def bindD (P : Prog) (s : AState) (q : Pl) (x : Nat) (own : List Pl) (b : Bool) : Bool :=
  (List.range P.cells.length).any fun y =>
    match P.cells[y]? with
    | some (.bind q' t p _) => q' == q && p == x && own.contains t && (s.bits y).d b
    | _ => false

/-- `own_flow.rs` `bound_drop`. -/
def boundDrop (P : Prog) (s : AState) (q : Pl) (x : Nat) (own : List Pl) : Bits :=
  ⟨false, false, (s.bits x).half true && bindD P s q x own true,
    (s.bits x).half false && bindD P s q x own false⟩

/-- `own_flow.rs` `drop_bound`. -/
def dropBound (P : Prog) (s : AState) (q : Pl) (own : List Pl) : AState :=
  let s' : AState :=
    { s with bits := fun x =>
        if P.owner x == some q then (s.bits x).or (boundDrop P s q x own) else s.bits x }
  if (P.pairsOf q).any (fun x => (boundDrop P s q x own).anyD) then invalidate P s' (P.lent q)
  else s'

/-- `own_flow.rs` `drop_shadow`'s `same`: the pairing of `q` under the flag of pairing
`x`. -/
def Prog.same (P : Prog) (q : Pl) (x : Nat) : Option Nat :=
  (P.guard x).bind fun g => (P.pairsOf q).find? fun p => P.guard p == some g

/-- What `drop_shadow` adds to pairing `x` of a join place that may be `q`. -/
def shadowDrop (P : Prog) (s : AState) (q : Pl) (own : List Pl) (x : Nat) : Bits :=
  let d := (s.bits x).drops
  match P.same q x with
  | some p =>
    let bd := boundDrop P s q p own
    ⟨false, false, d.dt && bd.dt, d.df && bd.df⟩
  | none => d

/-- `own_flow.rs` `drop_shadow`. -/
def dropShadow (P : Prog) (s : AState) (q t : Pl) (own : List Pl) : AState :=
  { s with bits := fun x =>
      if P.owner x == some t then (s.bits x).or (shadowDrop P s q own x) else s.bits x }

/-- The tracked join place a drop through `a` is of. -/
def Prog.own (P : Prog) (a : Nat) : List Pl :=
  match P.jname a with
  | some r => if !r.cut && P.tracked.contains r then [r] else []
  | none => []

/-- `own.rs` `Act::Drop`, for one name `r` of the address. -/
def dropName (P : Prog) (own : List Pl) (routed strong : Bool) (s : AState) (r : Pl) :
    AState :=
  if !r.cut && P.tracked.contains r then
    let bound := routed && P.cells.any fun c =>
      match c with
      | .bind q t _ _ => q == r && own.contains t
      | .pair .. => false
    let s := if bound then dropBound P s r own else dropPlace P s r strong
    ((P.shadow r).filter fun t => !own.any fun o => overlap o t).foldl (fun s t =>
      if bound then dropShadow P s r t own else dropPlace P s t false) s
  else s

/-- `own.rs` `Act::Drop`. -/
def applyDrop (P : Prog) (s : AState) (a : Nat) : AState :=
  let routed := (P.jname a).isSome
  let s := (P.own a).foldl (fun s t => dropPlace P s t true) s
  (P.names a).foldl (dropName P (P.own a) routed (!routed && (P.names a).length == 1)) s

/-- `own.rs` `end`, for every place of `Q`: a new value in a place ends the borrows whose
one dest it is. -/
def endAll (P : Prog) (lend : Nat → Bool) (Q : List Pl) : Nat → Bool := fun k =>
  lend k && !(match P.borrows[k]? with
    | some b =>
      (match b.dest with
        | [d] => Q.contains d
        | _ => false)
    | none => false)

/-- `end(q); mark(q, f, true)`, and `mark(t, f, false)` for each `t` in `shadow[q]`, for
every `q` in `Q`. -/
def renew (P : Prog) (s : AState) (Q : List Pl) (f : Bits → Bits) : AState :=
  { bits := fun x =>
      match P.owner x with
      | some q =>
        if Q.contains q then f (s.bits x)
        else if (Q.flatMap P.shadow).contains q then (s.bits x).or (f (s.bits x))
        else s.bits x
      | none => s.bits x
    lend := endAll P s.lend Q
    stale := s.stale }

/-- `rewrites[a]`: the borrows a write through `a` rewrites an anchor of. -/
def rewrites (P : Prog) (a : Nat) (b : Borrow) : Bool :=
  b.anchor.any fun f => (P.names a).any fun r => overlap f r

/-- `anchor_slots[slot]`. -/
def anchored (slot : Nat) (b : Borrow) : Bool := b.anchor.any fun f => f.root == slot

/-- A flag slot a write through `a` may touch. -/
def touches (P : Prog) (a f : Nat) : Bool :=
  decide (f < P.slots) && (P.names a).any fun r => r.root == f

/-- `own.rs` `Act::Write`, up to the places it begins: a rewritten anchor's views dangle,
and a flag slot the write may touch holds either value. -/
def havoc (P : Prog) (s : AState) (a : Nat) : AState :=
  let s := invalidate P s (rewrites P a)
  { s with
    bits := fun x =>
      match P.guard x with
      | some f => if touches P a f then (s.bits x).flag none else s.bits x
      | none => s.bits x
    stale := fun ri =>
      s.stale ri || (match P.reads[ri]? with
        | some f => touches P a f
        | none => false) }

/-- `own.rs` `Act::Write`. -/
def applyWrite (P : Prog) (s : AState) (a : Nat) : AState :=
  let s := havoc P s a
  match P.places a with
  | [r] => if r.cut then s else renew P s ((P.hits a).filter fun q => P.covers r q) Bits.begun
  | _ => s

/-- `own.rs` `Act::Begin` and `Act::Dead`. -/
def applySlot (P : Prog) (s : AState) (slot : Nat) (f : Bits → Bits) : AState :=
  renew P (invalidate P s (anchored slot)) (P.tracked.filter fun q => q.root == slot) f

/-- `own.rs` `apply`, without its checks. -/
def applyAct (P : Prog) (s : AState) : Act → AState
  | .use _ => s
  | .drop a => applyDrop P s a
  | .write a => applyWrite P s a
  | .invalidate a => invalidate P s (rewrites P a)
  | .begin slot => applySlot P s slot Bits.begun
  | .dead slot => applySlot P s slot Bits.drops
  | .flag f v =>
    { s with
      bits := fun x => if P.guard x == some f then (s.bits x).flag v else s.bits x
      stale := fun ri => s.stale ri || P.reads[ri]? == some f }
  | .fresh ri => { s with stale := fun r => if r == ri then false else s.stale r }
  | .lend k => { s with lend := fun j => j == k || s.lend j }

/-- The checks of `own.rs` `apply`. -/
def checkAct (P : Prog) (s : AState) : Act → Bool
  | .use a => (P.hits a).all fun q => !mayDrop P s q
  | .drop a =>
    ((P.names a).all fun r => !P.isView r.root) && (P.hits a).all fun q => !mayDrop P s q
  | _ => true

def applyActs (P : Prog) (s : AState) (acts : List Act) : AState := acts.foldl (applyAct P) s

def checkActs (P : Prog) : AState → List Act → Bool
  | _, [] => true
  | s, a :: acts => checkAct P s a && checkActs P (applyAct P s a) acts

def applyInsts (P : Prog) (s : AState) (insts : List Inst) : AState :=
  insts.foldl (fun s i => applyActs P s i.acts) s

def instsOk (P : Prog) : AState → List Inst → Bool
  | _, [] => true
  | s, i :: insts => checkActs P s i.acts && instsOk P (applyActs P s i.acts) insts

/-- What the failure edges carry: the state before a failing instruction's acts, or after
them for a `drop`. -/
def failOuts (P : Prog) : AState → List Inst → List (Nat × AState)
  | _, [] => []
  | s, i :: insts =>
    let s' := applyActs P s i.acts
    (match i.fail with
      | some t => [(t, if i.isDrop then s' else s)]
      | none => []) ++ failOuts P s' insts

/-- An edge of a branch on a fresh read of flag `f` keeps the pairs of its value. -/
def refine (P : Prog) (s : AState) (f : Nat) (v : Bool) : AState :=
  { s with bits := fun x => if P.guard x == some f then (s.bits x).keep v else s.bits x }

/-- The argument an edge gives the join a place is rooted at. -/
def Prog.argOf (P : Prog) (args : List (Nat × Src)) (t : Pl) : Option Src :=
  (P.paramOf t).bind fun w => args.lookup w

/-- `transfer`'s state for a place `t` of a join given `src`: dropped where a place it
now is may be dropped. -/
def joinBits (P : Prog) (s : AState) (src : Src) (t : Pl) : Bits :=
  let near := fun (x : Pl) => P.tracked.filter fun q => overlap (x.app t.path) q
  let d := (P.srcs src).any fun x => (near x).any fun q => mayDrop P s q
  let z := (P.srcs src).any fun x => (near x).any fun q =>
    (P.pairsOf q).any fun p => (s.bits p).anyZ
  if d then ⟨z, z, true, true⟩ else Bits.Z

/-- `binds[q]`'s entry for join place `t` and pairing `p`. -/
def Prog.bindCell (P : Prog) (q t : Pl) (p : Nat) : Option Nat :=
  (List.range P.cells.length).find? fun z =>
    match P.cells[z]? with
    | some (.bind q' t' p' _) => q' == q && t' == t && p' == p
    | _ => false

/-- What one place `y` of the argument gives the bind state of cell `x`: a joined
argument's binding, or bound where it overlaps `q`. -/
def bindOne (P : Prog) (s : AState) (q : Pl) (p x : Nat) (y : Pl) : Bits :=
  let under := fun (u : Bool) => if u then (s.bits x).drops else (s.bits x).begun
  if y.root < P.roots then under (overlap y q)
  else
    match P.bindCell q y p with
    | some z => s.bits z
    | none => under ((P.expand y).any fun u => overlap u q)

/-- `transfer`'s bind state for cell `x`, the entry `(t, p, x)` of `binds[q]`, when the
join of `t` is given `src`. -/
def bindBits (P : Prog) (s : AState) (src : Src) (q t : Pl) (p x : Nat) : Bits :=
  let v := (P.srcs src).foldl (fun v x0 => v.or (bindOne P s q p x (x0.app t.path))) Bits.zero
  if v.isZero then (s.bits x).begun else v

/-- `own_flow.rs` `transfer`: an edge binds each join parameter it gives an argument. -/
def transfer (P : Prog) (s : AState) (args : List (Nat × Src)) : AState :=
  { s with bits := fun x =>
      match P.cells[x]? with
      | some (.pair t _) =>
        (match P.argOf args t with
          | some src => joinBits P s src t
          | none => s.bits x)
      | some (.bind q t p _) =>
        (match P.argOf args t with
          | some src => bindBits P s src q t p x
          | none => s.bits x)
      | none => s.bits x }

/-- The states a terminator sends: a branch on a flag read that is not stale refines
each edge, and every edge transfers. -/
def termOuts (P : Prog) (s : AState) : Term → List (Nat × AState)
  | .flagBranch ri yes no =>
    (match (if s.stale ri then none else P.reads[ri]?) with
      | some f =>
        [(yes.target, transfer P (refine P s f true) yes.args),
          (no.target, transfer P (refine P s f false) no.args)]
      | none => [(yes.target, transfer P s yes.args), (no.target, transfer P s no.args)])
  | .other pre edges =>
    edges.map fun e => (e.target, transfer P (applyActs P s pre) e.args)

def termOk (P : Prog) (s : AState) : Term → Bool
  | .flagBranch .. => true
  | .other pre _ => checkActs P s pre

def blockOuts (P : Prog) (s : AState) (b : Block) : List (Nat × AState) :=
  failOuts P s b.insts ++ termOuts P (applyInsts P s b.insts) b.term

def blockOk (P : Prog) (s : AState) (b : Block) : Bool :=
  instsOk P s b.insts && termOk P (applyInsts P s b.insts) b.term

/-- `start` is a fixed point of `solve` that passes every check: the entry state is in
block 0's start, every reached block passes with its start state, and every state a
block sends is in its target's start, the entry block's included. -/
def validate (P : Prog) (start : Nat → Option AState) : Bool :=
  P.wf &&
  (match start 0 with
    | some s => AState.init.le P s
    | none => false) &&
  (List.range P.blocks.length).all fun bi =>
    match P.blocks[bi]?, start bi with
    | some b, some s =>
      blockOk P s b && (blockOuts P s b).all fun ts =>
        match start ts.1 with
        | some s' => ts.2.le P s'
        | none => false
    | _, _ => true

/-- `wf::own` accepts the body: the block start states `solve` computes are such a
`start`. -/
def Accepted (P : Prog) : Prop := ∃ start, validate P start = true

/-! ## The machine -/

/-- A place's contents: live, dropped, or a view whose anchor was dropped or rewritten. -/
inductive PSt where
  | live
  | dropped
  | dangling
deriving DecidableEq, Repr

/-- The run-time state: each place's contents, each flag slot's value, the value each
flag read last saw, the borrows in force, and the place each pointer parameter points at
(`none`: nothing the pass names). -/
structure Store where
  st : Pl → PSt
  flag : Nat → Bool
  read : Nat → Bool
  loan : Nat → Bool
  bind : Nat → Option Pl

inductive Fault where
  | doubleDrop
  | useAfterDrop
  | viewDrop
  | danglingView
deriving DecidableEq, Repr

inductive Res where
  | ok (σ : Store)
  | fault (k : Fault)

/-- The place an address is: the path from where its parameter points, or, for an address
into a slot, one of its names. -/
def Den (P : Prog) (σ : Store) (a : Nat) (y : Option Pl) : Prop :=
  match P.base a with
  | some (v, rel) => y = (σ.bind v).map fun x => x.app rel
  | none =>
    match y with
    | some r => r ∈ P.names a
    | none => P.names a = []

/-- The views left dangling when the anchors `seed` picks are dropped or rewritten: the
dests of those borrows in force, and so on from each dest. -/
inductive Hangs (P : Prog) (loan : Nat → Bool) (seed : Borrow → Bool) : Pl → Prop where
  | direct {d : Pl} : d ∈ P.seeds loan seed → Hangs P loan seed d
  | via {m d : Pl} : Hangs P loan seed m → d ∈ P.succs loan m → Hangs P loan seed d

/-- `p` is in a view left dangling. A join place is no place of the machine. -/
def Dangles (P : Prog) (loan : Nat → Bool) (seed : Borrow → Bool) (p : Pl) : Prop :=
  ∃ d, Hangs P loan seed d ∧ d.root < P.roots ∧ inside d p = true

/-- `σ'` is `σ` with the places of `kill` dropped and the views of `seed`'s anchors
dangling. -/
structure Hung (P : Prog) (seed : Borrow → Bool) (kill : Pl → Prop) (σ σ' : Store) :
    Prop where
  killed : ∀ p, kill p → σ'.st p = .dropped
  hung : ∀ p, ¬ kill p → Dangles P σ.loan seed p → σ'.st p = .dangling
  kept : ∀ p, ¬ kill p → ¬ Dangles P σ.loan seed p → σ'.st p = σ.st p
  flag : σ'.flag = σ.flag
  read : σ'.read = σ.read
  loan : σ'.loan = σ.loan
  bind : σ'.bind = σ.bind

/-- The borrows a new value in the places `fresh` picks ends: those whose one dest it
is. -/
def Store.endLoans (P : Prog) (σ : Store) (fresh : Pl → Bool) : Nat → Bool := fun k =>
  σ.loan k && !(match P.borrows[k]? with
    | some b =>
      (match b.dest with
        | [d] => fresh d
        | _ => false)
    | none => false)

/-- A write of `y`: the places it covers are live again. A write under a run-time offset
begins nothing. -/
def Store.revive (P : Prog) (σ : Store) (y : Pl) : Store :=
  if y.cut then σ else
    { σ with
      st := fun p => if P.revives y p then .live else σ.st p
      loan := σ.endLoans P (P.covers y) }

/-- `live` of a slot: every place in it begins anew. -/
def Store.begin (P : Prog) (σ : Store) (slot : Nat) : Store :=
  { σ with
    st := fun p => if p.root == slot then .live else σ.st p
    loan := σ.endLoans P fun d => d.root == slot }

/-- One act. A use or drop of a place faults when a part of it is not live. A drop kills
the place and all of its parts, and the views anchored on it dangle. -/
inductive ActStep (P : Prog) : Store → Act → Res → Prop where
  | use_ok {σ a} : ActStep P σ (.use a) (.ok σ)
  | use_dropped {σ a y p} : Den P σ a (some y) → inside y p = true → σ.st p = .dropped →
      ActStep P σ (.use a) (.fault .useAfterDrop)
  | use_dangling {σ a y p} : Den P σ a (some y) → inside y p = true → σ.st p = .dangling →
      ActStep P σ (.use a) (.fault .danglingView)
  | drop_view {σ a y} : Den P σ a (some y) → P.isView y.root = true →
      ActStep P σ (.drop a) (.fault .viewDrop)
  | drop_dropped {σ a y p} : Den P σ a (some y) → inside y p = true → σ.st p = .dropped →
      ActStep P σ (.drop a) (.fault .doubleDrop)
  | drop_dangling {σ a y p} : Den P σ a (some y) → inside y p = true →
      σ.st p = .dangling → ActStep P σ (.drop a) (.fault .danglingView)
  | drop_none {σ a} : Den P σ a none → ActStep P σ (.drop a) (.ok σ)
  | drop_cut {σ a y} : Den P σ a (some y) → y.cut = true → ActStep P σ (.drop a) (.ok σ)
  | drop_ok {σ σ' a y} : Den P σ a (some y) → y.cut = false →
      Hung P (P.lent y) (fun p => inside y p = true) σ σ' →
      ActStep P σ (.drop a) (.ok σ')
  | write_none {σ a} : Den P σ a none → ActStep P σ (.write a) (.ok σ)
  | write {σ σ' a y fl} : Den P σ a (some y) →
      Hung P (fun b => b.anchor.any fun f => overlap f y) (fun _ => False) σ σ' →
      (∀ f, f ≠ y.root → fl f = σ.flag f) →
      ActStep P σ (.write a) (.ok (Store.revive P { σ' with flag := fl } y))
  | invalidate_none {σ a} : Den P σ a none → ActStep P σ (.invalidate a) (.ok σ)
  | invalidate {σ σ' a y} : Den P σ a (some y) →
      Hung P (fun b => b.anchor.any fun f => overlap f y) (fun _ => False) σ σ' →
      ActStep P σ (.invalidate a) (.ok σ')
  | begin {σ σ' slot} : Hung P (anchored slot) (fun _ => False) σ σ' →
      ActStep P σ (.begin slot) (.ok (σ'.begin P slot))
  | dead {σ σ' slot} :
      Hung P (anchored slot)
        (fun p => ∃ t, t ∈ P.tracked ∧ t.root = slot ∧ inside t p = true) σ σ' →
      ActStep P σ (.dead slot)
        (.ok { σ' with loan := σ'.endLoans P fun d => d.root == slot })
  | flag {σ f v b} : (∀ c, v = some c → b = c) →
      ActStep P σ (.flag f v)
        (.ok { σ with flag := fun g => if g == f then b else σ.flag g })
  | fresh {σ ri} :
      ActStep P σ (.fresh ri)
        (.ok { σ with
          read := fun r => if r == ri then σ.flag (P.reads.getD ri 0) else σ.read r })
  | lend {σ k} :
      ActStep P σ (.lend k) (.ok { σ with loan := fun j => j == k || σ.loan j })

/-- The acts of one instruction, in order, stopping at a fault. -/
inductive ActsStep (P : Prog) : Store → List Act → Res → Prop where
  | nil {σ} : ActsStep P σ [] (.ok σ)
  | cons {σ σ' a acts res} : ActStep P σ a (.ok σ') → ActsStep P σ' acts res →
      ActsStep P σ (a :: acts) res
  | fault {σ a acts k} : ActStep P σ a (.fault k) → ActsStep P σ (a :: acts) (.fault k)

/-- Where an edge argument points. A pointer to a place under a run-time offset is none
the pass names. -/
def Src.eval (σ : Store) : Src → Option Pl
  | .param u => σ.bind u
  | .ptr u rel => if rel.cut then Option.none else (σ.bind u).map fun x => x.app rel
  | .at x => if x.cut then Option.none else some x
  | .none => Option.none

/-- Taking an edge binds each parameter it gives an argument, all at once. -/
def Store.enter (σ : Store) (args : List (Nat × Src)) : Store :=
  { σ with bind := fun w =>
      match args.lookup w with
      | some src => src.eval σ
      | none => σ.bind w }

inductive Cfg where
  | run (b pc : Nat) (σ : Store)
  | fault (k : Fault)
  | done (σ : Store)

/-- One step of the body: an instruction, a failure edge, or the terminator. A branch on
a flag read goes by the value read. -/
inductive Step (P : Prog) : Cfg → Cfg → Prop where
  | inst {b pc σ σ' blk i} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      ActsStep P σ i.acts (.ok σ') → Step P (.run b pc σ) (.run b (pc + 1) σ')
  | instFault {b pc σ blk i k} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      ActsStep P σ i.acts (.fault k) → Step P (.run b pc σ) (.fault k)
  | fail {b pc σ blk i t} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      i.fail = some t → i.isDrop = false → Step P (.run b pc σ) (.run t 0 σ)
  | dropFail {b pc σ σ' blk i t} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      i.fail = some t → i.isDrop = true → ActsStep P σ i.acts (.ok σ') →
      Step P (.run b pc σ) (.run t 0 σ')
  | branch {b pc σ blk ri yes no e} : P.blocks[b]? = some blk → pc = blk.insts.length →
      blk.term = .flagBranch ri yes no → e = (if σ.read ri then yes else no) →
      Step P (.run b pc σ) (.run e.target 0 (σ.enter e.args))
  | goto {b pc σ σ' blk pre edges e} : P.blocks[b]? = some blk → pc = blk.insts.length →
      blk.term = .other pre edges → ActsStep P σ pre (.ok σ') → e ∈ edges →
      Step P (.run b pc σ) (.run e.target 0 (σ'.enter e.args))
  | exit {b pc σ σ' blk pre} : P.blocks[b]? = some blk → pc = blk.insts.length →
      blk.term = .other pre [] → ActsStep P σ pre (.ok σ') →
      Step P (.run b pc σ) (.done σ')
  | termFault {b pc σ blk pre edges k} : P.blocks[b]? = some blk →
      pc = blk.insts.length → blk.term = .other pre edges →
      ActsStep P σ pre (.fault k) → Step P (.run b pc σ) (.fault k)

inductive Steps (P : Prog) : Cfg → Cfg → Prop where
  | refl {c} : Steps P c c
  | tail {c c' c''} : Steps P c c' → Step P c' c'' → Steps P c c''

/-- At entry nothing is dropped and no borrow is in force; the flags hold anything. A
join's parameter points at nothing yet, a parameter points only at a place it names, and
one that names a single place and is not a join points at it. -/
structure Initial (P : Prog) (σ : Store) : Prop where
  live : ∀ p, σ.st p = .live
  loan : ∀ k, σ.loan k = false
  join : ∀ v, (P.joinOf v).isSome = true → σ.bind v = none
  under : ∀ v y, σ.bind v = some y → y ∈ P.under v ∧ y.cut = false
  plain : ∀ v x, P.joinOf v = none → one (P.under v) = some x → σ.bind v = some x

/-- `c` is reached from the entry of the body. -/
def Reachable (P : Prog) (c : Cfg) : Prop :=
  ∃ σ, Initial P σ ∧ Steps P (.run 0 0 σ) c

end Hirsel.CoreIR.Own
