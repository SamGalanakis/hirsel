import OwnSoundness.Addr

/-!
# Writes, `live` and `dead`

`apply`'s `Act::Write`, `Act::Invalidate`, `Act::Begin` and `Act::Dead` leave a state that
describes the store after the act.
-/

namespace Hirsel.CoreIR.Own

/-! ## `renew` -/

theorem renew_half {P : Prog} {s : AState} {Q : List Pl} {f : Bits → Bits} {x : Nat}
    {b : Bool} (hf : ∀ t : Bits, (f t).half b = t.half b) (h : (s.bits x).half b = true) :
    ((renew P s Q f).bits x).half b = true := by
  simp only [renew]
  cases P.owner x with
  | none => exact h
  | some q =>
    simp only []
    split
    · rw [hf]
      exact h
    · split
      · rw [Bits.or_half, h]
        rfl
      · exact h

theorem renew_keep {P : Prog} {s : AState} {Q : List Pl} {f : Bits → Bits} {x : Nat} {q : Pl}
    {b : Bool} (ho : P.owner x = some q) (hq : q ∉ Q) (h : (s.bits x).d b = true) :
    ((renew P s Q f).bits x).d b = true := by
  have hc : ¬ Q.contains q = true := fun hc => hq (List.contains_iff_mem.mp hc)
  simp only [renew, ho]
  rw [ifNeg hc]
  split
  · rw [Bits.or_d, h]
    rfl
  · exact h

theorem renew_none {P : Prog} {s : AState} {Q : List Pl} {f : Bits → Bits} {x : Nat}
    (ho : P.owner x = none) : (renew P s Q f).bits x = s.bits x := by
  simp only [renew, ho]

theorem renew_drops_d {P : Prog} {s : AState} {Q : List Pl} {x : Nat} {b : Bool}
    (h : (s.bits x).d b = true) : ((renew P s Q Bits.drops).bits x).d b = true := by
  simp only [renew]
  cases P.owner x with
  | none => exact h
  | some q =>
    simp only []
    split
    · exact (Bits.drops_mono _ b).2 h
    · split
      · exact (Bits.or_mono _ _ b).2 h
      · exact h

/-- `dead` leaves every place of the slot, and every join place that may be one,
dropped. -/
theorem renew_drops_dcur {P : Prog} {σ : Store} {s : AState} {Q : List Pl} {q : Pl}
    (h : Abs P σ s) (hq : q ∈ Q ∨ q ∈ Q.flatMap P.shadow) :
    DCur P σ (renew P s Q Bits.drops) q := by
  intro x b ho hc
  have hh := h.half x b (owner_lt ho) hc
  simp only [renew, ho]
  split
  · rw [Bits.drops_d]
    exact hh
  · rename_i h1
    split
    · rw [Bits.or_d, Bits.drops_d, hh]
      exact Bool.or_true _
    · rename_i h2
      rcases hq with hq | hq
      · exact absurd (List.contains_iff_mem.mpr hq) h1
      · exact absurd (List.contains_iff_mem.mpr hq) h2

/-! ## Borrows a new value ends -/

theorem endLoans_true {P : Prog} {σ : Store} {fresh : Pl → Bool} {k : Nat}
    (h : σ.endLoans P fresh k = true) :
    σ.loan k = true ∧ ∀ b d, P.borrows[k]? = some b → b.dest = [d] → fresh d = false := by
  simp only [Store.endLoans, Bool.and_eq_true, Bool.not_eq_true'] at h
  refine ⟨h.1, fun b d hb hd => ?_⟩
  have h2 := h.2
  rw [hb] at h2
  simp only [hd] at h2
  exact h2

theorem endAll_true {P : Prog} {lend : Nat → Bool} {Q : List Pl} {k : Nat}
    (hl : lend k = true) (h : ∀ b d, P.borrows[k]? = some b → b.dest = [d] → d ∉ Q) :
    endAll P lend Q k = true := by
  simp only [endAll, hl, Bool.true_and, Bool.not_eq_true']
  cases hb : P.borrows[k]? with
  | none => rfl
  | some b =>
    simp only []
    cases hd : b.dest with
    | nil => rfl
    | cons d rest =>
      cases rest with
      | nil =>
        exact Bool.eq_false_iff.mpr fun hc => h b d hb hd (List.contains_iff_mem.mp hc)
      | cons _ _ => rfl

/-! ## `covers` -/

theorem covers_iff {P : Prog} {w t : Pl} :
    P.covers w t = true ↔
      w.cut = false ∧ w.root = t.root ∧
        (w.steps <+: t.steps ∨
          (t.steps <+: w.steps ∧ t.root < P.slots ∧
            P.soleChain t.root t.steps (w.steps.drop t.steps.length) = true)) := by
  simp only [Prog.covers, Bool.and_eq_true, Bool.or_eq_true, Bool.not_eq_true', beq_iff_eq,
    List.isPrefixOf_iff_prefix, decide_eq_true_eq, and_assoc]

/-- A place in one the write covers gets a new value. -/
theorem revives_of_covers {P : Prog} {y q p : Pl} (hc : P.covers y q = true)
    (hin : inside q p = true) : P.revives y p = true := by
  obtain ⟨hcut, hroot, hsteps⟩ := covers_iff.mp hc
  obtain ⟨hqp, hqs⟩ := inside_iff.mp hin
  unfold Prog.revives
  rw [List.any_eq_true]
  rcases hsteps with hpre | ⟨hpre, hlt, hsole⟩
  · refine ⟨y.steps.length, List.mem_range.mpr (Nat.lt_succ_self _), ?_⟩
    simp only [List.take_length, Bool.and_eq_true]
    exact ⟨covers_iff.mpr ⟨hcut, rfl, Or.inl List.prefix_rfl⟩,
      inside_iff.mpr ⟨hroot.trans hqp, hpre.trans hqs⟩⟩
  · have hlen : q.steps.length ≤ y.steps.length := hpre.length_le
    refine ⟨q.steps.length, List.mem_range.mpr (by omega), ?_⟩
    have htake : y.steps.take q.steps.length = q.steps := by
      obtain ⟨r, hr⟩ := hpre
      rw [← hr]
      simp
    simp only [htake, Bool.and_eq_true]
    refine ⟨covers_iff.mpr ⟨hcut, rfl, Or.inr ⟨hpre, ?_, ?_⟩⟩,
      inside_iff.mpr ⟨hroot.trans hqp, hqs⟩⟩
    · show y.root < P.slots
      rw [hroot]
      exact hlt
    · show P.soleChain y.root q.steps (y.steps.drop q.steps.length) = true
      rw [hroot]
      exact hsole

/-! ## The store after a write -/

theorem revive_cut {P : Prog} {σ : Store} {y : Pl} (h : y.cut = true) :
    Store.revive P σ y = σ := by
  simp [Store.revive, h]

theorem revive_st {P : Prog} {σ : Store} {y : Pl} (h : y.cut = false) (p : Pl) :
    (Store.revive P σ y).st p = if P.revives y p then .live else σ.st p := by
  simp [Store.revive, h]

theorem revive_loan {P : Prog} {σ : Store} {y : Pl} (h : y.cut = false) :
    (Store.revive P σ y).loan = σ.endLoans P (P.covers y) := by
  simp [Store.revive, h]

theorem revive_flag {P : Prog} {σ : Store} {y : Pl} : (Store.revive P σ y).flag = σ.flag := by
  unfold Store.revive
  split <;> rfl

theorem revive_read {P : Prog} {σ : Store} {y : Pl} : (Store.revive P σ y).read = σ.read := by
  unfold Store.revive
  split <;> rfl

theorem revive_bind {P : Prog} {σ : Store} {y : Pl} : (Store.revive P σ y).bind = σ.bind := by
  unfold Store.revive
  split <;> rfl

theorem revive_live {P : Prog} {σ : Store} {y : Pl} (h : y.cut = false) {p : Pl}
    (hp : (Store.revive P σ y).st p ≠ .live) : (Store.revive P σ y).st p = σ.st p := by
  rw [revive_st h] at hp ⊢
  by_cases hr : P.revives y p = true
  · rw [ifPos hr] at hp
    exact absurd rfl hp
  · rw [ifNeg hr]

/-- A write the analysis begins nothing for. -/
theorem Abs.revive_keep {P : Prog} {σ : Store} {s : AState} (h : Abs P σ s) (y : Pl) :
    Abs P (Store.revive P σ y) s := by
  cases hcut : y.cut with
  | true =>
    rw [revive_cut hcut]
    exact h
  | false =>
    refine Abs.revived (fun _ => False) h revive_flag revive_read revive_bind
      (fun p hp => revive_live hcut hp) (fun _ _ hh => hh) (fun _ _ _ _ _ hd => hd)
      (fun _ _ => rfl) (fun _ _ hR => hR.elim) (fun _ _ _ _ hR => hR.elim)
      (fun k hk hl => ?_) (fun _ hs => hs)
    rw [revive_loan hcut] at hl
    exact h.loan k hk (endLoans_true hl).1

/-! ## A write's flags and dangling views -/

theorem havoc_sub (P : Prog) (s : AState) (a : Nat) : Sub P s (havoc P s a) := by
  refine (invalidate_sub P s (rewrites P a)).trans
    (Sub.of_bits (fun x b => ?_) (fun _ h => h) (fun ri h => ?_))
  · simp only [havoc]
    cases P.guard x with
    | none => exact ⟨id, id⟩
    | some f =>
      simp only []
      split
      · exact ⟨fun h => Bits.flag_half h (fun c hc => by cases hc),
          fun h => Bits.flag_d h (fun c hc => by cases hc)⟩
      · exact ⟨id, id⟩
  · simp only [havoc, Bool.or_eq_false_iff] at h
    exact h.1

theorem untouched {P : Prog} {a f : Nat} {y : Pl} (hlt : f < P.slots) (hyn : y ∈ P.names a)
    (ht : ¬ touches P a f = true) : f ≠ y.root := by
  intro heq
  apply ht
  simp only [touches, Bool.and_eq_true, decide_eq_true_eq, List.any_eq_true, beq_iff_eq]
  exact ⟨hlt, y, hyn, heq.symm⟩

theorem hung_views {P : Prog} {σ σ1 : Store} {s : AState} {seed seed' : Borrow → Bool}
    (hwf : P.wf = true) (h : Abs P σ s) (hs : ∀ b, seed b = true → seed' b = true)
    (hh : Hung P seed (fun _ => False) σ σ1) : Abs P σ1 (invalidate P s seed') :=
  Abs.hung hwf h (invalidate_sub ..) hh (fun _ hk => hk.elim) (fun _ _ _ _ hk => hk.elim)
    (fun _ hd => invalidate_dcur hwf h hs hd)

theorem rewrites_of {P : Prog} {a : Nat} {y : Pl} (hyn : y ∈ P.names a) (b : Borrow)
    (hb : (b.anchor.any fun f => overlap f y) = true) : rewrites P a b = true := by
  obtain ⟨f, hf, hov⟩ := List.any_eq_true.mp hb
  exact List.any_eq_true.mpr ⟨f, hf, List.any_eq_true.mpr ⟨y, hyn, hov⟩⟩

/-- The flags a write may change hold either value after it. -/
theorem write_flags {P : Prog} {σ1 : Store} {s : AState} {a : Nat} {y : Pl} {fl : Nat → Bool}
    (hwf : P.wf = true) (h1 : Abs P σ1 (invalidate P s (rewrites P a)))
    (hyn : y ∈ P.names a) (hfl : ∀ f, f ≠ y.root → fl f = σ1.flag f) :
    Abs P { σ1 with flag := fl } (havoc P s a) := by
  obtain ⟨_, _, hgs, hrs⟩ := (wfCells_iff P).mp ((wf_iff P).mp hwf).2.2.2.2.2.2
  refine Abs.shift h1 (fun x b hx hc => ?_) rfl rfl (fun k hk hl => h1.loan k hk hl)
    (fun ri hr hst => ?_)
  · cases hg : P.guard x with
    | none =>
      refine ⟨b, fun f hf => ?_, ?_, ?_⟩
      · rw [hg] at hf
        cases hf
      · simp only [havoc, hg]
        exact id
      · simp only [havoc, hg]
        exact id
    | some f =>
      by_cases ht : touches P a f = true
      · refine ⟨σ1.flag f, fun f' hf' => ?_, fun hh => ?_, fun hd => ?_⟩
        · rw [hg] at hf'
          cases hf'
          rfl
        · simp only [havoc, hg]
          rw [ifPos ht]
          exact Bits.flag_half hh (fun c hc => by cases hc)
        · simp only [havoc, hg]
          rw [ifPos ht]
          exact Bits.flag_d hd (fun c hc => by cases hc)
      · refine ⟨b, fun f' hf' => ?_, ?_, ?_⟩
        · rw [hg] at hf'
          cases hf'
          rw [hc f hg]
          exact hfl f (untouched (hgs x f hg) hyn ht)
        · simp only [havoc, hg]
          rw [ifNeg ht]
          exact id
        · simp only [havoc, hg]
          rw [ifNeg ht]
          exact id
  · simp only [havoc, Bool.or_eq_false_iff] at hst
    have hf := reads_get hr
    have ht := hst.2
    rw [hf] at ht
    have hne : P.reads.getD ri 0 ≠ y.root :=
      untouched (hrs _ (List.mem_of_getElem? hf)) hyn (by simp only [ht]; exact Bool.false_ne_true)
    show σ1.read ri = fl (P.reads.getD ri 0)
    rw [hfl _ hne]
    exact h1.fresh ri hr hst.1

/-! ## The places a write begins -/

theorem applyWrite_cases (P : Prog) (s : AState) (a : Nat) :
    (∃ r, P.places a = [r] ∧ r.cut = false ∧
      applyWrite P s a =
        renew P (havoc P s a) ((P.hits a).filter fun q => P.covers r q) Bits.begun) ∨
    ((∀ r, P.places a = [r] → r.cut = true) ∧ applyWrite P s a = havoc P s a) := by
  unfold applyWrite
  cases hp : P.places a with
  | nil => exact Or.inr ⟨fun r h => (nomatch h), rfl⟩
  | cons r rest =>
    cases rest with
    | nil =>
      cases hc : r.cut with
      | false => exact Or.inl ⟨r, rfl, hc, by simp [hc]⟩
      | true => exact Or.inr ⟨fun r' h => (by cases h; exact hc), by simp [hc]⟩
    | cons r' rest' => exact Or.inr ⟨fun r h => (nomatch h), rfl⟩

/-- A join place a write through the join covers is rooted at the join. -/
theorem routed_root {P : Prog} {k : Nat} {rel : Path} {q : Pl}
    (hc : P.covers ((P.jroot k).app rel) q = true) : q.root = P.roots + k :=
  ((covers_iff.mp hc).2.1).symm

/-- A write through a join whose parameter points at nothing the pass names. -/
theorem write_unbound {P : Prog} {σ : Store} {s : AState} {a : Nat} {r : Pl}
    (hwf : P.wf = true) (hw : P.wfAddr a = true) (h : Abs P σ s) (hd : Den P σ a none)
    (hpl : P.places a = [r]) (hrc : r.cut = false) :
    Abs P σ (renew P s ((P.hits a).filter fun q => P.covers r q) Bits.begun) := by
  cases hj : P.jname a with
  | none =>
    simp only [Prog.places, hj] at hpl
    cases hb : P.base a with
    | none =>
      rw [hd.slot_none hb] at hpl
      cases hpl
    | some vr =>
      obtain ⟨v, rel⟩ := vr
      have hjo := jname_none hj hb
      rcases (wfAddr_base hw hb).2 with hs | hone
      · rw [hjo] at hs
        cases hs
      · rcases hone r hpl with hc | hc
        · rw [hrc] at hc
          cases hc
        · obtain ⟨x, hx⟩ := Option.isSome_iff_exists.mp hc
          have := h.plain v x hjo hx
          rw [hd.unbound hb] at this
          cases this
  | some rj =>
    simp only [Prog.places, hj] at hpl
    obtain ⟨v, rel, k, hb, hk, rfl⟩ := jname_some hj
    obtain rfl : (P.jroot k).app rel = r := List.singleton_inj.mp hpl
    have hbv := hd.unbound hb
    refine Abs.revived (fun q => q ∈ (P.hits a).filter fun q => P.covers ((P.jroot k).app rel) q)
      h rfl rfl rfl (fun _ _ => rfl)
      (fun _ b hh => renew_half (fun t => Bits.begun_half t b) hh)
      (fun _ _ _ ho hq hdd => renew_keep ho hq hdd) (fun _ ho => renew_none ho)
      (fun q p hq hlt _ => ?_) (fun t v' y' p ht hpar hbv' _ => ?_) (fun k' hk' hl => ?_)
      (fun _ hs => hs)
    · have := routed_root (List.mem_filter.mp hq).2
      omega
    · have hroot := routed_root (List.mem_filter.mp ht).2
      have hv : v = v' := Option.some.inj ((paramOf_jroot hk t hroot).symm.trans hpar)
      subst hv
      rw [hbv] at hbv'
      cases hbv'
    · refine endAll_true (h.loan k' hk' hl) fun b d hbb hdd hdQ => ?_
      have h1 := wf_dest_one hwf (List.mem_of_getElem? hbb) hdd
      have h2 := routed_root (List.mem_filter.mp hdQ).2
      omega

/-- A write of `y` that begins the places the address covers. -/
theorem write_renew {P : Prog} {σ : Store} {s : AState} {a : Nat} {y r : Pl}
    (hwf : P.wf = true) (hw : P.wfAddr a = true) (h : Abs P σ s) (hd : Den P σ a (some y))
    (hpl : P.places a = [r]) (hrc : r.cut = false) :
    Abs P (Store.revive P σ y)
      (renew P s ((P.hits a).filter fun q => P.covers r q) Bits.begun) := by
  have hyn := den_names h hw hd
  cases hj : P.jname a with
  | none =>
    simp only [Prog.places, hj] at hpl
    rw [hpl] at hyn
    obtain rfl : y = r := List.mem_singleton.mp hyn
    have hyr : y.root < P.roots := wfAddr_names hw (by rw [hpl]; exact List.mem_singleton.mpr rfl)
    refine Abs.revived (fun q => q ∈ (P.hits a).filter fun q => P.covers y q)
      h revive_flag revive_read revive_bind (fun p hp => revive_live hrc hp)
      (fun _ b hh => renew_half (fun t => Bits.begun_half t b) hh)
      (fun _ _ _ ho hq hdd => renew_keep ho hq hdd) (fun _ ho => renew_none ho)
      (fun q p hq _ hin => ?_) (fun t v y' p ht hpar _ _ => ?_) (fun k hk hl => ?_)
      (fun _ hs => hs)
    · rw [revive_st hrc, ifPos (revives_of_covers (List.mem_filter.mp hq).2 hin)]
    · have h1 := (covers_iff.mp (List.mem_filter.mp ht).2).2.1
      have h2 := (paramOf_some hpar).1
      omega
    · rw [revive_loan hrc] at hl
      obtain ⟨hl1, hl2⟩ := endLoans_true hl
      refine endAll_true (h.loan k hk hl1) fun b d hbb hdd hdQ => ?_
      have hcov := (List.mem_filter.mp hdQ).2
      rw [hl2 b d hbb hdd] at hcov
      cases hcov
  | some rj =>
    simp only [Prog.places, hj] at hpl
    obtain ⟨v, rel, k, x0, _, hk, hbv, rfl, rfl, hpar⟩ := den_routed hd hj
    obtain rfl : (P.jroot k).app rel = r := List.singleton_inj.mp hpl
    have hrel : rel.cut = false := hrc
    have hcut : (x0.app rel).cut = false := by
      simp only [Pl.app_cut, (h.binds v x0 hbv).2, hrel, Bool.or_self]
    have hsl := ((wf_iff P).mp hwf).1
    refine Abs.revived (fun q => q ∈ (P.hits a).filter fun q => P.covers ((P.jroot k).app rel) q)
      h revive_flag revive_read revive_bind (fun p hp => revive_live hcut hp)
      (fun _ b hh => renew_half (fun t => Bits.begun_half t b) hh)
      (fun _ _ _ ho hq hdd => renew_keep ho hq hdd) (fun _ ho => renew_none ho)
      (fun q p hq hlt _ => ?_) (fun t v' y' p ht hpar' hbv' hin => ?_) (fun k' hk' hl => ?_)
      (fun _ hs => hs)
    · have := routed_root (List.mem_filter.mp hq).2
      omega
    · have hcov := (List.mem_filter.mp ht).2
      have hroot := routed_root hcov
      have hv : v = v' := Option.some.inj ((paramOf_jroot hk t hroot).symm.trans hpar')
      subst hv
      obtain rfl : x0 = y' := Option.some.inj (hbv.symm.trans hbv')
      have hpre : rel.steps <+: t.steps := by
        rcases (covers_iff.mp hcov).2.2 with hp | ⟨_, hlt, _⟩
        · exact hp
        · omega
      obtain ⟨hr1, hr2⟩ := inside_iff.mp hin
      have hyp : inside (x0.app rel) p = true :=
        inside_iff.mpr ⟨hr1, ((List.prefix_append_right_inj x0.steps).mpr hpre).trans hr2⟩
      rw [revive_st hcut, ifPos (revives_of_covers
        (covers_iff.mpr ⟨hcut, rfl, Or.inl List.prefix_rfl⟩) hyp)]
    · rw [revive_loan hcut] at hl
      refine endAll_true (h.loan k' hk' (endLoans_true hl).1) fun b d hbb hdd hdQ => ?_
      have h1 := wf_dest_one hwf (List.mem_of_getElem? hbb) hdd
      have h2 := routed_root (List.mem_filter.mp hdQ).2
      omega

/-- A write through an address that is no place the pass names. -/
theorem write_none_sound {P : Prog} {σ : Store} {s : AState} {a : Nat} (hwf : P.wf = true)
    (hw : P.wfAddr a = true) (h : Abs P σ s) (hd : Den P σ a none) :
    Abs P σ (applyWrite P s a) := by
  have h2 := h.grow (havoc_sub P s a)
  rcases applyWrite_cases P s a with ⟨r, hpl, hrc, he⟩ | ⟨_, he⟩
  · rw [he]
    exact write_unbound hwf hw h2 hd hpl hrc
  · rw [he]
    exact h2

theorem write_sound {P : Prog} {σ σ1 : Store} {s : AState} {a : Nat} {y : Pl}
    {fl : Nat → Bool} (hwf : P.wf = true) (hw : P.wfAddr a = true) (h : Abs P σ s)
    (hd : Den P σ a (some y))
    (hh : Hung P (fun b => b.anchor.any fun f => overlap f y) (fun _ => False) σ σ1)
    (hfl : ∀ f, f ≠ y.root → fl f = σ.flag f) :
    Abs P (Store.revive P { σ1 with flag := fl } y) (applyWrite P s a) := by
  have hyn := den_names h hw hd
  have h1 := hung_views (seed' := rewrites P a) hwf h (rewrites_of hyn) hh
  have h2 := write_flags (fl := fl) hwf h1 hyn (fun f hf => by rw [hh.flag]; exact hfl f hf)
  have hd2 : Den P { σ1 with flag := fl } a (some y) := hd.congr hh.bind
  rcases applyWrite_cases P s a with ⟨r, hpl, hrc, he⟩ | ⟨hall, he⟩
  · rw [he]
    exact write_renew hwf hw h2 hd2 hpl hrc
  · rw [he]
    exact h2.revive_keep y

/-! ## `live` and `dead` -/

theorem begin_sound {P : Prog} {σ σ1 : Store} {s : AState} {slot : Nat} (hwf : P.wf = true)
    (hslot : slot < P.slots) (h : Abs P σ s)
    (hh : Hung P (anchored slot) (fun _ => False) σ σ1) :
    Abs P (σ1.begin P slot) (applySlot P s slot Bits.begun) := by
  have h1 := hung_views (seed' := anchored slot) hwf h (fun _ hb => hb) hh
  have hsl := ((wf_iff P).mp hwf).1
  refine Abs.revived (fun q => q ∈ P.tracked.filter fun q => q.root == slot) h1 rfl rfl rfl
    (fun p hp => ?_) (fun _ b hh => renew_half (fun t => Bits.begun_half t b) hh)
    (fun _ _ _ ho hq hdd => renew_keep ho hq hdd) (fun _ ho => renew_none ho)
    (fun q p hq _ hin => ?_) (fun t v y' p ht hpar _ _ => ?_) (fun k hk hl => ?_)
    (fun _ hs => hs)
  · by_cases hr : (p.root == slot) = true
    · exact absurd (ifPos hr) hp
    · exact ifNeg hr
  · have hq' : q.root = slot := by simpa using (List.mem_filter.mp hq).2
    have hp' : (p.root == slot) = true := by
      rw [← (inside_iff.mp hin).1, hq']
      exact beq_self_eq_true _
    exact ifPos hp'
  · have h1 : t.root = slot := by simpa using (List.mem_filter.mp ht).2
    have h2 := (paramOf_some hpar).1
    omega
  · obtain ⟨hl1, hl2⟩ := endLoans_true (P := P) (σ := σ1) (fresh := fun d => d.root == slot) hl
    refine endAll_true (h1.loan k hk hl1) fun b d hbb hdd hdQ => ?_
    have := hl2 b d hbb hdd
    rw [(List.mem_filter.mp hdQ).2] at this
    cases this

theorem dead_sound {P : Prog} {σ σ1 : Store} {s : AState} {slot : Nat} (hwf : P.wf = true)
    (hslot : slot < P.slots) (h : Abs P σ s)
    (hh : Hung P (anchored slot)
      (fun p => ∃ t, t ∈ P.tracked ∧ t.root = slot ∧ inside t p = true) σ σ1) :
    Abs P { σ1 with loan := σ1.endLoans P fun d => d.root == slot }
      (applySlot P s slot Bits.drops) := by
  have hsl := ((wf_iff P).mp hwf).1
  have hA1 : Abs P σ (invalidate P s (anchored slot)) := h.grow (invalidate_sub ..)
  have hsub : Sub P (invalidate P s (anchored slot))
      { renew P (invalidate P s (anchored slot)) (P.tracked.filter fun q => q.root == slot)
          Bits.drops with lend := (invalidate P s (anchored slot)).lend } :=
    Sub.of_bits
      (fun x b => ⟨fun hh => renew_half (fun t => Bits.drops_half t b) hh, renew_drops_d⟩)
      (fun _ hl => hl) (fun _ hs => hs)
  have hQ : ∀ t, t ∈ P.tracked → t.root = slot →
      t ∈ P.tracked.filter fun q => q.root == slot := fun t ht hr =>
    List.mem_filter.mpr ⟨ht, by rw [hr]; exact beq_self_eq_true _⟩
  have hb : Abs P σ1
      { renew P (invalidate P s (anchored slot)) (P.tracked.filter fun q => q.root == slot)
          Bits.drops with lend := (invalidate P s (anchored slot)).lend } := by
    refine Abs.hung hwf h ((invalidate_sub ..).trans hsub) hh (fun p hp => ?_)
      (fun t' v y' p hp hpar hbv ht' hin' => ?_) (fun d hd => ?_)
    · obtain ⟨t, ht, hr, hin⟩ := hp
      refine ⟨?_, t, ht, hin, renew_drops_dcur hA1 (Or.inl (hQ t ht hr))⟩
      rw [← (inside_iff.mp hin).1, hr]
      omega
    · obtain ⟨t, ht, hr, hin⟩ := hp
      exact ⟨t', ht', rfl, hin', renew_drops_dcur hA1 (Or.inr (List.mem_flatMap.mpr
        ⟨t, hQ t ht hr, mem_shadow.mpr ⟨ht', y'.app t'.path,
          mem_expand.mpr ⟨v, hpar, y', (h.binds v y' hbv).1, rfl⟩,
          overlap_of_inside hin' hin⟩⟩))⟩
    · obtain ⟨h1, h2⟩ := invalidate_dcur hwf h (fun _ hb => hb) hd
      exact ⟨h1.mono hsub, fun t ht => (h2 t ht).mono hsub⟩
  refine Abs.revived (fun _ => False) hb rfl rfl rfl (fun _ _ => rfl) (fun _ _ hh => hh)
    (fun _ _ _ _ _ hd => hd) (fun _ _ => rfl) (fun _ _ hR => hR.elim)
    (fun _ _ _ _ hR => hR.elim) (fun k hk hl => ?_) (fun _ hs => hs)
  obtain ⟨hl1, hl2⟩ := endLoans_true (P := P) (σ := σ1) (fresh := fun d => d.root == slot) hl
  refine endAll_true (hb.loan k hk hl1) fun b d hbb hdd hdQ => ?_
  have := hl2 b d hbb hdd
  rw [(List.mem_filter.mp hdQ).2] at this
  cases this

end Hirsel.CoreIR.Own
