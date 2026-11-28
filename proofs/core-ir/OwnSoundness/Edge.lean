import OwnSoundness.Addr

/-!
# Edges

`enter_sound`: `transfer`'s state describes the store after an edge binds the target's
pointer parameters. `refine_sound`: so does the state an edge of a branch on a fresh flag
read keeps.
-/

namespace Hirsel.CoreIR.Own

/-! ## What the tables promise of an edge argument -/

theorem wfArg_names {P : Prog} {w : Nat} {src : Src} {x : Pl} (h : P.wfArg w src = true)
    (hx : x ∈ P.srcNames src) : x ∈ P.under w := by
  simp only [Prog.wfArg, Bool.and_eq_true, List.all_eq_true, List.contains_iff_mem] at h
  exact h.1.1 x hx

theorem wfArg_join {P : Prog} {w u i : Nat} {t : Pl} (h : P.wfArg w (.param u) = true)
    (hj : P.joinOf u = some i) (ht : t ∈ P.tracked) (hp : P.paramOf t = some w) :
    (⟨P.roots + i, t.steps, t.cut⟩ : Pl) ∈ P.tracked := by
  simp only [Prog.wfArg, Bool.and_eq_true] at h
  have h2 := h.1.2
  simp only [hj, List.all_eq_true, Bool.or_eq_true, bne_iff_ne, ne_eq,
    List.contains_iff_mem] at h2
  rcases h2 t ht with h3 | h3
  · exact absurd hp h3
  · exact h3

theorem wfArg_named {P : Prog} {w : Nat} {src : Src} (h : P.wfArg w src = true)
    (hj : P.joinOf w = none) (ho : (one (P.under w)).isSome = true) : P.named src = true := by
  simp only [Prog.wfArg, Bool.and_eq_true, Bool.or_eq_true, Bool.not_eq_true'] at h
  rcases h.2 with h3 | h3
  · simp [hj, ho] at h3
  · exact h3

theorem named_param {P : Prog} {u : Nat}
    (h : ((P.joinOf u).isNone && !(P.under u).isEmpty) = true) :
    P.joinOf u = none ∧ P.under u ≠ [] := by
  rw [Bool.and_eq_true] at h
  refine ⟨?_, fun he => ?_⟩
  · cases hj : P.joinOf u with
    | none => rfl
    | some k =>
      have h1 := h.1
      rw [hj] at h1
      cases h1
  · have h2 := h.2
    rw [he] at h2
    cases h2

theorem overlap_congr {a a' b : Pl} (hr : a'.root = a.root) (hs : a'.steps = a.steps)
    (h : overlap a b = true) : overlap a' b = true := by
  unfold overlap at *
  rw [hr, hs]
  exact h

theorem app_inj {z x : Pl} {rel : Path} (hz : z.cut = false) (hx : x.cut = false)
    (he : z.app rel = x.app rel) : z = x := by
  cases z with
  | mk zr zs zc =>
    cases x with
    | mk xr xs xc =>
      simp only [Pl.app, Pl.mk.injEq] at he
      obtain ⟨h1, h2, _⟩ := he
      have h2' := List.append_cancel_right h2
      simp only at hz hx
      subst h1 h2' hz hx
      rfl

/-! ## Where an argument points -/

/-- Where an argument points, it names, and not under a run-time offset. -/
theorem eval_names {P : Prog} {σ : Store} {s : AState} {src : Src} {y : Pl} (h : Abs P σ s)
    (he : src.eval σ = some y) : y ∈ P.srcNames src ∧ y.cut = false := by
  cases src with
  | param u => exact h.binds u y he
  | ptr u rel =>
    have he' : (if rel.cut = true then Option.none else (σ.bind u).map fun x => x.app rel) =
        some y := he
    cases hrel : rel.cut with
    | true =>
      rw [ifPos hrel] at he'
      cases he'
    | false =>
      rw [ifNeg (by rw [hrel]; exact Bool.false_ne_true)] at he'
      cases hbu : σ.bind u with
      | none =>
        rw [hbu] at he'
        cases he'
      | some x =>
        rw [hbu] at he'
        obtain rfl : x.app rel = y := Option.some.inj he'
        obtain ⟨hx, hcx⟩ := h.binds u x hbu
        refine ⟨List.mem_map.mpr ⟨x, hx, ?_⟩, ?_⟩
        · show (if x.cut = true then x else x.app rel) = x.app rel
          rw [ifNeg (by rw [hcx]; exact Bool.false_ne_true)]
        · show (x.cut || rel.cut) = false
          rw [hcx, hrel]
          rfl
  | «at» x =>
    have he' : (if x.cut = true then Option.none else some x) = some y := he
    cases hc : x.cut with
    | true =>
      rw [ifPos hc] at he'
      cases he'
    | false =>
      rw [ifNeg (by rw [hc]; exact Bool.false_ne_true)] at he'
      obtain rfl : x = y := Option.some.inj he'
      exact ⟨List.mem_singleton.mpr rfl, hc⟩
  | none => cases he

/-- The join rule: a parameter that names one place and is no join is given that place. -/
theorem eval_plain {P : Prog} {σ : Store} {s : AState} {w : Nat} {src : Src} {x : Pl}
    (hwa : P.wfArg w src = true) (h : Abs P σ s) (hj : P.joinOf w = none)
    (ho : one (P.under w) = some x) : src.eval σ = some x := by
  have hnamed := wfArg_named hwa hj (by rw [ho]; rfl)
  obtain ⟨_, hxc, hall⟩ := one_some.mp ho
  have hsub : ∀ z, z ∈ P.srcNames src → z = x := fun z hz => hall z (wfArg_names hwa hz)
  cases src with
  | param u =>
    obtain ⟨hju, hne⟩ := named_param hnamed
    exact h.plain u x hju (one_some.mpr ⟨hne, hxc, hsub⟩)
  | ptr u rel =>
    obtain ⟨hju, hne⟩ := named_param hnamed
    obtain ⟨x1, hx1⟩ := List.exists_mem_of_ne_nil _ hne
    have key : ∀ z, z ∈ P.under u → z.cut = false ∧ z.app rel = x := fun z hz => by
      have hs : (if z.cut = true then z else z.app rel) = x :=
        hsub (z.step rel) (List.mem_map.mpr ⟨z, hz, rfl⟩)
      cases hzc : z.cut with
      | true =>
        rw [ifPos hzc] at hs
        rw [hs, hxc] at hzc
        cases hzc
      | false =>
        rw [ifNeg (by rw [hzc]; exact Bool.false_ne_true)] at hs
        exact ⟨rfl, hs⟩
    obtain ⟨hx1c, hx1a⟩ := key x1 hx1
    have hrel : rel.cut = false := by
      have hc : (x1.cut || rel.cut) = false := by
        rw [← hxc, ← hx1a]
        rfl
      rw [hx1c] at hc
      exact hc
    have hbu : σ.bind u = some x1 :=
      h.plain u x1 hju (one_some.mpr ⟨hne, hx1c, fun z hz =>
        app_inj (key z hz).1 hx1c ((key z hz).2.trans hx1a.symm)⟩)
    show (if rel.cut = true then Option.none else (σ.bind u).map fun x => x.app rel) = some x
    rw [ifNeg (by rw [hrel]; exact Bool.false_ne_true), hbu]
    exact congrArg some hx1a
  | «at» x' =>
    obtain rfl : x' = x := hsub x' (List.mem_singleton.mpr rfl)
    show (if x'.cut = true then Option.none else some x') = some x'
    rw [ifNeg (by rw [hxc]; exact Bool.false_ne_true)]
  | none => cases hnamed

theorem enter_bind_some {σ : Store} {args : List (Nat × Src)} {w : Nat} {src : Src}
    (h : args.lookup w = some src) : (σ.enter args).bind w = src.eval σ := by
  simp only [Store.enter, h]

theorem enter_bind_none {σ : Store} {args : List (Nat × Src)} {w : Nat}
    (h : args.lookup w = none) : (σ.enter args).bind w = σ.bind w := by
  simp only [Store.enter, h]

/-! ## `transfer`, cell by cell -/

theorem paramOf_low {P : Prog} {t : Pl} (h : t.root < P.roots) : P.paramOf t = none := by
  simp [Prog.paramOf, h]

theorem argOf_some {P : Prog} {args : List (Nat × Src)} {t : Pl} {v : Nat}
    (hp : P.paramOf t = some v) : P.argOf args t = args.lookup v := by
  simp [Prog.argOf, hp]

theorem argOf_low {P : Prog} {args : List (Nat × Src)} {t : Pl} (h : t.root < P.roots) :
    P.argOf args t = none := by
  simp [Prog.argOf, paramOf_low h]

theorem transfer_none {P : Prog} {s : AState} {args : List (Nat × Src)} {x : Nat}
    (hc : P.cells[x]? = none) : (transfer P s args).bits x = s.bits x := by
  simp only [transfer, hc]

theorem transfer_pair_none {P : Prog} {s : AState} {args : List (Nat × Src)} {x : Nat}
    {t : Pl} {g : Option Nat} (hc : P.cells[x]? = some (.pair t g))
    (ha : P.argOf args t = none) : (transfer P s args).bits x = s.bits x := by
  simp only [transfer, hc, ha]

theorem transfer_pair_some {P : Prog} {s : AState} {args : List (Nat × Src)} {x : Nat}
    {t : Pl} {g : Option Nat} {src : Src} (hc : P.cells[x]? = some (.pair t g))
    (ha : P.argOf args t = some src) : (transfer P s args).bits x = joinBits P s src t := by
  simp only [transfer, hc, ha]

theorem transfer_bind_none {P : Prog} {s : AState} {args : List (Nat × Src)} {x p : Nat}
    {q t : Pl} {g : Option Nat} (hc : P.cells[x]? = some (.bind q t p g))
    (ha : P.argOf args t = none) : (transfer P s args).bits x = s.bits x := by
  simp only [transfer, hc, ha]

theorem transfer_bind_some {P : Prog} {s : AState} {args : List (Nat × Src)} {x p : Nat}
    {q t : Pl} {g : Option Nat} {src : Src} (hc : P.cells[x]? = some (.bind q t p g))
    (ha : P.argOf args t = some src) :
    (transfer P s args).bits x = bindBits P s src q t p x := by
  simp only [transfer, hc, ha]

/-- A place no argument is given keeps its dropped state. -/
theorem transfer_dcur {P : Prog} {σ : Store} {s : AState} {args : List (Nat × Src)} {q : Pl}
    (hd : DCur P σ s q) (ha : P.argOf args q = none) :
    DCur P (σ.enter args) (transfer P s args) q := by
  intro x b ho hc
  obtain ⟨g, hcell⟩ := owner_iff.mp ho
  rw [transfer_pair_none hcell ha]
  exact hd x b ho hc

/-! ## A join's state -/

theorem joinBits_half (P : Prog) (s : AState) (src : Src) (t : Pl) (b : Bool) :
    (joinBits P s src t).half b = true := by
  unfold joinBits
  simp only []
  split
  · cases b <;> simp [Bits.half]
  · exact Bits.Z_half b

theorem joinBits_d {P : Prog} {s : AState} {src : Src} {t : Pl} (b : Bool)
    (h : ((P.srcs src).any fun x =>
      (P.tracked.filter fun q => overlap (x.app t.path) q).any fun q => mayDrop P s q) = true) :
    (joinBits P s src t).d b = true := by
  have hj : joinBits P s src t =
      ⟨(P.srcs src).any fun x => (P.tracked.filter fun q => overlap (x.app t.path) q).any
          fun q => (P.pairsOf q).any fun p => (s.bits p).anyZ,
        (P.srcs src).any fun x => (P.tracked.filter fun q => overlap (x.app t.path) q).any
          fun q => (P.pairsOf q).any fun p => (s.bits p).anyZ, true, true⟩ := by
    unfold joinBits
    exact ifPos h
  rw [hj]
  cases b <;> rfl

/-- A place a join now reaches that is not live: the join's state is dropped. -/
theorem join_any {P : Prog} {σ : Store} {s : AState} {src : Src} {t p y : Pl} {v : Nat}
    (hwa : P.wfArg v src = true) (h : Abs P σ s) (hpar : P.paramOf t = some v)
    (he : src.eval σ = some y) (ht : t ∈ P.tracked) (hin : inside (y.app t.path) p = true)
    (hp : σ.st p ≠ .live) :
    ((P.srcs src).any fun x =>
      (P.tracked.filter fun q => overlap (x.app t.path) q).any fun q => mayDrop P s q) =
      true := by
  obtain ⟨hyn, _⟩ := eval_names h he
  have hgen : y ∈ P.srcs src →
      ((P.srcs src).any fun x =>
        (P.tracked.filter fun q => overlap (x.app t.path) q).any fun q => mayDrop P s q) =
        true := fun hm => by
    obtain ⟨_, q, hq, hqin, hdc⟩ := h.dead p hp
    exact List.any_eq_true.mpr ⟨y, hm, List.any_eq_true.mpr
      ⟨q, List.mem_filter.mpr ⟨hq, overlap_of_inside hin hqin⟩, hdc.may⟩⟩
  cases src with
  | param u =>
    cases hj : P.joinOf u with
    | none => exact hgen (by simp only [Prog.srcs, hj]; exact hyn)
    | some i =>
      have hsrcs : P.srcs (.param u) = [P.jroot i] := by simp only [Prog.srcs, hj]
      rw [hsrcs]
      have htu := wfArg_join hwa hj ht hpar
      obtain ⟨t'', ht'', hr'', hin'', hdc⟩ :=
        h.join ⟨P.roots + i, t.steps, t.cut⟩ u y p (paramOf_jroot hj _ rfl) he htu hin hp
      exact List.any_eq_true.mpr ⟨P.jroot i, List.mem_singleton.mpr rfl, List.any_eq_true.mpr
        ⟨t'', List.mem_filter.mpr ⟨ht'', overlap_iff.mpr ⟨hr''.symm, app_total hin hin''⟩⟩,
          hdc.may⟩⟩
  | ptr u rel => exact hgen hyn
  | «at» x' => exact hgen hyn
  | none => exact hgen hyn

/-! ## A bind state -/

theorem Bits.isZero_of_d {v : Bits} {b : Bool} (h : v.d b = true) : v.isZero = false := by
  cases v with
  | mk zt zf dt df =>
    cases b
    · have h' : df = true := h
      subst h'
      simp [Bits.isZero]
    · have h' : dt = true := h
      subst h'
      simp [Bits.isZero]

theorem foldl_or_half {α : Type} (f : α → Bits) (b : Bool) : ∀ (l : List α) (v0 : Bits),
    (l.foldl (fun v e => v.or (f e)) v0).half b = (v0.half b || l.any fun e => (f e).half b)
  | [], v0 => by simp
  | e :: l, v0 => by
    simp only [List.foldl_cons, List.any_cons]
    rw [foldl_or_half f b l, Bits.or_half, Bool.or_assoc]

theorem foldl_or_d {α : Type} (f : α → Bits) (b : Bool) : ∀ (l : List α) (v0 : Bits),
    (l.foldl (fun v e => v.or (f e)) v0).d b = (v0.d b || l.any fun e => (f e).d b)
  | [], v0 => by simp
  | e :: l, v0 => by
    simp only [List.foldl_cons, List.any_cons]
    rw [foldl_or_d f b l, Bits.or_d, Bool.or_assoc]

theorem bindCell_some {P : Prog} {q t : Pl} {p z : Nat} (h : P.bindCell q t p = some z) :
    ∃ g, P.cells[z]? = some (.bind q t p g) := by
  have hp := List.find?_some h
  cases hc : P.cells[z]? with
  | none =>
    rw [hc] at hp
    cases hp
  | some c =>
    rw [hc] at hp
    cases c with
    | pair q' g' => cases hp
    | bind q' t' p' g' =>
      simp only [Bool.and_eq_true, beq_iff_eq] at hp
      obtain ⟨⟨rfl, rfl⟩, rfl⟩ := hp
      exact ⟨g', rfl⟩

/-- A bind state's flag is its pairing's. -/
theorem guard_bind_eq {P : Prog} {z p : Nat} {q t : Pl} {g : Option Nat} (hwf : P.wf = true)
    (hz : P.cells[z]? = some (.bind q t p g)) : P.guard z = P.guard p := by
  obtain ⟨_, hbc, _, _⟩ := (wfCells_iff P).mp ((wf_iff P).mp hwf).2.2.2.2.2.2
  rw [guard_bind hz, (hbc q t p g (List.mem_of_getElem? hz)).2]

theorem bindOne_cases (P : Prog) (s : AState) (q : Pl) (p x : Nat) (y : Pl) :
    bindOne P s q p x y = (s.bits x).drops ∨ bindOne P s q p x y = (s.bits x).begun ∨
      ∃ z, P.bindCell q y p = some z ∧ bindOne P s q p x y = s.bits z := by
  unfold bindOne
  simp only []
  split
  · split
    · exact Or.inl rfl
    · exact Or.inr (Or.inl rfl)
  · split
    · rename_i z hz
      exact Or.inr (Or.inr ⟨z, hz, rfl⟩)
    · split
      · exact Or.inl rfl
      · exact Or.inr (Or.inl rfl)

theorem bindOne_low {P : Prog} {s : AState} {q y : Pl} {p x : Nat} (hr : y.root < P.roots)
    (ho : overlap y q = true) : bindOne P s q p x y = (s.bits x).drops := by
  unfold bindOne
  simp only []
  rw [ifPos hr, ifPos ho]

theorem bindOne_cell {P : Prog} {s : AState} {q y : Pl} {p x z : Nat}
    (hr : ¬ y.root < P.roots) (hz : P.bindCell q y p = some z) :
    bindOne P s q p x y = s.bits z := by
  unfold bindOne
  simp only [hz]
  rw [ifNeg hr]

theorem bindOne_expand {P : Prog} {s : AState} {q y : Pl} {p x : Nat}
    (hr : ¬ y.root < P.roots) (hz : P.bindCell q y p = none)
    (he : ((P.expand y).any fun u => overlap u q) = true) :
    bindOne P s q p x y = (s.bits x).drops := by
  unfold bindOne
  simp only [hz]
  rw [ifNeg hr, ifPos he]

theorem bindOne_half {P : Prog} {σ : Store} {s : AState} {q t : Pl} {p x : Nat}
    {g : Option Nat} {b : Bool} (hwf : P.wf = true) (h : Abs P σ s)
    (hcell : P.cells[x]? = some (.bind q t p g)) (hc : Cur P σ x b) (y : Pl) :
    (bindOne P s q p x y).half b = true := by
  have hh := h.half x b (List.getElem?_eq_some_iff.mp hcell).1 hc
  rcases bindOne_cases P s q p x y with he | he | ⟨z, hz, he⟩
  · rw [he, Bits.drops_half]
    exact hh
  · rw [he, Bits.begun_half]
    exact hh
  · rw [he]
    obtain ⟨g', hz'⟩ := bindCell_some hz
    exact h.half z b (List.getElem?_eq_some_iff.mp hz').1 fun f hf =>
      hc f (by rw [guard_bind_eq hwf hcell, ← guard_bind_eq hwf hz']; exact hf)

theorem bindBits_half {P : Prog} {σ : Store} {s : AState} {src : Src} {q t : Pl} {p x : Nat}
    {g : Option Nat} {b : Bool} (hwf : P.wf = true) (h : Abs P σ s)
    (hcell : P.cells[x]? = some (.bind q t p g)) (hc : Cur P σ x b) :
    (bindBits P s src q t p x).half b = true := by
  have hh := h.half x b (List.getElem?_eq_some_iff.mp hcell).1 hc
  have hone : ∀ y, (bindOne P s q p x y).half b = true := bindOne_half hwf h hcell hc
  unfold bindBits
  simp only []
  split
  · rw [Bits.begun_half]
    exact hh
  · rename_i hz
    rw [foldl_or_half]
    cases hs : P.srcs src with
    | nil =>
      rw [hs] at hz
      exact absurd rfl hz
    | cons e l => simp [hone]

/-- Where a join is bound into `q`, its bind state for `q` holds `D`. -/
theorem bindBits_d {P : Prog} {σ : Store} {s : AState} {src : Src} {q t y : Pl}
    {p x v : Nat} {g : Option Nat} {b : Bool} (hwf : P.wf = true)
    (hwa : P.wfArg v src = true) (h : Abs P σ s)
    (hcell : P.cells[x]? = some (.bind q t p g)) (hpar : P.paramOf t = some v)
    (he : src.eval σ = some y) (ho : overlap (y.app t.path) q = true) (hc : Cur P σ x b) :
    (bindBits P s src q t p x).d b = true := by
  have hh := h.half x b (List.getElem?_eq_some_iff.mp hcell).1 hc
  have hany : ((P.srcs src).any fun x0 => (bindOne P s q p x (x0.app t.path)).d b) = true := by
    rw [List.any_eq_true]
    obtain ⟨hyn, _⟩ := eval_names h he
    have hyr : y.root < P.roots :=
      ((wf_iff P).mp hwf).2.1 v (paramOf_mem hpar) y (wfArg_names hwa hyn)
    have hgen : y ∈ P.srcs src →
        ∃ x0, x0 ∈ P.srcs src ∧ (bindOne P s q p x (x0.app t.path)).d b = true := fun hm =>
      ⟨y, hm, by rw [bindOne_low (by exact hyr) ho, Bits.drops_d]; exact hh⟩
    cases src with
    | param u =>
      cases hj : P.joinOf u with
      | none => exact hgen (by simp only [Prog.srcs, hj]; exact hyn)
      | some i =>
        have hsrcs : P.srcs (.param u) = [P.jroot i] := by simp only [Prog.srcs, hj]
        rw [hsrcs]
        refine ⟨P.jroot i, List.mem_singleton.mpr rfl, ?_⟩
        have hbu : σ.bind u = some y := he
        have hroot : ¬ ((P.jroot i).app t.path).root < P.roots := by
          show ¬ P.roots + i < P.roots
          omega
        have hpu : P.paramOf ((P.jroot i).app t.path) = some u := paramOf_jroot hj _ rfl
        have hov : overlap (y.app ((P.jroot i).app t.path).path) q = true :=
          overlap_congr rfl rfl ho
        cases hz : P.bindCell q ((P.jroot i).app t.path) p with
        | some z =>
          rw [bindOne_cell hroot hz]
          obtain ⟨g', hz'⟩ := bindCell_some hz
          exact h.bound z q _ p g' u y hz' hpu hbu hov b fun f hf =>
            hc f (by rw [guard_bind_eq hwf hcell, ← guard_bind_eq hwf hz']; exact hf)
        | none =>
          rw [bindOne_expand hroot hz (List.any_eq_true.mpr
            ⟨_, mem_expand.mpr ⟨u, hpu, y, (h.binds u y hbu).1, rfl⟩, hov⟩), Bits.drops_d]
          exact hh
    | ptr u rel => exact hgen hyn
    | «at» x' => exact hgen hyn
    | none => exact hgen hyn
  have hv : ((P.srcs src).foldl (fun v x0 => v.or (bindOne P s q p x (x0.app t.path)))
      Bits.zero).d b = true := by
    rw [foldl_or_d, hany]
    exact Bool.or_true _
  unfold bindBits
  simp only []
  rw [ifNeg (by rw [Bits.isZero_of_d hv]; exact Bool.false_ne_true)]
  exact hv

theorem transfer_half {P : Prog} {σ : Store} {s : AState} {args : List (Nat × Src)} {x : Nat}
    {b : Bool} (hwf : P.wf = true) (h : Abs P σ s) (hx : x < P.cells.length)
    (hc : Cur P σ x b) : ((transfer P s args).bits x).half b = true := by
  have hh := h.half x b hx hc
  cases hcell : P.cells[x]? with
  | none =>
    rw [transfer_none hcell]
    exact hh
  | some c =>
    cases c with
    | pair t g =>
      cases ha : P.argOf args t with
      | none =>
        rw [transfer_pair_none hcell ha]
        exact hh
      | some src =>
        rw [transfer_pair_some hcell ha]
        exact joinBits_half ..
    | bind q t p g =>
      cases ha : P.argOf args t with
      | none =>
        rw [transfer_bind_none hcell ha]
        exact hh
      | some src =>
        rw [transfer_bind_some hcell ha]
        exact bindBits_half hwf h hcell hc

/-! ## The edge -/

/-- An edge taken from a store the state describes: `transfer`'s state describes the
store with the target's parameters bound. -/
theorem enter_sound {P : Prog} {σ : Store} {s : AState} {args : List (Nat × Src)}
    (hwf : P.wf = true) (hargs : ∀ ws, ws ∈ args → P.wfArg ws.1 ws.2 = true)
    (h : Abs P σ s) : Abs P (σ.enter args) (transfer P s args) where
  half := fun _ _ hx hc => transfer_half hwf h hx hc
  dead := fun p hp => by
    obtain ⟨hr, q, hq, hin, hd⟩ := h.dead p hp
    exact ⟨hr, q, hq, hin,
      transfer_dcur hd (argOf_low (by rw [(inside_iff.mp hin).1]; exact hr))⟩
  loan := fun k hk hl => h.loan k hk hl
  bound := fun x q t p g v y hcell hpar hb ho b hc => by
    cases hl : args.lookup v with
    | none =>
      rw [enter_bind_none hl] at hb
      rw [transfer_bind_none hcell ((argOf_some hpar).trans hl)]
      exact h.bound x q t p g v y hcell hpar hb ho b hc
    | some src =>
      rw [enter_bind_some hl] at hb
      rw [transfer_bind_some hcell ((argOf_some hpar).trans hl)]
      exact bindBits_d hwf (hargs _ (mem_of_lookup hl)) h hcell hpar hb ho hc
  join := fun t v y p hpar hb ht hin hp => by
    cases hl : args.lookup v with
    | none =>
      rw [enter_bind_none hl] at hb
      obtain ⟨t', ht', hr, hin', hd⟩ := h.join t v y p hpar hb ht hin hp
      exact ⟨t', ht', hr, hin',
        transfer_dcur hd ((argOf_some ((paramOf_root hr).trans hpar)).trans hl)⟩
    | some src =>
      rw [enter_bind_some hl] at hb
      refine ⟨t, ht, rfl, hin, fun x b ho _ => ?_⟩
      obtain ⟨g, hcell⟩ := owner_iff.mp ho
      rw [transfer_pair_some hcell ((argOf_some hpar).trans hl)]
      exact joinBits_d b (join_any (hargs _ (mem_of_lookup hl)) h hpar hb ht hin hp)
  fresh := fun ri hr hst => h.fresh ri hr hst
  binds := fun v y hb => by
    cases hl : args.lookup v with
    | none =>
      rw [enter_bind_none hl] at hb
      exact h.binds v y hb
    | some src =>
      rw [enter_bind_some hl] at hb
      obtain ⟨hyn, hc⟩ := eval_names h hb
      exact ⟨wfArg_names (hargs _ (mem_of_lookup hl)) hyn, hc⟩
  plain := fun w x hj ho => by
    cases hl : args.lookup w with
    | none =>
      rw [enter_bind_none hl]
      exact h.plain w x hj ho
    | some src =>
      rw [enter_bind_some hl]
      exact eval_plain (hargs _ (mem_of_lookup hl)) h hj ho

/-- The edge of a branch on a flag keeps the pairs of the flag's value. -/
theorem refine_sound {P : Prog} {σ : Store} {s : AState} (f : Nat) (h : Abs P σ s) :
    Abs P σ (refine P s f (σ.flag f)) := by
  refine Abs.shift h (fun x b _ hc => ⟨b, hc, ?_, ?_⟩) rfl rfl (fun k hk hl => h.loan k hk hl)
    (fun ri hr hst => h.fresh ri hr hst)
  · by_cases hg : (P.guard x == some f) = true
    · have hb : b = σ.flag f := hc f (beq_iff_eq.mp hg)
      simp only [refine]
      rw [ifPos hg, ← hb, Bits.keep_half]
      exact id
    · simp only [refine]
      rw [ifNeg hg]
      exact id
  · by_cases hg : (P.guard x == some f) = true
    · have hb : b = σ.flag f := hc f (beq_iff_eq.mp hg)
      simp only [refine]
      rw [ifPos hg, ← hb, Bits.keep_d]
      exact id
    · simp only [refine]
      rw [ifNeg hg]
      exact id

end Hirsel.CoreIR.Own
