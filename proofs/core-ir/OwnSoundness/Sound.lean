import OwnSoundness.Acts
import OwnSoundness.Edge

/-!
# Soundness

A body `wf::own` accepts reaches no fault: no double drop (R1), no use after a drop (R2),
no drop of a view (R3), no use of a dangling view (OWN-20). The theorems take no
hypothesis besides `Accepted`.
-/

namespace Hirsel.CoreIR.Own

/-! ## Blocks -/

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

theorem mem_allActs_term {P : Prog} {b : Nat} {blk : Block} {pre : List Act}
    {edges : List Edge} {a : Act} (hb : P.blocks[b]? = some blk)
    (ht : blk.term = .other pre edges) (ha : a ∈ pre) : a ∈ P.allActs := by
  unfold Prog.allActs
  rw [List.mem_flatMap]
  refine ⟨blk, List.mem_of_getElem? hb, ?_⟩
  rw [List.mem_append]
  right
  rw [ht]
  exact ha

theorem mem_allEdges {P : Prog} {b : Nat} {blk : Block} {e : Edge}
    (hb : P.blocks[b]? = some blk) (he : e ∈ blk.term.edges) : e ∈ P.allEdges :=
  List.mem_flatMap.mpr ⟨blk, List.mem_of_getElem? hb, he⟩

theorem applyInsts_take_succ {P : Prog} {s : AState} {insts : List Inst} {pc : Nat} {i : Inst}
    (h : insts[pc]? = some i) :
    applyInsts P s (insts.take (pc + 1)) =
      applyActs P (applyInsts P s (insts.take pc)) i.acts := by
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

/-! ## What `validate` checks -/

theorem validate_wf {P : Prog} {start : Nat → Option AState} (h : validate P start = true) :
    P.wf = true := by
  unfold validate at h
  simp only [Bool.and_eq_true] at h
  exact h.1.1

theorem validate_entry {P : Prog} {start : Nat → Option AState} (h : validate P start = true) :
    ∃ s, start 0 = some s ∧ AState.init.le P s = true := by
  unfold validate at h
  simp only [Bool.and_eq_true] at h
  cases hs : start 0 with
  | none => simp [hs] at h
  | some s => exact ⟨s, rfl, by simpa [hs] using h.1.2⟩

/-- A reached block passes with its start state, and every state it sends is in its
target's start state: the entry block's too. -/
theorem validate_block {P : Prog} {start : Nat → Option AState} {b : Nat} {blk : Block}
    {s : AState} (h : validate P start = true) (hb : P.blocks[b]? = some blk)
    (hs : start b = some s) :
    blockOk P s blk = true ∧ ∀ t s', (t, s') ∈ blockOuts P s blk →
      ∃ s'', start t = some s'' ∧ s'.le P s'' = true := by
  unfold validate at h
  simp only [Bool.and_eq_true, List.all_eq_true, List.mem_range] at h
  obtain ⟨hlt, _⟩ := List.getElem?_eq_some_iff.mp hb
  have hblk := h.2 b hlt
  simp only [hb, hs, Bool.and_eq_true, List.all_eq_true] at hblk
  refine ⟨hblk.1, fun t s' hm => ?_⟩
  have hout := hblk.2 (t, s') hm
  cases hst : start t with
  | none => simp [hst] at hout
  | some s'' => exact ⟨s'', rfl, by simpa [hst] using hout⟩

/-! ## A branch on a flag read -/

/-- The edge a branch on a flag read takes carries a state that describes the store: a
read that is not stale saw the flag's value, so the edge taken is the one refined to it. -/
theorem branch_out {P : Prog} {σ : Store} {s : AState} {ri : Nat} {yes no : Edge}
    (hwf : P.wf = true) (hy : ∀ ws, ws ∈ yes.args → P.wfArg ws.1 ws.2 = true)
    (hn : ∀ ws, ws ∈ no.args → P.wfArg ws.1 ws.2 = true) (h : Abs P σ s) :
    ∃ s', ((if σ.read ri then yes else no).target, s') ∈
        termOuts P s (.flagBranch ri yes no) ∧
      Abs P (σ.enter (if σ.read ri then yes else no).args) s' := by
  cases hm : (if s.stale ri then none else P.reads[ri]?) with
  | none =>
    have ho : termOuts P s (.flagBranch ri yes no) =
        [(yes.target, transfer P s yes.args), (no.target, transfer P s no.args)] := by
      simp only [termOuts, hm]
    rw [ho]
    cases hr : σ.read ri with
    | true =>
      rw [ifPos rfl]
      exact ⟨_, List.mem_cons_self .., enter_sound hwf hy h⟩
    | false =>
      rw [ifNeg Bool.false_ne_true]
      exact ⟨_, List.mem_cons_of_mem _ (List.mem_cons_self ..), enter_sound hwf hn h⟩
  | some f =>
    have ho : termOuts P s (.flagBranch ri yes no) =
        [(yes.target, transfer P (refine P s f true) yes.args),
          (no.target, transfer P (refine P s f false) no.args)] := by
      simp only [termOuts, hm]
    rw [ho]
    cases hst : s.stale ri with
    | true =>
      rw [hst, ifPos rfl] at hm
      cases hm
    | false =>
      rw [hst, ifNeg Bool.false_ne_true] at hm
      have hlt : ri < P.reads.length := (List.getElem?_eq_some_iff.mp hm).1
      have hf : P.reads.getD ri 0 = f := Option.some.inj ((reads_get hlt).symm.trans hm)
      have hfr := h.fresh ri hlt hst
      rw [hf] at hfr
      cases hr : σ.read ri with
      | true =>
        rw [ifPos rfl]
        have ha := enter_sound hwf hy (refine_sound f h)
        rw [← hfr, hr] at ha
        exact ⟨_, List.mem_cons_self .., ha⟩
      | false =>
        rw [ifNeg Bool.false_ne_true]
        have ha := enter_sound hwf hn (refine_sound f h)
        rw [← hfr, hr] at ha
        exact ⟨_, List.mem_cons_of_mem _ (List.mem_cons_self ..), ha⟩

/-! ## The invariant -/

/-- The invariant of a run: at instruction `pc` of block `b`, the block's start state
carried through its first `pc` instructions describes the store. A fault never holds it. -/
def Inv (P : Prog) (start : Nat → Option AState) : Cfg → Prop
  | .run b pc σ => ∃ s, start b = some s ∧
      ∀ blk, P.blocks[b]? = some blk → Abs P σ (applyInsts P s (blk.insts.take pc))
  | .fault _ => False
  | .done _ => True

theorem inv_initial {P : Prog} {start : Nat → Option AState} {σ : Store}
    (hv : validate P start = true) (hi : Initial P σ) : Inv P start (.run 0 0 σ) := by
  obtain ⟨s, hs, hle⟩ := validate_entry hv
  exact ⟨s, hs, fun _ _ => (Abs.init hi).grow (AState.le.sub hle)⟩

/-- An edge the block sends out lands on a start state that describes the store. -/
theorem inv_arrive {P : Prog} {start : Nat → Option AState} {b t : Nat} {blk : Block}
    {s s' : AState} {σ : Store} (hv : validate P start = true)
    (hb : P.blocks[b]? = some blk) (hs : start b = some s)
    (hout : (t, s') ∈ blockOuts P s blk) (hA : Abs P σ s') : Inv P start (.run t 0 σ) := by
  obtain ⟨s'', hs'', hle⟩ := (validate_block hv hb hs).2 t s' hout
  exact ⟨s'', hs'', fun _ _ => hA.grow (AState.le.sub hle)⟩

/-- **Preservation.** A step from a configuration the accepted analysis describes reaches
one it describes; in particular it does not fault. -/
theorem preservation {P : Prog} {start : Nat → Option AState} (hv : validate P start = true)
    {c c' : Cfg} (hinv : Inv P start c) (hstep : Step P c c') : Inv P start c' := by
  have hwf := validate_wf hv
  have hargs : ∀ {b : Nat} {blk : Block} {e : Edge}, P.blocks[b]? = some blk →
      e ∈ blk.term.edges → ∀ ws, ws ∈ e.args → P.wfArg ws.1 ws.2 = true :=
    fun hb he => ((wf_iff P).mp hwf).2.2.2.2.2.1 _ (mem_allEdges hb he)
  cases hstep with
  | inst hb hi hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true] at hok
    refine ⟨s, hs, fun blk' hb' => ?_⟩
    have : blk' = _ := Option.some.inj (hb'.symm.trans hb)
    subst this
    rw [applyInsts_take_succ hi]
    exact acts_sound hwf (fun a ha => mem_allActs_inst hb hi ha) (hA _ hb)
      (instsOk_at _ _ _ _ hok.1 hi) hacts
  | instFault hb hi hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true] at hok
    exact acts_sound hwf (fun a ha => mem_allActs_inst hb hi ha) (hA _ hb)
      (instsOk_at _ _ _ _ hok.1 hi) hacts
  | fail hb hi hf hd =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hout := failOuts_at (P := P) _ s _ _ _ hi hf
    simp only [hd, Bool.false_eq_true, ite_false] at hout
    exact inv_arrive hv hb hs (List.mem_append_left _ hout) (hA _ hb)
  | dropFail hb hi hf hd hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true] at hok
    have hout := failOuts_at (P := P) _ s _ _ _ hi hf
    simp only [hd, ite_true] at hout
    exact inv_arrive hv hb hs (List.mem_append_left _ hout)
      (acts_sound hwf (fun a ha => mem_allActs_inst hb hi ha) (hA _ hb)
        (instsOk_at _ _ _ _ hok.1 hi) hacts)
  | @branch b pc σ blk ri yes no e hb hpc ht he =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hA := hA _ hb
    rw [hpc, List.take_length] at hA
    subst he
    obtain ⟨s', hout, hA'⟩ := branch_out (ri := ri)
      hwf (hargs hb (e := yes) (by rw [ht]; exact List.mem_cons_self ..))
      (hargs hb (e := no) (by rw [ht]; exact List.mem_cons_of_mem _ (List.mem_cons_self ..)))
      hA
    exact inv_arrive hv hb hs (List.mem_append_right _ (by rw [ht]; exact hout)) hA'
  | @goto b pc σ σ' blk pre edges e hb hpc ht hacts hmem =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hA := hA _ hb
    rw [hpc, List.take_length] at hA
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true, ht, termOk] at hok
    have hout : (e.target, transfer P (applyActs P (applyInsts P s blk.insts) pre) e.args) ∈
        blockOuts P s blk := by
      apply List.mem_append_right
      rw [ht]
      exact List.mem_map.mpr ⟨e, hmem, rfl⟩
    exact inv_arrive hv hb hs hout
      (enter_sound hwf (hargs hb (by rw [ht]; exact hmem))
        (acts_sound hwf (fun a ha => mem_allActs_term hb ht ha) hA hok.2 hacts))
  | exit => trivial
  | @termFault b pc σ blk pre edges k hb hpc ht hacts =>
    obtain ⟨s, hs, hA⟩ := hinv
    have hA := hA _ hb
    rw [hpc, List.take_length] at hA
    have hok := (validate_block hv hb hs).1
    simp only [blockOk, Bool.and_eq_true, ht, termOk] at hok
    exact acts_sound hwf (fun a ha => mem_allActs_term hb ht ha) hA hok.2 hacts

theorem inv_steps {P : Prog} {start : Nat → Option AState} (hv : validate P start = true)
    {c c' : Cfg} (hinv : Inv P start c) (hsteps : Steps P c c') : Inv P start c' := by
  induction hsteps with
  | refl => exact hinv
  | tail _ hstep ih => exact preservation hv ih hstep

/-- **Soundness.** A body `wf::own` accepts reaches no fault. -/
theorem sound {P : Prog} (hacc : Accepted P) (k : Fault) : ¬ Reachable P (.fault k) := by
  intro ⟨σ, hi, hsteps⟩
  obtain ⟨start, hv⟩ := hacc
  exact inv_steps hv (inv_initial hv hi) hsteps

/-- **R1.** No path of an accepted body drops a place overlapping one dropped already. -/
theorem no_double_drop {P : Prog} (hacc : Accepted P) :
    ¬ Reachable P (.fault .doubleDrop) := sound hacc _

/-- **R2.** No path of an accepted body uses a place overlapping a dropped one. -/
theorem no_use_after_drop {P : Prog} (hacc : Accepted P) :
    ¬ Reachable P (.fault .useAfterDrop) := sound hacc _

/-- **R3.** No path of an accepted body drops a `view` parameter's place. -/
theorem no_view_drop {P : Prog} (hacc : Accepted P) :
    ¬ Reachable P (.fault .viewDrop) := sound hacc _

/-- **OWN-20.** No path of an accepted body uses or drops a dest view after its anchor was
dropped or rewritten while the borrow was in force. -/
theorem no_dangling_view {P : Prog} (hacc : Accepted P) :
    ¬ Reachable P (.fault .danglingView) := sound hacc _

end Hirsel.CoreIR.Own
