import OwnSoundness.Lemmas

/-!
# The analysis state describes the store

`Abs P σ s` relates the analysis state `s` to the run-time store `σ`. The lemmas here say
how it survives the ways `apply` changes a state: adding states (`Sub`), a change of a
flag (`Shift`), views left dangling (`Abs.hung`) and places given a new value
(`Abs.revived`).
-/

namespace Hirsel.CoreIR.Own

/-- `b` is a value the guarding flag of cell `x` may hold now: its value, or either when the
cell has no flag. -/
def Cur (P : Prog) (σ : Store) (x : Nat) (b : Bool) : Prop :=
  ∀ f, P.guard x = some f → b = σ.flag f

/-- Every pairing of `q` holds the dropped state, in the half of its flag's value. -/
def DCur (P : Prog) (σ : Store) (s : AState) (q : Pl) : Prop :=
  ∀ x b, P.owner x = some q → Cur P σ x b → (s.bits x).d b = true

/-- The analysis state `s` describes the store `σ`.

* `half`: each cell's half for its flag's value is reachable.
* `dead`: a place that is not live is in a tracked place every pairing of which holds the
  dropped state.
* `loan`: a borrow in force has its bit set.
* `bound`: where a join's parameter points into a tracked place `q`, the bind states of
  `q` for that join's places hold `D`.
* `join`: where a join's parameter points and a tracked place of the join reaches a place
  that is not live, a tracked place of the join that reaches it holds the dropped state.
* `fresh`: a flag read that is not stale saw the value the flag holds.
* `binds`, `plain`: a parameter points only at a place it names, and one that names a
  single place and is no join points at it. -/
structure Abs (P : Prog) (σ : Store) (s : AState) : Prop where
  half : ∀ x b, x < P.cells.length → Cur P σ x b → (s.bits x).half b = true
  dead : ∀ p, σ.st p ≠ .live →
    p.root < P.roots ∧ ∃ q, q ∈ P.tracked ∧ inside q p = true ∧ DCur P σ s q
  loan : ∀ k, k < P.borrows.length → σ.loan k = true → s.lend k = true
  bound : ∀ x q t p g v y, P.cells[x]? = some (.bind q t p g) → P.paramOf t = some v →
    σ.bind v = some y → overlap (y.app t.path) q = true →
    ∀ b, Cur P σ x b → (s.bits x).d b = true
  join : ∀ t v y p, P.paramOf t = some v → σ.bind v = some y → t ∈ P.tracked →
    inside (y.app t.path) p = true → σ.st p ≠ .live →
    ∃ t', t' ∈ P.tracked ∧ t'.root = t.root ∧ inside (y.app t'.path) p = true ∧
      DCur P σ s t'
  fresh : ∀ ri, ri < P.reads.length → s.stale ri = false →
    σ.read ri = σ.flag (P.reads.getD ri 0)
  binds : ∀ v y, σ.bind v = some y → y ∈ P.under v ∧ y.cut = false
  plain : ∀ v x, P.joinOf v = none → one (P.under v) = some x → σ.bind v = some x

theorem cur_exists (P : Prog) (σ : Store) (x : Nat) : ∃ b, Cur P σ x b := by
  cases hg : P.guard x with
  | none => exact ⟨true, fun f hf => by rw [hg] at hf; cases hf⟩
  | some f => exact ⟨σ.flag f, fun f' hf => by rw [hg] at hf; cases hf; rfl⟩

theorem Cur.congr {P : Prog} {σ σ' : Store} {x : Nat} {b : Bool} (h : σ'.flag = σ.flag)
    (hc : Cur P σ' x b) : Cur P σ x b := fun f hf => by rw [← h]; exact hc f hf

theorem DCur.congr {P : Prog} {σ σ' : Store} {s : AState} {q : Pl} (h : σ'.flag = σ.flag)
    (hd : DCur P σ s q) : DCur P σ' s q := fun x b ho hc => hd x b ho (hc.congr h)

theorem DCur.may {P : Prog} {σ : Store} {s : AState} {q : Pl} (h : DCur P σ s q) :
    mayDrop P s q = true := by
  simp only [mayDrop, List.all_eq_true, List.mem_range, Bool.or_eq_true, bne_iff_ne, ne_eq]
  intro x _
  by_cases ho : P.owner x = some q
  · obtain ⟨b, hb⟩ := cur_exists P σ x
    exact Or.inr (Bits.anyD_of_d (h x b ho hb))
  · exact Or.inl ho

theorem mem_expand {P : Prog} {t x : Pl} :
    x ∈ P.expand t ↔ ∃ v, P.paramOf t = some v ∧ ∃ y, y ∈ P.under v ∧ x = y.app t.path := by
  unfold Prog.expand
  cases hp : P.paramOf t with
  | none => simp
  | some v =>
    simp only [List.mem_map, Option.some.injEq, exists_eq_left']
    exact ⟨fun ⟨y, hy, he⟩ => ⟨y, hy, he.symm⟩, fun ⟨y, hy, he⟩ => ⟨y, hy, he.symm⟩⟩

/-! ## Adding states -/

/-- `s'` holds every state `s` does: more reachable halves, more dropped states, more
borrows in force, more stale reads. -/
structure Sub (P : Prog) (s s' : AState) : Prop where
  half : ∀ x b, x < P.cells.length → (s.bits x).half b = true → (s'.bits x).half b = true
  d : ∀ x b, x < P.cells.length → (s.bits x).d b = true → (s'.bits x).d b = true
  lend : ∀ k, k < P.borrows.length → s.lend k = true → s'.lend k = true
  stale : ∀ ri, ri < P.reads.length → s'.stale ri = false → s.stale ri = false

theorem Sub.refl (P : Prog) (s : AState) : Sub P s s :=
  ⟨fun _ _ _ h => h, fun _ _ _ h => h, fun _ _ h => h, fun _ _ h => h⟩

theorem Sub.trans {P : Prog} {s s' s'' : AState} (h1 : Sub P s s') (h2 : Sub P s' s'') :
    Sub P s s'' :=
  ⟨fun x b hx h => h2.half x b hx (h1.half x b hx h),
    fun x b hx h => h2.d x b hx (h1.d x b hx h),
    fun k hk h => h2.lend k hk (h1.lend k hk h),
    fun ri hr h => h1.stale ri hr (h2.stale ri hr h)⟩

theorem Sub.of_bits {P : Prog} {s s' : AState}
    (hb : ∀ x b, ((s.bits x).half b = true → (s'.bits x).half b = true) ∧
      ((s.bits x).d b = true → (s'.bits x).d b = true))
    (hl : ∀ k, s.lend k = true → s'.lend k = true)
    (hs : ∀ ri, s'.stale ri = false → s.stale ri = false) : Sub P s s' :=
  ⟨fun x b _ h => (hb x b).1 h, fun x b _ h => (hb x b).2 h, fun k _ h => hl k h,
    fun ri _ h => hs ri h⟩

theorem AState.le.sub {P : Prog} {s t : AState} (h : s.le P t = true) : Sub P s t := by
  simp only [AState.le, Bool.and_eq_true, List.all_eq_true, List.mem_range, Bool.or_eq_true,
    Bool.not_eq_true'] at h
  obtain ⟨⟨h1, h2⟩, h3⟩ := h
  refine ⟨fun x b hx hh => Bits.le_half (h1 x hx) hh, fun x b hx hd => Bits.le_d (h1 x hx) hd,
    fun k hk hl => ?_, fun ri hr hst => ?_⟩
  · rcases h2 k hk with h | h
    · rw [h] at hl
      cases hl
    · exact h
  · rcases h3 ri hr with h | h
    · exact h
    · rw [h] at hst
      cases hst

theorem DCur.mono {P : Prog} {σ : Store} {s s' : AState} {q : Pl} (h : DCur P σ s q)
    (hs : Sub P s s') : DCur P σ s' q :=
  fun x b ho hc => hs.d x b (owner_lt ho) (h x b ho hc)

/-! ## A change of flags -/

/-- Each cell's state in `s'` under a flag value of `σ'` covers its state in `s` under one
of `σ`. -/
def Shift (P : Prog) (σ σ' : Store) (s s' : AState) : Prop :=
  ∀ x b, x < P.cells.length → Cur P σ' x b →
    ∃ b0, Cur P σ x b0 ∧ ((s.bits x).half b0 = true → (s'.bits x).half b = true) ∧
      ((s.bits x).d b0 = true → (s'.bits x).d b = true)

theorem Sub.shift {P : Prog} {s s' : AState} (h : Sub P s s') (σ : Store) :
    Shift P σ σ s s' :=
  fun x b hx hc => ⟨b, hc, h.half x b hx, h.d x b hx⟩

theorem DCur.shift {P : Prog} {σ σ' : Store} {s s' : AState} {q : Pl} (h : DCur P σ s q)
    (hsh : Shift P σ σ' s s') : DCur P σ' s' q := by
  intro x b ho hc
  obtain ⟨b0, hc0, _, hd⟩ := hsh x b (owner_lt ho) hc
  exact hd (h x b0 ho hc0)

theorem Abs.shift {P : Prog} {σ σ' : Store} {s s' : AState} (h : Abs P σ s)
    (hsh : Shift P σ σ' s s') (hst : σ'.st = σ.st) (hbind : σ'.bind = σ.bind)
    (hloan : ∀ k, k < P.borrows.length → σ'.loan k = true → s'.lend k = true)
    (hfresh : ∀ ri, ri < P.reads.length → s'.stale ri = false →
      σ'.read ri = σ'.flag (P.reads.getD ri 0)) : Abs P σ' s' where
  half := fun x b hx hc => by
    obtain ⟨b0, hc0, hh, _⟩ := hsh x b hx hc
    exact hh (h.half x b0 hx hc0)
  dead := fun p hp => by
    rw [hst] at hp
    obtain ⟨hr, q, hq, hin, hd⟩ := h.dead p hp
    exact ⟨hr, q, hq, hin, hd.shift hsh⟩
  loan := hloan
  bound := fun x q t p g v y hc hpar hb ho b hcur => by
    rw [hbind] at hb
    obtain ⟨b0, hc0, _, hd⟩ := hsh x b (List.getElem?_eq_some_iff.mp hc).1 hcur
    exact hd (h.bound x q t p g v y hc hpar hb ho b0 hc0)
  join := fun t v y p hpar hb ht hin hp => by
    rw [hbind] at hb
    rw [hst] at hp
    obtain ⟨t', ht', hr, hin', hd⟩ := h.join t v y p hpar hb ht hin hp
    exact ⟨t', ht', hr, hin', hd.shift hsh⟩
  fresh := hfresh
  binds := fun v y hb => by
    rw [hbind] at hb
    exact h.binds v y hb
  plain := fun v x hj ho => by
    rw [hbind]
    exact h.plain v x hj ho

/-- A state that holds more describes the same store. -/
theorem Abs.grow {P : Prog} {σ : Store} {s s' : AState} (h : Abs P σ s) (hs : Sub P s s') :
    Abs P σ s' :=
  h.shift (hs.shift σ) rfl rfl (fun k hk hl => hs.lend k hk (h.loan k hk hl))
    (fun ri hr hst => h.fresh ri hr (hs.stale ri hr hst))

theorem Abs.init {P : Prog} {σ : Store} (hi : Initial P σ) : Abs P σ AState.init where
  half := fun _ b _ _ => Bits.Z_half b
  dead := fun p hp => absurd (hi.live p) hp
  loan := fun k _ hl => by
    rw [hi.loan k] at hl
    cases hl
  bound := fun x q t p g v y _ hpar hb => by
    rw [hi.join v (joinOf_isSome (paramOf_mem hpar))] at hb
    cases hb
  join := fun t v y p hpar hb => by
    rw [hi.join v (joinOf_isSome (paramOf_mem hpar))] at hb
    cases hb
  fresh := fun ri _ hst => by cases hst
  binds := hi.under
  plain := hi.plain

/-! ## Marks -/

theorem Bits.drops_mono (s : Bits) (b : Bool) :
    (s.half b = true → s.drops.half b = true) ∧ (s.d b = true → s.drops.d b = true) := by
  rw [Bits.drops_half, Bits.drops_d]
  exact ⟨id, Bits.d_half⟩

theorem Bits.or_mono (s t : Bits) (b : Bool) :
    (s.half b = true → (s.or t).half b = true) ∧ (s.d b = true → (s.or t).d b = true) := by
  rw [Bits.or_half, Bits.or_d]
  exact ⟨fun h => by simp [h], fun h => by simp [h]⟩

theorem mark_sub (P : Prog) (s : AState) (Q : List Pl) (strong : Bool) :
    Sub P s (mark P s Q Bits.drops strong) := by
  refine Sub.of_bits (fun x b => ?_) (fun _ h => h) (fun _ h => h)
  simp only [mark]
  cases P.owner x with
  | none => exact ⟨id, id⟩
  | some q =>
    simp only
    split
    · cases strong
      · simp only [Bool.false_eq_true, ite_false]
        exact Bits.or_mono _ _ b
      · simp only [ite_true]
        exact Bits.drops_mono _ b
    · exact ⟨id, id⟩

/-- A mark of `q` as dropped, strong or weak, leaves every pairing of it dropped. -/
theorem mark_dcur {P : Prog} {σ : Store} {s : AState} {Q : List Pl} {q : Pl} (strong : Bool)
    (h : Abs P σ s) (hq : q ∈ Q) : DCur P σ (mark P s Q Bits.drops strong) q := by
  intro x b ho hc
  have hQ : Q.contains q = true := List.contains_iff_mem.mpr hq
  have hh := h.half x b (owner_lt ho) hc
  simp only [mark, ho, hQ, ite_true]
  cases strong
  · simp only [Bool.false_eq_true, ite_false, Bits.or_d, Bits.drops_d, hh, Bool.or_true]
  · simp only [ite_true, Bits.drops_d, hh]

theorem invalidate_sub (P : Prog) (s : AState) (seed : Borrow → Bool) :
    Sub P s (invalidate P s seed) := mark_sub ..

/-- `invalidate` marks every view left dangling, and every join place that may be one. -/
theorem invalidate_dcur {P : Prog} {σ : Store} {s : AState} {seed seed' : Borrow → Bool}
    {d : Pl} (hwf : P.wf = true) (h : Abs P σ s) (hs : ∀ b, seed b = true → seed' b = true)
    (hh : Hangs P σ.loan seed d) :
    DCur P σ (invalidate P s seed') d ∧
      ∀ t, t ∈ P.shadow d → DCur P σ (invalidate P s seed') t := by
  have hr : d ∈ reach P s.lend seed' := hangs_reach hwf h.loan hs hh
  refine ⟨mark_dcur false h (List.mem_append_left _ hr), fun t ht => ?_⟩
  exact mark_dcur false h (List.mem_append_right _ (List.mem_flatMap.mpr ⟨d, hr, ht⟩))

theorem dropPlace_sub (P : Prog) (s : AState) (q : Pl) (strong : Bool) :
    Sub P s (dropPlace P s q strong) :=
  (mark_sub P s [q] strong).trans (invalidate_sub ..)

/-! ## Folds -/

theorem foldl_sub {α : Type} {P : Prog} {f : AState → α → AState}
    (hf : ∀ s e, Sub P s (f s e)) : ∀ (l : List α) (s : AState), Sub P s (l.foldl f s)
  | [], s => Sub.refl P s
  | e :: l, s => (hf s e).trans (foldl_sub hf l (f s e))

/-- What the step of one element of a fold establishes, the fold has, when the later
steps only add states. -/
theorem foldl_mem {α : Type} {P : Prog} {f : AState → α → AState} {Q : AState → Prop}
    (hf : ∀ s e, Sub P s (f s e)) (hQ : ∀ s s', Sub P s s' → Q s → Q s') {e : α} :
    ∀ (l : List α) (s : AState), e ∈ l → (∀ s0, Sub P s s0 → Q (f s0 e)) → Q (l.foldl f s)
  | [], _, h, _ => nomatch h
  | e' :: l, s, h, hstep => by
    simp only [List.foldl_cons]
    rcases List.mem_cons.mp h with rfl | h
    · exact hQ _ _ (foldl_sub hf l _) (hstep s (Sub.refl P s))
    · exact foldl_mem hf hQ l (f s e') h fun s0 hs0 => hstep s0 ((hf s e').trans hs0)

/-! ## Views left dangling -/

/-- The store after a drop or a rewrite of an anchor, described by a state `s'` that adds
to `s` the dropped state of each place killed and each view left dangling. -/
theorem Abs.hung {P : Prog} {σ σ' : Store} {s s' : AState} {seed : Borrow → Bool}
    {kill : Pl → Prop} (hwf : P.wf = true) (h : Abs P σ s) (hs : Sub P s s')
    (hh : Hung P seed kill σ σ')
    (hk : ∀ p, kill p →
      p.root < P.roots ∧ ∃ q, q ∈ P.tracked ∧ inside q p = true ∧ DCur P σ s' q)
    (hkj : ∀ t v y p, kill p → P.paramOf t = some v → σ.bind v = some y → t ∈ P.tracked →
      inside (y.app t.path) p = true →
      ∃ t', t' ∈ P.tracked ∧ t'.root = t.root ∧ inside (y.app t'.path) p = true ∧
        DCur P σ s' t')
    (hd : ∀ d, Hangs P σ.loan seed d →
      DCur P σ s' d ∧ ∀ t, t ∈ P.shadow d → DCur P σ s' t) : Abs P σ' s' := by
  have hg := h.grow hs
  have hfl := hh.flag
  refine ⟨fun x b hx hc => hg.half x b hx (hc.congr hfl), fun p hp => ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · by_cases hkp : kill p
    · obtain ⟨hr, q, hq, hin, hdc⟩ := hk p hkp
      exact ⟨hr, q, hq, hin, hdc.congr hfl⟩
    · by_cases hdg : Dangles P σ.loan seed p
      · obtain ⟨d, hd1, hdr, hdin⟩ := hdg
        refine ⟨?_, d, hd1.tracked hwf, hdin, ((hd d hd1).1).congr hfl⟩
        rw [← (inside_iff.mp hdin).1]
        exact hdr
      · rw [hh.kept p hkp hdg] at hp
        obtain ⟨hr, q, hq, hin, hdc⟩ := hg.dead p hp
        exact ⟨hr, q, hq, hin, hdc.congr hfl⟩
  · intro k hk' hl
    rw [hh.loan] at hl
    exact hg.loan k hk' hl
  · intro x q t p g v y hc hpar hb ho b hcur
    rw [hh.bind] at hb
    exact hg.bound x q t p g v y hc hpar hb ho b (hcur.congr hfl)
  · intro t v y p hpar hb ht hin hp
    rw [hh.bind] at hb
    by_cases hkp : kill p
    · obtain ⟨t', ht', hr, hin', hdc⟩ := hkj t v y p hkp hpar hb ht hin
      exact ⟨t', ht', hr, hin', hdc.congr hfl⟩
    · by_cases hdg : Dangles P σ.loan seed p
      · obtain ⟨d, hd1, _, hdin⟩ := hdg
        have hsh : t ∈ P.shadow d :=
          mem_shadow.mpr ⟨ht, y.app t.path,
            mem_expand.mpr ⟨v, hpar, y, (h.binds v y hb).1, rfl⟩, overlap_of_inside hin hdin⟩
        exact ⟨t, ht, rfl, hin, ((hd d hd1).2 t hsh).congr hfl⟩
      · rw [hh.kept p hkp hdg] at hp
        obtain ⟨t', ht', hr, hin', hdc⟩ := hg.join t v y p hpar hb ht hin hp
        exact ⟨t', ht', hr, hin', hdc.congr hfl⟩
  · intro ri hr hst
    rw [hh.read, hfl]
    exact hg.fresh ri hr hst
  · intro v y hb
    rw [hh.bind] at hb
    exact h.binds v y hb
  · intro v x hj ho
    rw [hh.bind]
    exact h.plain v x hj ho

/-! ## Places given a new value -/

/-- The store after some places are given a new value, described by a state `s'` that
clears the dropped state only of tracked places `R` picks, all of which are live again. -/
theorem Abs.revived {P : Prog} {σ σ' : Store} {s s' : AState} (R : Pl → Prop)
    (h : Abs P σ s) (hfl : σ'.flag = σ.flag) (hrd : σ'.read = σ.read)
    (hbd : σ'.bind = σ.bind) (hst : ∀ p, σ'.st p ≠ .live → σ'.st p = σ.st p)
    (hhalf : ∀ x b, (s.bits x).half b = true → (s'.bits x).half b = true)
    (hkeep : ∀ x q b, P.owner x = some q → ¬ R q → (s.bits x).d b = true →
      (s'.bits x).d b = true)
    (hbind : ∀ x, P.owner x = none → s'.bits x = s.bits x)
    (hc1 : ∀ q p, R q → q.root < P.roots → inside q p = true → σ'.st p = .live)
    (hc2 : ∀ t v y p, R t → P.paramOf t = some v → σ.bind v = some y →
      inside (y.app t.path) p = true → σ'.st p = .live)
    (hloan : ∀ k, k < P.borrows.length → σ'.loan k = true → s'.lend k = true)
    (hstale : ∀ ri, s'.stale ri = false → s.stale ri = false) : Abs P σ' s' := by
  have hdc : ∀ q, ¬ R q → DCur P σ s q → DCur P σ' s' q := fun q hq hd x b ho hc =>
    hkeep x q b ho hq (hd x b ho (hc.congr hfl))
  refine ⟨fun x b hx hc => hhalf x b (h.half x b hx (hc.congr hfl)), fun p hp => ?_, hloan,
    ?_, ?_, ?_, ?_, ?_⟩
  · have hp' : σ.st p ≠ .live := by
      rw [← hst p hp]
      exact hp
    obtain ⟨hr, q, hq, hin, hd⟩ := h.dead p hp'
    refine ⟨hr, q, hq, hin, hdc q (fun hR => hp (hc1 q p hR ?_ hin)) hd⟩
    rw [(inside_iff.mp hin).1]
    exact hr
  · intro x q t p g v y hc hpar hb ho b hcur
    rw [hbd] at hb
    rw [hbind x (owner_bind hc)]
    exact h.bound x q t p g v y hc hpar hb ho b (hcur.congr hfl)
  · intro t v y p hpar hb ht hin hp
    rw [hbd] at hb
    have hp' : σ.st p ≠ .live := by
      rw [← hst p hp]
      exact hp
    obtain ⟨t', ht', hr, hin', hd⟩ := h.join t v y p hpar hb ht hin hp'
    refine ⟨t', ht', hr, hin', hdc t' (fun hR => hp (hc2 t' v y p hR ?_ hb hin')) hd⟩
    rw [paramOf_root hr]
    exact hpar
  · intro ri hr hstl
    rw [hrd, hfl]
    exact h.fresh ri hr (hstale ri hstl)
  · intro v y hb
    rw [hbd] at hb
    exact h.binds v y hb
  · intro v x hj ho
    rw [hbd]
    exact h.plain v x hj ho

end Hirsel.CoreIR.Own
