/-
  Proofs about the generated runner.triggers (Generated/Triggers.lean, 100% extracted).
  Hand-written; zero sorry. Every theorem is about `Tables.handle`, the handle whose tables
  are distinct fields of `Tables.Data` (the store gives each declared table its own key
  space). Store assumptions S1..S5 are in Store.lean and REPORT.md; S5 is the explicit
  hypothesis `Op.named_below`.
-/
import Generated.Triggers

set_option linter.unusedSimpArgs false
namespace runner.triggers
open Hirsel Hirsel.store

abbrev T := Tx Tables.Data

/-! ## Stale-attempt fencing: a stale attempt is refused and writes nothing -/

theorem fence_stale (a : Attempt) (tx : T) (row : SubRow)
    (h : tx.data.rows.get a.id = some row) (hne : row.attempt ≠ a.number) :
    run (fence Tables.handle a) tx = (.ok (.error .StaleAttempt), tx) := by
  simp [fence, current, read, stored, Tables.handle, current_attempt, h, hne]

theorem finish_stale (a : Attempt) (t : Terminal) (tx : T) (row : SubRow)
    (h : tx.data.rows.get a.id = some row) (hne : row.attempt ≠ a.number) :
    run (finish Tables.handle a t) tx = (.ok (.error .StaleAttempt), tx) := by
  simp [finish, read, stored, Tables.handle, h, hne]

theorem hold_stale (a : Attempt) (step : UInt64) (x y : Bytes) (tx : T) (row : SubRow)
    (h : tx.data.rows.get a.id = some row) (hne : row.attempt ≠ a.number) :
    run (hold Tables.handle a step x y) tx = (.ok (.error .StaleAttempt), tx) := by
  simp [hold, current, read, stored, Tables.handle, current_attempt, h, hne]

/-! ## start issues the row's previous attempt plus one -/

theorem start_issues_next (p : Pending) (rev : Bytes) (tx tx' : T) (a : Attempt)
    (h : run (start Tables.handle p rev) tx = (.ok (.ok (.Body a)), tx')) :
    ∃ row, tx.data.rows.get p.id = some row ∧ a.number.toNat = row.attempt.toNat + 1 ∧
      tx'.data.rows.get p.id = some { row with
        attempt := a.number
        state := .Running
        revision := (if row.attempt = 0 then rev else row.revision) } := by
  simp only [start, read, stored, Tables.handle, copy_bytes, run_bind, run_info, run_fetch, run_pure] at h
  rcases hr : tx.data.rows.get p.id with _ | row <;> simp [hr] at h
  refine ⟨row, rfl, ?_⟩
  by_cases h1 : tx.info.at < row.due <;> simp [h1] at h
  by_cases h2 : row.cancel = true ∨ parked row.state = true <;> simp [h2] at h
  by_cases h3 : revision_mismatch row rev = true <;> simp [h3] at h
  rcases hn : int.checked_add row.attempt 1 with _ | n
  · split at h <;> simp [hn] at h
  · have hn' := int.checked_add_u64 hn
    have h1' : (1 : UInt64).toNat = 1 := rfl
    split at h <;> simp [hn] at h <;> obtain ⟨rfl, rfl⟩ := h <;> simp_all

/-! ## Loops -/

/-- A loop whose body leaves the state alone and keeps `P` of its carried values leaves the
    state alone and returns a value with `P`. -/
theorem loop_inv {S E σ : Type} {P : σ → Prop} {body : σ → Prog S E (Step σ)}
    (hb : ∀ x s, P x → (run (body x) s).2 = s ∧
      ∀ y, ((run (body x) s).1 = .ok (.ok (.next y)) ∨ (run (body x) s).1 = .ok (.ok (.done y))) → P y)
    {x : σ} {s s' : S} {r : Except Cause (Except E σ)} (hx : P x)
    (h : run (Hirsel.loop x body) s = (r, s')) : s' = s ∧ ∀ y, r = .ok (.ok y) → P y := by
  unfold Hirsel.loop at h
  generalize Hirsel.fuel = n at h
  induction n generalizing x with
  | zero => simp at h; obtain ⟨rfl, rfl⟩ := h; simp
  | succ n ih =>
    rw [run_loopN] at h
    obtain ⟨hs, hy⟩ := hb x s hx
    rcases hbx : run (body x) s with ⟨_ | _ | (y | y), s1⟩ <;> rw [hbx] at hs hy <;>
      simp only at hs <;> subst hs <;> simp [hbx] at h
    · obtain ⟨rfl, rfl⟩ := h; simp
    · obtain ⟨rfl, rfl⟩ := h; simp
    · exact ih (hy y (.inl rfl)) h
    · obtain ⟨rfl, rfl⟩ := h; exact ⟨rfl, fun z hz => by cases hz; exact hy y (.inr rfl)⟩

/-! ## The history law: a terminal, once written, never changes -/

/-- Every trigger operation a transaction body can run. -/
inductive Op
  | write (entries : Entries) (target : Entry Bytes) (arg : Bytes) (when : Due)
  | start (p : Pending) (rev : Bytes)
  | hold (a : Attempt) (step : UInt64) (expected actual : Bytes)
  | release (id : Id) (rev : Option Bytes)
  | requestCancel (id : Id)
  | finish (a : Attempt) (t : Terminal)
  | finishCancel (p : Pending)
  | finishTransact (p : Pending) (t : Terminal)

def Op.body : Op → Prog T Error Unit
  | .write e tg arg w => do let _ ← runner.triggers.write Tables.handle e tg arg w
  | .start p rev => do let _ ← runner.triggers.start Tables.handle p rev
  | .hold a s x y => runner.triggers.hold Tables.handle a s x y
  | .release id r => runner.triggers.release Tables.handle id r
  | .requestCancel id => runner.triggers.request_cancel Tables.handle id
  | .finish a t => runner.triggers.finish Tables.handle a t
  | .finishCancel p => runner.triggers.finish_cancel Tables.handle p
  | .finishTransact p t => runner.triggers.finish_transact Tables.handle p t

/-- S5, narrowed to the operations that write a terminal: the id they settle comes from a
    committed snapshot or an earlier attempt, so its version is below the running
    transaction's. (A trigger written and settled in one transaction is outside the law.) -/
def Op.named_below (v : UInt64) : Op → Prop
  | .finish a _ => a.id.version < v
  | .finishCancel p | .finishTransact p _ => p.id.version < v
  | _ => True

/-- The invariant: an id with a terminal has no pending row, and was settled below `v`. -/
def Settled (d : Tables.Data) (v : UInt64) : Prop :=
  ∀ id t, d.outcomes.get id = some t → d.rows.get id = none ∧ id.version < v

/-- A body that rewrites only rows that exist and leaves the outcomes and call info alone. -/
def Rewrites (tx tx' : T) : Prop :=
  tx'.info = tx.info ∧ tx'.data.outcomes = tx.data.outcomes ∧
    ∀ j, tx.data.rows.get j = none → tx'.data.rows.get j = none

theorem Rewrites.rfl' (tx : T) : Rewrites tx tx := ⟨rfl, rfl, fun _ h => h⟩

theorem Rewrites.put {tx : T} {k : Id} {old new : SubRow} (h : tx.data.rows.get k = some old) :
    Rewrites tx { tx with data := { tx.data with rows := tx.data.rows.insert k new } } := by
  refine ⟨rfl, rfl, fun j hj => ?_⟩
  by_cases e : j = k
  · subst e; simp_all
  · simp [e, hj]

/-- A body that changes only rows of the running transaction's version. -/
def CreatesFresh (tx tx' : T) : Prop :=
  tx'.info = tx.info ∧ tx'.data.outcomes = tx.data.outcomes ∧
    ∀ j, tx'.data.rows.get j ≠ tx.data.rows.get j → j.version = tx.info.id.version

macro "run_simp" : tactic => `(tactic| simp only [Op.body, write, start, hold, release,
  request_cancel, finish, finish_cancel, finish_transact, consume, current, read, stored,
  copy_bytes, Tables.handle, run_bind, run_info, run_fetch, run_put, run_delete, run_pure,
  run_throw, run_map, run_ite, run_tryMap, run_get, run_modify, andThen_ok, andThen_error,
  andThen_cause, andThen_ite] at *)

-- Split every `if` and `match` a symbolically executed body leaves in `h`.
set_option hygiene false in
macro "split_run" : tactic => `(tactic| (repeat' (split at h <;> try run_simp)))

theorem hold_rewrites (a : Attempt) (s : UInt64) (x y : Bytes) (tx tx' : T) r
    (h : run (hold Tables.handle a s x y) tx = (r, tx')) : Rewrites tx tx' := by
  run_simp
  rcases hf : tx.data.rows.get a.id with _ | row <;> simp [hf] at h
  · obtain ⟨rfl, rfl⟩ := h; exact .rfl' _
  · split_run
    all_goals (cases h)
    all_goals first | exact .rfl' _ | exact Rewrites.put hf

theorem start_rewrites (p : Pending) (rev : Bytes) (tx tx' : T) r
    (h : run (start Tables.handle p rev) tx = (r, tx')) : Rewrites tx tx' := by
  run_simp
  rcases hf : tx.data.rows.get p.id with _ | row <;> simp [hf] at h
  · obtain ⟨rfl, rfl⟩ := h; exact .rfl' _
  · split_run
    all_goals (cases h)
    all_goals first | exact .rfl' _ | exact Rewrites.put hf

theorem release_rewrites (id : Id) (rev : Option Bytes) (tx tx' : T) r
    (h : run (release Tables.handle id rev) tx = (r, tx')) : Rewrites tx tx' := by
  run_simp
  rcases hf : tx.data.rows.get id with _ | row <;> simp [hf] at h
  · obtain ⟨rfl, rfl⟩ := h; exact .rfl' _
  · split_run
    all_goals (cases h)
    all_goals first | exact .rfl' _ | exact Rewrites.put hf

theorem request_cancel_rewrites (id : Id) (tx tx' : T) r
    (h : run (request_cancel Tables.handle id) tx = (r, tx')) : Rewrites tx tx' := by
  run_simp
  rcases hf : tx.data.rows.get id with _ | row <;> simp [hf] at h
  · obtain ⟨rfl, rfl⟩ := h; exact .rfl' _
  · obtain ⟨rfl, rfl⟩ := h; exact Rewrites.put hf

/-- The probe loop keeps the proposed version and leaves the store unchanged. -/
theorem allocate_id_fresh {A : Type} (e : Entries) (tg : Entry A) (tx tx' : T) r
    (h : run (allocate_id Tables.handle e tg) tx = (r, tx')) :
    tx' = tx ∧ ∀ p, r = .ok (.ok p) → p.1.version = tx.info.id.version := by
  simp only [allocate_id, run_bind, run_ite, run_throw, run_info, run_pure, andThen_ok, andThen_error,
    andThen_ite] at h
  split at h
  · cases h
    simp
  rcases andThen_eq h with ⟨id, tx1, hl, hk⟩ | ⟨e, tx1, hl, hk⟩ | ⟨c, tx1, hl, hk⟩ <;> clear h
  all_goals
    obtain ⟨rfl, hv⟩ := loop_inv (P := fun i : Id => i.version = tx.info.id.version) (hx := rfl)
      (hb := by
        intro x s hx
        simp only [stored, Tables.handle, run_bind, run_fetch, run_pure, run_throw, andThen_ok,
          andThen_error]
        split <;> (try split) <;> simp_all) hl
  · obtain ⟨rfl, rfl⟩ := Prod.mk.inj hk
    exact ⟨rfl, fun p hp => by cases hp; exact hv _ rfl⟩
  · obtain ⟨rfl, rfl⟩ := Prod.mk.inj hk
    simp
  · obtain ⟨rfl, rfl⟩ := Prod.mk.inj hk
    simp

/-- A successful write creates a row only at the running version. -/
theorem write_fresh {A : Type} (e : Entries) (tg : Entry A) (arg : A) (w : Due) (tx tx' : T) r
    (h : run (write Tables.handle e tg arg w) tx = (r, tx')) : CreatesFresh tx tx' := by
  simp only [write, run_bind] at h
  rcases andThen_eq h with ⟨p, s, ha, hs⟩ | ⟨_, s, ha, hs⟩ | ⟨_, s, ha, hs⟩ <;>
    obtain ⟨rfl, fresh⟩ := allocate_id_fresh e tg _ _ _ ha
  · have hv := fresh p rfl
    rcases p with ⟨id, dueAt⟩
    have h := hs
    clear hs
    simp only [write_at] at h
    run_simp
    split_run
    all_goals
      obtain ⟨rfl, rfl⟩ := Prod.mk.inj h
      refine ⟨rfl, rfl, fun j hj => ?_⟩
      first
      | exact absurd rfl hj
      | simp only [Hirsel.Map.get_insert] at hj
        split at hj
        · subst_vars; exact hv
        · exact absurd rfl hj
  · obtain ⟨rfl, rfl⟩ := Prod.mk.inj hs; exact ⟨rfl, rfl, fun _ h => absurd rfl h⟩
  · obtain ⟨rfl, rfl⟩ := Prod.mk.inj hs; exact ⟨rfl, rfl, fun _ h => absurd rfl h⟩

theorem settled_of_rewrites {tx tx' : T} {v : UInt64} (hinv : Settled tx.data v)
    (hr : Rewrites tx tx') : Settled tx'.data v := by
  obtain ⟨-, ho, hrows⟩ := hr
  intro id t h
  rw [ho] at h
  exact ⟨hrows id (hinv id t h).1, (hinv id t h).2⟩

theorem settled_of_fresh {tx tx' : T} (hinv : Settled tx.data tx.info.id.version)
    (hf : CreatesFresh tx tx') : Settled tx'.data tx.info.id.version := by
  obtain ⟨-, ho, hrows⟩ := hf
  intro id t h
  rw [ho] at h
  obtain ⟨hn, hv⟩ := hinv id t h
  refine ⟨?_, hv⟩
  by_cases e : tx'.data.rows.get id = tx.data.rows.get id
  · rw [e, hn]
  · have := hrows id e; rw [this] at hv; exact absurd hv (UInt64.lt_irrefl _)

/-- What `consume` leaves, for an id with a row below the running version: the invariant and
    every earlier terminal still hold. -/
theorem consume_settles {tx : T} {k : Id} {term : Terminal} {row : SubRow}
    (hrow : tx.data.rows.get k = some row) (hk : k.version < tx.info.id.version)
    (hinv : Settled tx.data tx.info.id.version) :
    Settled ⟨tx.data.rows.erase k, tx.data.outcomes.insert k term⟩ tx.info.id.version ∧
      ∀ id t, tx.data.outcomes.get id = some t →
        (tx.data.outcomes.insert k term).get id = some t := by
  refine ⟨fun id t ho => ?_, fun id t ho => ?_⟩
  · by_cases e : id = k
    · subst e; simp_all
    · simp [e] at ho ⊢; exact hinv id t ho
  · by_cases e : id = k
    · subst e; have := (hinv _ _ ho).1; simp_all
    · simp [e, ho]

theorem op_preserves (o : Op) (tx tx' : T) (r : Except Error Unit)
    (hinv : Settled tx.data tx.info.id.version) (hs : o.named_below tx.info.id.version)
    (h : run o.body tx = (.ok r, tx')) :
    tx'.info = tx.info ∧ Settled tx'.data tx.info.id.version ∧
      ∀ id t, tx.data.outcomes.get id = some t → tx'.data.outcomes.get id = some t := by
  have ofR : Rewrites tx tx' → tx'.info = tx.info ∧ Settled tx'.data tx.info.id.version ∧
      ∀ id t, tx.data.outcomes.get id = some t → tx'.data.outcomes.get id = some t := fun hr =>
    ⟨hr.1, settled_of_rewrites hinv hr, fun _ _ ho => hr.2.1 ▸ ho⟩
  cases o with
  | write e tg arg w =>
    simp only [Op.body, run_bind] at h
    rcases hw : run (write Tables.handle e tg arg w) tx with ⟨_ | _ | id, tx1⟩ <;> simp [hw] at h <;>
      obtain ⟨rfl, rfl⟩ := h <;>
      exact ⟨(write_fresh _ _ _ _ _ _ _ hw).1, settled_of_fresh hinv (write_fresh _ _ _ _ _ _ _ hw),
        fun _ _ ho => (write_fresh _ _ _ _ _ _ _ hw).2.1 ▸ ho⟩
  | start p rev =>
    simp only [Op.body, run_bind] at h
    rcases hw : run (start Tables.handle p rev) tx with ⟨_ | _ | st, tx1⟩ <;> simp [hw] at h <;>
      obtain ⟨rfl, rfl⟩ := h <;> exact ofR (start_rewrites _ _ _ _ _ hw)
  | hold a s x y => exact ofR (hold_rewrites _ _ _ _ _ _ _ h)
  | release id rev => exact ofR (release_rewrites _ _ _ _ _ h)
  | requestCancel id => exact ofR (request_cancel_rewrites _ _ _ _ h)
  | finish a t =>
    simp only [Op.named_below] at hs
    simp only [Op.body, finish, read, stored, Tables.handle, run_bind, run_fetch, run_pure, run_throw,
      run_ite, consume, run_put, run_delete] at h
    rcases hf : tx.data.rows.get a.id with _ | row <;> simp [hf] at h
    · obtain ⟨rfl, rfl⟩ := h; exact ofR (.rfl' _)
    · split_run
      all_goals (cases h)
      all_goals first
        | exact ofR (.rfl' _)
        | exact ⟨rfl, consume_settles hf hs hinv⟩
  | finishCancel p =>
    simp only [Op.named_below] at hs
    simp only [Op.body, finish_cancel, read, stored, Tables.handle, run_bind, run_fetch, run_pure,
      run_throw, run_ite, consume, run_put, run_delete] at h
    rcases hf : tx.data.rows.get p.id with _ | row <;> simp [hf] at h
    · obtain ⟨rfl, rfl⟩ := h; exact ofR (.rfl' _)
    · split_run
      all_goals (cases h)
      all_goals first
        | exact ofR (.rfl' _)
        | exact ⟨rfl, consume_settles hf hs hinv⟩
  | finishTransact p t =>
    simp only [Op.named_below] at hs
    simp only [Op.body, finish_transact, read, stored, Tables.handle, run_bind, run_fetch, run_pure,
      run_throw, run_ite, consume, run_put, run_delete] at h
    rcases hf : tx.data.rows.get p.id with _ | row <;> simp [hf] at h
    · obtain ⟨rfl, rfl⟩ := h; exact ofR (.rfl' _)
    · split_run
      all_goals (cases h)
      all_goals first
        | exact ofR (.rfl' _)
        | exact ⟨rfl, consume_settles hf hs hinv⟩

/-! ## Histories: transactions of operations, committed at increasing versions -/

/-- The operations one transaction body runs, in order. An operation may return `Error` and
    keep its writes (S1 says the store discards them; keeping them only widens the set of
    histories the law covers). An operation that raises a cause aborts the transaction. -/
inductive Steps : T → T → Prop
  | refl (tx : T) : Steps tx tx
  | op {tx tx1 tx2 : T} (o : Op) (r : Except Error Unit) :
      Steps tx tx1 → o.named_below tx1.info.id.version → run o.body tx1 = (.ok r, tx2) →
      Steps tx tx2

/-- One committed transaction: it reads the committed tables, runs at a version above every
    earlier commit (S3), and its writes become the committed tables. An aborted or refused
    transaction keeps nothing (S1, S2), so it is no step. -/
inductive Commit : Tables.Data → UInt64 → Tables.Data → UInt64 → Prop
  | mk {d : Tables.Data} {v : UInt64} (info : CallInfo) (tx' : T) :
      v < info.id.version → Steps ⟨info, d⟩ tx' → Commit d v tx'.data info.id.version

inductive History : Tables.Data → UInt64 → Tables.Data → UInt64 → Prop
  | refl (d : Tables.Data) (v : UInt64) : History d v d v
  | step {d v d1 v1 d2 v2} : History d v d1 v1 → Commit d1 v1 d2 v2 → History d v d2 v2

def Tables.Data.empty : Tables.Data := ⟨Map.empty, Map.empty⟩

theorem steps_preserve {tx tx' : T} (hs : Steps tx tx')
    (hinv : Settled tx.data tx.info.id.version) :
    tx'.info = tx.info ∧ Settled tx'.data tx.info.id.version ∧
      ∀ id t, tx.data.outcomes.get id = some t → tx'.data.outcomes.get id = some t := by
  induction hs with
  | refl => exact ⟨rfl, hinv, fun _ _ h => h⟩
  | @op tx1 tx2 o r _ hb hr ih =>
    obtain ⟨hi, hset, hout⟩ := ih
    have hset' : Settled tx1.data tx1.info.id.version := by rw [hi]; exact hset
    obtain ⟨hi2, hset2, hout2⟩ := op_preserves o tx1 tx2 r hset' hb hr
    rw [hi] at hset2
    exact ⟨hi2.trans hi, hset2, fun id t h => hout2 id t (hout id t h)⟩

theorem history_trans {d v d1 v1 d2 v2} (h1 : History d v d1 v1) (h2 : History d1 v1 d2 v2) :
    History d v d2 v2 := by
  induction h2 with
  | refl => exact h1
  | step _ hc ih => exact .step ih hc

/-- Every committed state reached from the empty store is settled. -/
theorem history_settled {d v} (h : History Tables.Data.empty 0 d v) :
    ∀ id t, d.outcomes.get id = some t → d.rows.get id = none ∧ id.version ≤ v := by
  generalize he : Tables.Data.empty = d0 at h
  generalize hz : (0 : UInt64) = v0 at h
  induction h with
  | refl => subst he; intro id t h; simp [Tables.Data.empty] at h
  | @step d1 v1 _ _ _ hc ih =>
    obtain ⟨info, tx', hlt, hs⟩ := hc
    have hinv : Settled d1 info.id.version := by
      intro id t ho
      obtain ⟨hn, hv⟩ := ih id t ho
      exact ⟨hn, UInt64.lt_of_le_of_lt hv hlt⟩
    obtain ⟨hi, hset, -⟩ := steps_preserve hs hinv
    intro id t ho
    obtain ⟨hn, hv⟩ := hset id t ho
    exact ⟨hn, UInt64.le_of_lt hv⟩

/-- **The settlement law.** Along any committed history from the empty store, under S1–S5, a
    terminal once committed for an id stays that terminal in every later committed state. -/
theorem terminal_never_changes {d v d' v'} (h0 : History Tables.Data.empty 0 d v)
    (h : History d v d' v') {id : Id} {t : Terminal} (ho : d.outcomes.get id = some t) :
    d'.outcomes.get id = some t := by
  induction h with
  | refl => exact ho
  | @step d1 v1 _ _ hprev hc ih =>
    obtain ⟨info, tx', hlt, hs⟩ := hc
    have hset := history_settled (history_trans h0 hprev)
    have hinv : Settled d1 info.id.version := by
      intro id t ho
      obtain ⟨hn, hv⟩ := hset id t ho
      exact ⟨hn, UInt64.lt_of_le_of_lt hv hlt⟩
    exact (steps_preserve hs hinv).2.2 id t ih

end runner.triggers

/-! ## Axiom audit: only propext, Quot.sound and Classical.choice may appear. -/
#print axioms runner.triggers.fence_stale
#print axioms runner.triggers.finish_stale
#print axioms runner.triggers.hold_stale
#print axioms runner.triggers.start_issues_next
#print axioms runner.triggers.terminal_never_changes
