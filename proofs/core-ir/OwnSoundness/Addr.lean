import OwnSoundness.Abs

/-!
# Addresses

What the tables promise of an address (`wfAddr`, `wfAct`), the place an address is at run
time, and `stuck`: a use or drop of a place with a part that is not live fails `apply`'s
check.
-/

namespace Hirsel.CoreIR.Own

/-! ## What the tables promise -/

theorem wfAddr_names {P : Prog} {a : Nat} {r : Pl} (h : P.wfAddr a = true)
    (hr : r ∈ P.names a) : r.root < P.roots := by
  simp only [Prog.wfAddr, Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq] at h
  exact h.1.1 r hr

theorem wfAddr_base {P : Prog} {a v : Nat} {rel : Path} (h : P.wfAddr a = true)
    (hb : P.base a = some (v, rel)) :
    (∀ x, x ∈ P.under v → x.cut = true ∨ x.app rel ∈ P.names a) ∧
    ((P.joinOf v).isSome = true ∨
      ∀ r, P.names a = [r] → r.cut = true ∨ (one (P.under v)).isSome = true) := by
  simp only [Prog.wfAddr, Bool.and_eq_true] at h
  obtain ⟨⟨_, h2⟩, _⟩ := h
  rw [hb] at h2
  simp only [Bool.and_eq_true, List.all_eq_true, Bool.or_eq_true, List.contains_iff_mem] at h2
  refine ⟨h2.1, ?_⟩
  rcases h2.2 with hj | hm
  · exact Or.inl hj
  · refine Or.inr fun r hr => ?_
    rw [hr] at hm
    simpa using hm

theorem wfAddr_jname {P : Prog} {a : Nat} {r x t : Pl} (h : P.wfAddr a = true)
    (hj : P.jname a = some r) (hx : x ∈ P.expand r) (ht : t ∈ P.tracked)
    (hr : t.root < P.roots) (ho : overlap x t = true) :
    ({ r with cut := false } : Pl) ∈ P.tracked := by
  simp only [Prog.wfAddr, Bool.and_eq_true] at h
  obtain ⟨_, h3⟩ := h
  rw [hj] at h3
  simp only [Bool.or_eq_true, Bool.not_eq_true', List.contains_iff_mem] at h3
  rcases h3 with h3 | h3
  · have : ((P.expand r).any fun x =>
        P.tracked.any fun t => decide (t.root < P.roots) && overlap x t) = true :=
      List.any_eq_true.mpr ⟨x, hx, List.any_eq_true.mpr ⟨t, ht, by simp [hr, ho]⟩⟩
    rw [this] at h3
    cases h3
  · exact h3

theorem wfAct_drop {P : Prog} {a : Nat} {r : Pl} (h : P.wfAct (.drop a) = true)
    (hr : r ∈ P.names a ∨ P.jname a = some r) (hc : r.cut = false) : r ∈ P.tracked := by
  simp only [Prog.wfAct, List.all_eq_true, List.mem_append, Bool.or_eq_true,
    List.contains_iff_mem] at h
  have hm : r ∈ P.names a ∨ r ∈ (P.jname a).toList := by
    rcases hr with hr | hr
    · exact Or.inl hr
    · exact Or.inr (by rw [hr]; exact List.mem_singleton.mpr rfl)
  rcases h r hm with h | h
  · rw [hc] at h
    cases h
  · exact h

theorem jname_some {P : Prog} {a : Nat} {r : Pl} (h : P.jname a = some r) :
    ∃ v rel k, P.base a = some (v, rel) ∧ P.joinOf v = some k ∧ r = (P.jroot k).app rel := by
  unfold Prog.jname at h
  cases hb : P.base a with
  | none =>
    rw [hb] at h
    cases h
  | some vr =>
    obtain ⟨v, rel⟩ := vr
    rw [hb] at h
    cases hk : P.joinOf v with
    | none => simp [hk] at h
    | some k =>
      simp only [hk, Option.map_some, Option.some.injEq] at h
      exact ⟨v, rel, k, rfl, hk, h.symm⟩

theorem jname_none {P : Prog} {a v : Nat} {rel : Path} (h : P.jname a = none)
    (hb : P.base a = some (v, rel)) : P.joinOf v = none := by
  unfold Prog.jname at h
  rw [hb] at h
  cases hk : P.joinOf v with
  | none => rfl
  | some k => simp [hk] at h

theorem inside_congr {a a' p : Pl} (hr : a'.root = a.root) (hs : a'.steps = a.steps)
    (h : inside a p = true) : inside a' p = true := by
  unfold inside at *
  rw [hr, hs]
  exact h

theorem reads_get {P : Prog} {ri : Nat} (hr : ri < P.reads.length) :
    P.reads[ri]? = some (P.reads.getD ri 0) := by
  simp [List.getD_eq_getElem?_getD, List.getElem?_eq_getElem hr]

/-! ## What an address is -/

theorem Den.base {P : Prog} {σ : Store} {a v : Nat} {rel : Path} {y : Option Pl}
    (hd : Den P σ a y) (hb : P.base a = some (v, rel)) :
    y = (σ.bind v).map fun x => x.app rel := by
  unfold Den at hd
  rw [hb] at hd
  exact hd

theorem Den.bound {P : Prog} {σ : Store} {a v : Nat} {rel : Path} {y : Pl}
    (hd : Den P σ a (some y)) (hb : P.base a = some (v, rel)) :
    ∃ x, σ.bind v = some x ∧ y = x.app rel := by
  have he := hd.base hb
  cases hbv : σ.bind v with
  | none =>
    rw [hbv] at he
    cases he
  | some x =>
    rw [hbv] at he
    exact ⟨x, rfl, Option.some.inj he⟩

theorem Den.unbound {P : Prog} {σ : Store} {a v : Nat} {rel : Path} (hd : Den P σ a none)
    (hb : P.base a = some (v, rel)) : σ.bind v = none := by
  have he := hd.base hb
  cases hbv : σ.bind v with
  | none => rfl
  | some x =>
    rw [hbv] at he
    cases he

theorem Den.slot {P : Prog} {σ : Store} {a : Nat} {y : Pl} (hd : Den P σ a (some y))
    (hb : P.base a = none) : y ∈ P.names a := by
  unfold Den at hd
  rw [hb] at hd
  exact hd

theorem Den.slot_none {P : Prog} {σ : Store} {a : Nat} (hd : Den P σ a none)
    (hb : P.base a = none) : P.names a = [] := by
  unfold Den at hd
  rw [hb] at hd
  exact hd

theorem Den.congr {P : Prog} {σ σ' : Store} {a : Nat} {y : Option Pl}
    (hb : σ'.bind = σ.bind) (hd : Den P σ a y) : Den P σ' a y := by
  unfold Den at *
  rw [hb]
  exact hd

/-- The place an address is, the pass names. -/
theorem den_names {P : Prog} {σ : Store} {s : AState} {a : Nat} {y : Pl} (h : Abs P σ s)
    (hw : P.wfAddr a = true) (hd : Den P σ a (some y)) : y ∈ P.names a := by
  cases hb : P.base a with
  | none => exact hd.slot hb
  | some vr =>
    obtain ⟨v, rel⟩ := vr
    obtain ⟨x, hbv, rfl⟩ := hd.bound hb
    obtain ⟨hx, hcut⟩ := h.binds v x hbv
    rcases (wfAddr_base hw hb).1 x hx with hc | hm
    · rw [hcut] at hc
      cases hc
    · exact hm

/-- An address based on a join's parameter: where the parameter points, and the join
place the address is. -/
theorem den_routed {P : Prog} {σ : Store} {a : Nat} {y rj : Pl} (hd : Den P σ a (some y))
    (hj : P.jname a = some rj) :
    ∃ v rel k x0, P.base a = some (v, rel) ∧ P.joinOf v = some k ∧ σ.bind v = some x0 ∧
      y = x0.app rel ∧ rj = (P.jroot k).app rel ∧ P.paramOf rj = some v := by
  obtain ⟨v, rel, k, hb, hk, rfl⟩ := jname_some hj
  obtain ⟨x0, hbv, rfl⟩ := hd.bound hb
  exact ⟨v, rel, k, x0, hb, hk, hbv, rfl, rfl, paramOf_jroot hk _ rfl⟩

/-- A part of the place an address is that is not live: the address overlaps a tracked
place that may be dropped, so `apply`'s check fails. -/
theorem stuck {P : Prog} {σ : Store} {s : AState} {a : Nat} {y p : Pl} (h : Abs P σ s)
    (hw : P.wfAddr a = true) (hd : Den P σ a (some y)) (hin : inside y p = true)
    (hp : σ.st p ≠ .live) : ∃ q, q ∈ P.hits a ∧ mayDrop P s q = true := by
  obtain ⟨hr, q, hq, hqin, hdc⟩ := h.dead p hp
  cases hj : P.jname a with
  | none =>
    refine ⟨q, mem_hits.mpr ⟨hq, y, ?_, overlap_of_inside hin hqin⟩, hdc.may⟩
    simp only [Prog.places, hj]
    exact den_names h hw hd
  | some rj =>
    obtain ⟨v, rel, k, x0, _, _, hbv, rfl, rfl, hpar⟩ := den_routed hd hj
    have hx0 := (h.binds v x0 hbv).1
    have hin0 : inside (x0.app ((P.jroot k).app rel).path) p = true :=
      inside_congr rfl rfl hin
    have ht0 : ({ (P.jroot k).app rel with cut := false } : Pl) ∈ P.tracked :=
      wfAddr_jname hw hj (mem_expand.mpr ⟨v, hpar, x0, hx0, rfl⟩) hq
        (by rw [(inside_iff.mp hqin).1]; exact hr) (overlap_of_inside hin0 hqin)
    obtain ⟨t', ht', hr', hin', hdc'⟩ :=
      h.join { (P.jroot k).app rel with cut := false } v x0 p
        ((paramOf_root (t := (P.jroot k).app rel) rfl).trans hpar) hbv ht0
        (inside_congr rfl rfl hin) hp
    refine ⟨t', mem_hits.mpr ⟨ht', (P.jroot k).app rel, ?_,
      overlap_iff.mpr ⟨hr'.symm, app_total hin hin'⟩⟩, hdc'.may⟩
    simp only [Prog.places, hj, List.mem_singleton]

end Hirsel.CoreIR.Own
