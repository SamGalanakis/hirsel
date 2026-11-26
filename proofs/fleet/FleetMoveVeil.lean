module

public import Veil

/-!
# FleetMoveVeil: All-N Inductive Fencing Proof for Hirsel Fleet Move Authorization

Proves the Fleet per-Place move-authorization fencing invariants (docs/spec/fleet.md FLT-2,
rules O1-O4 and Rule F1) inductively for **arbitrary cluster size N** (`type node`),
**unbounded epochs** (`type epoch` with `TotalOrder` and successor relation `succ`),
**unbounded holders** (`type holder`), and **unbounded attempts** (`type attempt`),
cross-checked against the bounded TLC model in `FleetMove.tla`.

Assumptions explicitly encoded:
- Per-key linearizable CAS on the objects@1 head (`ClaimAuth.expect` checked atomically before write).
- Fresh non-repeating holder nonces per physical open (`¬ usedHolder h`).
- Monotone non-wrapping epochs (`TotalOrder epoch` + strict successor `succ`).
- Atomic durable state + outbox persistence before permit release (K11).
- Lawful credential holders (enforced at L1-L3 bucket boundaries, FLT-6).
-/

veil module FleetMoveVeil

type node
type epoch
type holder
type attempt

instantiate epOrd : TotalOrder epoch
open TotalOrder

enum ClaimClass = { ccAdopt, ccPermit, ccContinue }

immutable individual ep0 : epoch
immutable individual h0 : holder
immutable individual noAttempt : attempt
immutable relation succ : epoch → epoch → Bool

individual headEpoch : epoch
individual headHolder : holder
individual originEpoch : epoch
individual originAttempt : attempt
individual originClass : ClaimClass
individual permitEpoch : epoch
individual permitAttempt : attempt
individual incumbent : node

relation usedHolder : holder → Bool
relation usedAttempt : attempt → Bool
relation epochWinner : epoch → holder → Bool
relation authorizedPermit : attempt → epoch → holder → node → Bool
relation openPermit : attempt → epoch → holder → node → Bool
relation inflight : attempt → epoch → holder → ClaimClass → holder → node → Bool
relation consumedPred : epoch → epoch → Bool

#gen_state

-- Genesis epoch is minimal; succ is a strict, functional, injective successor with no intervening epoch.
assumption [ep0_min] ∀ e : epoch, le ep0 e
assumption [succ_strict] ∀ e1 e2 : epoch, succ e1 e2 → le e1 e2 ∧ e1 ≠ e2
assumption [succ_func] ∀ e1 e2 e3 : epoch, succ e1 e2 ∧ succ e1 e3 → e2 = e3
assumption [succ_inj] ∀ e1 e2 e3 : epoch, succ e1 e3 ∧ succ e2 e3 → e1 = e2
assumption [succ_tight] ∀ e1 e2 e3 : epoch, succ e1 e2 ∧ le e1 e3 ∧ e3 ≠ e1 → le e2 e3

after_init {
  headEpoch := ep0
  headHolder := h0
  originEpoch := ep0
  originAttempt := noAttempt
  originClass := ccAdopt
  permitEpoch := ep0
  permitAttempt := noAttempt
  usedHolder H := decide (H = h0)
  usedAttempt A := decide (A = noAttempt)
  epochWinner E H := decide (E = ep0 ∧ H = h0)
  authorizedPermit A E H N := false
  openPermit A E H N := false
  inflight A E H C H2 N := false
  consumedPred E1 E2 := false
}

-- Rule F1 primary move authorization when no permits are open.
action authPrimary (target : node) (att : attempt) (h : holder) {
  require target ≠ incumbent
  require att ≠ noAttempt ∧ ¬ usedAttempt att
  require ¬ usedHolder h
  require ∀ a e hh n, ¬ openPermit a e hh n
  usedAttempt att := true
  usedHolder h := true
  openPermit att headEpoch headHolder target := true
  authorizedPermit att headEpoch headHolder target := true
  inflight att headEpoch headHolder ccPermit h target := true
}

-- Rule F1 recovery contender sharing the exact predecessor pair (pExpE, pExpH).
action authRecovery (target : node) (pAtt : attempt) (pExpE : epoch) (pExpH : holder) (pTarget : node) (att : attempt) (h : holder) {
  require openPermit pAtt pExpE pExpH pTarget
  require target ≠ incumbent
  require target ≠ pTarget
  require att ≠ noAttempt ∧ ¬ usedAttempt att
  require ¬ usedHolder h
  usedAttempt att := true
  usedHolder h := true
  openPermit att pExpE pExpH target := true
  authorizedPermit att pExpE pExpH target := true
  inflight att pExpE pExpH ccPermit h target := true
}

-- Same-host Continue open (local restart or attached-stream janitor) expecting current head.
action spawnContinue (att : attempt) (h : holder) {
  require att ≠ noAttempt ∧ ¬ usedAttempt att
  require ¬ usedHolder h
  usedAttempt att := true
  usedHolder h := true
  inflight att headEpoch headHolder ccContinue h incumbent := true
}

-- Concurrent head CAS succeeds when ClaimAuth.expect matches (headEpoch, headHolder).
-- Atomically installs (newEp, h) and writes ORIGIN per O1 and O3.
action execCasWon (att : attempt) (expE : epoch) (expH : holder) (cls : ClaimClass) (h : holder) (target : node) (newEp : epoch) {
  require inflight att expE expH cls h target
  require expE = headEpoch ∧ expH = headHolder
  require succ headEpoch newEp
  inflight att expE expH cls h target := false
  headEpoch := newEp
  headHolder := h
  originEpoch := newEp
  originAttempt := att
  originClass := cls
  if cls = ccPermit then
    permitEpoch := newEp
    permitAttempt := att
  epochWinner newEp h := true
  consumedPred expE newEp := true
}

-- Concurrent head CAS refused (Superseded) before any write when expect mismatches head.
action execCasSuperseded (att : attempt) (expE : epoch) (expH : holder) (cls : ClaimClass) (h : holder) (target : node) {
  require inflight att expE expH cls h target
  require expE ≠ headEpoch ∨ expH ≠ headHolder
  inflight att expE expH cls h target := false
}

-- Read-only HeadView settlement (Won): closes open permits and updates incumbent without writing head.
action readSettleWon (pAtt : attempt) (pExpE : epoch) (pExpH : holder) (pTarget : node) {
  require openPermit pAtt pExpE pExpH pTarget
  require le pExpE headEpoch ∧ pExpE ≠ headEpoch
  require permitAttempt = pAtt ∧ le pExpE permitEpoch ∧ pExpE ≠ permitEpoch
  incumbent := pTarget
  openPermit A E H N := false
}

-- Read-only HeadView settlement (Dead): closes open permits when another claim took the predecessor.
action readSettleDead {
  require ∃ a e h n, openPermit a e h n
  require ∀ a e h n, openPermit a e h n → (le e headEpoch ∧ e ≠ headEpoch) ∧ ¬(permitAttempt = a ∧ le e permitEpoch ∧ e ≠ permitEpoch)
  openPermit A E H N := false
}

-- ---- Safety Properties (all N, unbounded epochs) ----

-- Rule O4: ORIGIN.epoch always equals HEAD.epoch.
safety [OriginEpochMatchesHead]
  originEpoch = headEpoch

-- Core fencing safety: at most one holder is ever installed at any epoch.
safety [UniqueHolderPerEpoch]
  ∀ e h1 h2, epochWinner e h1 ∧ epochWinner e h2 → h1 = h2

-- Fresh random holder discipline: no holder nonce is ever installed at two distinct epochs.
safety [FreshHoldersNeverRepeat]
  ∀ e1 e2 h, epochWinner e1 h ∧ epochWinner e2 h → e1 = e2

-- Rule F1: every open permit shares the exact same (expectEpoch, expectHolder).
safety [RuleF1SharedExpect]
  ∀ a1 e1 h1 n1 a2 e2 h2 n2,
    openPermit a1 e1 h1 n1 ∧ openPermit a2 e2 h2 n2 → e1 = e2 ∧ h1 = h2

-- Every Permit head corresponds to a previously authorized permit for the predecessor of headEpoch.
safety [PermitWinnerAuthorized]
  originClass = ccPermit →
    ∃ n e0 h0, authorizedPermit originAttempt e0 h0 n ∧ succ e0 headEpoch

-- Rule O3: non-zero permitAttempt always names an authorized permit at permitEpoch <= headEpoch.
safety [OriginLineagePreserved]
  permitAttempt ≠ noAttempt →
    permitEpoch ≠ ep0 ∧ le permitEpoch headEpoch ∧
      ∃ n e0 h0, authorizedPermit permitAttempt e0 h0 n ∧ succ e0 permitEpoch

-- Each predecessor epoch produces at most one successor epoch.
safety [PredecessorConsumedAtMostOnce]
  ∀ e s1 s2, consumedPred e s1 ∧ consumedPred e s2 → s1 = s2

-- ---- Inductive Strengthening Invariants ----

invariant [inv_head_is_winner]
  epochWinner headEpoch headHolder

invariant [inv_winners_le_head_and_used]
  ∀ e h, epochWinner e h → le e headEpoch ∧ usedHolder h

invariant [inv_inflight_used_and_auth]
  ∀ a e h c h2 n, inflight a e h c h2 n →
    usedHolder h2 ∧
    usedAttempt a ∧
    (c = ccPermit → authorizedPermit a e h n ∧ a ≠ noAttempt)

invariant [inv_inflight_fresh_holders]
  ∀ a e h c h2 n e2, inflight a e h c h2 n → ¬ epochWinner e2 h2

invariant [inv_inflight_distinct_holders]
  ∀ a1 e1 h1 c1 h2 n1 a2 e2 h3 c2 n2,
    inflight a1 e1 h1 c1 h2 n1 ∧ inflight a2 e2 h3 c2 h2 n2 →
      a1 = a2 ∧ e1 = e2 ∧ h1 = h3 ∧ c1 = c2 ∧ n1 = n2

invariant [inv_consumed_lt_head]
  ∀ e s, consumedPred e s → le e headEpoch ∧ e ≠ headEpoch ∧ succ e s

#gen_spec

#check_invariants

end FleetMoveVeil
