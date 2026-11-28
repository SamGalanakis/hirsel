import OwnSoundness.Addr

/-!
# Drops

`drop_sound`: `apply`'s `Act::Drop` (with `drop_bound` and `drop_shadow`) leaves a state
that describes the store after the drop.
-/

namespace Hirsel.CoreIR.Own

/-! ## `drop_bound` and `drop_shadow` only add states -/

/-- `drop_bound`'s state before its `invalidate`. -/
def boundSt (P : Prog) (s : AState) (q : Pl) (own : List Pl) : AState :=
  { s with bits := fun x =>
      if P.owner x == some q then (s.bits x).or (boundDrop P s q x own) else s.bits x }

theorem dropBound_eq (P : Prog) (s : AState) (q : Pl) (own : List Pl) :
    dropBound P s q own =
      if (P.pairsOf q).any (fun x => (boundDrop P s q x own).anyD) then
        invalidate P (boundSt P s q own) (P.lent q)
      else boundSt P s q own := rfl

theorem boundSt_sub (P : Prog) (s : AState) (q : Pl) (own : List Pl) :
    Sub P s (boundSt P s q own) := by
  refine Sub.of_bits (fun x b => ?_) (fun _ h => h) (fun _ h => h)
  simp only [boundSt]
  split
  · exact Bits.or_mono _ _ b
  · exact ⟨id, id⟩

theorem dropBound_sub (P : Prog) (s : AState) (q : Pl) (own : List Pl) :
    Sub P s (dropBound P s q own) := by
  rw [dropBound_eq]
  split
  · exact (boundSt_sub ..).trans (invalidate_sub ..)
  · exact boundSt_sub ..

theorem dropShadow_sub (P : Prog) (s : AState) (q t : Pl) (own : List Pl) :
    Sub P s (dropShadow P s q t own) := by
  refine Sub.of_bits (fun x b => ?_) (fun _ h => h) (fun _ h => h)
  simp only [dropShadow]
  split
  · exact Bits.or_mono _ _ b
  · exact ⟨id, id⟩

/-- `Act::Drop`'s `bound`: the drop goes through a join that has a bind state for `r`. -/
def isBound (P : Prog) (own : List Pl) (routed : Bool) (r : Pl) : Bool :=
  routed && P.cells.any fun c =>
    match c with
    | .bind q t _ _ => q == r && own.contains t
    | .pair .. => false

theorem dropName_eq (P : Prog) (own : List Pl) (routed strong : Bool) (s : AState) {r : Pl}
    (h : r.cut = false) (ht : r ∈ P.tracked) :
    dropName P own routed strong s r =
      ((P.shadow r).filter fun t => !own.any fun o => overlap o t).foldl
        (fun s t =>
          if isBound P own routed r then dropShadow P s r t own else dropPlace P s t false)
        (if isBound P own routed r then dropBound P s r own else dropPlace P s r strong) := by
  have hc : (!r.cut && P.tracked.contains r) = true := by
    simp [h, ht]
  unfold dropName
  rw [ifPos hc]
  rfl

theorem dropName_sub (P : Prog) (own : List Pl) (routed strong : Bool) (s : AState) (r : Pl) :
    Sub P s (dropName P own routed strong s r) := by
  by_cases hc : (!r.cut && P.tracked.contains r) = true
  · simp only [Bool.and_eq_true, Bool.not_eq_true', List.contains_iff_mem] at hc
    rw [dropName_eq P own routed strong s hc.1 hc.2]
    refine Sub.trans ?_ (foldl_sub (fun s t => ?_) _ _)
    · split
      · exact dropBound_sub ..
      · exact dropPlace_sub ..
    · split
      · exact dropShadow_sub ..
      · exact dropPlace_sub ..
  · unfold dropName
    rw [ifNeg hc]
    exact Sub.refl P s

theorem applyDrop_sub (P : Prog) (s : AState) (a : Nat) : Sub P s (applyDrop P s a) :=
  (foldl_sub (fun s t => dropPlace_sub P s t true) _ _).trans
    (foldl_sub (fun _ _ => dropName_sub ..) _ _)

theorem isBound_true {P : Prog} {own : List Pl} {routed : Bool} {r : Pl}
    (h : isBound P own routed r = true) :
    routed = true ∧ ∃ t p g, Cell.bind r t p g ∈ P.cells ∧ t ∈ own := by
  simp only [isBound, Bool.and_eq_true, List.any_eq_true] at h
  obtain ⟨hr, c, hc, hm⟩ := h
  cases c with
  | pair q g => cases hm
  | bind q t p g =>
    simp only [Bool.and_eq_true, beq_iff_eq, List.contains_iff_mem] at hm
    obtain ⟨rfl, ht⟩ := hm
    exact ⟨hr, t, p, g, hc, ht⟩

/-! ## A join bound to the place dropped -/

/-- The drop of `y` goes through join place `rj`, whose parameter points at `x0`: `rj` is
`y` now. -/
structure Routed (P : Prog) (σ : Store) (own : List Pl) (y rj : Pl) (v : Nat) (x0 : Pl) :
    Prop where
  mem : rj ∈ own
  only : ∀ t, t ∈ own → t = rj
  shadow : rj ∈ P.shadow y
  param : P.paramOf rj = some v
  bind : σ.bind v = some x0
  root : x0.root = y.root
  steps : x0.steps ++ rj.steps = y.steps

theorem Routed.overlap {P : Prog} {σ : Store} {own : List Pl} {y rj x0 : Pl} {v : Nat}
    (hr : Routed P σ own y rj v x0) : overlap (x0.app rj.path) y = true :=
  overlap_iff.mpr ⟨hr.root, Or.inl (by rw [← hr.steps]; exact List.prefix_rfl)⟩

theorem boundDrop_d (P : Prog) (s : AState) (q : Pl) (x : Nat) (own : List Pl) (b : Bool) :
    (boundDrop P s q x own).d b = ((s.bits x).half b && bindD P s q x own b) := by
  cases b <;> rfl

/-- The bind state of a pairing of `y` for the join the drop goes through holds `D`. -/
theorem bindD_cur {P : Prog} {σ : Store} {s : AState} {own : List Pl} {y rj x0 : Pl}
    {v x : Nat} {b : Bool} (hwf : P.wf = true) (h : Abs P σ s) (hy : y ∈ P.tracked)
    (hr : Routed P σ own y rj v x0) (ho : P.owner x = some y) (hc : Cur P σ x b) :
    bindD P s y x own b = true := by
  obtain ⟨hcells, _, _, _⟩ := (wfCells_iff P).mp ((wf_iff P).mp hwf).2.2.2.2.2.2
  obtain ⟨z, hz⟩ := List.mem_iff_getElem?.mp (hcells y hy rj hr.shadow x ho)
  have hd := h.bound z y rj x (P.guard x) v x0 hz hr.param hr.bind hr.overlap b
    (fun f hf => hc f (by rw [guard_bind hz] at hf; exact hf))
  simp only [bindD, List.any_eq_true, List.mem_range]
  refine ⟨z, (List.getElem?_eq_some_iff.mp hz).1, ?_⟩
  rw [hz]
  simp [hr.mem, hd]

theorem boundSt_dcur {P : Prog} {σ : Store} {s : AState} {own : List Pl} {y rj x0 : Pl}
    {v : Nat} (hwf : P.wf = true) (h : Abs P σ s) (hy : y ∈ P.tracked)
    (hr : Routed P σ own y rj v x0) : DCur P σ (boundSt P s y own) y := by
  intro x b ho hc
  have hb := bindD_cur hwf h hy hr ho hc
  have hh := h.half x b (owner_lt ho) hc
  simp [boundSt, ho, Bits.or_d, boundDrop_d, hh, hb]

theorem same_some {P : Prog} {q : Pl} {x p : Nat} (h : P.same q x = some p) :
    ∃ g, P.guard x = some g ∧ P.owner p = some q ∧ P.guard p = some g := by
  unfold Prog.same at h
  cases hg : P.guard x with
  | none =>
    rw [hg] at h
    cases h
  | some g =>
    rw [hg] at h
    have h' : ((P.pairsOf q).find? fun p => P.guard p == some g) = some p := h
    exact ⟨g, rfl, mem_pairsOf.mp (List.mem_of_find?_eq_some h'),
      by simpa using List.find?_some h'⟩

theorem shadowDrop_d_some {P : Prog} {s : AState} {q : Pl} {own : List Pl} {x p : Nat}
    (b : Bool) (h : P.same q x = some p) :
    (shadowDrop P s q own x).d b =
      ((s.bits x).half b && ((s.bits p).half b && bindD P s q p own b)) := by
  unfold shadowDrop
  rw [h]
  cases b <;> rfl

theorem shadowDrop_d_none {P : Prog} {s : AState} {q : Pl} {own : List Pl} {x : Nat}
    (b : Bool) (h : P.same q x = none) :
    (shadowDrop P s q own x).d b = (s.bits x).half b := by
  unfold shadowDrop
  rw [h]
  cases b <;> rfl

/-- `drop_shadow` of `t` leaves every pairing of `t` dropped: the bind state it reads holds
`D`, as the join is bound to the place dropped. -/
theorem dropShadow_dcur {P : Prog} {σ : Store} {s : AState} {own : List Pl} {y rj x0 : Pl}
    {v : Nat} (hwf : P.wf = true) (h : Abs P σ s) (hy : y ∈ P.tracked)
    (hr : Routed P σ own y rj v x0) (t : Pl) : DCur P σ (dropShadow P s y t own) t := by
  intro x b ho hc
  have hh := h.half x b (owner_lt ho) hc
  have hsd : (shadowDrop P s y own x).d b = true := by
    cases hs : P.same y x with
    | none => rw [shadowDrop_d_none b hs, hh]
    | some p =>
      obtain ⟨g, hgx, hop, hgp⟩ := same_some hs
      have hcp : Cur P σ p b := fun f hf => hc f (by rw [hgx, ← hgp]; exact hf)
      rw [shadowDrop_d_some b hs, hh, h.half p b (owner_lt hop) hcp,
        bindD_cur hwf h hy hr hop hcp]
      rfl
  simp [dropShadow, ho, Bits.or_d, hsd]

/-! ## One name of the address -/

/-- What a drop of `y` must leave in the analysis state: `y` dropped, the views anchored
on it and their join places dropped, and each join place that may be `y` dropped, or
overlapping the join the drop goes through, itself dropped. -/
structure Dropped (P : Prog) (σ : Store) (own : List Pl) (y : Pl) (s : AState) : Prop where
  self : DCur P σ s y
  hung : ∀ d, Hangs P σ.loan (P.lent y) d →
    DCur P σ s d ∧ ∀ t, t ∈ P.shadow d → DCur P σ s t
  shadow : ∀ t, t ∈ P.shadow y →
    DCur P σ s t ∨ ∃ o, o ∈ own ∧ overlap o t = true ∧ DCur P σ s o

theorem Dropped.mono {P : Prog} {σ : Store} {own : List Pl} {y : Pl} {s s' : AState}
    (hs : Sub P s s') (h : Dropped P σ own y s) : Dropped P σ own y s' where
  self := h.self.mono hs
  hung := fun d hd => ⟨(h.hung d hd).1.mono hs, fun t ht => ((h.hung d hd).2 t ht).mono hs⟩
  shadow := fun t ht => by
    rcases h.shadow t ht with hd | ⟨o, ho, hot, hd⟩
    · exact Or.inl (hd.mono hs)
    · exact Or.inr ⟨o, ho, hot, hd.mono hs⟩

theorem dropPlace_dcur {P : Prog} {σ : Store} {s : AState} (h : Abs P σ s) (q : Pl)
    (strong : Bool) : DCur P σ (dropPlace P s q strong) q :=
  (mark_dcur strong h (List.mem_singleton.mpr rfl)).mono (invalidate_sub ..)

theorem dropName_spec {P : Prog} {σ : Store} {s : AState} {own : List Pl}
    {routed strong : Bool} {y : Pl} (hwf : P.wf = true) (h : Abs P σ s)
    (hy : y ∈ P.tracked) (hcut : y.cut = false) (hown : ∀ o, o ∈ own → DCur P σ s o)
    (hr : routed = true → ∃ rj v x0, Routed P σ own y rj v x0) :
    Dropped P σ own y (dropName P own routed strong s y) := by
  rw [dropName_eq P own routed strong s hcut hy]
  cases hbd : isBound P own routed y with
  | false =>
    simp only [Bool.false_eq_true, ite_false]
    have hf : ∀ s t, Sub P s (dropPlace P s t false) := fun s t => dropPlace_sub P s t false
    have hA : Abs P σ (mark P s [y] Bits.drops strong) := h.grow (mark_sub ..)
    have hsub := foldl_sub hf ((P.shadow y).filter fun t => !own.any fun o => overlap o t)
      (dropPlace P s y strong)
    refine ⟨(dropPlace_dcur h y strong).mono hsub, fun d hd => ?_, fun t ht => ?_⟩
    · obtain ⟨h1, h2⟩ := invalidate_dcur hwf hA (fun _ hb => hb) hd
      exact ⟨h1.mono hsub, fun t ht => (h2 t ht).mono hsub⟩
    · by_cases hov : (own.any fun o => overlap o t) = true
      · obtain ⟨o, ho, hot⟩ := List.any_eq_true.mp hov
        exact Or.inr ⟨o, ho, hot, ((hown o ho).mono (dropPlace_sub ..)).mono hsub⟩
      · refine Or.inl (foldl_mem (Q := fun s => DCur P σ s t) hf
          (fun _ _ hs hq => hq.mono hs) _ _
          (List.mem_filter.mpr ⟨ht, by simpa using hov⟩) fun s0 hs0 => ?_)
        exact dropPlace_dcur (h.grow ((dropPlace_sub ..).trans hs0)) t false
  | true =>
    simp only [ite_true]
    obtain ⟨hrt, t0, p0, g0, hcell, _⟩ := isBound_true hbd
    obtain ⟨rj, v, x0, hrj⟩ := hr hrt
    have hf : ∀ s t, Sub P s (dropShadow P s y t own) := fun s t => dropShadow_sub ..
    have hB : Abs P σ (boundSt P s y own) := h.grow (boundSt_sub ..)
    obtain ⟨_, hbc, _, _⟩ := (wfCells_iff P).mp ((wf_iff P).mp hwf).2.2.2.2.2.2
    have hp0 := (hbc y t0 p0 g0 hcell).1
    obtain ⟨b, hb⟩ := cur_exists P σ p0
    have hany : ((P.pairsOf y).any fun x => (boundDrop P s y x own).anyD) = true :=
      List.any_eq_true.mpr ⟨p0, mem_pairsOf.mpr hp0, Bits.anyD_of_d (b := b) (by
        rw [boundDrop_d, h.half p0 b (owner_lt hp0) hb, bindD_cur hwf h hy hrj hp0 hb]
        rfl)⟩
    have hdb : dropBound P s y own = invalidate P (boundSt P s y own) (P.lent y) := by
      rw [dropBound_eq, ifPos hany]
    rw [hdb]
    have hsub := foldl_sub hf ((P.shadow y).filter fun t => !own.any fun o => overlap o t)
      (invalidate P (boundSt P s y own) (P.lent y))
    have hsb : Sub P s (invalidate P (boundSt P s y own) (P.lent y)) :=
      (boundSt_sub ..).trans (invalidate_sub ..)
    refine ⟨((boundSt_dcur hwf h hy hrj).mono (invalidate_sub ..)).mono hsub,
      fun d hd => ?_, fun t ht => ?_⟩
    · obtain ⟨h1, h2⟩ := invalidate_dcur hwf hB (fun _ hb => hb) hd
      exact ⟨h1.mono hsub, fun t ht => (h2 t ht).mono hsub⟩
    · by_cases hov : (own.any fun o => overlap o t) = true
      · obtain ⟨o, ho, hot⟩ := List.any_eq_true.mp hov
        exact Or.inr ⟨o, ho, hot, ((hown o ho).mono hsb).mono hsub⟩
      · refine Or.inl (foldl_mem (Q := fun s => DCur P σ s t) hf
          (fun _ _ hs hq => hq.mono hs) _ _
          (List.mem_filter.mpr ⟨ht, by simpa using hov⟩) fun s0 hs0 => ?_)
        exact dropShadow_dcur hwf (h.grow (hsb.trans hs0)) hy hrj t

/-! ## The act -/

/-- A drop of a place the pass names, not under a run-time offset. -/
theorem drop_sound {P : Prog} {σ σ' : Store} {s : AState} {a : Nat} {y : Pl}
    (hwf : P.wf = true) (hwa : P.wfAct (.drop a) = true) (hw : P.wfAddr a = true)
    (h : Abs P σ s) (hd : Den P σ a (some y)) (hcut : y.cut = false)
    (hh : Hung P (P.lent y) (fun p => inside y p = true) σ σ') :
    Abs P σ' (applyDrop P s a) := by
  have hyn : y ∈ P.names a := den_names h hw hd
  have hyt : y ∈ P.tracked := wfAct_drop hwa (Or.inl hyn) hcut
  have hyr : y.root < P.roots := wfAddr_names hw hyn
  have hf1 : ∀ s t, Sub P s (dropPlace P s t true) := fun s t => dropPlace_sub P s t true
  have hs1 : Sub P s ((P.own a).foldl (fun s t => dropPlace P s t true) s) :=
    foldl_sub hf1 _ _
  have hown : ∀ o, o ∈ P.own a →
      DCur P σ ((P.own a).foldl (fun s t => dropPlace P s t true) s) o := fun o ho =>
    foldl_mem (Q := fun s => DCur P σ s o) hf1 (fun _ _ hs hq => hq.mono hs) _ _ ho
      fun s0 hs0 => dropPlace_dcur (h.grow hs0) o true
  have hr : ∀ rj, P.jname a = some rj → ∃ v x0, Routed P σ (P.own a) y rj v x0 := by
    intro rj hj
    obtain ⟨v, rel, k, x0, _, _, hbv, rfl, rfl, hpar⟩ := den_routed hd hj
    have hrel : rel.cut = false := by
      simp only [Pl.app_cut, Bool.or_eq_false_iff] at hcut
      exact hcut.2
    have hrjt : (P.jroot k).app rel ∈ P.tracked := wfAct_drop hwa (Or.inr hj) hrel
    have hown : P.own a = [(P.jroot k).app rel] := by
      have hc : (!((P.jroot k).app rel).cut &&
          P.tracked.contains ((P.jroot k).app rel)) = true := by
        simp only [Bool.and_eq_true, Bool.not_eq_true', List.contains_iff_mem]
        exact ⟨hrel, hrjt⟩
      simp only [Prog.own, hj]
      rw [ifPos hc]
    refine ⟨v, x0, ?_, ?_, ?_, hpar, hbv, rfl, rfl⟩
    · rw [hown]
      exact List.mem_singleton.mpr rfl
    · intro t ht
      rw [hown] at ht
      exact List.mem_singleton.mp ht
    · exact mem_shadow.mpr ⟨hrjt, x0.app ((P.jroot k).app rel).path,
        mem_expand.mpr ⟨v, hpar, x0, (h.binds v x0 hbv).1, rfl⟩,
        overlap_iff.mpr ⟨rfl, Or.inl List.prefix_rfl⟩⟩
  have hQ : Dropped P σ (P.own a) y (applyDrop P s a) :=
    foldl_mem (Q := Dropped P σ (P.own a) y) (fun _ _ => dropName_sub ..)
      (fun _ _ hs hq => hq.mono hs) (P.names a) _ hyn fun s0 hs0 =>
        dropName_spec hwf (h.grow (hs1.trans hs0)) hyt hcut
          (fun o ho => (hown o ho).mono hs0) fun hrt => by
            cases hj : P.jname a with
            | none =>
              rw [hj] at hrt
              cases hrt
            | some rj => exact ⟨rj, hr rj hj⟩
  refine Abs.hung hwf h (applyDrop_sub P s a) hh (fun p hp => ?_)
    (fun t v x p hp hpar hbv ht hin => ?_) hQ.hung
  · refine ⟨?_, y, hyt, hp, hQ.self⟩
    rw [← (inside_iff.mp hp).1]
    exact hyr
  · have hsh : t ∈ P.shadow y :=
      mem_shadow.mpr ⟨ht, x.app t.path,
        mem_expand.mpr ⟨v, hpar, x, (h.binds v x hbv).1, rfl⟩, overlap_of_inside hin hp⟩
    rcases hQ.shadow t hsh with hdc | ⟨o, ho, hot, hdc⟩
    · exact ⟨t, ht, rfl, hin, hdc⟩
    · cases hj : P.jname a with
      | none =>
        simp only [Prog.own, hj] at ho
        cases ho
      | some rj =>
        obtain ⟨v0, x0, hrj⟩ := hr rj hj
        have hrt : rj.root = t.root := (overlap_iff.mp (hrj.only o ho ▸ hot)).1
        have hv : v0 = v :=
          Option.some.inj (hrj.param.symm.trans ((paramOf_root hrt).trans hpar))
        subst hv
        have hx : x0 = x := Option.some.inj (hrj.bind.symm.trans hbv)
        subst hx
        exact ⟨rj, (mem_shadow.mp hrj.shadow).1, hrt,
          inside_congr hrj.root hrj.steps hp, hrj.only o ho ▸ hdc⟩

end Hirsel.CoreIR.Own
