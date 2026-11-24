/-
  The Hirsel prelude every extracted module imports (hand-written, trusted).

  A Result-returning Hirsel body extracts to `Hirsel.Prog S E α`:
    * `E` is the body's own `Result` error (`throw`, `?` is `←`),
    * below it the FAIL-2 cause layer (`Hirsel.Cause`: overflow, division, list bounds,
      capacity, the fuel stop `outOfTime`, ...), raised by checked arithmetic and loops,
    * below that the body's state `S`: its `store.Tx` or its single `mut` parameter.
  A cause sits outside the state, so the state a run reaches survives the cause (the caller
  decides: a transaction that raised keeps nothing, S1).

  Every primitive here has a `@[simp]` lemma about `Hirsel.run`, so a proof about an
  extracted body is symbolic execution by `simp`.
-/
namespace Hirsel

/-- FAIL-2's causes. -/
inductive Cause
  | overflow | bounds | division | unwrap | fail | cancelled | capacity | outOfTime
  deriving DecidableEq, Repr, Inhabited

abbrev Bytes := List UInt8

/-- A body's cause layer over its state. -/
abbrev M (S : Type) := ExceptT Cause (StateM S)

/-- A body returning `Result[α, E]` with state `S`. -/
abbrev Prog (S E α : Type) := ExceptT E (M S) α

/-- Run a body from state `s`: its cause or its `Result`, and the state it leaves. -/
def run {S E α : Type} (m : Prog S E α) (s : S) : Except Cause (Except E α) × S :=
  (m.run.run).run s

/-- Raise a cause. -/
def cause {S E α : Type} (c : Cause) : Prog S E α := ExceptT.lift (throw c)

/-- The body's state. -/
def get {S E : Type} : Prog S E S := ExceptT.lift (ExceptT.lift MonadState.get)

/-- Replace the body's state. -/
def modify {S E : Type} (f : S → S) : Prog S E Unit :=
  ExceptT.lift (ExceptT.lift (MonadState.modifyGet fun s => ((), f s)))

/-- `r ?=> f`: a `Result` of another error type, its error mapped by `f`. -/
def tryMap {S E ε α : Type} (r : Except ε α) (f : ε → E) : Prog S E α :=
  match r with
  | .ok a => pure a
  | .error e => throw (f e)

/-! ## Loops: fuel-bounded, structurally recursive -/

/-- One turn of a loop: continue with the carried values, or stop with them. -/
inductive Step (σ : Type)
  | next (s : σ)
  | done (s : σ)

/-- The number of turns a loop may take before the run stops with `outOfTime` (MEM-30's
    run stop; any bound is sound for the partial-correctness theorems). -/
def fuel : Nat := 2 ^ 64

def loopN {S E σ : Type} (body : σ → Prog S E (Step σ)) : Nat → σ → Prog S E σ
  | 0, _ => cause .outOfTime
  | n + 1, s => do
    match (← body s) with
    | .next s' => loopN body n s'
    | .done s' => pure s'

/-- `while` with carried values `σ`. -/
def loop {S E σ : Type} (init : σ) (body : σ → Prog S E (Step σ)) : Prog S E σ :=
  loopN body fuel init

/-! ## Checked integer arithmetic over every width -/

class Width (α : Type) where
  toNat : α → Nat
  ofNat : Nat → α
  size : Nat

instance : Width UInt8 := ⟨UInt8.toNat, UInt8.ofNat, UInt8.size⟩
instance : Width UInt16 := ⟨UInt16.toNat, UInt16.ofNat, UInt16.size⟩
instance : Width UInt32 := ⟨UInt32.toNat, UInt32.ofNat, UInt32.size⟩
instance : Width UInt64 := ⟨UInt64.toNat, UInt64.ofNat, UInt64.size⟩

namespace int

/-- `int.checked_add`: `None` when the sum leaves the width. -/
def checked_add {α : Type} [Width α] (a b : α) : Option α :=
  if Width.toNat a + Width.toNat b < Width.size α then some (Width.ofNat (Width.toNat a + Width.toNat b))
  else none

/-- `int.checked_sub`. -/
def checked_sub {α : Type} [Width α] (a b : α) : Option α :=
  if Width.toNat b ≤ Width.toNat a then some (Width.ofNat (Width.toNat a - Width.toNat b)) else none

/-- `int.checked_mul`. -/
def checked_mul {α : Type} [Width α] (a b : α) : Option α :=
  if Width.toNat a * Width.toNat b < Width.size α then some (Width.ofNat (Width.toNat a * Width.toNat b))
  else none

end int

/-- `a + b`: raises `overflow` outside the width. -/
def add {S E α : Type} [Width α] (a b : α) : Prog S E α :=
  match int.checked_add a b with
  | some c => pure c
  | none => cause .overflow

/-- `a - b`: raises `overflow` below zero. -/
def sub {S E α : Type} [Width α] (a b : α) : Prog S E α :=
  match int.checked_sub a b with
  | some c => pure c
  | none => cause .overflow

/-- `a * b`: raises `overflow` outside the width. -/
def mul {S E α : Type} [Width α] (a b : α) : Prog S E α :=
  match int.checked_mul a b with
  | some c => pure c
  | none => cause .overflow

/-- `a / b`: raises `division` on a zero divisor. -/
def div {S E α : Type} [Width α] (a b : α) : Prog S E α :=
  if Width.toNat b = 0 then cause .division else pure (Width.ofNat (Width.toNat a / Width.toNat b))

/-- `a % b`: raises `division` on a zero divisor. -/
def rem {S E α : Type} [Width α] (a b : α) : Prog S E α :=
  if Width.toNat b = 0 then cause .division else pure (Width.ofNat (Width.toNat a % Width.toNat b))

theorem int.checked_add_u64 {a b c : UInt64} (h : int.checked_add a b = some c) :
    c.toNat = a.toNat + b.toNat := by
  unfold int.checked_add at h
  split at h
  · cases h
    rename_i hlt
    simp only [Width.toNat, Width.ofNat, Width.size] at hlt ⊢
    rw [UInt64.toNat_ofNat_of_lt' hlt]
  · cases h

/-! ## Lists -/

namespace list

/-- `list.push(mut l, x)`. -/
def push {α : Type} (l : List α) (x : α) : List α := l ++ [x]

/-- `l[i]`: raises `bounds` outside the list. -/
def «at» {S E α : Type} (l : List α) (i : UInt64) : Prog S E α :=
  match l[i.toNat]? with
  | some x => pure x
  | none => cause .bounds

end list

/-! ## An insertion-ordered map: a store table's contents -/

structure Map (K V : Type) where
  entries : List (K × V)
  deriving Repr

namespace Map

variable {K V : Type} [DecidableEq K]

def getL (k : K) : List (K × V) → Option V
  | [] => none
  | (j, v) :: r => if j = k then some v else getL k r

def insertL (k : K) (v : V) : List (K × V) → List (K × V)
  | [] => [(k, v)]
  | (j, w) :: r => if j = k then (k, v) :: r else (j, w) :: insertL k v r

def eraseL (k : K) : List (K × V) → List (K × V)
  | [] => []
  | (j, w) :: r => if j = k then eraseL k r else (j, w) :: eraseL k r

def empty : Map K V := ⟨[]⟩
def get (m : Map K V) (k : K) : Option V := getL k m.entries
/-- Replace the value of a present key in place; append a new key. -/
def insert (m : Map K V) (k : K) (v : V) : Map K V := ⟨insertL k v m.entries⟩
def erase (m : Map K V) (k : K) : Map K V := ⟨eraseL k m.entries⟩

theorem getL_insertL (j k : K) (v : V) (l : List (K × V)) :
    getL j (insertL k v l) = if j = k then some v else getL j l := by
  induction l with
  | nil => simp [insertL, getL, eq_comm]
  | cons p r ih =>
    obtain ⟨i, w⟩ := p
    by_cases hik : i = k
    · subst hik; by_cases hj : j = i <;> simp [insertL, getL, hj, eq_comm]
    · simp only [insertL, getL, hik, ite_false, ih]
      by_cases hj : j = k
      · subst hj; simp [hik]
      · simp [hj]

theorem getL_eraseL (j k : K) (l : List (K × V)) :
    getL j (eraseL k l) = if j = k then none else getL j l := by
  induction l with
  | nil => simp [eraseL, getL]
  | cons p r ih =>
    obtain ⟨i, w⟩ := p
    by_cases hik : i = k
    · subst hik
      by_cases hj : j = i
      · subst hj; simp [eraseL, ih]
      · simp [eraseL, getL, ih, hj, Ne.symm hj]
    · simp only [eraseL, getL, hik, ite_false, ih]
      by_cases hj : j = k
      · subst hj; simp [hik]
      · simp [hj]

@[simp] theorem get_empty (j : K) : (empty : Map K V).get j = none := rfl

@[simp] theorem get_insert (m : Map K V) (j k : K) (v : V) :
    (m.insert k v).get j = if j = k then some v else m.get j := getL_insertL j k v m.entries

@[simp] theorem get_erase (m : Map K V) (j k : K) :
    (m.erase k).get j = if j = k then none else m.get j := getL_eraseL j k m.entries

end Map

/-! ## The run lemmas: symbolic execution -/

section run
variable {S E α β : Type}

@[simp] theorem run_pure (a : α) (s : S) : run (pure a : Prog S E α) s = (.ok (.ok a), s) := rfl

@[simp] theorem run_throw (e : E) (s : S) : run (throw e : Prog S E α) s = (.ok (.error e), s) := rfl

@[simp] theorem run_cause (c : Cause) (s : S) : run (cause c : Prog S E α) s = (.error c, s) := rfl

@[simp] theorem run_get (s : S) : run (get : Prog S E S) s = (.ok (.ok s), s) := rfl

@[simp] theorem run_modify (f : S → S) (s : S) : run (modify f : Prog S E Unit) s = (.ok (.ok ()), f s) := rfl

/-- What a run does next: continue `k` with the value and state of an `Ok`, or stop with an
    `Error` or a cause. First-order, so `simp` pushes it through `if` (`andThen_ite`). -/
def andThen {S E α β : Type} (r : Except Cause (Except E α) × S)
    (k : α → S → Except Cause (Except E β) × S) : Except Cause (Except E β) × S :=
  match r with
  | (.ok (.ok a), s') => k a s'
  | (.ok (.error e), s') => (.ok (.error e), s')
  | (.error c, s') => (.error c, s')

@[simp] theorem andThen_ok (a : α) (s : S) (k : α → S → Except Cause (Except E β) × S) :
    andThen (.ok (.ok a), s) k = k a s := rfl

@[simp] theorem andThen_error (e : E) (s : S) (k : α → S → Except Cause (Except E β) × S) :
    andThen (.ok (.error e), s) k = (.ok (.error e), s) := rfl

@[simp] theorem andThen_cause (c : Cause) (s : S) (k : α → S → Except Cause (Except E β) × S) :
    andThen (.error c, s) k = (.error c, s) := rfl

@[simp] theorem andThen_ite (c : Prop) [Decidable c] (x y : Except Cause (Except E α) × S)
    (k : α → S → Except Cause (Except E β) × S) :
    andThen (if c then x else y) k = if c then andThen x k else andThen y k := by
  split <;> rfl

/-- Invert a run that continued: which outcome the first part had. -/
theorem andThen_eq {r : Except Cause (Except E α) × S} {k : α → S → Except Cause (Except E β) × S}
    {out : Except Cause (Except E β) × S} (h : andThen r k = out) :
    (∃ a s', r = (.ok (.ok a), s') ∧ k a s' = out) ∨
      (∃ e s', r = (.ok (.error e), s') ∧ out = (.ok (.error e), s')) ∨
      (∃ c s', r = (.error c, s') ∧ out = (.error c, s')) := by
  rcases r with ⟨_ | _ | a, s'⟩
  · exact .inr (.inr ⟨_, _, rfl, h.symm⟩)
  · exact .inr (.inl ⟨_, _, rfl, h.symm⟩)
  · exact .inl ⟨_, _, rfl, h⟩

@[simp] theorem run_bind (m : Prog S E α) (f : α → Prog S E β) (s : S) :
    run (m >>= f) s = andThen (run m s) (fun a s' => run (f a) s') := by
  unfold run andThen
  rcases h : (m.run.run).run s with ⟨_ | _ | a, s'⟩ <;>
    simp_all [bind, ExceptT.bind, ExceptT.mk, ExceptT.bindCont, ExceptT.run, StateT.bind, StateT.run,
      pure, ExceptT.pure, StateT.pure]

@[simp] theorem run_map (f : α → β) (m : Prog S E α) (s : S) :
    run (f <$> m) s = andThen (run m s) (fun a s' => (.ok (.ok (f a)), s')) := by
  rw [map_eq_pure_bind, run_bind]
  rfl

@[simp] theorem run_ite (c : Prop) [Decidable c] (a b : Prog S E α) (s : S) :
    run (if c then a else b) s = if c then run a s else run b s := by
  split <;> rfl

@[simp] theorem run_tryMap {ε : Type} (r : Except ε α) (f : ε → E) (s : S) :
    run (tryMap r f : Prog S E α) s =
      match r with
      | .ok a => (.ok (.ok a), s)
      | .error e => (.ok (.error (f e)), s) := by
  cases r <;> rfl

theorem run_loopN {σ : Type} (body : σ → Prog S E (Step σ)) (n : Nat) (x : σ) (s : S) :
    run (loopN body (n + 1) x) s =
      andThen (run (body x) s) (fun st s' => match st with
        | .next x' => run (loopN body n x') s'
        | .done x' => (.ok (.ok x'), s')) := by
  simp only [loopN, run_bind]
  rcases run (body x) s with ⟨_ | _ | (x' | x'), s'⟩ <;> rfl

@[simp] theorem run_loopN_zero {σ : Type} (body : σ → Prog S E (Step σ)) (x : σ) (s : S) :
    run (loopN body 0 x) s = (.error .outOfTime, s) := rfl

end run

end Hirsel

namespace Hirsel

def list.len (xs : List α) : UInt64 := UInt64.ofNat xs.length

def list.slice (xs : List α) (lo hi : UInt64) : Except Cause (List α) :=
  if lo ≤ hi ∧ hi.toNat ≤ xs.length then
    .ok ((xs.drop lo.toNat).take (hi.toNat - lo.toNat))
  else .error .bounds

def range (lo hi : UInt64) : List UInt64 :=
  (List.range' lo.toNat (hi.toNat - lo.toNat)).map UInt64.ofNat

def getIndex {C α : Type} {valid : C → Nat → Prop} [GetElem? C Nat α valid]
    (xs : C) (i : UInt64) : Except Cause α :=
  match xs[i.toNat]? with
  | some x => .ok x
  | none => .error .bounds

def checkedAdd [Width α] (a b : α) : Except Cause α :=
  match int.checked_add a b with
  | some x => .ok x
  | none => .error .overflow

def andAlso (a b : Except Cause Bool) : Except Cause Bool := do
  if ← a then b else return false

def orElse (a b : Except Cause Bool) : Except Cause Bool := do
  if ← a then return true else b

end Hirsel
