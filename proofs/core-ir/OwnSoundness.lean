/-!
# OwnSoundness: soundness of CoreIR `wf::own` (R1-R3, OWN-20) for join-free bodies

A model of the ownership pass `crates/hirsel-core-ir/src/wf/own.rs` and a proof that a body
the pass accepts never, on any path,

* drops a place overlapping one that is dropped already (R1, `Fault.doubleDrop`),
* uses a place overlapping one that is dropped (R2, `Fault.useAfterDrop`),
* drops a `view` parameter's place (R3, `Fault.viewDrop`),
* uses or drops a dest view whose anchor was dropped while the borrow held (OWN-20,
  `Fault.danglingView`).

The model sits at the pass's own seam. `Own::acts` turns each instruction into acts by
address; `Own::apply` and `flow` are the analysis. Here a program (`Prog`) is the control
flow graph of those acts together with the tables `check` builds (`names`, `tracked`,
`pairs`/`guards`, `borrows`), the analysis is `applyAct`/`checkAct`/`blockOuts`, and
`Accepted` asks for any post-fixpoint of it that passes every check. The least fixpoint
`check` computes is one, so a body `check` accepts is `Accepted`.

The machine (`Step`) runs the same acts over a store that holds, for every place, whether
it is live, dropped, or a view left dangling by its anchor's drop, the value of every drop
flag, and the borrows in force.

## Scope

Join-free bodies: no pointer block parameter carries more than one place, so `joins` is
empty and `routed`, `shadow`, `binds`, `transfer` and `drop_bound` do nothing. The join
machinery is not modelled.

## Hypotheses

The theorems hold under `Hyp`, whose fields name what `wf::own` relies on and does not
check. For each field but `wf` there is a body the analysis accepts that breaks only that
field and drops a place twice (`EntryEdge.needed`, `StaleFlag.needed`, `FlagWrite.needed`,
`PartialWrite.needed`); `wf::module` accepts a CoreIR body of the same shape as each
(README.md).

* `noEntryEdge`: no edge or failure edge targets the entry block.
* `flagFresh`: a branch on a read of a drop flag sees the flag's current value. A `Prog`
  has no read act, so this is a property of the machine (`Sem.branch`), not of the body:
  for a CoreIR body it is the obligation that no store to the flag falls between the read
  and the branch, and no check here discharges it.
* `flagsStored`: no write other than a whole `store` reaches a guarding flag's slot.
* `writesBegin`: a write through a single place begins every tracked place it overlaps.

`Sem.own` is the machine own.rs describes, where any overlapping write begins a place
anew; there `writesBegin` holds of every body (`hyp_own`). `Sem.cells` is the machine
where a write begins only what lies inside the written place; there it is the check
`Prog.writesWhole` (`hyp_cells`).

Built into the machine: every address denotes, when it is used, one of the places `names`
gives it (the pass's `spread`); a pointer from an `alloc`, a load or a call's result
names nothing the pass checks. A drop through a place under a run-time offset (`cut`) is
recorded by neither the pass nor the machine. An instruction that fails leaves the store
as it was, except a `drop`, which fails after its acts.

The pass is about places, not values: a move leaves its source's state alone, a write
does not need its target live, the end of an anchor's storage does not poison a view, and
nothing is checked at `return`. The theorems claim none of these.
-/

namespace Hirsel.CoreIR.Own

/-! ## Places (own.rs:46-75) -/

/-- One static step, as own.rs `Steps` holds it: `(0, field, 0)`, `(1, case, field)` or
`(2, seq word, 0)`. -/
abbrev PStep := Nat × Nat × Nat

/-- own.rs `Pl`: a root (a slot, or an entry parameter's place at `slots + k`), its static
steps, and `cut` when a run-time offset stands for the rest of the place. -/
structure Pl where
  root : Nat
  steps : List PStep
  cut : Bool
deriving DecidableEq, Repr

/-- own.rs `overlap`: one root, and one's steps a prefix of the other's. -/
def overlap (a b : Pl) : Bool :=
  a.root == b.root && (b.steps.isPrefixOf a.steps || a.steps.isPrefixOf b.steps)

/-- `p` lies inside `r`: a write of `r` replaces all of `p`. -/
def covers (r p : Pl) : Bool :=
  r.root == p.root && r.steps.isPrefixOf p.steps

/-! ## Pairing states (own.rs:215-244)

A pairing's state is the set of reachable (dropped, flag) pairs: `zt` is own.rs `ZT`
(not dropped, flag true), `zf` is `ZF`, `dt` is `DT` and `df` is `DF`. -/

structure Bits where
  zt : Bool
  zf : Bool
  dt : Bool
  df : Bool
deriving DecidableEq, Repr

namespace Bits

/-- own.rs `Z`: not dropped, under either flag value. The entry state. -/
def Z : Bits := ⟨true, true, false, false⟩

/-- own.rs `halves`: whether the half for flag value `b` is reachable. -/
def half (s : Bits) (b : Bool) : Bool := if b then s.zt || s.dt else s.zf || s.df

/-- The dropped bit of the half for flag value `b`. -/
def d (s : Bits) (b : Bool) : Bool := if b then s.dt else s.df

/-- `s & D != 0`. -/
def anyD (s : Bits) : Bool := s.dt || s.df

def or (s t : Bits) : Bits := ⟨s.zt || t.zt, s.zf || t.zf, s.dt || t.dt, s.df || t.df⟩

/-- own.rs `dropped`: dropped in each reachable half; added to `s` when not `strong`. -/
def dropped (s : Bits) (strong : Bool) : Bits :=
  let m : Bits := ⟨false, false, s.half true, s.half false⟩
  if strong then m else s.or m

/-- own.rs `begun`: not dropped in each reachable half. -/
def begun (s : Bits) : Bits := ⟨s.half true, s.half false, false, false⟩

/-- own.rs `flag`: a store of the flag moves both of its states to the stored half. -/
def flag (s : Bits) (v : Option Bool) : Bits :=
  let z := s.zt || s.zf
  let dd := s.dt || s.df
  match v with
  | some true => ⟨z, false, dd, false⟩
  | some false => ⟨false, z, false, dd⟩
  | none => ⟨z, z, dd, dd⟩

/-- `s & T` or `s & F`: the pairs of one edge of a branch on the flag (own.rs:916-919). -/
def keep (s : Bits) (b : Bool) : Bits :=
  if b then ⟨s.zt, false, s.dt, false⟩ else ⟨false, s.zf, false, s.df⟩

def le (s t : Bits) : Bool :=
  (!s.zt || t.zt) && (!s.zf || t.zf) && (!s.dt || t.dt) && (!s.df || t.df)

end Bits

/-! ## Programs: the acts of a body and the tables `check` builds -/

/-- own.rs `Act` (246-255), by address. -/
inductive Act where
  | use (a : Nat)
  | drop (a : Nat)
  | write (a : Nat)
  | begin (slot : Nat)
  | flag (f : Nat) (v : Option Bool)
  | lend (k : Nat)
deriving DecidableEq, Repr

/-- An instruction: its acts (own.rs `acts`, 329-381), the failure block it names, and
whether it is a `drop` (whose failure edge leaves after its acts, own.rs:899-909). -/
structure Inst where
  acts : List Act
  fail : Option Nat
  isDrop : Bool

/-- A terminator, as `flow` reads it (own.rs:911-957): a branch on a read of flag `f`, or
anything else, with the acts it applies first (`raise fail a` uses `a`) and its targets. -/
inductive Term where
  | flagBranch (f yes no : Nat)
  | other (pre : List Act) (succs : List Nat)

structure Block where
  insts : List Inst
  term : Term

/-- A call whose dest views its anchors (own.rs:684-709): the dest's places and the
anchor arguments' places. -/
structure Borrow where
  dest : List Pl
  anchor : List Pl

/-- A body as `check` sees it. `names` is own.rs `n.addrs` and `derefs` is `n.derefs`.
Pairing `x` is `pairing[x]`: the tracked place it belongs to (`x ∈ pairs[q]`) and the flag
slot guarding it, if any (`x ∈ guards[f]`). Borrow `k`'s bit is `now[base + k]`. -/
structure Prog where
  slots : Nat
  views : List Bool
  names : Nat → List Pl
  derefs : Nat → List Pl
  tracked : List Pl
  pairing : List (Pl × Option Nat)
  borrows : List Borrow
  blocks : List Block

namespace Prog

/-- own.rs:409: the root is an entry value that is a `view` parameter's pointer. -/
def isView (P : Prog) (root : Nat) : Bool :=
  decide (P.slots ≤ root) && P.views.getD (root - P.slots) false

def owner (P : Prog) (x : Nat) : Option Pl := (P.pairing[x]?).map Prod.fst

def guard (P : Prog) (x : Nat) : Option Nat := (P.pairing[x]?).bind Prod.snd

/-- own.rs `hits`: the tracked places an address's names overlap. -/
def hits (P : Prog) (a : Nat) : List Pl :=
  P.tracked.filter fun q => (P.names a).any fun r => overlap r q

/-- own.rs `deref_hits`. -/
def derefHits (P : Prog) (a : Nat) : List Pl :=
  P.tracked.filter fun q => (P.derefs a).any fun r => overlap r q

/-- The dests a drop of `q` reaches in one step: `borrows[k]` for each `k ∈ lent[q]`
(own.rs:776-780) whose bit `on k` is set. -/
def succs (P : Prog) (on : Nat → Bool) (q : Pl) : List Pl :=
  P.borrows.zipIdx.flatMap fun bk =>
    if on bk.2 && bk.1.anchor.any (fun f => overlap f q) && !bk.1.dest.contains q
    then bk.1.dest else []

/-- Every act of the body. -/
def allActs (P : Prog) : List Act :=
  P.blocks.flatMap fun b =>
    b.insts.flatMap (fun i => i.acts) ++
      match b.term with
      | .other pre _ => pre
      | .flagBranch .. => []

/-- Every block an edge or a failure edge enters. -/
def targets (P : Prog) : List Nat :=
  P.blocks.flatMap fun b =>
    b.insts.flatMap (fun i => i.fail.toList) ++
      match b.term with
      | .other _ succs => succs
      | .flagBranch _ yes no => [yes, no]

/-- The tables are built as `check` builds them: a drop's uncut names are tracked
(own.rs:656-672) and so is every borrow's dest (own.rs:702-707). -/
def wf (P : Prog) : Bool :=
  (P.allActs.all fun
    | .drop a => (P.names a).all fun r => r.cut || P.tracked.contains r
    | _ => true) &&
  P.borrows.all fun b => b.dest.all fun q => P.tracked.contains q

/-- No edge or failure edge enters the entry block. -/
def noEntryEdge (P : Prog) : Bool := P.targets.all fun t => t != 0

/-- No write names a place in a slot that guards a pairing: such a slot changes only by
`Act.flag`, the whole `store` own.rs:339-345 reads. -/
def flagsStored (P : Prog) : Bool :=
  P.allActs.all fun
    | .write a => (P.names a).all fun r =>
        (List.range P.pairing.length).all fun x => P.guard x != some r.root
    | _ => true

end Prog

/-! ## The analysis (own.rs `apply`, `drop`, `end`, `mark`, `flow`) -/

/-- The analysis state: `bits x` is `now[x]` for pairing `x`, `lend k` is
`now[base + k] != 0`. -/
structure AState where
  bits : Nat → Bits
  lend : Nat → Bool

/-- own.rs:878: `vec![Z; width]`. Every borrow's bit starts nonzero. -/
def AState.init : AState := ⟨fun _ => Bits.Z, fun _ => true⟩

def AState.le (P : Prog) (s t : AState) : Bool :=
  ((List.range P.pairing.length).all fun x => (s.bits x).le (t.bits x)) &&
  (List.range P.borrows.length).all fun k => !s.lend k || t.lend k

/-- own.rs `may_drop`: every pairing of `q` has a dropped state. -/
def mayDrop (P : Prog) (s : AState) (q : Pl) : Bool :=
  (List.range P.pairing.length).all fun x => P.owner x != some q || (s.bits x).anyD

/-- own.rs `drop`'s walk (494-507) with its `seen` list: the places a drop of `q`
reaches through the borrows in force. `fuel` bounds the depth; `P.tracked.length + 1`
is never exhausted (`dfs_spec`). -/
def dfs (P : Prog) (on : Nat → Bool) : Nat → Pl → List Pl → List Pl
  | 0, _, seen => seen
  | fuel + 1, q, seen =>
    if seen.contains q then seen
    else (P.succs on q).foldl (fun acc d => dfs P on fuel d acc) (q :: seen)

/-- `mark(q, dropped, strong)` for every `q` in `R`. -/
def dropAll (P : Prog) (s : AState) (R : List Pl) (strong : Bool) : AState :=
  { s with bits := fun x =>
      match P.owner x with
      | some q => if R.contains q then (s.bits x).dropped strong else s.bits x
      | none => s.bits x }

/-- own.rs `drop(now, q, strong, &mut vec![])`. -/
def dropPlace (P : Prog) (s : AState) (q : Pl) (strong : Bool) : AState :=
  dropAll P s (dfs P s.lend (P.tracked.length + 1) q []) strong

/-- `end(q); mark(q, begun, true)` for every `q` in `Q` (own.rs:463-469, 473-481). -/
def beginAll (P : Prog) (s : AState) (Q : List Pl) : AState :=
  { bits := fun x =>
      match P.owner x with
      | some q => if Q.contains q then (s.bits x).begun else s.bits x
      | none => s.bits x
    lend := fun k =>
      s.lend k && !(match P.borrows[k]? with
        | some b => b.dest.any fun q => Q.contains q
        | none => false) }

/-- own.rs:433-442 with `routed` false: `drop` for each uncut tracked name in turn. -/
def dropRoots (P : Prog) (s : AState) (roots : List Pl) (strong : Bool) : AState :=
  roots.foldl (fun s r =>
    if !r.cut && P.tracked.contains r then dropPlace P s r strong else s) s

/-- own.rs `apply`, without its checks. -/
def applyAct (P : Prog) (s : AState) : Act → AState
  | .use _ => s
  | .drop a => dropRoots P s (P.names a) ((P.names a).length == 1)
  | .write a =>
    match P.names a with
    | [r] => if r.cut then s else beginAll P s (P.hits a)
    | _ => s
  | .begin slot => beginAll P s (P.tracked.filter fun q => q.root == slot)
  | .flag f v =>
    { s with bits := fun x => if P.guard x == some f then (s.bits x).flag v else s.bits x }
  | .lend k => { s with lend := fun j => j == k || s.lend j }

/-- The checks of own.rs `apply` (388-422). -/
def checkAct (P : Prog) (s : AState) : Act → Bool
  | .use a => (P.hits a ++ P.derefHits a).all fun q => !mayDrop P s q
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

/-- What the failure edges carry (own.rs:899-909): the state before a failing
instruction's acts, or after them for a `drop`. -/
def failOuts (P : Prog) : AState → List Inst → List (Nat × AState)
  | _, [] => []
  | s, i :: insts =>
    let s' := applyActs P s i.acts
    (match i.fail with
      | some t => [(t, if i.isDrop then s' else s)]
      | none => []) ++ failOuts P s' insts

/-- own.rs:915-919: an edge of a branch on flag `f` keeps the pairs of its value. -/
def refine (P : Prog) (s : AState) (f : Nat) (v : Bool) : AState :=
  { s with bits := fun x => if P.guard x == some f then (s.bits x).keep v else s.bits x }

def termOuts (P : Prog) (s : AState) : Term → List (Nat × AState)
  | .flagBranch f yes no => [(yes, refine P s f true), (no, refine P s f false)]
  | .other pre succs => succs.map fun t => (t, applyActs P s pre)

def termOk (P : Prog) (s : AState) : Term → Bool
  | .flagBranch .. => true
  | .other pre _ => checkActs P s pre

/-- own.rs `flow`: each target of block `blk` with the state it receives. -/
def blockOuts (P : Prog) (blk : Block) (s : AState) : List (Nat × AState) :=
  failOuts P s blk.insts ++ termOuts P (applyInsts P s blk.insts) blk.term

/-- own.rs `flow` with `check`: every check of the block passes. -/
def blockOk (P : Prog) (blk : Block) (s : AState) : Bool :=
  instsOk P s blk.insts && termOk P (applyInsts P s blk.insts) blk.term

/-- `start` is a post-fixpoint of the analysis that passes every check: the entry starts
at `init`, each block with a start state passes its checks, and each target other than
block 0 (own.rs:962) starts with at least what the edge carries. -/
def validate (P : Prog) (start : Nat → Option AState) : Bool :=
  (match start 0 with
    | some s => AState.init.le P s
    | none => false) &&
  (List.range P.blocks.length).all fun bi =>
    match P.blocks[bi]?, start bi with
    | some blk, some s =>
      blockOk P blk s &&
      (blockOuts P blk s).all fun ts =>
        ts.1 == 0 || match start ts.1 with
          | some s' => ts.2.le P s'
          | none => false
    | _, _ => true

/-- `wf::own` accepts the body: the block start states `check` computes (own.rs:875-966)
are such a `start`. -/
def Accepted (P : Prog) : Prop := ∃ start, validate P start = true

/-! ## The machine -/

/-- A place's contents: live, dropped, or a view whose anchor was dropped. -/
inductive PSt where
  | live
  | dropped
  | dangling
deriving DecidableEq, Repr

structure Store where
  st : Pl → PSt
  flag : Nat → Bool
  loan : Nat → Bool

inductive Fault where
  | doubleDrop
  | useAfterDrop
  | viewDrop
  | danglingView
deriving DecidableEq, Repr

inductive Res where
  | ok (σ : Store)
  | fault (k : Fault)

/-- What the machine does where the pass assumes: which places a write of `r` begins anew,
and which values a branch on a read of flag `f` may see. -/
structure Sem where
  revive : Pl → Pl → Bool
  branch : Store → Nat → Bool → Prop

/-- The machine own.rs's header describes: a write begins every place it overlaps, and a
flag branch sees the flag. -/
def Sem.own : Sem := ⟨overlap, fun σ f v => v = σ.flag f⟩

/-- The views a drop of `q` leaves dangling: the dests of the borrows in force anchored
on a place overlapping `q`, and so on from each of those. -/
inductive Hangs (P : Prog) (loan : Nat → Bool) (q : Pl) : Pl → Prop where
  | direct {d : Pl} : d ∈ P.succs loan q → Hangs P loan q d
  | via {m d : Pl} : Hangs P loan q m → d ∈ P.succs loan m → Hangs P loan q d

/-- A write of `r`: the places it begins are live again, and a borrow whose dest it
begins has ended. A place under a run-time offset begins nothing. -/
def Store.write (S : Sem) (P : Prog) (σ : Store) (r : Pl) (fl : Nat → Bool) : Store :=
  if r.cut then { σ with flag := fl } else
  { st := fun p => if S.revive r p then .live else σ.st p
    flag := fl
    loan := fun k =>
      σ.loan k && !(match P.borrows[k]? with
        | some b => b.dest.any fun q => S.revive r q
        | none => false) }

/-- `live` or `dead` of a slot: every place in it begins anew. -/
def Store.begin (P : Prog) (σ : Store) (slot : Nat) : Store :=
  { st := fun p => if p.root == slot then .live else σ.st p
    flag := σ.flag
    loan := fun k =>
      σ.loan k && !(match P.borrows[k]? with
        | some b => b.dest.any fun q => q.root == slot
        | none => false) }

/-- One act. An address denotes one of its names; an address with no names touches
nothing the pass tracks. -/
inductive ActStep (S : Sem) (P : Prog) : Store → Act → Res → Prop where
  | use_ok {σ a} : ActStep S P σ (.use a) (.ok σ)
  | use_dropped {σ a r p} : r ∈ P.names a ++ P.derefs a → overlap r p = true →
      σ.st p = .dropped → ActStep S P σ (.use a) (.fault .useAfterDrop)
  | use_dangling {σ a r p} : r ∈ P.names a ++ P.derefs a → overlap r p = true →
      σ.st p = .dangling → ActStep S P σ (.use a) (.fault .danglingView)
  | drop_view {σ a r} : r ∈ P.names a → P.isView r.root = true →
      ActStep S P σ (.drop a) (.fault .viewDrop)
  | drop_dropped {σ a r p} : r ∈ P.names a → overlap r p = true → σ.st p = .dropped →
      ActStep S P σ (.drop a) (.fault .doubleDrop)
  | drop_dangling {σ a r p} : r ∈ P.names a → overlap r p = true → σ.st p = .dangling →
      ActStep S P σ (.drop a) (.fault .danglingView)
  | drop_cut {σ a r} : r ∈ P.names a → P.isView r.root = false →
      (∀ p, overlap r p = true → σ.st p = .live) → r.cut = true →
      ActStep S P σ (.drop a) (.ok σ)
  | drop_ok {σ σ' a r} : r ∈ P.names a → P.isView r.root = false →
      (∀ p, overlap r p = true → σ.st p = .live) → r.cut = false →
      σ'.st r = .dropped →
      (∀ p, p ≠ r → Hangs P σ.loan r p → σ'.st p = .dangling) →
      (∀ p, p ≠ r → ¬ Hangs P σ.loan r p → σ'.st p = σ.st p) →
      σ'.flag = σ.flag → σ'.loan = σ.loan →
      ActStep S P σ (.drop a) (.ok σ')
  | drop_unnamed {σ a} : P.names a = [] → ActStep S P σ (.drop a) (.ok σ)
  | write {σ a r fl} : r ∈ P.names a → (∀ f, f ≠ r.root → fl f = σ.flag f) →
      ActStep S P σ (.write a) (.ok (σ.write S P r fl))
  | write_unnamed {σ a} : P.names a = [] → ActStep S P σ (.write a) (.ok σ)
  | begin {σ slot} : ActStep S P σ (.begin slot) (.ok (σ.begin P slot))
  | flag {σ f v b} : (∀ c, v = some c → b = c) →
      ActStep S P σ (.flag f v)
        (.ok { σ with flag := fun g => if g == f then b else σ.flag g })
  | lend {σ k} :
      ActStep S P σ (.lend k) (.ok { σ with loan := fun j => j == k || σ.loan j })

/-- The acts of one instruction, in order, stopping at a fault. -/
inductive ActsStep (S : Sem) (P : Prog) : Store → List Act → Res → Prop where
  | nil {σ} : ActsStep S P σ [] (.ok σ)
  | cons {σ σ' a acts res} : ActStep S P σ a (.ok σ') → ActsStep S P σ' acts res →
      ActsStep S P σ (a :: acts) res
  | fault {σ a acts k} : ActStep S P σ a (.fault k) → ActsStep S P σ (a :: acts) (.fault k)

inductive Cfg where
  | run (b pc : Nat) (σ : Store)
  | fault (k : Fault)
  | done (σ : Store)

/-- One step of the body: an instruction, a failure edge, or the terminator. -/
inductive Step (S : Sem) (P : Prog) : Cfg → Cfg → Prop where
  | inst {b pc σ σ' blk i} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      ActsStep S P σ i.acts (.ok σ') → Step S P (.run b pc σ) (.run b (pc + 1) σ')
  | instFault {b pc σ blk i k} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      ActsStep S P σ i.acts (.fault k) → Step S P (.run b pc σ) (.fault k)
  | fail {b pc σ blk i t} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      i.fail = some t → i.isDrop = false → Step S P (.run b pc σ) (.run t 0 σ)
  | dropFail {b pc σ σ' blk i t} : P.blocks[b]? = some blk → blk.insts[pc]? = some i →
      i.fail = some t → i.isDrop = true → ActsStep S P σ i.acts (.ok σ') →
      Step S P (.run b pc σ) (.run t 0 σ')
  | branch {b pc σ blk f yes no v} : P.blocks[b]? = some blk → pc = blk.insts.length →
      blk.term = .flagBranch f yes no → S.branch σ f v →
      Step S P (.run b pc σ) (.run (if v then yes else no) 0 σ)
  | goto {b pc σ σ' blk pre succs t} : P.blocks[b]? = some blk → pc = blk.insts.length →
      blk.term = .other pre succs → ActsStep S P σ pre (.ok σ') → t ∈ succs →
      Step S P (.run b pc σ) (.run t 0 σ')
  | exit {b pc σ σ' blk pre} : P.blocks[b]? = some blk → pc = blk.insts.length →
      blk.term = .other pre [] → ActsStep S P σ pre (.ok σ') →
      Step S P (.run b pc σ) (.done σ')
  | termFault {b pc σ blk pre succs k} : P.blocks[b]? = some blk →
      pc = blk.insts.length → blk.term = .other pre succs →
      ActsStep S P σ pre (.fault k) → Step S P (.run b pc σ) (.fault k)

inductive Steps (S : Sem) (P : Prog) : Cfg → Cfg → Prop where
  | refl {c} : Steps S P c c
  | tail {c c' c''} : Steps S P c c' → Step S P c' c'' → Steps S P c c''

/-- At entry nothing is dropped and no borrow is in force; the flags hold anything. -/
def Initial (σ : Store) : Prop := (∀ p, σ.st p = .live) ∧ ∀ k, σ.loan k = false

/-- `c` is reached from the entry of the body. -/
def Reachable (S : Sem) (P : Prog) (c : Cfg) : Prop :=
  ∃ σ, Initial σ ∧ Steps S P (.run 0 0 σ) c

/-! ## Hypotheses -/

/-- A write through a single uncut place begins every tracked place it overlaps
(own.rs:454-470 marks each of them begun). -/
def WritesBegin (S : Sem) (P : Prog) : Prop :=
  ∀ a r, Act.write a ∈ P.allActs → P.names a = [r] → r.cut = false →
    ∀ q, q ∈ P.tracked → overlap r q = true → S.revive r q = true

/-- What `wf::own` relies on and does not check. -/
structure Hyp (S : Sem) (P : Prog) : Prop where
  wf : P.wf = true
  noEntryEdge : P.noEntryEdge = true
  flagFresh : ∀ σ f v, S.branch σ f v → v = σ.flag f
  flagsStored : P.flagsStored = true
  writesBegin : WritesBegin S P

/-! ## Proof: pairing states -/

namespace Bits

theorem d_half {s : Bits} {b : Bool} : s.d b = true → s.half b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> decide

theorem d_anyD {s : Bits} {b : Bool} : s.d b = true → s.anyD = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> decide

theorem le_half {s t : Bits} {b : Bool} : s.le t = true → s.half b = true → t.half b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases t with ⟨zt', zf', dt', df'⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;>
    cases zt' <;> cases zf' <;> cases dt' <;> cases df' <;> decide

theorem le_d {s t : Bits} {b : Bool} : s.le t = true → s.d b = true → t.d b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases t with ⟨zt', zf', dt', df'⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;>
    cases zt' <;> cases zf' <;> cases dt' <;> cases df' <;> decide

theorem dropped_half (s : Bits) (strong b : Bool) : (s.dropped strong).half b = s.half b := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases strong <;> cases b <;> rfl

theorem dropped_d (s : Bits) (strong b : Bool) : (s.dropped strong).d b = s.half b := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases strong <;> cases b <;> rfl

theorem begun_half (s : Bits) (b : Bool) : s.begun.half b = s.half b := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> rfl

theorem keep_half (s : Bits) (b : Bool) : (s.keep b).half b = s.half b := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> rfl

theorem keep_d (s : Bits) (b : Bool) : (s.keep b).d b = s.d b := by
  rcases s with ⟨zt, zf, dt, df⟩
  cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> rfl

/-- A store of the flag carries whatever was reachable into the stored half. -/
theorem flag_half {s : Bits} {v : Option Bool} {b b' : Bool} (hv : ∀ c, v = some c → b = c) :
    s.half b' = true → (s.flag v).half b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases v with _ | c
  · cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> cases b' <;> decide
  · have hb := hv c rfl
    cases c <;> cases hb <;>
      cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b' <;> decide

theorem flag_d {s : Bits} {v : Option Bool} {b b' : Bool} (hv : ∀ c, v = some c → b = c) :
    s.d b' = true → (s.flag v).d b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases v with _ | c
  · cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b <;> cases b' <;> decide
  · have hb := hv c rfl
    cases c <;> cases hb <;>
      cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b' <;> decide

end Bits

/-! ## Proof: the tables -/

theorem owner_lt {P : Prog} {x : Nat} {q : Pl} (h : P.owner x = some q) :
    x < P.pairing.length := by
  unfold Prog.owner at h
  cases hx : P.pairing[x]? with
  | none => simp [hx] at h
  | some v =>
    obtain ⟨hlt, _⟩ := List.getElem?_eq_some_iff.mp hx
    exact hlt

theorem guard_lt {P : Prog} {x f : Nat} (h : P.guard x = some f) : x < P.pairing.length := by
  unfold Prog.guard at h
  cases hx : P.pairing[x]? with
  | none => simp [hx] at h
  | some v =>
    obtain ⟨hlt, _⟩ := List.getElem?_eq_some_iff.mp hx
    exact hlt

theorem mem_hits {P : Prog} {a : Nat} {q : Pl} :
    q ∈ P.hits a ↔ q ∈ P.tracked ∧ ∃ r, r ∈ P.names a ∧ overlap r q = true := by
  simp [Prog.hits, List.mem_filter, List.any_eq_true]

theorem mem_derefHits {P : Prog} {a : Nat} {q : Pl} :
    q ∈ P.derefHits a ↔ q ∈ P.tracked ∧ ∃ r, r ∈ P.derefs a ∧ overlap r q = true := by
  simp [Prog.derefHits, List.mem_filter, List.any_eq_true]

theorem mem_succs {P : Prog} {on : Nat → Bool} {q d : Pl} :
    d ∈ P.succs on q ↔ ∃ k b, P.borrows[k]? = some b ∧ on k = true ∧
      (∃ f, f ∈ b.anchor ∧ overlap f q = true) ∧ q ∉ b.dest ∧ d ∈ b.dest := by
  simp only [Prog.succs, List.mem_flatMap, List.mem_zipIdx_iff_getElem?, Prod.exists]
  constructor
  · rintro ⟨b, k, hb, hd⟩
    split at hd
    · rename_i hc
      simp only [Bool.and_eq_true, List.any_eq_true, Bool.not_eq_true',
        List.contains_eq_mem, decide_eq_false_iff_not] at hc
      exact ⟨k, b, hb, hc.1.1, hc.1.2, hc.2, hd⟩
    · simp at hd
  · rintro ⟨k, b, hb, hon, ⟨f, hf, hfo⟩, hq, hd⟩
    refine ⟨b, k, hb, ?_⟩
    have hc : (on k && b.anchor.any (fun f => overlap f q) && !b.dest.contains q) = true := by
      simp only [Bool.and_eq_true, List.any_eq_true, Bool.not_eq_true',
        List.contains_eq_mem, decide_eq_false_iff_not]
      exact ⟨⟨hon, f, hf, hfo⟩, hq⟩
    simp only [hc, ite_true]
    exact hd

theorem succs_mono {P : Prog} {on on' : Nat → Bool} {q d : Pl}
    (h : ∀ k, k < P.borrows.length → on k = true → on' k = true) (hd : d ∈ P.succs on q) :
    d ∈ P.succs on' q := by
  obtain ⟨k, b, hb, hon, ha, hq, hd⟩ := mem_succs.mp hd
  obtain ⟨hlt, _⟩ := List.getElem?_eq_some_iff.mp hb
  exact mem_succs.mpr ⟨k, b, hb, h k hlt hon, ha, hq, hd⟩

theorem wf_drop {P : Prog} {a : Nat} {r : Pl} (h : P.wf = true) (ha : Act.drop a ∈ P.allActs)
    (hr : r ∈ P.names a) (hc : r.cut = false) : r ∈ P.tracked := by
  unfold Prog.wf at h
  simp only [Bool.and_eq_true, List.all_eq_true] at h
  have := h.1 _ ha
  simp only [List.all_eq_true] at this
  simpa [hc] using this r hr

theorem wf_dest {P : Prog} {b : Borrow} {q : Pl} (h : P.wf = true) (hb : b ∈ P.borrows)
    (hq : q ∈ b.dest) : q ∈ P.tracked := by
  unfold Prog.wf at h
  simp only [Bool.and_eq_true, List.all_eq_true] at h
  simpa using h.2 b hb q hq

theorem succs_tracked {P : Prog} {on : Nat → Bool} {q d : Pl} (h : P.wf = true)
    (hd : d ∈ P.succs on q) : d ∈ P.tracked := by
  obtain ⟨k, b, hb, _, _, _, hd⟩ := mem_succs.mp hd
  exact wf_dest h (List.mem_of_getElem? hb) hd

theorem mem_allActs_inst {P : Prog} {b pc : Nat} {blk : Block} {i : Inst} {a : Act}
    (hb : P.blocks[b]? = some blk) (hi : blk.insts[pc]? = some i) (ha : a ∈ i.acts) :
    a ∈ P.allActs := by
  unfold Prog.allActs
  rw [List.mem_flatMap]
  refine ⟨blk, List.mem_of_getElem? hb, ?_⟩
  rw [List.mem_append]
  left
  rw [List.mem_flatMap]
  exact ⟨i, List.mem_of_getElem? hi, ha⟩

theorem mem_allActs_term {P : Prog} {b : Nat} {blk : Block} {pre : List Act} {succs : List Nat}
    {a : Act} (hb : P.blocks[b]? = some blk) (ht : blk.term = .other pre succs) (ha : a ∈ pre) :
    a ∈ P.allActs := by
  unfold Prog.allActs
  rw [List.mem_flatMap]
  refine ⟨blk, List.mem_of_getElem? hb, ?_⟩
  rw [List.mem_append]
  right
  rw [ht]
  exact ha

theorem target_ne_zero {P : Prog} {t : Nat} (h : P.noEntryEdge = true) (ht : t ∈ P.targets) :
    t ≠ 0 := by
  unfold Prog.noEntryEdge at h
  simp only [List.all_eq_true] at h
  simpa using h t ht

theorem mem_targets_fail {P : Prog} {b pc t : Nat} {blk : Block} {i : Inst}
    (hb : P.blocks[b]? = some blk) (hi : blk.insts[pc]? = some i) (hf : i.fail = some t) :
    t ∈ P.targets := by
  unfold Prog.targets
  rw [List.mem_flatMap]
  refine ⟨blk, List.mem_of_getElem? hb, ?_⟩
  rw [List.mem_append]
  left
  rw [List.mem_flatMap]
  exact ⟨i, List.mem_of_getElem? hi, by simp [hf]⟩

theorem mem_targets_branch {P : Prog} {b f yes no : Nat} {blk : Block} (v : Bool)
    (hb : P.blocks[b]? = some blk) (ht : blk.term = .flagBranch f yes no) :
    (if v then yes else no) ∈ P.targets := by
  unfold Prog.targets
  rw [List.mem_flatMap]
  refine ⟨blk, List.mem_of_getElem? hb, ?_⟩
  rw [List.mem_append]
  right
  rw [ht]
  cases v <;> simp

theorem mem_targets_goto {P : Prog} {b t : Nat} {blk : Block} {pre : List Act}
    {succs : List Nat} (hb : P.blocks[b]? = some blk) (ht : blk.term = .other pre succs)
    (h : t ∈ succs) : t ∈ P.targets := by
  unfold Prog.targets
  rw [List.mem_flatMap]
  refine ⟨blk, List.mem_of_getElem? hb, ?_⟩
  rw [List.mem_append]
  right
  rw [ht]
  exact h

/-! ## Proof: the walk of `drop` reaches every view the drop leaves dangling -/

/-- How many places of `V` are not in `seen`: what bounds the walk. -/
def unseen (V seen : List Pl) : Nat := V.countP fun v => !seen.contains v

theorem unseen_mono {V seen seen' : List Pl} (h : ∀ v, v ∈ seen → v ∈ seen') :
    unseen V seen' ≤ unseen V seen := by
  apply List.countP_mono_left
  intro v _ hv
  simp only [Bool.not_eq_true', List.contains_eq_mem, decide_eq_false_iff_not] at hv ⊢
  exact fun hm => hv (h v hm)

theorem unseen_lt {V seen : List Pl} {q : Pl} (hq : q ∈ V) (hs : q ∉ seen) :
    unseen V (q :: seen) < unseen V seen := by
  induction V with
  | nil => cases hq
  | cons v V ih =>
    have hm : unseen V (q :: seen) ≤ unseen V seen :=
      unseen_mono fun v hv => List.mem_cons_of_mem _ hv
    simp only [unseen, List.countP_cons] at hm ih ⊢
    by_cases hvq : v = q
    · have h1 : (q :: seen).contains v = true := by simp [hvq]
      have h2 : seen.contains v = false := by simpa [hvq] using hs
      rw [h1, h2]
      simp only [Bool.not_true, Bool.not_false, Bool.false_eq_true, ite_false, ite_true]
      omega
    · have hq' : q ∈ V := by
        rcases List.mem_cons.mp hq with h | h
        · exact absurd h.symm hvq
        · exact h
      have ih := ih hq'
      have he : (q :: seen).contains v = seen.contains v := by
        simp [hvq]
      rw [he]
      omega

/-- The walk keeps what it had seen, reaches the place it starts from, and holds every
successor of each place it adds. -/
theorem dfs_fold (P : Prog) (on : Nat → Bool) (V : List Pl) (fuel : Nat)
    (ih : ∀ q seen, q ∈ V → unseen V seen ≤ fuel →
      (∀ v, v ∈ seen → v ∈ dfs P on fuel q seen) ∧ q ∈ dfs P on fuel q seen ∧
      ∀ v, v ∈ dfs P on fuel q seen → v ∉ seen →
        ∀ e, e ∈ P.succs on v → e ∈ dfs P on fuel q seen) :
    ∀ (ds acc : List Pl), (∀ d, d ∈ ds → d ∈ V) → unseen V acc ≤ fuel →
      (∀ v, v ∈ acc → v ∈ ds.foldl (fun acc d => dfs P on fuel d acc) acc) ∧
      (∀ d, d ∈ ds → d ∈ ds.foldl (fun acc d => dfs P on fuel d acc) acc) ∧
      ∀ v, v ∈ ds.foldl (fun acc d => dfs P on fuel d acc) acc → v ∉ acc →
        ∀ e, e ∈ P.succs on v → e ∈ ds.foldl (fun acc d => dfs P on fuel d acc) acc := by
  intro ds
  induction ds with
  | nil =>
    intro acc _ _
    refine ⟨fun v hv => hv, ?_, fun v hv hnv => absurd hv hnv⟩
    intro d hd
    cases hd
  | cons d ds ihd =>
    intro acc hds hf
    obtain ⟨h1, h2, h3⟩ := ih d acc (hds d (List.mem_cons_self ..)) hf
    obtain ⟨g1, g2, g3⟩ := ihd (dfs P on fuel d acc)
      (fun e he => hds e (List.mem_cons_of_mem _ he)) (Nat.le_trans (unseen_mono h1) hf)
    simp only [List.foldl_cons]
    refine ⟨fun v hv => g1 v (h1 v hv), ?_, ?_⟩
    · intro e he
      rcases List.mem_cons.mp he with rfl | he
      · exact g1 _ h2
      · exact g2 e he
    · intro v hv hnv e he
      by_cases hv1 : v ∈ dfs P on fuel d acc
      · exact g1 _ (h3 v hv1 hnv e he)
      · exact g3 v hv hv1 e he

theorem dfs_spec (P : Prog) (on : Nat → Bool) (V : List Pl)
    (hV : ∀ v d, d ∈ P.succs on v → d ∈ V) :
    ∀ fuel q seen, q ∈ V → unseen V seen ≤ fuel →
      (∀ v, v ∈ seen → v ∈ dfs P on fuel q seen) ∧ q ∈ dfs P on fuel q seen ∧
      ∀ v, v ∈ dfs P on fuel q seen → v ∉ seen →
        ∀ e, e ∈ P.succs on v → e ∈ dfs P on fuel q seen := by
  intro fuel
  induction fuel with
  | zero =>
    intro q seen hq hf
    have hqs : q ∈ seen := by
      apply Classical.byContradiction
      intro hn
      have := unseen_lt hq hn
      omega
    simp only [dfs]
    exact ⟨fun v hv => hv, hqs, fun v hv hnv => absurd hv hnv⟩
  | succ fuel ih =>
    intro q seen hq hf
    by_cases hqs : q ∈ seen
    · have hc : seen.contains q = true := by simpa using hqs
      simp only [dfs, hc, ite_true]
      exact ⟨fun v hv => hv, hqs, fun v hv hnv => absurd hv hnv⟩
    · have hc : seen.contains q = false := by simpa using hqs
      have hlt := unseen_lt hq hqs
      obtain ⟨g1, g2, g3⟩ := dfs_fold P on V fuel ih (P.succs on q) (q :: seen)
        (fun d hd => hV q d hd) (by omega)
      simp only [dfs, hc, Bool.false_eq_true, ite_false]
      refine ⟨fun v hv => g1 v (List.mem_cons_of_mem _ hv), g1 q (List.mem_cons_self ..), ?_⟩
      intro v hv hnv e he
      by_cases hvq : v = q
      · subst hvq
        exact g2 e he
      · exact g3 v hv (fun hm => by
          rcases List.mem_cons.mp hm with h | h
          · exact hvq h
          · exact hnv h) e he

theorem Hangs.mono {P : Prog} {on on' : Nat → Bool} {q p : Pl}
    (h : ∀ k, k < P.borrows.length → on k = true → on' k = true) (hh : Hangs P on q p) :
    Hangs P on' q p := by
  induction hh with
  | direct hd => exact .direct (succs_mono h hd)
  | via _ hd ih => exact .via ih (succs_mono h hd)

theorem Hangs.tracked {P : Prog} {on : Nat → Bool} {q p : Pl} (h : P.wf = true)
    (hh : Hangs P on q p) : p ∈ P.tracked := by
  cases hh with
  | direct hd => exact succs_tracked h hd
  | via _ hd => exact succs_tracked h hd

/-- own.rs `drop` marks every place a drop of `r` leaves dangling. -/
theorem hangs_dfs {P : Prog} {on : Nat → Bool} {r p : Pl} (h : P.wf = true)
    (hr : r ∈ P.tracked) :
    r ∈ dfs P on (P.tracked.length + 1) r [] ∧
    (Hangs P on r p → p ∈ dfs P on (P.tracked.length + 1) r []) := by
  have hle : unseen P.tracked [] ≤ P.tracked.length + 1 :=
    Nat.le_trans List.countP_le_length (Nat.le_succ _)
  obtain ⟨_, g2, g3⟩ := dfs_spec P on P.tracked (fun _ _ hd => succs_tracked h hd)
    (P.tracked.length + 1) r [] hr hle
  refine ⟨g2, fun hh => ?_⟩
  induction hh with
  | direct hd => exact g3 r g2 (by simp) _ hd
  | via _ hd ih => exact g3 _ ih (by simp) _ hd

/-! ## Proof: the analysis state describes the store -/

/-- The analysis state `s` describes the store `σ`. For each pairing of a place `q`, under
the value its guarding flag holds (either value when it has none), that half is reachable
in `s`, and holds the dropped state when `q` is not live. A place that is not live is
tracked, and a borrow in force has its bit set. -/
structure Abs (P : Prog) (σ : Store) (s : AState) : Prop where
  half : ∀ x q b, P.owner x = some q → (∀ f, P.guard x = some f → b = σ.flag f) →
    (s.bits x).half b = true
  dead : ∀ x q b, P.owner x = some q → σ.st q ≠ .live →
    (∀ f, P.guard x = some f → b = σ.flag f) → (s.bits x).d b = true
  tracked : ∀ q, σ.st q ≠ .live → q ∈ P.tracked
  loan : ∀ k, k < P.borrows.length → σ.loan k = true → s.lend k = true

theorem Abs.mono {P : Prog} {σ : Store} {s t : AState} (hA : Abs P σ s)
    (hle : s.le P t = true) : Abs P σ t := by
  unfold AState.le at hle
  simp only [Bool.and_eq_true, List.all_eq_true, List.mem_range] at hle
  obtain ⟨hb, hl⟩ := hle
  refine ⟨?_, ?_, hA.tracked, ?_⟩
  · intro x q b ho hs
    exact Bits.le_half (hb x (owner_lt ho)) (hA.half x q b ho hs)
  · intro x q b ho hq hs
    exact Bits.le_d (hb x (owner_lt ho)) (hA.dead x q b ho hq hs)
  · intro k hk hlo
    have := hl k hk
    simpa [hA.loan k hk hlo] using this

theorem Abs.init {P : Prog} {σ : Store} (h : Initial σ) : Abs P σ AState.init := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro x q b _ _
    cases b <;> rfl
  · intro x q b _ hq
    exact absurd (h.1 q) hq
  · intro q hq
    exact absurd (h.1 q) hq
  · intro k _ _
    rfl

/-- A place that is not live fails `may_drop`'s negation: every check on it rejects. -/
theorem mayDrop_of_dead {P : Prog} {σ : Store} {s : AState} {q : Pl} (hA : Abs P σ s)
    (hq : σ.st q ≠ .live) : mayDrop P s q = true := by
  unfold mayDrop
  rw [List.all_eq_true]
  intro x _
  cases ho : P.owner x with
  | none => simp
  | some q' =>
    by_cases hqq : q' = q
    · subst hqq
      have hd := hA.dead x q' (match P.guard x with | some f => σ.flag f | none => true) ho hq
        (by intro f hf; simp [hf])
      simp [Bits.d_anyD hd]
    · simp [hqq]

/-- The checks of a use or a drop reject an address one of whose names overlaps a place
that is not live. -/
theorem checked_live {P : Prog} {σ : Store} {s : AState} {names : List Pl} {r p : Pl}
    (hA : Abs P σ s) (hr : r ∈ names) (ho : overlap r p = true) (hp : σ.st p ≠ .live)
    (hc : ∀ q, q ∈ P.tracked → (∃ r, r ∈ names ∧ overlap r q = true) →
      mayDrop P s q = false) : False := by
  have := hc p (hA.tracked p hp) ⟨r, hr, ho⟩
  rw [mayDrop_of_dead hA hp] at this
  cases this

theorem use_safe {P : Prog} {σ : Store} {s : AState} {a : Nat} {r p : Pl} (hA : Abs P σ s)
    (hc : checkAct P s (.use a) = true) (hr : r ∈ P.names a ++ P.derefs a)
    (ho : overlap r p = true) (hp : σ.st p ≠ .live) : False := by
  simp only [checkAct, List.all_eq_true, List.mem_append, Bool.not_eq_true'] at hc
  apply checked_live hA hr ho hp
  intro q hq ⟨r', hr', ho'⟩
  rcases List.mem_append.mp hr' with h | h
  · exact hc q (Or.inl (mem_hits.mpr ⟨hq, r', h, ho'⟩))
  · exact hc q (Or.inr (mem_derefHits.mpr ⟨hq, r', h, ho'⟩))

theorem drop_safe {P : Prog} {σ : Store} {s : AState} {a : Nat} {r p : Pl} (hA : Abs P σ s)
    (hc : checkAct P s (.drop a) = true) (hr : r ∈ P.names a)
    (ho : overlap r p = true) (hp : σ.st p ≠ .live) : False := by
  simp only [checkAct, Bool.and_eq_true, List.all_eq_true, Bool.not_eq_true'] at hc
  apply checked_live hA hr ho hp
  intro q hq hex
  exact hc.2 q (mem_hits.mpr ⟨hq, hex⟩)

/-! ### Drop -/

theorem dropAll_half (P : Prog) (s : AState) (R : List Pl) (strong : Bool) (x : Nat)
    (b : Bool) : ((dropAll P s R strong).bits x).half b = (s.bits x).half b := by
  simp only [dropAll]
  split
  · split
    · exact Bits.dropped_half ..
    · rfl
  · rfl

theorem dropAll_d {P : Prog} {s : AState} {R : List Pl} {strong : Bool} {x : Nat} {b : Bool}
    (h : (s.bits x).d b = true) : ((dropAll P s R strong).bits x).d b = true := by
  simp only [dropAll]
  split
  · split
    · rw [Bits.dropped_d]
      exact Bits.d_half h
    · exact h
  · exact h

theorem dropAll_mark {P : Prog} {s : AState} {R : List Pl} {strong : Bool} {x : Nat} {b : Bool}
    {q : Pl} (ho : P.owner x = some q) (hq : q ∈ R) (h : (s.bits x).half b = true) :
    ((dropAll P s R strong).bits x).d b = true := by
  have hc : R.contains q = true := by simpa using hq
  simp only [dropAll, ho, hc, ite_true]
  rw [Bits.dropped_d]
  exact h

/-- What the drops of an address's names do to the analysis state: borrows and reachable
halves stay, a dropped state stays, and each place the walk from a tracked uncut name
reaches is dropped in every reachable half. -/
theorem dropRoots_spec (P : Prog) (strong : Bool) :
    ∀ (roots : List Pl) (s : AState),
      (dropRoots P s roots strong).lend = s.lend ∧
      (∀ x b, ((dropRoots P s roots strong).bits x).half b = (s.bits x).half b) ∧
      (∀ x b, (s.bits x).d b = true → ((dropRoots P s roots strong).bits x).d b = true) ∧
      ∀ r, r ∈ roots → r.cut = false → r ∈ P.tracked →
        ∀ q, q ∈ dfs P s.lend (P.tracked.length + 1) r [] →
          ∀ x b, P.owner x = some q → (s.bits x).half b = true →
            ((dropRoots P s roots strong).bits x).d b = true := by
  intro roots
  induction roots with
  | nil =>
    intro s
    refine ⟨rfl, fun _ _ => rfl, fun _ _ h => h, ?_⟩
    intro r hr
    cases hr
  | cons r roots ih =>
    intro s
    by_cases hc : (!r.cut && P.tracked.contains r) = true
    · have hstep : dropRoots P s (r :: roots) strong =
          dropRoots P (dropPlace P s r strong) roots strong := by
        simp only [dropRoots, List.foldl_cons, hc, ite_true]
      obtain ⟨g1, g2, g3, g4⟩ := ih (dropPlace P s r strong)
      rw [hstep]
      refine ⟨g1, ?_, ?_, ?_⟩
      · intro x b
        rw [g2]
        exact dropAll_half ..
      · intro x b h
        exact g3 x b (dropAll_d h)
      · intro r' hr' hcut ht q hq x b ho hh
        rcases List.mem_cons.mp hr' with rfl | hr'
        · exact g3 x b (dropAll_mark ho hq hh)
        · exact g4 r' hr' hcut ht q hq x b ho (by
            rw [show ((dropPlace P s r strong).bits x).half b = (s.bits x).half b from
              dropAll_half ..]
            exact hh)
    · have hstep : dropRoots P s (r :: roots) strong = dropRoots P s roots strong := by
        simp only [dropRoots, List.foldl_cons, hc, Bool.false_eq_true, ite_false]
      obtain ⟨g1, g2, g3, g4⟩ := ih s
      rw [hstep]
      refine ⟨g1, g2, g3, ?_⟩
      intro r' hr' hcut ht q hq x b ho hh
      rcases List.mem_cons.mp hr' with rfl | hr'
      · exact absurd (by simp [hcut, ht]) hc
      · exact g4 r' hr' hcut ht q hq x b ho hh

/-- A drop that changes nothing in the store keeps the description. -/
theorem abs_dropRoots {P : Prog} {σ : Store} {s : AState} (hA : Abs P σ s) (roots : List Pl)
    (strong : Bool) : Abs P σ (dropRoots P s roots strong) := by
  obtain ⟨f1, f2, f3, _⟩ := dropRoots_spec P strong roots s
  refine ⟨?_, ?_, hA.tracked, ?_⟩
  · intro x q b ho hs
    rw [f2]
    exact hA.half x q b ho hs
  · intro x q b ho hq hs
    exact f3 x b (hA.dead x q b ho hq hs)
  · intro k hk hl
    rw [f1]
    exact hA.loan k hk hl

/-- A drop of a tracked place `r`: the analysis marks `r` and every view left dangling. -/
theorem abs_drop {P : Prog} {σ σ' : Store} {s : AState} {roots : List Pl} {strong : Bool}
    {r : Pl} (hwf : P.wf = true) (hA : Abs P σ s) (hr : r ∈ roots) (hcut : r.cut = false)
    (ht : r ∈ P.tracked)
    (hn : ∀ p, p ≠ r → ¬ Hangs P σ.loan r p → σ'.st p = σ.st p)
    (hfl : σ'.flag = σ.flag) (hlo : σ'.loan = σ.loan) :
    Abs P σ' (dropRoots P s roots strong) := by
  obtain ⟨f1, f2, f3, f4⟩ := dropRoots_spec P strong roots s
  have reach : ∀ p, p = r ∨ Hangs P σ.loan r p →
      p ∈ dfs P s.lend (P.tracked.length + 1) r [] := by
    intro p hp
    rcases hp with rfl | hp
    · exact (hangs_dfs hwf ht (p := p)).1
    · exact (hangs_dfs hwf ht).2 (hp.mono hA.loan)
  have hsee : ∀ {x b}, (∀ f, P.guard x = some f → b = σ'.flag f) →
      ∀ f, P.guard x = some f → b = σ.flag f := by
    intro x b hs f hf
    rw [← hfl]
    exact hs f hf
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro x q b ho hs
    rw [f2]
    exact hA.half x q b ho (hsee hs)
  · intro x q b ho hq hs
    by_cases hqr : q = r ∨ Hangs P σ.loan r q
    · exact f4 r hr hcut ht q (reach q hqr) x b ho (hA.half x q b ho (hsee hs))
    · have he : σ'.st q = σ.st q := hn q (fun h => hqr (Or.inl h)) (fun h => hqr (Or.inr h))
      exact f3 x b (hA.dead x q b ho (he ▸ hq) (hsee hs))
  · intro q hq
    by_cases hqr : q = r
    · exact hqr ▸ ht
    · by_cases hh : Hangs P σ.loan r q
      · exact hh.tracked hwf
      · exact hA.tracked q (hn q hqr hh ▸ hq)
  · intro k hk hl
    rw [f1]
    exact hA.loan k hk (hlo ▸ hl)

/-! ### Write and begin -/

theorem beginAll_half (P : Prog) (s : AState) (Q : List Pl) (x : Nat) (b : Bool) :
    ((beginAll P s Q).bits x).half b = (s.bits x).half b := by
  simp only [beginAll]
  split
  · split
    · exact Bits.begun_half ..
    · rfl
  · rfl

theorem beginAll_bits {P : Prog} {s : AState} {Q : List Pl} {x : Nat} {q : Pl}
    (ho : P.owner x = some q) (hq : q ∉ Q) : (beginAll P s Q).bits x = s.bits x := by
  have hc : Q.contains q = false := by simpa using hq
  simp only [beginAll, ho, hc, Bool.false_eq_true, ite_false]

/-- The store moved toward live: places only came alive and borrows only ended, and the
flags that guard a pairing kept their values. The same analysis state describes it. -/
theorem abs_revive {P : Prog} {σ σ' : Store} {s : AState} (hA : Abs P σ s)
    (hst : ∀ q, σ'.st q ≠ .live → σ.st q ≠ .live)
    (hfl : ∀ x f, P.guard x = some f → σ'.flag f = σ.flag f)
    (hlo : ∀ k, σ'.loan k = true → σ.loan k = true) : Abs P σ' s := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro x q b ho hs
    exact hA.half x q b ho fun f hf => (hs f hf).trans (hfl x f hf)
  · intro x q b ho hq hs
    exact hA.dead x q b ho (hst q hq) fun f hf => (hs f hf).trans (hfl x f hf)
  · intro q hq
    exact hA.tracked q (hst q hq)
  · intro k hk hl
    exact hA.loan k hk (hlo k hl)

/-- The places of `Q` begin anew: every place that is still not live was so before and is
outside `Q`, and every borrow still in force was so before and has no dest in `Q`. -/
theorem abs_beginAll {P : Prog} {σ σ' : Store} {s : AState} {Q : List Pl} (hA : Abs P σ s)
    (hst : ∀ q, σ'.st q ≠ .live → σ.st q ≠ .live ∧ q ∉ Q)
    (hfl : ∀ x f, P.guard x = some f → σ'.flag f = σ.flag f)
    (hlo : ∀ k b, P.borrows[k]? = some b → σ'.loan k = true →
      σ.loan k = true ∧ ∀ q, q ∈ b.dest → q ∉ Q) : Abs P σ' (beginAll P s Q) := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro x q b ho hs
    rw [beginAll_half]
    exact hA.half x q b ho fun f hf => (hs f hf).trans (hfl x f hf)
  · intro x q b ho hq hs
    rw [beginAll_bits ho (hst q hq).2]
    exact hA.dead x q b ho (hst q hq).1 fun f hf => (hs f hf).trans (hfl x f hf)
  · intro q hq
    exact hA.tracked q (hst q hq).1
  · intro k hk hl
    have hb : P.borrows[k]? = some P.borrows[k] := List.getElem?_eq_getElem hk
    obtain ⟨h1, h2⟩ := hlo k _ hb hl
    have hany : (P.borrows[k].dest.any fun q => Q.contains q) = false := by
      rw [List.any_eq_false]
      intro q hq
      simpa using h2 q hq
    simp only [beginAll, hb, hA.loan k hk h1, hany, Bool.not_false, Bool.and_self]

/-! ### One act -/

/-- A result the analysis state `s` describes: a store it describes, never a fault. -/
def Res.Good (P : Prog) (s : AState) : Res → Prop
  | .ok σ => Abs P σ s
  | .fault _ => False

theorem abs_flag {P : Prog} {σ : Store} {s : AState} {f : Nat} {v : Option Bool} {b : Bool}
    (hA : Abs P σ s) (hb : ∀ c, v = some c → b = c) :
    Abs P { σ with flag := fun g => if g == f then b else σ.flag g }
      (applyAct P s (.flag f v)) := by
  have hother : ∀ {x b'}, P.guard x ≠ some f →
      (∀ g, P.guard x = some g → b' = if g == f then b else σ.flag g) →
      ∀ g, P.guard x = some g → b' = σ.flag g := by
    intro x b' hg hs g hx
    have hgf : (g == f) = false := by
      simp only [beq_eq_false_iff_ne, ne_eq]
      intro h
      exact hg (h ▸ hx)
    have := hs g hx
    rwa [hgf] at this
  refine ⟨?_, ?_, hA.tracked, hA.loan⟩
  · intro x q b' ho hs
    by_cases hg : P.guard x = some f
    · have hb' : b' = b := by simpa using hs f hg
      subst hb'
      simp only [applyAct, hg, beq_self_eq_true, ite_true]
      exact Bits.flag_half hb (hA.half x q (σ.flag f) ho fun g hx => by
        rw [hg] at hx
        cases hx
        rfl)
    · have hgb : (P.guard x == some f) = false := by simpa using hg
      simp only [applyAct, hgb, Bool.false_eq_true, ite_false]
      exact hA.half x q b' ho (hother hg hs)
  · intro x q b' ho hq hs
    by_cases hg : P.guard x = some f
    · have hb' : b' = b := by simpa using hs f hg
      subst hb'
      simp only [applyAct, hg, beq_self_eq_true, ite_true]
      exact Bits.flag_d hb (hA.dead x q (σ.flag f) ho hq fun g hx => by
        rw [hg] at hx
        cases hx
        rfl)
    · have hgb : (P.guard x == some f) = false := by simpa using hg
      simp only [applyAct, hgb, Bool.false_eq_true, ite_false]
      exact hA.dead x q b' ho hq (hother hg hs)

theorem abs_write {S : Sem} {P : Prog} (hyp : Hyp S P) {σ : Store} {s : AState} {a : Nat}
    {r : Pl} {fl : Nat → Bool} (hA : Abs P σ s) (hin : Act.write a ∈ P.allActs)
    (hr : r ∈ P.names a) (hfl : ∀ f, f ≠ r.root → fl f = σ.flag f) :
    Abs P (σ.write S P r fl) (applyAct P s (.write a)) := by
  have hg : ∀ x f, P.guard x = some f → fl f = σ.flag f := by
    intro x f hx
    apply hfl
    intro hfr
    have h := hyp.flagsStored
    unfold Prog.flagsStored at h
    simp only [List.all_eq_true] at h
    have := h _ hin
    simp only [List.all_eq_true, List.mem_range] at this
    have := this r hr x (guard_lt hx)
    simp [hx, hfr] at this
  by_cases hcut : r.cut = true
  · have hs : applyAct P s (.write a) = s := by
      simp only [applyAct]
      split
      · rename_i r' heq
        rw [heq] at hr
        have : r = r' := by simpa using hr
        subst this
        simp only [hcut, ite_true]
      · rfl
    rw [hs]
    simp only [Store.write, hcut, ite_true]
    exact abs_revive hA (fun q hq => hq) hg (fun k hk => hk)
  · have hcut : r.cut = false := by simpa using hcut
    have hst : ∀ q, (σ.write S P r fl).st q ≠ .live → σ.st q ≠ .live ∧ S.revive r q = false := by
      intro q hq
      simp only [Store.write, hcut, Bool.false_eq_true, ite_false] at hq
      by_cases hrev : S.revive r q = true
      · simp [hrev] at hq
      · have hrev : S.revive r q = false := by simpa using hrev
        simp only [hrev, Bool.false_eq_true, ite_false] at hq
        exact ⟨hq, hrev⟩
    have hlo : ∀ k b, P.borrows[k]? = some b → (σ.write S P r fl).loan k = true →
        σ.loan k = true ∧ ∀ q, q ∈ b.dest → S.revive r q = false := by
      intro k b hb hl
      simp only [Store.write, hcut, Bool.false_eq_true, ite_false, hb, Bool.and_eq_true,
        Bool.not_eq_true', List.any_eq_false] at hl
      exact ⟨hl.1, fun q hq => by simpa using hl.2 q hq⟩
    have hflag : (σ.write S P r fl).flag = fl := by
      simp only [Store.write, hcut, Bool.false_eq_true, ite_false]
    cases hn : P.names a with
    | nil => simp [hn] at hr
    | cons r' rest =>
      cases rest with
      | nil =>
        rw [hn] at hr
        have : r = r' := by simpa using hr
        subst this
        have hs : applyAct P s (.write a) = beginAll P s (P.hits a) := by
          simp only [applyAct, hn, hcut, Bool.false_eq_true, ite_false]
        rw [hs]
        have hmiss : ∀ q, q ∈ P.tracked → S.revive r q = false → q ∉ P.hits a := by
          intro q hq hrev hh
          obtain ⟨_, r', hr', ho⟩ := mem_hits.mp hh
          rw [hn] at hr'
          have : r' = r := by simpa using hr'
          subst this
          have := hyp.writesBegin a r' hin hn hcut q hq ho
          rw [hrev] at this
          cases this
        refine abs_beginAll hA ?_ (fun x f hx => by rw [hflag]; exact hg x f hx) ?_
        · intro q hq
          obtain ⟨h1, h2⟩ := hst q hq
          exact ⟨h1, hmiss q (hA.tracked q h1) h2⟩
        · intro k b hb hl
          obtain ⟨h1, h2⟩ := hlo k b hb hl
          exact ⟨h1, fun q hq => hmiss q (wf_dest hyp.wf (List.mem_of_getElem? hb) hq) (h2 q hq)⟩
      | cons r'' rest =>
        have hs : applyAct P s (.write a) = s := by
          simp only [applyAct, hn]
        rw [hs]
        refine abs_revive hA (fun q hq => (hst q hq).1)
          (fun x f hx => by rw [hflag]; exact hg x f hx) ?_
        intro k hl
        simp only [Store.write, hcut, Bool.false_eq_true, ite_false, Bool.and_eq_true] at hl
        exact hl.1

theorem abs_begin {P : Prog} {σ : Store} {s : AState} {slot : Nat} (hA : Abs P σ s) :
    Abs P (σ.begin P slot) (applyAct P s (.begin slot)) := by
  simp only [applyAct]
  refine abs_beginAll hA ?_ (fun _ _ _ => rfl) ?_
  · intro q hq
    simp only [Store.begin] at hq
    by_cases hroot : (q.root == slot) = true
    · simp [hroot] at hq
    · have hroot : (q.root == slot) = false := by simpa using hroot
      simp only [hroot, Bool.false_eq_true, ite_false] at hq
      refine ⟨hq, fun hm => ?_⟩
      have := (List.mem_filter.mp hm).2
      rw [hroot] at this
      cases this
  · intro k b hb hl
    simp only [Store.begin, hb, Bool.and_eq_true, Bool.not_eq_true', List.any_eq_false] at hl
    refine ⟨hl.1, fun q hq hm => ?_⟩
    exact hl.2 q hq (List.mem_filter.mp hm).2

/-- One act the checks pass: the machine does not fault, and the analysis state after the
act describes the store after it. -/
theorem act_sound {S : Sem} {P : Prog} (hyp : Hyp S P) {σ : Store} {s : AState} {a : Act}
    {res : Res} (hA : Abs P σ s) (hin : a ∈ P.allActs) (hc : checkAct P s a = true)
    (hstep : ActStep S P σ a res) : res.Good P (applyAct P s a) := by
  cases hstep with
  | use_ok => exact hA
  | use_dropped hr ho hp => exact use_safe hA hc hr ho (by rw [hp]; decide)
  | use_dangling hr ho hp => exact use_safe hA hc hr ho (by rw [hp]; decide)
  | drop_view hr hv =>
    simp only [checkAct, Bool.and_eq_true, List.all_eq_true, Bool.not_eq_true'] at hc
    have := hc.1 _ hr
    rw [hv] at this
    cases this
  | drop_dropped hr ho hp => exact drop_safe hA hc hr ho (by rw [hp]; decide)
  | drop_dangling hr ho hp => exact drop_safe hA hc hr ho (by rw [hp]; decide)
  | drop_cut => exact abs_dropRoots hA _ _
  | drop_ok hr _ _ hcut _ _ hn hfl hlo =>
    exact abs_drop hyp.wf hA hr hcut (wf_drop hyp.wf hin hr hcut) hn hfl hlo
  | drop_unnamed => exact abs_dropRoots hA _ _
  | write hr hfl => exact abs_write hyp hA hin hr hfl
  | write_unnamed hn =>
    simp only [Res.Good, applyAct, hn]
    exact hA
  | «begin» => exact abs_begin hA
  | flag hb => exact abs_flag hA hb
  | lend =>
    refine ⟨hA.half, hA.dead, hA.tracked, ?_⟩
    intro j hj hl
    simp only [applyAct, Bool.or_eq_true] at hl ⊢
    rcases hl with h | h
    · exact Or.inl h
    · exact Or.inr (hA.loan j hj h)

theorem acts_sound {S : Sem} {P : Prog} (hyp : Hyp S P) {σ : Store} {s : AState}
    {acts : List Act} {res : Res} (hA : Abs P σ s) (hin : ∀ a, a ∈ acts → a ∈ P.allActs)
    (hc : checkActs P s acts = true) (hstep : ActsStep S P σ acts res) :
    res.Good P (applyActs P s acts) := by
  induction hstep generalizing s with
  | nil => exact hA
  | cons h1 _ ih =>
    simp only [checkActs, Bool.and_eq_true] at hc
    exact ih (act_sound hyp hA (hin _ (List.mem_cons_self ..)) hc.1 h1)
      (fun a ha => hin a (List.mem_cons_of_mem _ ha)) hc.2
  | fault h1 =>
    simp only [checkActs, Bool.and_eq_true] at hc
    exact (act_sound hyp hA (hin _ (List.mem_cons_self ..)) hc.1 h1).elim

/-! ### Blocks and edges -/

theorem applyInsts_take_succ {P : Prog} {s : AState} {insts : List Inst} {pc : Nat} {i : Inst}
    (h : insts[pc]? = some i) :
    applyInsts P s (insts.take (pc + 1)) = applyActs P (applyInsts P s (insts.take pc)) i.acts := by
  rw [List.take_add_one, h]
  simp only [applyInsts, Option.toList_some, List.foldl_append, List.foldl_cons, List.foldl_nil]

theorem instsOk_at {P : Prog} : ∀ (insts : List Inst) (s : AState) (pc : Nat) (i : Inst),
    instsOk P s insts = true → insts[pc]? = some i →
    checkActs P (applyInsts P s (insts.take pc)) i.acts = true
  | [], _, _, _, _, h => by simp at h
  | j :: insts, s, 0, i, hok, h => by
    simp only [List.getElem?_cons_zero, Option.some.injEq] at h
    subst h
    simp only [instsOk, Bool.and_eq_true] at hok
    exact hok.1
  | j :: insts, s, pc + 1, i, hok, h => by
    simp only [List.getElem?_cons_succ] at h
    simp only [instsOk, Bool.and_eq_true] at hok
    exact instsOk_at insts _ pc i hok.2 h

theorem failOuts_at {P : Prog} : ∀ (insts : List Inst) (s : AState) (pc : Nat) (i : Inst)
    (t : Nat), insts[pc]? = some i → i.fail = some t →
    (t, if i.isDrop then applyActs P (applyInsts P s (insts.take pc)) i.acts
        else applyInsts P s (insts.take pc)) ∈ failOuts P s insts
  | [], _, _, _, _, h, _ => by simp at h
  | j :: insts, s, 0, i, t, h, hf => by
    simp only [List.getElem?_cons_zero, Option.some.injEq] at h
    subst h
    simp only [failOuts, hf, List.mem_append]
    left
    exact List.mem_singleton.mpr rfl
  | j :: insts, s, pc + 1, i, t, h, hf => by
    simp only [List.getElem?_cons_succ] at h
    simp only [failOuts, List.mem_append]
    right
    exact failOuts_at insts _ pc i t h hf

theorem validate_entry {P : Prog} {start : Nat → Option AState} (h : validate P start = true) :
    ∃ s, start 0 = some s ∧ AState.init.le P s = true := by
  unfold validate at h
  simp only [Bool.and_eq_true] at h
  cases hs : start 0 with
  | none => simp [hs] at h
  | some s => exact ⟨s, rfl, by simpa [hs] using h.1⟩

theorem validate_block {P : Prog} {start : Nat → Option AState} {b : Nat} {blk : Block}
    {s : AState} (h : validate P start = true) (hb : P.blocks[b]? = some blk)
    (hs : start b = some s) :
    blockOk P blk s = true ∧ ∀ t s', (t, s') ∈ blockOuts P blk s →
      t = 0 ∨ ∃ s'', start t = some s'' ∧ s'.le P s'' = true := by
  unfold validate at h
  simp only [Bool.and_eq_true, List.all_eq_true, List.mem_range] at h
  obtain ⟨hlt, _⟩ := List.getElem?_eq_some_iff.mp hb
  have hblk := h.2 b hlt
  simp only [hb, hs, Bool.and_eq_true, List.all_eq_true] at hblk
  refine ⟨hblk.1, fun t s' hm => ?_⟩
  have hout := hblk.2 (t, s') hm
  simp only [Bool.or_eq_true, beq_iff_eq] at hout
  rcases hout with h0 | h1
  · exact Or.inl h0
  · right
    cases hst : start t with
    | none => simp [hst] at h1
    | some s'' => exact ⟨s'', rfl, by simpa [hst] using h1⟩

theorem abs_refine {P : Prog} {σ : Store} {s : AState} {f : Nat} {v : Bool} (hA : Abs P σ s)
    (hv : v = σ.flag f) : Abs P σ (refine P s f v) := by
  refine ⟨?_, ?_, hA.tracked, hA.loan⟩
  · intro x q b ho hs
    by_cases hg : P.guard x = some f
    · have hb : b = v := by rw [hv]; exact hs f hg
      subst hb
      simp only [refine, hg, beq_self_eq_true, ite_true]
      rw [Bits.keep_half]
      exact hA.half x q b ho hs
    · have hgb : (P.guard x == some f) = false := by simpa using hg
      simp only [refine, hgb, Bool.false_eq_true, ite_false]
      exact hA.half x q b ho hs
  · intro x q b ho hq hs
    by_cases hg : P.guard x = some f
    · have hb : b = v := by rw [hv]; exact hs f hg
      subst hb
      simp only [refine, hg, beq_self_eq_true, ite_true]
      rw [Bits.keep_d]
      exact hA.dead x q b ho hq hs
    · have hgb : (P.guard x == some f) = false := by simpa using hg
      simp only [refine, hgb, Bool.false_eq_true, ite_false]
      exact hA.dead x q b ho hq hs

/-! ## Soundness -/

/-- The invariant of a run: at instruction `pc` of block `b`, the block's start state
carried through its first `pc` instructions describes the store. A fault never holds it. -/
def Inv (P : Prog) (start : Nat → Option AState) : Cfg → Prop
  | .run b pc σ => ∃ s, start b = some s ∧
      ∀ blk, P.blocks[b]? = some blk → Abs P σ (applyInsts P s (blk.insts.take pc))
  | .fault _ => False
  | .done _ => True

theorem inv_initial {P : Prog} {start : Nat → Option AState} {σ : Store}
    (hv : validate P start = true) (hi : Initial σ) : Inv P start (.run 0 0 σ) := by
  obtain ⟨s, hs, hle⟩ := validate_entry hv
  exact ⟨s, hs, fun _ _ => (Abs.init hi).mono hle⟩

/-- An edge the block sends out lands on a start state that describes the store. -/
theorem inv_arrive {P : Prog} {start : Nat → Option AState} {b t : Nat} {blk : Block}
    {s s' : AState} {σ : Store} (hv : validate P start = true)
    (hb : P.blocks[b]? = some blk) (hs : start b = some s)
    (hout : (t, s') ∈ blockOuts P blk s) (ht : t ≠ 0) (hA : Abs P σ s') :
    Inv P start (.run t 0 σ) := by
  rcases (validate_block hv hb hs).2 t s' hout with h0 | ⟨s'', hs'', hle⟩
  · exact absurd h0 ht
  · exact ⟨s'', hs'', fun _ _ => hA.mono hle⟩

/-- **Preservation.** A step from a configuration the accepted analysis describes reaches
one it describes; in particular it does not fault. -/
theorem preservation {S : Sem} {P : Prog} {start : Nat → Option AState} (hyp : Hyp S P)
    (hv : validate P start = true) {c c' : Cfg} (hinv : Inv P start c)
    (hstep : Step S P c c') : Inv P start c' := by
  cases hstep with
  | inst hb hi hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true] at hok
    refine ⟨s, hs, fun blk' hb' => ?_⟩
    have : blk' = _ := Option.some.inj (hb'.symm.trans hb)
    subst this
    rw [applyInsts_take_succ hi]
    exact acts_sound hyp (hA _ hb) (fun a ha => mem_allActs_inst hb hi ha)
      (instsOk_at _ _ _ _ hok.1 hi) hacts
  | instFault hb hi hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true] at hok
    exact acts_sound hyp (hA _ hb) (fun a ha => mem_allActs_inst hb hi ha)
      (instsOk_at _ _ _ _ hok.1 hi) hacts
  | fail hb hi hf hd =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hout := failOuts_at (P := P) _ s _ _ _ hi hf
    simp only [hd, Bool.false_eq_true, ite_false] at hout
    exact inv_arrive hv hb hs (List.mem_append_left _ hout)
      (target_ne_zero hyp.noEntryEdge (mem_targets_fail hb hi hf)) (hA _ hb)
  | dropFail hb hi hf hd hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true] at hok
    have hout := failOuts_at (P := P) _ s _ _ _ hi hf
    simp only [hd, ite_true] at hout
    exact inv_arrive hv hb hs (List.mem_append_left _ hout)
      (target_ne_zero hyp.noEntryEdge (mem_targets_fail hb hi hf))
      (acts_sound hyp (hA _ hb) (fun a ha => mem_allActs_inst hb hi ha)
        (instsOk_at _ _ _ _ hok.1 hi) hacts)
  | @branch b pc σ blk f yes no v hb hpc ht hbr =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hA := hA _ hb
    rw [hpc, List.take_length] at hA
    have hv' := hyp.flagFresh σ f v hbr
    have hout : ((if v then yes else no), refine P (applyInsts P s blk.insts) f v) ∈
        blockOuts P blk s := by
      apply List.mem_append_right
      rw [ht]
      cases v <;> simp [termOuts]
    exact inv_arrive hv hb hs hout
      (target_ne_zero hyp.noEntryEdge (mem_targets_branch v hb ht)) (abs_refine hA hv')
  | @goto b pc σ σ' blk pre succs t hb hpc ht hacts hmem =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hA := hA _ hb
    rw [hpc, List.take_length] at hA
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true, ht, termOk] at hok
    have hout : (t, applyActs P (applyInsts P s blk.insts) pre) ∈ blockOuts P blk s := by
      apply List.mem_append_right
      rw [ht]
      exact List.mem_map.mpr ⟨t, hmem, rfl⟩
    exact inv_arrive hv hb hs hout
      (target_ne_zero hyp.noEntryEdge (mem_targets_goto hb ht hmem))
      (acts_sound hyp hA (fun a ha => mem_allActs_term hb ht ha) hok.2 hacts)
  | exit => trivial
  | @termFault b pc σ blk pre succs k hb hpc ht hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hA := hA _ hb
    rw [hpc, List.take_length] at hA
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true, ht, termOk] at hok
    exact acts_sound hyp hA (fun a ha => mem_allActs_term hb ht ha) hok.2 hacts

theorem inv_steps {S : Sem} {P : Prog} {start : Nat → Option AState} (hyp : Hyp S P)
    (hv : validate P start = true) {c c' : Cfg} (hinv : Inv P start c)
    (hsteps : Steps S P c c') : Inv P start c' := by
  induction hsteps with
  | refl => exact hinv
  | tail _ hstep ih => exact preservation hyp hv ih hstep

/-- **Soundness.** A body `wf::own` accepts reaches no fault. -/
theorem sound {S : Sem} {P : Prog} (hyp : Hyp S P) (hacc : Accepted P) (k : Fault) :
    ¬ Reachable S P (.fault k) := by
  intro ⟨σ, hi, hsteps⟩
  obtain ⟨start, hv⟩ := hacc
  exact inv_steps hyp hv (inv_initial hv hi) hsteps

/-- **R1.** No path of an accepted body drops a place overlapping one dropped already. -/
theorem no_double_drop {S : Sem} {P : Prog} (hyp : Hyp S P) (hacc : Accepted P) :
    ¬ Reachable S P (.fault .doubleDrop) := sound hyp hacc _

/-- **R2.** No path of an accepted body uses a place overlapping a dropped one. -/
theorem no_use_after_drop {S : Sem} {P : Prog} (hyp : Hyp S P) (hacc : Accepted P) :
    ¬ Reachable S P (.fault .useAfterDrop) := sound hyp hacc _

/-- **R3.** No path of an accepted body drops a `view` parameter's place. -/
theorem no_view_drop {S : Sem} {P : Prog} (hyp : Hyp S P) (hacc : Accepted P) :
    ¬ Reachable S P (.fault .viewDrop) := sound hyp hacc _

/-- **OWN-20.** No path of an accepted body uses or drops a dest view after a drop of its
anchor while the borrow was in force. -/
theorem no_dangling_view {S : Sem} {P : Prog} (hyp : Hyp S P) (hacc : Accepted P) :
    ¬ Reachable S P (.fault .danglingView) := sound hyp hacc _

/-! ## The two machines -/

/-- On the machine own.rs describes, `Hyp` follows from three checks of the body.
`flagFresh` holds because this machine branches on the flag's value at the branch; that a
CoreIR body's branch does so is assumed, not checked. -/
theorem hyp_own {P : Prog} (hwf : P.wf = true) (hentry : P.noEntryEdge = true)
    (hflags : P.flagsStored = true) : Hyp Sem.own P :=
  ⟨hwf, hentry, fun _ _ _ h => h, hflags, fun _ _ _ _ _ _ _ ho => ho⟩

/-- A machine in which a write begins only the places inside the written one: a write of a
field leaves the rest of a dropped place dropped. -/
def Sem.cells : Sem := ⟨covers, fun σ f v => v = σ.flag f⟩

/-- Every write through a single uncut place covers each tracked place it overlaps. -/
def Prog.writesWhole (P : Prog) : Bool :=
  P.allActs.all fun
    | .write a =>
      match P.names a with
      | [r] => r.cut || P.tracked.all fun q => !overlap r q || covers r q
      | _ => true
    | _ => true

/-- On the machine of cells, `Hyp` follows from four checks of the body. `flagFresh` is
assumed of the machine, as in `hyp_own`. -/
theorem hyp_cells {P : Prog} (hwf : P.wf = true) (hentry : P.noEntryEdge = true)
    (hflags : P.flagsStored = true) (hwrites : P.writesWhole = true) : Hyp Sem.cells P := by
  refine ⟨hwf, hentry, fun _ _ _ h => h, hflags, ?_⟩
  intro a r hin hn hcut q hq ho
  unfold Prog.writesWhole at hwrites
  simp only [List.all_eq_true] at hwrites
  have := hwrites _ hin
  simp only [hn, hcut, Bool.false_or, List.all_eq_true] at this
  have := this q hq
  simp only [ho, Bool.not_true, Bool.false_or] at this
  exact this

/-! ## Examples

Bodies written as `check` sees them: instructions with no act and no failure edge are left
out. The first two are tests of `crates/hirsel-core-ir/src/tests/own.rs`; the rest show
that each hypothesis of `Hyp` is needed. -/

/-- A store at entry: every place live, no borrow in force, every flag false. -/
def Store.entry : Store := ⟨fun _ => .live, fun _ => false, fun _ => false⟩

theorem Store.entry_initial : Initial Store.entry := ⟨fun _ => rfl, fun _ => rfl⟩

/-- The store after a drop of `r` that leaves the views `ds` dangling. -/
def Store.kill (σ : Store) (r : Pl) (ds : List Pl) : Store :=
  { σ with st := fun p =>
      if p = r then .dropped else if ds.contains p then .dangling else σ.st p }

theorem hangs_dest {P : Prog} {on : Nat → Bool} {q d : Pl} (h : Hangs P on q d) :
    ∃ b, b ∈ P.borrows ∧ d ∈ b.dest := by
  cases h with
  | direct hd =>
    obtain ⟨k, b, hb, _, _, _, hd⟩ := mem_succs.mp hd
    exact ⟨b, List.mem_of_getElem? hb, hd⟩
  | via _ hd =>
    obtain ⟨k, b, hb, _, _, _, hd⟩ := mem_succs.mp hd
    exact ⟨b, List.mem_of_getElem? hb, hd⟩

theorem ActStep.drop_kill {S : Sem} {P : Prog} {σ : Store} {a : Nat} {r : Pl} {ds : List Pl}
    (hr : r ∈ P.names a) (hv : P.isView r.root = false)
    (hl : ∀ p, overlap r p = true → σ.st p = .live) (hc : r.cut = false)
    (hds : ∀ p, p ≠ r → (Hangs P σ.loan r p ↔ p ∈ ds)) :
    ActStep S P σ (.drop a) (.ok (σ.kill r ds)) := by
  refine ActStep.drop_ok (σ' := σ.kill r ds) hr hv hl hc ?_ ?_ ?_ rfl rfl
  · simp [Store.kill]
  · intro p hp hh
    simp [Store.kill, hp, (hds p hp).mp hh]
  · intro p hp hh
    have : p ∉ ds := fun h => hh ((hds p hp).mpr h)
    simp [Store.kill, hp, this]

/-- A drop in a body with no borrows. -/
theorem ActStep.drop_plain {S : Sem} {P : Prog} {σ : Store} {a : Nat} {r : Pl}
    (hb : P.borrows = []) (hr : r ∈ P.names a) (hv : P.isView r.root = false)
    (hl : ∀ p, overlap r p = true → σ.st p = .live) (hc : r.cut = false) :
    ActStep S P σ (.drop a) (.ok (σ.kill r [])) := by
  refine .drop_kill hr hv hl hc fun p _ => ⟨fun hh => ?_, fun hm => nomatch hm⟩
  obtain ⟨b, hb', _⟩ := hangs_dest hh
  rw [hb] at hb'
  cases hb'

/-! ### `ownership_rejects_a_drop_repeated_on_its_failure_edge` (tests/own.rs:7-33)

`b0` drops parameter 1 with failure edge `b1`; `b1` drops it again when `again`. -/

namespace FailEdge

def p : Pl := ⟨1, [], false⟩

def body (again : Bool) : Prog where
  slots := 0
  views := [false, false]
  names := fun a => if a == 0 then [p] else []
  derefs := fun _ => []
  tracked := [p]
  pairing := [(p, none)]
  borrows := []
  blocks := [
    ⟨[⟨[.drop 0], some 1, true⟩], .other [] []⟩,
    ⟨if again then [⟨[.drop 0], none, true⟩] else [], .other [] []⟩]

theorem once_accepted : Accepted (body false) :=
  ⟨fun b => if b == 0 then some .init
    else some ⟨fun _ => ⟨false, false, true, true⟩, fun _ => true⟩, by decide⟩

/-- The accepted body reaches no fault, on the machine of cells. -/
theorem once_safe (k : Fault) : ¬ Reachable Sem.cells (body false) (.fault k) :=
  sound (hyp_cells (by decide) (by decide) (by decide) (by decide)) once_accepted k

theorem twice_double_drops : Reachable Sem.own (body true) (.fault .doubleDrop) := by
  refine ⟨.entry, Store.entry_initial, ?_⟩
  have hd : ActStep Sem.own (body true) .entry (.drop 0) (.ok (Store.entry.kill p [])) :=
    .drop_plain rfl (by decide) rfl (fun _ _ => rfl) rfl
  have h1 : Steps Sem.own (body true) (.run 0 0 .entry) (.run 1 0 (Store.entry.kill p [])) :=
    .tail .refl (.dropFail rfl rfl rfl rfl (.cons hd .nil))
  exact .tail h1 (.instFault rfl rfl
    (.fault (.drop_dropped (r := p) (p := p) (by decide) (by decide) (by decide))))

theorem twice_rejected : ¬ Accepted (body true) := fun h =>
  no_double_drop (hyp_own (by decide) (by decide) (by decide)) h twice_double_drops

end FailEdge

/-! ### `ownership_rejects_a_view_used_after_its_anchor_is_dropped` (tests/own.rs:109-176)

`b1` fills `s0`, calls `me` with dest `s1` and anchor `s0`, and drops `s0`; `b5` then uses
`s1`. When `me`'s dest is a view (`view`), the call lends and the use is of a dangling
view. -/

namespace ViewAnchor

def s0 : Pl := ⟨0, [], false⟩
def s1 : Pl := ⟨1, [], false⟩

def body (view : Bool) : Prog where
  slots := 2
  views := []
  names := fun
    | 0 => [s0]
    | 1 => [s1]
    | 2 => [s0]
    | 3 => [s0]
    | 4 => [s0]
    | 5 => [s1]
    | _ => []
  derefs := fun _ => []
  tracked := if view then [s0, s1] else [s0]
  pairing := if view then [(s0, none), (s1, none)] else [(s0, none)]
  borrows := if view then [⟨[s1], [s0]⟩] else []
  blocks := [
    ⟨[], .other [] [1]⟩,
    ⟨[⟨[.begin 0], none, false⟩,
      ⟨[.write 0], none, false⟩,
      ⟨[], some 2, false⟩,
      ⟨[.begin 1], none, false⟩,
      ⟨[.write 1, .use 2] ++ (if view then [.lend 0] else []), some 2, false⟩,
      ⟨[.drop 3], some 4, true⟩], .other [] [5]⟩,
    ⟨[⟨[.drop 4], some 3, true⟩], .other [] [3]⟩,
    ⟨[], .other [] []⟩,
    ⟨[], .other [] []⟩,
    ⟨[⟨[.use 5], none, false⟩], .other [] []⟩]

theorem owned_accepted : Accepted (body false) :=
  ⟨fun b => if b ≤ 2 then some .init
    else some ⟨fun _ => ⟨true, true, true, true⟩, fun _ => true⟩, by decide⟩

/-- The accepted body reaches no fault, on the machine of cells. -/
theorem owned_safe (k : Fault) : ¬ Reachable Sem.cells (body false) (.fault k) :=
  sound (hyp_cells (by decide) (by decide) (by decide) (by decide)) owned_accepted k

theorem drop_anchor {σ : Store} (hl : ∀ p, σ.st p = .live) (h0 : σ.loan 0 = true) :
    ActStep Sem.own (body true) σ (.drop 3) (.ok (σ.kill s0 [s1])) := by
  refine .drop_kill (by decide) rfl (fun p _ => hl p) rfl fun p _ => ⟨fun hh => ?_, fun hm => ?_⟩
  · obtain ⟨b, hb, hd⟩ := hangs_dest hh
    simp only [body, ite_true, List.mem_singleton] at hb
    subst hb
    exact hd
  · have : p = s1 := by simpa using hm
    subst this
    exact .direct (mem_succs.mpr
      ⟨0, ⟨[s1], [s0]⟩, rfl, h0, ⟨s0, by decide, by decide⟩, by decide, by decide⟩)

theorem view_dangles : Reachable Sem.own (body true) (.fault .danglingView) := by
  refine ⟨.entry, Store.entry_initial, ?_⟩
  have h1 : Steps Sem.own (body true) (.run 0 0 .entry) (.run 1 0 .entry) :=
    .tail .refl (.goto rfl rfl rfl .nil (by decide))
  have h2 := Steps.tail h1 (.inst rfl rfl (.cons .begin .nil))
  have h3 := Steps.tail h2 (.inst rfl rfl
    (.cons (.write (r := s0) (fl := fun _ => false) (by decide) (fun _ _ => rfl)) .nil))
  have h4 := Steps.tail h3 (.inst rfl rfl .nil)
  have h5 := Steps.tail h4 (.inst rfl rfl (.cons .begin .nil))
  have h6 := Steps.tail h5 (.inst rfl rfl
    (.cons (.write (r := s1) (fl := fun _ => false) (by decide) (fun _ _ => rfl))
      (.cons .use_ok (.cons .lend .nil))))
  have h7 := Steps.tail h6 (.inst rfl rfl (.cons (drop_anchor
    (by intro p; simp [Store.write, Store.begin, Store.entry, s0, s1]) rfl) .nil))
  have h8 := Steps.tail h7 (.goto (t := 5) rfl rfl rfl .nil (by decide))
  exact .tail h8 (.instFault rfl rfl
    (.fault (.use_dangling (r := s1) (p := s1) (by decide) (by decide) (by decide))))

theorem view_rejected : ¬ Accepted (body true) := fun h =>
  no_dangling_view (hyp_own (by decide) (by decide) (by decide)) h view_dangles

end ViewAnchor

/-! ### Each hypothesis is needed

Four bodies the analysis accepts and that drop a place twice. Each breaks one field of
`Hyp` and keeps the rest. `wf::module` accepts a CoreIR body of the same shape as each
(README.md). -/

/-! `noEntryEdge`: `b0` drops `s0` and jumps to `b0`. The edge into the entry block is
discarded (own.rs:962), so the entry state never learns of the drop. -/
namespace EntryEdge

def s0 : Pl := ⟨0, [], false⟩

def body : Prog where
  slots := 1
  views := []
  names := fun a => if a == 0 then [s0] else []
  derefs := fun _ => []
  tracked := [s0]
  pairing := [(s0, none)]
  borrows := []
  blocks := [⟨[⟨[.drop 0], none, true⟩], .other [] [0]⟩]

theorem accepted : Accepted body := ⟨fun b => if b == 0 then some .init else none, by decide⟩

theorem double_drops : Reachable Sem.own body (.fault .doubleDrop) := by
  refine ⟨.entry, Store.entry_initial, ?_⟩
  have h1 : Steps Sem.own body (.run 0 0 .entry) (.run 0 1 (Store.entry.kill s0 [])) :=
    .tail .refl (.inst rfl rfl
      (.cons (.drop_plain rfl (by decide) rfl (fun _ _ => rfl) rfl) .nil))
  have h2 := Steps.tail h1 (.goto (t := 0) rfl rfl rfl .nil (by decide))
  exact .tail h2 (.instFault rfl rfl
    (.fault (.drop_dropped (r := s0) (p := s0) (by decide) (by decide) (by decide))))

/-- Every hypothesis but `noEntryEdge` holds, the body is accepted, and it double-drops. -/
theorem needed : body.wf = true ∧ body.flagsStored = true ∧ body.noEntryEdge = false ∧
    Accepted body ∧ Reachable Sem.own body (.fault .doubleDrop) :=
  ⟨by decide, by decide, by decide, accepted, double_drops⟩

end EntryEdge

/-! `flagFresh`: `b0` drops `s0`, stores false to its drop flag, and branches on a value
of the flag read before the store. The analysis refines both edges by the flag's value at
the branch, so the `yes` edge carries no dropped state. -/
namespace StaleFlag

def s0 : Pl := ⟨0, [], false⟩

/-- A machine whose flag branch sees any value: the flag was read earlier. -/
def stale : Sem := ⟨overlap, fun _ _ _ => True⟩

def body : Prog where
  slots := 2
  views := []
  names := fun a => if a == 0 then [s0] else []
  derefs := fun _ => []
  tracked := [s0]
  pairing := [(s0, none), (s0, some 1)]
  borrows := []
  blocks := [
    ⟨[⟨[.drop 0], none, true⟩, ⟨[.flag 1 (some false)], none, false⟩], .flagBranch 1 1 2⟩,
    ⟨[⟨[.drop 0], none, true⟩], .other [] []⟩,
    ⟨[], .other [] []⟩]

theorem accepted : Accepted body :=
  ⟨fun b => if b == 0 then some .init
    else if b == 1 then
      some ⟨fun x => if x == 0 then ⟨false, false, true, true⟩ else ⟨false, false, false, false⟩,
        fun _ => true⟩
    else some ⟨fun _ => ⟨true, true, true, true⟩, fun _ => true⟩, by decide⟩

theorem double_drops : Reachable stale body (.fault .doubleDrop) := by
  refine ⟨.entry, Store.entry_initial, ?_⟩
  have h1 : Steps stale body (.run 0 0 .entry) (.run 0 1 (Store.entry.kill s0 [])) :=
    .tail .refl (.inst rfl rfl
      (.cons (.drop_plain rfl (by decide) rfl (fun _ _ => rfl) rfl) .nil))
  have h2 := Steps.tail h1 (.inst rfl rfl
    (.cons (.flag (b := false) (fun c h => by cases h; rfl)) .nil))
  have h3 := Steps.tail h2 (.branch (v := true) rfl rfl rfl trivial)
  exact .tail h3 (.instFault rfl rfl
    (.fault (.drop_dropped (r := s0) (p := s0) (by decide) (by decide) (by decide))))

/-- Every hypothesis but `flagFresh` holds, the body is accepted, and it double-drops. -/
theorem needed : body.wf = true ∧ body.noEntryEdge = true ∧ body.flagsStored = true ∧
    WritesBegin stale body ∧ Accepted body ∧ Reachable stale body (.fault .doubleDrop) :=
  ⟨by decide, by decide, by decide, fun _ _ _ _ _ _ _ ho => ho, accepted, double_drops⟩

end StaleFlag

/-! `flagsStored`: `b0` stores false to `s0`'s drop flag, drops `s0`, then writes the flag's
slot by something other than a whole `store` (a `copy` into it), and branches on the flag.
The analysis sees a write of an untracked place and keeps the flag false. -/
namespace FlagWrite

def s0 : Pl := ⟨0, [], false⟩
def f1 : Pl := ⟨1, [], false⟩

def body : Prog where
  slots := 2
  views := []
  names := fun a => if a == 0 then [s0] else if a == 1 then [f1] else []
  derefs := fun _ => []
  tracked := [s0]
  pairing := [(s0, none), (s0, some 1)]
  borrows := []
  blocks := [
    ⟨[⟨[.flag 1 (some false)], none, false⟩, ⟨[.drop 0], none, true⟩,
      ⟨[.write 1], none, false⟩], .flagBranch 1 1 2⟩,
    ⟨[⟨[.drop 0], none, true⟩], .other [] []⟩,
    ⟨[], .other [] []⟩]

theorem accepted : Accepted body :=
  ⟨fun b => if b == 0 then some .init
    else if b == 1 then
      some ⟨fun x => if x == 0 then ⟨false, false, true, true⟩ else ⟨false, false, false, false⟩,
        fun _ => true⟩
    else some ⟨fun _ => ⟨true, true, true, true⟩, fun _ => true⟩, by decide⟩

theorem double_drops : Reachable Sem.own body (.fault .doubleDrop) := by
  refine ⟨.entry, Store.entry_initial, ?_⟩
  have h1 : Steps Sem.own body (.run 0 0 .entry) (.run 0 0 .entry) := .refl
  have h2 := Steps.tail h1 (.inst rfl rfl
    (.cons (.flag (b := false) (fun c h => by cases h; rfl)) .nil))
  have h3 := Steps.tail h2 (.inst rfl rfl
    (.cons (.drop_plain (r := s0) rfl (by decide) rfl (fun _ _ => rfl) rfl) .nil))
  have h4 := Steps.tail h3 (.inst rfl rfl
    (.cons (.write (r := f1) (fl := fun g => g == 1) (by decide) (fun g hg => by
      have hg : g ≠ 1 := hg
      have : (g == 1) = false := by simpa using hg
      simp [Store.kill, Store.entry, this])) .nil))
  have h5 := Steps.tail h4 (.branch (v := true) rfl rfl rfl rfl)
  exact .tail h5 (.instFault rfl rfl
    (.fault (.drop_dropped (r := s0) (p := s0) (by decide) (by decide) (by decide))))

/-- Every hypothesis but `flagsStored` holds, the body is accepted, and it double-drops. -/
theorem needed : body.wf = true ∧ body.noEntryEdge = true ∧ body.flagsStored = false ∧
    Accepted body ∧ Reachable Sem.own body (.fault .doubleDrop) :=
  ⟨by decide, by decide, by decide, accepted, double_drops⟩

end FlagWrite

/-! `writesBegin`: `b0` drops `s0`, writes its field 0, and drops `s0` again. The analysis
marks every tracked place the write overlaps begun (own.rs:454-470); on the machine of
cells the rest of `s0` is still dropped. -/
namespace PartialWrite

def s0 : Pl := ⟨0, [], false⟩
def s0f : Pl := ⟨0, [(0, 0, 0)], false⟩

def body : Prog where
  slots := 1
  views := []
  names := fun a => if a == 0 then [s0] else if a == 1 then [s0f] else []
  derefs := fun _ => []
  tracked := [s0]
  pairing := [(s0, none)]
  borrows := []
  blocks := [
    ⟨[⟨[.drop 0], none, true⟩, ⟨[.write 1], none, false⟩, ⟨[.drop 0], none, true⟩],
      .other [] []⟩]

theorem accepted : Accepted body := ⟨fun b => if b == 0 then some .init else none, by decide⟩

theorem double_drops : Reachable Sem.cells body (.fault .doubleDrop) := by
  refine ⟨.entry, Store.entry_initial, ?_⟩
  have h1 : Steps Sem.cells body (.run 0 0 .entry) (.run 0 1 (Store.entry.kill s0 [])) :=
    .tail .refl (.inst rfl rfl
      (.cons (.drop_plain rfl (by decide) rfl (fun _ _ => rfl) rfl) .nil))
  have h2 := Steps.tail h1 (.inst rfl rfl
    (.cons (.write (r := s0f) (fl := fun _ => false) (by decide) (fun _ _ => rfl)) .nil))
  exact .tail h2 (.instFault rfl rfl
    (.fault (.drop_dropped (r := s0) (p := s0) (by decide) (by decide) (by decide))))

/-- Every hypothesis but `writesBegin` holds, the body is accepted, and it double-drops. -/
theorem needed : body.wf = true ∧ body.noEntryEdge = true ∧ body.flagsStored = true ∧
    body.writesWhole = false ∧ Accepted body ∧
    Reachable Sem.cells body (.fault .doubleDrop) :=
  ⟨by decide, by decide, by decide, by decide, accepted, double_drops⟩

end PartialWrite

/-! ## Axioms

Each theorem rests on Lean's three standard axioms at most. `lake build` fails if one of
these messages changes. -/

/--
info: 'Hirsel.CoreIR.Own.preservation' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms preservation

/-- info: 'Hirsel.CoreIR.Own.sound' depends on axioms: [propext, Classical.choice, Quot.sound] -/
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

/-- info: 'Hirsel.CoreIR.Own.hyp_own' depends on axioms: [propext] -/
#guard_msgs in
#print axioms hyp_own

/-- info: 'Hirsel.CoreIR.Own.hyp_cells' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms hyp_cells

/--
info: 'Hirsel.CoreIR.Own.FailEdge.once_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms FailEdge.once_safe

/--
info: 'Hirsel.CoreIR.Own.FailEdge.twice_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms FailEdge.twice_rejected

/--
info: 'Hirsel.CoreIR.Own.ViewAnchor.owned_safe' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms ViewAnchor.owned_safe

/--
info: 'Hirsel.CoreIR.Own.ViewAnchor.view_rejected' depends on axioms: [propext, Classical.choice, Quot.sound]
-/
#guard_msgs in
#print axioms ViewAnchor.view_rejected

/-- info: 'Hirsel.CoreIR.Own.EntryEdge.needed' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms EntryEdge.needed

/-- info: 'Hirsel.CoreIR.Own.StaleFlag.needed' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms StaleFlag.needed

/-- info: 'Hirsel.CoreIR.Own.FlagWrite.needed' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms FlagWrite.needed

/-- info: 'Hirsel.CoreIR.Own.PartialWrite.needed' depends on axioms: [propext, Quot.sound] -/
#guard_msgs in
#print axioms PartialWrite.needed

end Hirsel.CoreIR.Own
