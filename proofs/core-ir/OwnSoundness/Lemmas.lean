import OwnSoundness.Model

/-!
# Lemmas on pairing states, places, the tables and `invalidate`'s walk
-/

namespace Hirsel.CoreIR.Own

/-! ## Pairing states -/

namespace Bits

theorem drops_d (s : Bits) (b : Bool) : s.drops.d b = s.half b := by cases b <;> rfl

theorem drops_half (s : Bits) (b : Bool) : s.drops.half b = s.half b := by
  cases b <;> simp [drops, half]

theorem begun_half (s : Bits) (b : Bool) : s.begun.half b = s.half b := by
  cases b <;> simp [begun, half]

theorem or_d (s t : Bits) (b : Bool) : (s.or t).d b = (s.d b || t.d b) := by cases b <;> rfl

theorem or_half (s t : Bits) (b : Bool) : (s.or t).half b = (s.half b || t.half b) := by
  cases b <;> simp only [Bits.or, half, ite_true, Bool.false_eq_true, ite_false] <;> ac_rfl

theorem d_half {s : Bits} {b : Bool} (h : s.d b = true) : s.half b = true := by
  cases b <;> simp_all [d, half]

theorem anyD_of_d {s : Bits} {b : Bool} (h : s.d b = true) : s.anyD = true := by
  cases b <;> simp_all [d, anyD]

theorem flag_half {s : Bits} {v : Option Bool} {b0 b : Bool} (h : s.half b0 = true)
    (hv : ∀ c, v = some c → b = c) : (s.flag v).half b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases v with _ | c
  · clear hv
    revert h
    cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b0 <;> cases b <;> decide
  · have := hv c rfl
    subst this
    clear hv
    revert h
    cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b0 <;> cases b <;> decide

theorem flag_d {s : Bits} {v : Option Bool} {b0 b : Bool} (h : s.d b0 = true)
    (hv : ∀ c, v = some c → b = c) : (s.flag v).d b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases v with _ | c
  · clear hv
    revert h
    cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b0 <;> cases b <;> decide
  · have := hv c rfl
    subst this
    clear hv
    revert h
    cases zt <;> cases zf <;> cases dt <;> cases df <;> cases b0 <;> cases b <;> decide

theorem keep_half (s : Bits) (b : Bool) : (s.keep b).half b = s.half b := by
  cases b <;> simp [keep, half]

theorem keep_d (s : Bits) (b : Bool) : (s.keep b).d b = s.d b := by cases b <;> rfl

theorem le_half {s t : Bits} {b : Bool} (h : s.le t = true) (hs : s.half b = true) :
    t.half b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases t with ⟨zt', zf', dt', df'⟩
  revert h hs
  cases b <;> cases zt <;> cases zf <;> cases dt <;> cases df <;> cases zt' <;> cases zf' <;>
    cases dt' <;> cases df' <;> decide

theorem le_d {s t : Bits} {b : Bool} (h : s.le t = true) (hs : s.d b = true) :
    t.d b = true := by
  rcases s with ⟨zt, zf, dt, df⟩
  rcases t with ⟨zt', zf', dt', df'⟩
  revert h hs
  cases b <;> cases zt <;> cases zf <;> cases dt <;> cases df <;> cases zt' <;> cases zf' <;>
    cases dt' <;> cases df' <;> decide

theorem Z_half (b : Bool) : Z.half b = true := by cases b <;> rfl

end Bits

theorem ifPos {α : Sort u} {c : Prop} [Decidable c] {a b : α} (h : c) :
    (if c then a else b) = a := by simp [h]

theorem ifNeg {α : Sort u} {c : Prop} [Decidable c] {a b : α} (h : ¬ c) :
    (if c then a else b) = b := by simp [h]

/-! ## Places -/

theorem overlap_iff {a b : Pl} :
    overlap a b = true ↔ a.root = b.root ∧ (b.steps <+: a.steps ∨ a.steps <+: b.steps) := by
  simp [overlap, List.isPrefixOf_iff_prefix]

theorem inside_iff {r p : Pl} : inside r p = true ↔ r.root = p.root ∧ r.steps <+: p.steps := by
  simp [inside, List.isPrefixOf_iff_prefix]

theorem overlap_comm {a b : Pl} (h : overlap a b = true) : overlap b a = true := by
  rw [overlap_iff] at h ⊢
  exact ⟨h.1.symm, h.2.symm⟩

theorem overlap_self (a : Pl) : overlap a a = true :=
  overlap_iff.mpr ⟨rfl, Or.inl List.prefix_rfl⟩

theorem inside_self (a : Pl) : inside a a = true := inside_iff.mpr ⟨rfl, List.prefix_rfl⟩

theorem inside_trans {a b c : Pl} (h1 : inside a b = true) (h2 : inside b c = true) :
    inside a c = true := by
  rw [inside_iff] at h1 h2 ⊢
  exact ⟨h1.1.trans h2.1, h1.2.trans h2.2⟩

theorem prefix_total {α : Type} {l₁ l₂ l₃ : List α} (h1 : l₁ <+: l₃) (h2 : l₂ <+: l₃) :
    l₂ <+: l₁ ∨ l₁ <+: l₂ := by
  rcases Nat.le_total l₁.length l₂.length with h | h
  · exact Or.inr (List.prefix_of_prefix_length_le h1 h2 h)
  · exact Or.inl (List.prefix_of_prefix_length_le h2 h1 h)

/-- Two places with a part in common overlap. -/
theorem overlap_of_inside {r q p : Pl} (h1 : inside r p = true) (h2 : inside q p = true) :
    overlap r q = true := by
  rw [inside_iff] at h1 h2
  exact overlap_iff.mpr ⟨h1.1.trans h2.1.symm, prefix_total h1.2 h2.2⟩

@[simp] theorem Pl.app_root (x : Pl) (r : Path) : (x.app r).root = x.root := rfl

@[simp] theorem Pl.app_steps (x : Pl) (r : Path) : (x.app r).steps = x.steps ++ r.steps := rfl

@[simp] theorem Pl.app_cut (x : Pl) (r : Path) : (x.app r).cut = (x.cut || r.cut) := rfl

@[simp] theorem Pl.path_steps (t : Pl) : t.path.steps = t.steps := rfl

@[simp] theorem Pl.path_cut (t : Pl) : t.path.cut = t.cut := rfl

/-- Two paths from one place, each reaching a part `p` has, are one a prefix of the
other. -/
theorem app_total {y p : Pl} {r r' : Path} (h1 : inside (y.app r) p = true)
    (h2 : inside (y.app r') p = true) : r'.steps <+: r.steps ∨ r.steps <+: r'.steps := by
  rw [inside_iff] at h1 h2
  rcases prefix_total h1.2 h2.2 with h | h
  · exact Or.inl ((List.prefix_append_right_inj _).mp h)
  · exact Or.inr ((List.prefix_append_right_inj _).mp h)

theorem one_some {l : List Pl} {x : Pl} :
    one l = some x ↔ l ≠ [] ∧ x.cut = false ∧ ∀ z, z ∈ l → z = x := by
  cases l with
  | nil => simp [one]
  | cons y rest =>
    simp only [one, ne_eq, reduceCtorEq, not_false_eq_true, List.mem_cons, true_and]
    constructor
    · intro h
      split at h
      · rename_i hc
        cases h
        simp only [Bool.and_eq_true, Bool.not_eq_true', List.all_eq_true, beq_iff_eq] at hc
        refine ⟨hc.1, fun z hz => ?_⟩
        rcases hz with rfl | hz
        · rfl
        · exact hc.2 z hz
      · cases h
    · rintro ⟨hc, hall⟩
      have hy : y = x := hall y (Or.inl rfl)
      subst hy
      have : (!y.cut && rest.all (· == y)) = true := by
        simp only [Bool.and_eq_true, Bool.not_eq_true', List.all_eq_true, beq_iff_eq]
        exact ⟨hc, fun z hz => hall z (Or.inr hz)⟩
      simp [this]

/-! ## The tables -/

theorem owner_iff {P : Prog} {x : Nat} {q : Pl} :
    P.owner x = some q ↔ ∃ g, P.cells[x]? = some (.pair q g) := by
  unfold Prog.owner
  cases hx : P.cells[x]? with
  | none => simp
  | some c => cases c <;> simp

theorem owner_lt {P : Prog} {x : Nat} {q : Pl} (h : P.owner x = some q) :
    x < P.cells.length := by
  obtain ⟨g, hg⟩ := owner_iff.mp h
  exact (List.getElem?_eq_some_iff.mp hg).1

theorem guard_pair {P : Prog} {x : Nat} {q : Pl} {g : Option Nat}
    (h : P.cells[x]? = some (.pair q g)) : P.guard x = g := by
  simp [Prog.guard, h]

theorem guard_bind {P : Prog} {x p : Nat} {q t : Pl} {g : Option Nat}
    (h : P.cells[x]? = some (.bind q t p g)) : P.guard x = g := by
  simp [Prog.guard, h]

theorem owner_bind {P : Prog} {x p : Nat} {q t : Pl} {g : Option Nat}
    (h : P.cells[x]? = some (.bind q t p g)) : P.owner x = none := by
  simp [Prog.owner, h]

theorem mem_pairsOf {P : Prog} {x : Nat} {q : Pl} : x ∈ P.pairsOf q ↔ P.owner x = some q := by
  simp only [Prog.pairsOf, List.mem_filter, List.mem_range, beq_iff_eq]
  exact ⟨fun h => h.2, fun h => ⟨owner_lt h, h⟩⟩

theorem mem_hits {P : Prog} {a : Nat} {q : Pl} :
    q ∈ P.hits a ↔ q ∈ P.tracked ∧ ∃ r, r ∈ P.places a ∧ overlap r q = true := by
  simp [Prog.hits, List.mem_filter, List.any_eq_true]

theorem mem_shadow {P : Prog} {q t : Pl} :
    t ∈ P.shadow q ↔ t ∈ P.tracked ∧ ∃ x, x ∈ P.expand t ∧ overlap x q = true := by
  simp [Prog.shadow, List.mem_filter, List.any_eq_true]

theorem mem_seeds {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} {d : Pl} :
    d ∈ P.seeds on seed ↔
      ∃ k b, P.borrows[k]? = some b ∧ on k = true ∧ seed b = true ∧ d ∈ b.dest := by
  simp only [Prog.seeds, List.mem_flatMap, List.mem_zipIdx_iff_getElem?, Prod.exists]
  constructor
  · rintro ⟨b, k, hb, hd⟩
    split at hd
    · rename_i hc
      simp only [Bool.and_eq_true] at hc
      exact ⟨k, b, hb, hc.1, hc.2, hd⟩
    · simp at hd
  · rintro ⟨k, b, hb, hon, hs, hd⟩
    exact ⟨b, k, hb, by simpa [hon, hs] using hd⟩

theorem seeds_mono {P : Prog} {on on' : Nat → Bool} {seed seed' : Borrow → Bool} {d : Pl}
    (h : ∀ k, k < P.borrows.length → on k = true → on' k = true)
    (hs : ∀ b, seed b = true → seed' b = true) (hd : d ∈ P.seeds on seed) :
    d ∈ P.seeds on' seed' := by
  obtain ⟨k, b, hb, hon, hsd, hd⟩ := mem_seeds.mp hd
  exact mem_seeds.mpr ⟨k, b, hb, h k (List.getElem?_eq_some_iff.mp hb).1 hon, hs b hsd, hd⟩

theorem wf_iff (P : Prog) : P.wf = true ↔
    P.slots ≤ P.roots ∧
    (∀ v, v ∈ P.joins → ∀ x, x ∈ P.under v → x.root < P.roots) ∧
    (∀ a, a ∈ P.allActs → P.wfAct a = true) ∧
    (∀ a, a ∈ P.addrs → P.wfAddr a = true) ∧
    (∀ b, b ∈ P.borrows → P.wfBorrow b = true) ∧
    (∀ e, e ∈ P.allEdges → ∀ ws, ws ∈ e.args → P.wfArg ws.1 ws.2 = true) ∧
    P.wfCells = true := by
  simp only [Prog.wf, Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq, and_assoc]

theorem wfCells_iff (P : Prog) : P.wfCells = true ↔
    (∀ q, q ∈ P.tracked → ∀ t, t ∈ P.shadow q → ∀ x, P.owner x = some q →
      .bind q t x (P.guard x) ∈ P.cells) ∧
    (∀ q t p g, Cell.bind q t p g ∈ P.cells → P.owner p = some q ∧ P.guard p = g) ∧
    (∀ x f, P.guard x = some f → f < P.slots) ∧
    (∀ f, f ∈ P.reads → f < P.slots) := by
  simp only [Prog.wfCells, Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq,
    List.contains_iff_mem, mem_pairsOf, List.mem_range]
  constructor
  · rintro ⟨⟨⟨h1, h2⟩, h3⟩, h4⟩
    refine ⟨h1, fun q t p g hm => ?_, fun x f hg => ?_, h4⟩
    · simpa using h2 _ hm
    · have hlt : x < P.cells.length := by
        unfold Prog.guard at hg
        cases hx : P.cells[x]? with
        | none => simp [hx] at hg
        | some c => exact (List.getElem?_eq_some_iff.mp hx).1
      simpa [hg] using h3 x hlt
  · rintro ⟨h1, h2, h3, h4⟩
    refine ⟨⟨⟨h1, fun c hc => ?_⟩, fun x _ => ?_⟩, h4⟩
    · cases c with
      | pair => rfl
      | bind q t p g => simpa using h2 q t p g hc
    · cases hg : P.guard x with
      | none => rfl
      | some f => simpa using h3 x f hg

theorem joinOf_some {P : Prog} {v k : Nat} (h : P.joinOf v = some k) : P.joins[k]? = some v := by
  obtain ⟨hlt, hp, _⟩ := List.findIdx?_eq_some_iff_getElem.mp h
  have : P.joins[k] = v := by simpa using hp
  exact List.getElem?_eq_some_iff.mpr ⟨hlt, this⟩

theorem joinOf_isSome {P : Prog} {v : Nat} (h : v ∈ P.joins) : (P.joinOf v).isSome = true := by
  cases hj : P.joinOf v with
  | some k => rfl
  | none =>
    have := List.findIdx?_eq_none_iff.mp hj v h
    simp at this

theorem paramOf_some {P : Prog} {t : Pl} {v : Nat} (h : P.paramOf t = some v) :
    P.roots ≤ t.root ∧ P.joins[t.root - P.roots]? = some v := by
  unfold Prog.paramOf at h
  split at h
  · cases h
  · exact ⟨by omega, h⟩

theorem paramOf_mem {P : Prog} {t : Pl} {v : Nat} (h : P.paramOf t = some v) : v ∈ P.joins :=
  List.mem_of_getElem? (paramOf_some h).2

/-- `paramOf` reads only the root. -/
theorem paramOf_root {P : Prog} {t t' : Pl} (h : t'.root = t.root) :
    P.paramOf t' = P.paramOf t := by
  simp [Prog.paramOf, h]

theorem paramOf_jroot {P : Prog} {v k : Nat} (h : P.joinOf v = some k) (t : Pl)
    (ht : t.root = P.roots + k) : P.paramOf t = some v := by
  have hk := joinOf_some h
  unfold Prog.paramOf
  have : ¬ t.root < P.roots := by omega
  simp only [this, ite_false]
  have : t.root - P.roots = k := by omega
  rw [this]
  exact hk

theorem mem_addrs {P : Prog} {a : Nat} {act : Act} (h : act ∈ P.allActs)
    (ha : act = .use a ∨ act = .drop a ∨ act = .write a ∨ act = .invalidate a) :
    a ∈ P.addrs := by
  unfold Prog.addrs
  rw [List.mem_filterMap]
  refine ⟨act, h, ?_⟩
  rcases ha with rfl | rfl | rfl | rfl <;> rfl

theorem mem_of_lookup {α : Type} : ∀ {l : List (Nat × α)} {w : Nat} {s : α},
    l.lookup w = some s → (w, s) ∈ l
  | [], _, _, h => by simp [List.lookup] at h
  | (k, v) :: l, w, s, h => by
    simp only [List.lookup] at h
    split at h
    · rename_i heq
      have hw : w = k := by simpa using heq
      cases h
      subst hw
      exact List.mem_cons_self ..
    · exact List.mem_cons_of_mem _ (mem_of_lookup h)

/-! ## The walk of `invalidate` reaches every view left dangling -/

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

theorem wf_dest {P : Prog} {b : Borrow} {q : Pl} (h : P.wf = true) (hb : b ∈ P.borrows)
    (hq : q ∈ b.dest) : q ∈ P.tracked := by
  have := ((wf_iff P).mp h).2.2.2.2.1 b hb
  simp only [Prog.wfBorrow, Bool.and_eq_true, List.all_eq_true, List.contains_iff_mem] at this
  exact this.1 q hq

theorem wf_dest_one {P : Prog} {b : Borrow} {d : Pl} (h : P.wf = true) (hb : b ∈ P.borrows)
    (hd : b.dest = [d]) : d.root < P.roots := by
  have := ((wf_iff P).mp h).2.2.2.2.1 b hb
  simp only [Prog.wfBorrow, Bool.and_eq_true, hd] at this
  simpa using this.2

theorem seeds_tracked {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} {d : Pl}
    (h : P.wf = true) (hd : d ∈ P.seeds on seed) : d ∈ P.tracked := by
  obtain ⟨k, b, hb, _, _, hd⟩ := mem_seeds.mp hd
  exact wf_dest h (List.mem_of_getElem? hb) hd

theorem Hangs.tracked {P : Prog} {on : Nat → Bool} {seed : Borrow → Bool} {d : Pl}
    (h : P.wf = true) (hh : Hangs P on seed d) : d ∈ P.tracked := by
  cases hh with
  | direct hd => exact seeds_tracked h hd
  | via _ hd => exact seeds_tracked h hd

/-- `invalidate`'s walk reaches every view left dangling, with more borrows in force and
more of them picked. -/
theorem hangs_reach {P : Prog} {on on' : Nat → Bool} {seed seed' : Borrow → Bool} {d : Pl}
    (h : P.wf = true) (hon : ∀ k, k < P.borrows.length → on k = true → on' k = true)
    (hs : ∀ b, seed b = true → seed' b = true) (hh : Hangs P on seed d) :
    d ∈ reach P on' seed' := by
  have hle : unseen P.tracked [] ≤ P.tracked.length + 1 :=
    Nat.le_trans List.countP_le_length (Nat.le_succ _)
  obtain ⟨_, g2, g3⟩ := dfs_fold P on' P.tracked (P.tracked.length + 1)
    (dfs_spec P on' P.tracked (fun _ _ hd => seeds_tracked h hd) (P.tracked.length + 1))
    (P.seeds on' seed') [] (fun _ hd => seeds_tracked h hd) hle
  induction hh with
  | direct hd => exact g2 _ (seeds_mono hon hs hd)
  | via _ hd ih => exact g3 _ ih (by simp) _ (seeds_mono hon (fun _ hb => hb) hd)

end Hirsel.CoreIR.Own
