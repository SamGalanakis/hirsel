import OwnSoundness.Lemmas

/-!
# Running a body

`exec` runs a body along a list of moves, and `exec_steps` shows that what it does is a
run of the machine of `Model.lean`. A body that faults under `exec` therefore reaches the
fault (`reach_of_exec`), which the kernel checks by evaluating `exec`.

`exec` resolves the machine's choices: an address into a slot is its first name, a write to
a flag slot and an unknown flag hold `wr`, and a use faults only when the place itself is
not live.
-/

namespace Hirsel.CoreIR.Own

/-! ## The views left dangling, computed -/

/-- The dests reached in at most `n` steps from the borrows `seed` picks. -/
def hang (P : Prog) (on : Nat → Bool) (seed : Borrow → Bool) : Nat → List Pl
  | 0 => P.seeds on seed
  | n + 1 => hang P on seed n ++ (hang P on seed n).flatMap (P.succs on)

def walk (P : Prog) (on : Nat → Bool) (seed : Borrow → Bool) : List Pl :=
  hang P on seed P.borrows.length

/-- `walk` has every dest: a step from one of its places stays in it. -/
def closed (P : Prog) (on : Nat → Bool) (seed : Borrow → Bool) : Bool :=
  ((walk P on seed).flatMap (P.succs on)).all fun d => (walk P on seed).contains d

def dang (P : Prog) (on : Nat → Bool) (seed : Borrow → Bool) (p : Pl) : Bool :=
  (walk P on seed).any fun d => decide (d.root < P.roots) && inside d p

theorem hang_sound {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} :
    ∀ n d, d ∈ hang P on seed n → Hangs P on seed d
  | 0, _, h => .direct h
  | n + 1, d, h => by
    rcases List.mem_append.mp h with h | h
    · exact hang_sound n d h
    · obtain ⟨m, hm, hd⟩ := List.mem_flatMap.mp h
      exact .via (hang_sound n m hm) hd

theorem hang_seeds {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} :
    ∀ n d, d ∈ P.seeds on seed → d ∈ hang P on seed n
  | 0, _, h => h
  | n + 1, d, h => List.mem_append_left _ (hang_seeds n d h)

theorem walk_complete {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} {d : Pl}
    (hc : closed P on seed = true) (h : Hangs P on seed d) : d ∈ walk P on seed := by
  simp only [closed, List.all_eq_true, List.contains_iff_mem] at hc
  induction h with
  | direct hd => exact hang_seeds _ _ hd
  | via _ hd ih => exact hc _ (List.mem_flatMap.mpr ⟨_, ih, hd⟩)

theorem dang_iff {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} {p : Pl}
    (hc : closed P on seed = true) : dang P on seed p = true ↔ Dangles P on seed p := by
  unfold Dangles
  simp only [dang, List.any_eq_true, Bool.and_eq_true, decide_eq_true_eq]
  constructor
  · rintro ⟨d, hd, hr, hin⟩
    exact ⟨d, hang_sound _ _ hd, hr, hin⟩
  · rintro ⟨d, hd, hr, hin⟩
    exact ⟨d, walk_complete hc hd, hr, hin⟩

/-- The store with the places of `kill` dropped and the views of `seed`'s anchors
dangling. -/
def Store.hung (σ : Store) (P : Prog) (seed : Borrow → Bool) (kill : Pl → Bool) : Store :=
  { σ with
    st := fun p =>
      if kill p then .dropped else if dang P σ.loan seed p then .dangling else σ.st p }

theorem hung_exec {P : Prog} {σ : Store} {seed : Borrow → Bool} {kill : Pl → Bool}
    {K : Pl → Prop} (hk : ∀ p, K p ↔ kill p = true) (hc : closed P σ.loan seed = true) :
    Hung P seed K σ (σ.hung P seed kill) where
  killed := fun p h => by
    show (if kill p = true then PSt.dropped
      else if dang P σ.loan seed p = true then PSt.dangling else σ.st p) = PSt.dropped
    rw [ifPos ((hk p).mp h)]
  hung := fun p hn hd => by
    show (if kill p = true then PSt.dropped
      else if dang P σ.loan seed p = true then PSt.dangling else σ.st p) = PSt.dangling
    rw [ifNeg (fun h => hn ((hk p).mpr h)), ifPos ((dang_iff hc).mpr hd)]
  kept := fun p hn hd => by
    show (if kill p = true then PSt.dropped
      else if dang P σ.loan seed p = true then PSt.dangling else σ.st p) = σ.st p
    rw [ifNeg (fun h => hn ((hk p).mpr h)), ifNeg (fun h => hd ((dang_iff hc).mp h))]
  flag := rfl
  read := rfl
  loan := rfl
  bind := rfl

theorem never (p : Pl) : False ↔ (fun (_ : Pl) => false) p = true :=
  ⟨False.elim, fun h => Bool.false_ne_true h⟩

/-! ## One act -/

/-- The place an address is: of an address into a slot, its first name. -/
def den (P : Prog) (σ : Store) (a : Nat) : Option Pl :=
  match P.base a with
  | some (v, rel) => (σ.bind v).map fun x => x.app rel
  | none => (P.names a).head?

theorem den_spec (P : Prog) (σ : Store) (a : Nat) : Den P σ a (den P σ a) := by
  unfold Den den
  cases hb : P.base a with
  | some vr =>
    obtain ⟨v, rel⟩ := vr
    exact rfl
  | none =>
    cases hn : P.names a with
    | nil => exact rfl
    | cons r l => exact List.mem_cons_self ..

def execUse (P : Prog) (σ : Store) (a : Nat) : Res :=
  match den P σ a with
  | none => .ok σ
  | some y =>
    match σ.st y with
    | .live => .ok σ
    | .dropped => .fault .useAfterDrop
    | .dangling => .fault .danglingView

theorem execUse_sound {P : Prog} {σ : Store} {a : Nat} :
    ActStep P σ (.use a) (execUse P σ a) := by
  have hd := den_spec P σ a
  unfold execUse
  cases hy : den P σ a with
  | none => exact .use_ok
  | some y =>
    rw [hy] at hd
    dsimp only
    cases hs : σ.st y with
    | live => exact .use_ok
    | dropped => exact .use_dropped hd (inside_self y) hs
    | dangling => exact .use_dangling hd (inside_self y) hs

/-- A drop of the place `y`. -/
def execDropAt (P : Prog) (σ : Store) (y : Pl) : Option Res :=
  if P.isView y.root then some (.fault .viewDrop)
  else
    match σ.st y with
    | .dropped => some (.fault .doubleDrop)
    | .dangling => some (.fault .danglingView)
    | .live =>
      if y.cut then some (.ok σ)
      else if closed P σ.loan (P.lent y) then some (.ok (σ.hung P (P.lent y) (inside y)))
      else none

def execDrop (P : Prog) (σ : Store) (a : Nat) : Option Res :=
  match den P σ a with
  | none => some (.ok σ)
  | some y => execDropAt P σ y

theorem execDropAt_sound {P : Prog} {σ : Store} {a : Nat} {y : Pl} {res : Res}
    (hd : Den P σ a (some y)) (h : execDropAt P σ y = some res) :
    ActStep P σ (.drop a) res := by
  unfold execDropAt at h
  by_cases hv : P.isView y.root = true
  · rw [ifPos hv] at h
    obtain rfl : Res.fault .viewDrop = res := Option.some.inj h
    exact .drop_view hd hv
  · rw [ifNeg hv] at h
    cases hs : σ.st y with
    | dropped =>
      rw [hs] at h
      obtain rfl : Res.fault .doubleDrop = res := Option.some.inj h
      exact .drop_dropped hd (inside_self y) hs
    | dangling =>
      rw [hs] at h
      obtain rfl : Res.fault .danglingView = res := Option.some.inj h
      exact .drop_dangling hd (inside_self y) hs
    | live =>
      rw [hs] at h
      replace h : (if y.cut = true then some (Res.ok σ)
        else if closed P σ.loan (P.lent y) = true then
          some (Res.ok (σ.hung P (P.lent y) (inside y)))
        else none) = some res := h
      by_cases hcut : y.cut = true
      · rw [ifPos hcut] at h
        obtain rfl : Res.ok σ = res := Option.some.inj h
        exact .drop_cut hd hcut
      · rw [ifNeg hcut] at h
        by_cases hc : closed P σ.loan (P.lent y) = true
        · rw [ifPos hc] at h
          obtain rfl : Res.ok _ = res := Option.some.inj h
          exact .drop_ok hd ((Bool.not_eq_true _).mp hcut) (hung_exec (fun _ => Iff.rfl) hc)
        · rw [ifNeg hc] at h
          cases h

theorem execDrop_sound {P : Prog} {σ : Store} {a : Nat} {res : Res}
    (h : execDrop P σ a = some res) : ActStep P σ (.drop a) res := by
  have hd := den_spec P σ a
  unfold execDrop at h
  cases hy : den P σ a with
  | none =>
    rw [hy] at h hd
    obtain rfl : Res.ok σ = res := Option.some.inj h
    exact .drop_none hd
  | some y =>
    rw [hy] at h hd
    exact execDropAt_sound hd h

/-- A write or an `invalidate` of the place `y`: the views anchored on it dangle. -/
def Store.rewritten (σ : Store) (P : Prog) (y : Pl) : Store :=
  σ.hung P (fun b => b.anchor.any fun f => overlap f y) (fun _ => false)

def execWriteAt (P : Prog) (wr : Bool) (σ : Store) (y : Pl) : Option Res :=
  if closed P σ.loan (fun b => b.anchor.any fun f => overlap f y) then
    some (.ok (Store.revive P
      { σ.rewritten P y with flag := fun f => if f == y.root then wr else σ.flag f } y))
  else none

def execWrite (P : Prog) (wr : Bool) (σ : Store) (a : Nat) : Option Res :=
  match den P σ a with
  | none => some (.ok σ)
  | some y => execWriteAt P wr σ y

theorem execWrite_sound {P : Prog} {wr : Bool} {σ : Store} {a : Nat} {res : Res}
    (h : execWrite P wr σ a = some res) : ActStep P σ (.write a) res := by
  have hd := den_spec P σ a
  unfold execWrite at h
  cases hy : den P σ a with
  | none =>
    rw [hy] at h hd
    obtain rfl : Res.ok σ = res := Option.some.inj h
    exact .write_none hd
  | some y =>
    rw [hy] at h hd
    replace h : execWriteAt P wr σ y = some res := h
    unfold execWriteAt at h
    by_cases hc : closed P σ.loan (fun b => b.anchor.any fun f => overlap f y) = true
    · rw [ifPos hc] at h
      obtain rfl : Res.ok _ = res := Option.some.inj h
      refine .write (fl := fun f => if f == y.root then wr else σ.flag f) hd
        (hung_exec never hc) (fun f hf => ?_)
      show (if (f == y.root) = true then wr else σ.flag f) = σ.flag f
      rw [ifNeg (by simpa using hf)]
    · rw [ifNeg hc] at h
      cases h

def execInvalidateAt (P : Prog) (σ : Store) (y : Pl) : Option Res :=
  if closed P σ.loan (fun b => b.anchor.any fun f => overlap f y) then
    some (.ok (σ.rewritten P y))
  else none

def execInvalidate (P : Prog) (σ : Store) (a : Nat) : Option Res :=
  match den P σ a with
  | none => some (.ok σ)
  | some y => execInvalidateAt P σ y

theorem execInvalidate_sound {P : Prog} {σ : Store} {a : Nat} {res : Res}
    (h : execInvalidate P σ a = some res) : ActStep P σ (.invalidate a) res := by
  have hd := den_spec P σ a
  unfold execInvalidate at h
  cases hy : den P σ a with
  | none =>
    rw [hy] at h hd
    obtain rfl : Res.ok σ = res := Option.some.inj h
    exact .invalidate_none hd
  | some y =>
    rw [hy] at h hd
    replace h : execInvalidateAt P σ y = some res := h
    unfold execInvalidateAt at h
    by_cases hc : closed P σ.loan (fun b => b.anchor.any fun f => overlap f y) = true
    · rw [ifPos hc] at h
      obtain rfl : Res.ok _ = res := Option.some.inj h
      exact .invalidate hd (hung_exec never hc)
    · rw [ifNeg hc] at h
      cases h

def execBegin (P : Prog) (σ : Store) (slot : Nat) : Option Res :=
  if closed P σ.loan (anchored slot) then
    some (.ok ((σ.hung P (anchored slot) (fun _ => false)).begin P slot))
  else none

theorem execBegin_sound {P : Prog} {σ : Store} {slot : Nat} {res : Res}
    (h : execBegin P σ slot = some res) : ActStep P σ (.begin slot) res := by
  unfold execBegin at h
  by_cases hc : closed P σ.loan (anchored slot) = true
  · rw [ifPos hc] at h
    obtain rfl : Res.ok _ = res := Option.some.inj h
    exact .begin (hung_exec never hc)
  · rw [ifNeg hc] at h
    cases h

/-- The places a `dead` of the slot ends: those inside its tracked places. -/
def ended (P : Prog) (slot : Nat) (p : Pl) : Bool :=
  P.tracked.any fun t => t.root == slot && inside t p

def execDead (P : Prog) (σ : Store) (slot : Nat) : Option Res :=
  if closed P σ.loan (anchored slot) then
    some (.ok { σ.hung P (anchored slot) (ended P slot) with
      loan := (σ.hung P (anchored slot) (ended P slot)).endLoans P fun d => d.root == slot })
  else none

theorem execDead_sound {P : Prog} {σ : Store} {slot : Nat} {res : Res}
    (h : execDead P σ slot = some res) : ActStep P σ (.dead slot) res := by
  unfold execDead at h
  by_cases hc : closed P σ.loan (anchored slot) = true
  · rw [ifPos hc] at h
    obtain rfl : Res.ok _ = res := Option.some.inj h
    refine .dead (hung_exec (fun p => ?_) hc)
    simp only [ended, List.any_eq_true, Bool.and_eq_true, beq_iff_eq]
  · rw [ifNeg hc] at h
    cases h

/-- One act, or `none` where `exec` cannot say what it does. -/
def execAct (P : Prog) (wr : Bool) (σ : Store) : Act → Option Res
  | .use a => some (execUse P σ a)
  | .drop a => execDrop P σ a
  | .write a => execWrite P wr σ a
  | .invalidate a => execInvalidate P σ a
  | .begin slot => execBegin P σ slot
  | .dead slot => execDead P σ slot
  | .flag f v => some (.ok { σ with flag := fun g => if g == f then v.getD wr else σ.flag g })
  | .fresh ri =>
    some (.ok { σ with
      read := fun r => if r == ri then σ.flag (P.reads.getD ri 0) else σ.read r })
  | .lend k => some (.ok { σ with loan := fun j => j == k || σ.loan j })

theorem execAct_sound {P : Prog} {wr : Bool} {σ : Store} {act : Act} {res : Res}
    (h : execAct P wr σ act = some res) : ActStep P σ act res := by
  cases act with
  | use a =>
    obtain rfl : execUse P σ a = res := Option.some.inj h
    exact execUse_sound
  | drop a => exact execDrop_sound h
  | write a => exact execWrite_sound h
  | invalidate a => exact execInvalidate_sound h
  | «begin» slot => exact execBegin_sound h
  | dead slot => exact execDead_sound h
  | flag f v =>
    obtain rfl : Res.ok _ = res := Option.some.inj h
    exact .flag (fun c hc => by rw [hc]; rfl)
  | fresh ri =>
    obtain rfl : Res.ok _ = res := Option.some.inj h
    exact .fresh
  | lend k =>
    obtain rfl : Res.ok _ = res := Option.some.inj h
    exact .lend

def execActs (P : Prog) (wr : Bool) : Store → List Act → Option Res
  | σ, [] => some (.ok σ)
  | σ, a :: acts =>
    match execAct P wr σ a with
    | some (.ok σ') => execActs P wr σ' acts
    | some (.fault k) => some (.fault k)
    | none => none

theorem execActs_sound {P : Prog} {wr : Bool} : ∀ (acts : List Act) (σ : Store) (res : Res),
    execActs P wr σ acts = some res → ActsStep P σ acts res
  | [], σ, res, h => by
    obtain rfl : Res.ok σ = res := Option.some.inj h
    exact .nil
  | a :: acts, σ, res, h => by
    rw [execActs] at h
    cases he : execAct P wr σ a with
    | none =>
      rw [he] at h
      cases h
    | some r =>
      rw [he] at h
      cases r with
      | ok σ' => exact .cons (execAct_sound he) (execActs_sound acts σ' res h)
      | fault k =>
        obtain rfl : Res.fault k = res := Option.some.inj h
        exact .fault (execAct_sound he)

/-! ## A run -/

/-- What to do next: the instruction or the terminator's acts, an instruction's failure
edge, or the terminator's edge `n`. -/
inductive Move where
  | next
  | fail
  | edge (n : Nat)

def execInst (P : Prog) (wr : Bool) (m : Move) (b pc : Nat) (σ : Store) (i : Inst) : Cfg :=
  match m with
  | .next =>
    (match execActs P wr σ i.acts with
      | some (.ok σ') => .run b (pc + 1) σ'
      | some (.fault k) => .fault k
      | none => .run b pc σ)
  | .fail =>
    (match i.fail with
      | some t =>
        if i.isDrop then
          (match execActs P wr σ i.acts with
            | some (.ok σ') => .run t 0 σ'
            | some (.fault _) => .run b pc σ
            | none => .run b pc σ)
        else .run t 0 σ
      | none => .run b pc σ)
  | .edge _ => .run b pc σ

theorem execInst_steps {P : Prog} {wr : Bool} {m : Move} {b pc : Nat} {σ : Store}
    {blk : Block} {i : Inst} (hb : P.blocks[b]? = some blk) (hi : blk.insts[pc]? = some i) :
    Steps P (.run b pc σ) (execInst P wr m b pc σ i) := by
  unfold execInst
  cases m with
  | next =>
    cases he : execActs P wr σ i.acts with
    | none => exact .refl
    | some r =>
      cases r with
      | ok σ' => exact .tail .refl (.inst hb hi (execActs_sound _ _ _ he))
      | fault k => exact .tail .refl (.instFault hb hi (execActs_sound _ _ _ he))
  | fail =>
    cases hf : i.fail with
    | none => exact .refl
    | some t =>
      by_cases hd : i.isDrop = true
      · show Steps P (.run b pc σ)
          (if i.isDrop = true then
            (match execActs P wr σ i.acts with
              | some (.ok σ') => .run t 0 σ'
              | some (.fault _) => .run b pc σ
              | none => .run b pc σ)
          else .run t 0 σ)
        rw [ifPos hd]
        cases he : execActs P wr σ i.acts with
        | none => exact .refl
        | some r =>
          cases r with
          | ok σ' => exact .tail .refl (.dropFail hb hi hf hd (execActs_sound _ _ _ he))
          | fault k => exact .refl
      · show Steps P (.run b pc σ)
          (if i.isDrop = true then
            (match execActs P wr σ i.acts with
              | some (.ok σ') => .run t 0 σ'
              | some (.fault _) => .run b pc σ
              | none => .run b pc σ)
          else .run t 0 σ)
        rw [ifNeg hd]
        exact .tail .refl (.fail hb hi hf ((Bool.not_eq_true _).mp hd))
  | edge n => exact .refl

def execTerm (P : Prog) (wr : Bool) (m : Move) (b pc : Nat) (σ : Store) : Term → Cfg
  | .flagBranch ri yes no =>
    .run (if σ.read ri then yes else no).target 0
      (σ.enter (if σ.read ri then yes else no).args)
  | .other pre edges =>
    match execActs P wr σ pre with
    | some (.fault k) => .fault k
    | some (.ok σ') =>
      (match m with
        | .edge n =>
          (match edges[n]? with
            | some e => .run e.target 0 (σ'.enter e.args)
            | none => .run b pc σ)
        | .next => .run b pc σ
        | .fail => .run b pc σ)
    | none => .run b pc σ

theorem execTerm_steps {P : Prog} {wr : Bool} {m : Move} {b pc : Nat} {σ : Store}
    {blk : Block} (hb : P.blocks[b]? = some blk) (hpc : pc = blk.insts.length) :
    Steps P (.run b pc σ) (execTerm P wr m b pc σ blk.term) := by
  cases ht : blk.term with
  | flagBranch ri yes no =>
    exact .tail .refl (.branch (e := if σ.read ri then yes else no) hb hpc ht rfl)
  | other pre edges =>
    show Steps P (.run b pc σ)
      (match execActs P wr σ pre with
        | some (.fault k) => .fault k
        | some (.ok σ') =>
          (match m with
            | .edge n =>
              (match edges[n]? with
                | some e => .run e.target 0 (σ'.enter e.args)
                | none => .run b pc σ)
            | .next => .run b pc σ
            | .fail => .run b pc σ)
        | none => .run b pc σ)
    cases he : execActs P wr σ pre with
    | none => exact .refl
    | some r =>
      cases r with
      | fault k => exact .tail .refl (.termFault hb hpc ht (execActs_sound _ _ _ he))
      | ok σ' =>
        cases m with
        | next => exact .refl
        | fail => exact .refl
        | edge n =>
          dsimp only
          cases hn : edges[n]? with
          | none => exact .refl
          | some e =>
            exact .tail .refl
              (.goto hb hpc ht (execActs_sound _ _ _ he) (List.mem_of_getElem? hn))

def execMove (P : Prog) (wr : Bool) (m : Move) : Cfg → Cfg
  | .run b pc σ =>
    (match P.blocks[b]? with
      | none => .run b pc σ
      | some blk =>
        (match blk.insts[pc]? with
          | some i => execInst P wr m b pc σ i
          | none =>
            if pc = blk.insts.length then execTerm P wr m b pc σ blk.term else .run b pc σ))
  | .fault k => .fault k
  | .done σ => .done σ

theorem execMove_steps (P : Prog) (wr : Bool) (m : Move) :
    ∀ c, Steps P c (execMove P wr m c)
  | .fault _ => .refl
  | .done _ => .refl
  | .run b pc σ => by
    show Steps P (.run b pc σ)
      (match P.blocks[b]? with
        | none => .run b pc σ
        | some blk =>
          (match blk.insts[pc]? with
            | some i => execInst P wr m b pc σ i
            | none =>
              if pc = blk.insts.length then execTerm P wr m b pc σ blk.term
              else .run b pc σ))
    cases hb : P.blocks[b]? with
    | none => exact .refl
    | some blk =>
      dsimp only
      cases hi : blk.insts[pc]? with
      | some i => exact execInst_steps hb hi
      | none =>
        by_cases hpc : pc = blk.insts.length
        · show Steps P (.run b pc σ)
            (if pc = blk.insts.length then execTerm P wr m b pc σ blk.term else .run b pc σ)
          rw [ifPos hpc]
          exact execTerm_steps hb hpc
        · show Steps P (.run b pc σ)
            (if pc = blk.insts.length then execTerm P wr m b pc σ blk.term else .run b pc σ)
          rw [ifNeg hpc]
          exact .refl

/-- The body run along `ms`. A move that does not apply is skipped. -/
def exec (P : Prog) (wr : Bool) : List Move → Cfg → Cfg
  | [], c => c
  | m :: ms, c => exec P wr ms (execMove P wr m c)

theorem Steps.trans {P : Prog} {a b c : Cfg} (h1 : Steps P a b) (h2 : Steps P b c) :
    Steps P a c := by
  induction h2 with
  | refl => exact h1
  | tail _ hs ih => exact .tail ih hs

theorem exec_steps (P : Prog) (wr : Bool) : ∀ ms c, Steps P c (exec P wr ms c)
  | [], _ => .refl
  | m :: ms, c => (execMove_steps P wr m c).trans (exec_steps P wr ms _)

/-! ## From entry -/

/-- An entry store: every flag holds `fl`, and each parameter that names one place and is
no join points at it. -/
def Store.entry (P : Prog) (fl : Bool) : Store :=
  ⟨fun _ => .live, fun _ => fl, fun _ => false, fun _ => false,
    fun v => if (P.joinOf v).isSome then none else one (P.under v)⟩

theorem entry_initial (P : Prog) (fl : Bool) : Initial P (Store.entry P fl) where
  live := fun _ => rfl
  loan := fun _ => rfl
  join := fun _ h => ifPos h
  under := fun v y h => by
    have h' : (if (P.joinOf v).isSome = true then none else one (P.under v)) = some y := h
    by_cases hj : (P.joinOf v).isSome = true
    · rw [ifPos hj] at h'
      cases h'
    · rw [ifNeg hj] at h'
      obtain ⟨hne, hc, hall⟩ := one_some.mp h'
      obtain ⟨z, hz⟩ := List.exists_mem_of_ne_nil _ hne
      exact ⟨hall z hz ▸ hz, hc⟩
  plain := fun v x hj ho => by
    show (if (P.joinOf v).isSome = true then none else one (P.under v)) = some x
    rw [ifNeg (by rw [hj]; exact Bool.false_ne_true), ho]

def Cfg.faultOf : Cfg → Option Fault
  | .fault k => some k
  | .run .. => none
  | .done _ => none

/-- A body that faults when run along `ms` reaches the fault. -/
theorem reach_of_exec {P : Prog} {k : Fault} (wr : Bool) (ms : List Move)
    (h : (exec P wr ms (.run 0 0 (Store.entry P wr))).faultOf = some k) :
    Reachable P (.fault k) := by
  refine ⟨Store.entry P wr, entry_initial P wr, ?_⟩
  have hs := exec_steps P wr ms (.run 0 0 (Store.entry P wr))
  cases hc : exec P wr ms (.run 0 0 (Store.entry P wr)) with
  | fault k' =>
    rw [hc] at h hs
    obtain rfl : k' = k := Option.some.inj h
    exact hs
  | run b pc σ =>
    rw [hc] at h
    cases h
  | done σ =>
    rw [hc] at h
    cases h

end Hirsel.CoreIR.Own
