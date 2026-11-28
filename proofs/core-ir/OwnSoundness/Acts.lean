import OwnSoundness.Drop
import OwnSoundness.Write

/-!
# Each act keeps the analysis state a description of the store

`act_sound`: when `apply`'s checks pass on a state that describes the store, the act does
not fault, and the state `apply` gives describes the store the act leaves.
-/

namespace Hirsel.CoreIR.Own

/-! ## Flags, flag reads and borrows -/

theorem flag_sound {P : Prog} {σ : Store} {s : AState} {f : Nat} {v : Option Bool} {b : Bool}
    (h : Abs P σ s) (hv : ∀ c, v = some c → b = c) :
    Abs P { σ with flag := fun g => if g == f then b else σ.flag g }
      (applyAct P s (.flag f v)) := by
  refine Abs.shift h (fun x b' _ hc => ?_) rfl rfl (fun k hk hl => h.loan k hk hl)
    (fun ri hr hst => ?_)
  · by_cases hg : P.guard x = some f
    · have hgb : (P.guard x == some f) = true := by rw [hg]; exact beq_self_eq_true _
      have hb : b' = b := by
        have := hc f hg
        simpa using this
      refine ⟨σ.flag f, fun f' hf' => ?_, fun hh => ?_, fun hd => ?_⟩
      · rw [hg] at hf'
        cases hf'
        rfl
      · simp only [applyAct]
        rw [ifPos hgb]
        exact Bits.flag_half hh (fun c hcv => hb ▸ hv c hcv)
      · simp only [applyAct]
        rw [ifPos hgb]
        exact Bits.flag_d hd (fun c hcv => hb ▸ hv c hcv)
    · have hgb : ¬ (P.guard x == some f) = true := by simpa using hg
      refine ⟨b', fun f' hf' => ?_, ?_, ?_⟩
      · have hne : f' ≠ f := fun he => hg (he ▸ hf')
        have := hc f' hf'
        simpa [hne] using this
      · simp only [applyAct]
        rw [ifNeg hgb]
        exact id
      · simp only [applyAct]
        rw [ifNeg hgb]
        exact id
  · simp only [applyAct, Bool.or_eq_false_iff] at hst
    have hf := reads_get hr
    have hne : ¬ (P.reads.getD ri 0 == f) = true := by
      intro he
      have h2 := hst.2
      rw [hf, beq_iff_eq.mp he] at h2
      simp at h2
    show σ.read ri = if (P.reads.getD ri 0 == f) = true then b else σ.flag (P.reads.getD ri 0)
    rw [ifNeg hne]
    exact h.fresh ri hr hst.1

theorem fresh_sound {P : Prog} {σ : Store} {s : AState} {ri : Nat} (h : Abs P σ s) :
    Abs P { σ with
        read := fun r => if r == ri then σ.flag (P.reads.getD ri 0) else σ.read r }
      (applyAct P s (.fresh ri)) := by
  refine Abs.shift h (fun x b _ hc => ⟨b, hc, id, id⟩) rfl rfl (fun k hk hl => h.loan k hk hl)
    (fun r hr hst => ?_)
  by_cases he : (r == ri) = true
  · show (if (r == ri) = true then σ.flag (P.reads.getD ri 0) else σ.read r) =
      σ.flag (P.reads.getD r 0)
    rw [ifPos he, beq_iff_eq.mp he]
  · have hst' : (if (r == ri) = true then false else s.stale r) = false := hst
    rw [ifNeg he] at hst'
    show (if (r == ri) = true then σ.flag (P.reads.getD ri 0) else σ.read r) =
      σ.flag (P.reads.getD r 0)
    rw [ifNeg he]
    exact h.fresh r hr hst'

theorem lend_sound {P : Prog} {σ : Store} {s : AState} {k : Nat} (h : Abs P σ s) :
    Abs P { σ with loan := fun j => j == k || σ.loan j } (applyAct P s (.lend k)) := by
  refine Abs.shift h (fun x b _ hc => ⟨b, hc, id, id⟩) rfl rfl (fun j hj hl => ?_)
    (fun ri hr hst => h.fresh ri hr hst)
  have hl' : (j == k || σ.loan j) = true := hl
  show (j == k || s.lend j) = true
  simp only [Bool.or_eq_true] at hl' ⊢
  exact hl'.imp id (h.loan j hj)

/-! ## One act -/

/-- An act went well: it did not fault, and `s'` describes the store it left. -/
def Res.Good (P : Prog) (s' : AState) : Res → Prop
  | .ok σ' => Abs P σ' s'
  | .fault _ => False

theorem act_sound {P : Prog} {σ : Store} {s : AState} {act : Act} {res : Res}
    (hwf : P.wf = true) (hact : act ∈ P.allActs) (h : Abs P σ s)
    (hc : checkAct P s act = true) (hstep : ActStep P σ act res) :
    res.Good P (applyAct P s act) := by
  have hwa := ((wf_iff P).mp hwf).2.2.1 act hact
  have hw : ∀ a, (act = .use a ∨ act = .drop a ∨ act = .write a ∨ act = .invalidate a) →
      P.wfAddr a = true := fun a ha => ((wf_iff P).mp hwf).2.2.2.1 a (mem_addrs hact ha)
  cases hstep with
  | use_ok => exact h
  | use_dropped hd hin hp =>
    obtain ⟨q, hq, hm⟩ := stuck h (hw _ (Or.inl rfl)) hd hin (by rw [hp]; exact fun hl => nomatch hl)
    have := List.all_eq_true.mp hc q hq
    rw [hm] at this
    cases this
  | use_dangling hd hin hp =>
    obtain ⟨q, hq, hm⟩ := stuck h (hw _ (Or.inl rfl)) hd hin (by rw [hp]; exact fun hl => nomatch hl)
    have := List.all_eq_true.mp hc q hq
    rw [hm] at this
    cases this
  | drop_view hd hv =>
    simp only [checkAct, Bool.and_eq_true, List.all_eq_true] at hc
    have := hc.1 _ (den_names h (hw _ (Or.inr (Or.inl rfl))) hd)
    rw [hv] at this
    cases this
  | drop_dropped hd hin hp =>
    simp only [checkAct, Bool.and_eq_true, List.all_eq_true] at hc
    obtain ⟨q, hq, hm⟩ := stuck h (hw _ (Or.inr (Or.inl rfl))) hd hin
      (by rw [hp]; exact fun hl => nomatch hl)
    have := hc.2 q hq
    rw [hm] at this
    cases this
  | drop_dangling hd hin hp =>
    simp only [checkAct, Bool.and_eq_true, List.all_eq_true] at hc
    obtain ⟨q, hq, hm⟩ := stuck h (hw _ (Or.inr (Or.inl rfl))) hd hin
      (by rw [hp]; exact fun hl => nomatch hl)
    have := hc.2 q hq
    rw [hm] at this
    cases this
  | drop_none _ => exact h.grow (applyDrop_sub P s _)
  | drop_cut _ _ => exact h.grow (applyDrop_sub P s _)
  | drop_ok hd hcut hh => exact drop_sound hwf hwa (hw _ (Or.inr (Or.inl rfl))) h hd hcut hh
  | write_none hd => exact write_none_sound hwf (hw _ (Or.inr (Or.inr (Or.inl rfl)))) h hd
  | write hd hh hfl =>
    exact write_sound hwf (hw _ (Or.inr (Or.inr (Or.inl rfl)))) h hd hh hfl
  | invalidate_none _ => exact h.grow (invalidate_sub ..)
  | invalidate hd hh =>
    exact hung_views hwf h
      (rewrites_of (den_names h (hw _ (Or.inr (Or.inr (Or.inr rfl)))) hd)) hh
  | «begin» hh => exact begin_sound hwf (of_decide_eq_true hwa) h hh
  | dead hh => exact dead_sound hwf (of_decide_eq_true hwa) h hh
  | flag hv => exact flag_sound h hv
  | fresh => exact fresh_sound h
  | lend => exact lend_sound h

/-- The acts of an instruction, in order. -/
theorem acts_sound {P : Prog} (hwf : P.wf = true) {acts : List Act} {σ : Store} {s : AState}
    {res : Res} (hall : ∀ a, a ∈ acts → a ∈ P.allActs) (h : Abs P σ s)
    (hc : checkActs P s acts = true) (hstep : ActsStep P σ acts res) :
    res.Good P (applyActs P s acts) := by
  induction hstep generalizing s with
  | nil => exact h
  | cons h1 _ ih =>
    simp only [checkActs, Bool.and_eq_true] at hc
    exact ih (fun a ha => hall a (List.mem_cons_of_mem _ ha))
      (act_sound hwf (hall _ (List.mem_cons_self ..)) h hc.1 h1) hc.2
  | fault h1 =>
    simp only [checkActs, Bool.and_eq_true] at hc
    exact act_sound hwf (hall _ (List.mem_cons_self ..)) h hc.1 h1

end Hirsel.CoreIR.Own
