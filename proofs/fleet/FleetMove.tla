----------------------------- MODULE FleetMove -----------------------------
(*
 * Abstract TLA+ specification of Hirsel Fleet per-Place move authorization,
 * ClaimAuth precondition checking, ORIGIN head frame (O1-O4), Rule F1
 * (single predecessor per open permit set), and read-only settlement.
 *
 * Independent of core/programs/fleet/move_kernel.hirsel (docs/spec/fleet.md FLT-2, K8).
 *)
EXTENDS Naturals, FiniteSets, Sequences

CONSTANTS
    Nodes,       \* Set of host identities, e.g. {1, 2, 3}
    MaxEpoch,    \* Bounded max epoch for TLC, e.g. 5
    MaxInFlight  \* Bounded concurrent in-flight opens for TLC, e.g. 3

VARIABLES
    head,                  \* Current objects@1 head record (pair + ORIGIN)
    incumbent,             \* Current known incumbent node
    next_holder,           \* Monotone fresh random holder per physical open
    next_permit_attempt,   \* Monotone permit attempt sequence
    next_continue_attempt, \* Monotone continue/janitor attempt sequence
    open_permits,          \* Set of open write-ahead permit rows (Rule F1)
    inflight,              \* Set of concurrent in-flight opens (Permit & Continue)
    epoch_winners,         \* Map: epoch -> set of holders ever installed at that epoch
    authorized,            \* Set of authorized Permit records
    consumed_predecessors  \* Map: [epoch, holder] -> set of successor epochs produced

vars == <<head, incumbent, next_holder, next_permit_attempt, next_continue_attempt,
          open_permits, inflight, epoch_winners, authorized, consumed_predecessors>>

Classes == {"Adopt", "Permit", "Continue"}

Init ==
    /\ head = [
           epoch          |-> 1,
           holder         |-> 1,
           origin_epoch   |-> 1,
           origin_attempt |-> 0,
           origin_class   |-> "Adopt",
           permit_epoch   |-> 0,
           permit_attempt |-> 0
       ]
    /\ incumbent = CHOOSE n \in Nodes : TRUE
    /\ next_holder = 2
    /\ next_permit_attempt = 1
    /\ next_continue_attempt = 1001
    /\ open_permits = {}
    /\ inflight = {}
    /\ epoch_winners = [e \in 1..MaxEpoch |-> IF e = 1 THEN {1} ELSE {}]
    /\ authorized = {}
    /\ consumed_predecessors = [e \in 1..MaxEpoch |-> {}]

(*
 * Rule F1: authorize a primary Permit when no permits are open, or at most one
 * recovery contender sharing the exact same (expect_epoch, expect_holder).
 *)
AuthPrimary(target) ==
    /\ head.epoch < MaxEpoch
    /\ open_permits = {}
    /\ Cardinality(inflight) < MaxInFlight
    /\ target \in Nodes
    /\ target # incumbent
    /\ LET att == next_permit_attempt
           h   == next_holder
           p   == [attempt       |-> att,
                   expect_epoch  |-> head.epoch,
                   expect_holder |-> head.holder,
                   target        |-> target]
           op  == [attempt       |-> att,
                   expect_epoch  |-> head.epoch,
                   expect_holder |-> head.holder,
                   class         |-> "Permit",
                   holder        |-> h,
                   target        |-> target]
       IN /\ open_permits' = {p}
          /\ authorized' = authorized \cup {p}
          /\ inflight' = inflight \cup {op}
          /\ next_permit_attempt' = next_permit_attempt + 1
          /\ next_holder' = next_holder + 1
          /\ UNCHANGED <<head, incumbent, next_continue_attempt, epoch_winners, consumed_predecessors>>

AuthRecovery(target) ==
    /\ head.epoch < MaxEpoch
    /\ Cardinality(open_permits) = 1
    /\ Cardinality(inflight) < MaxInFlight
    /\ target \in Nodes
    /\ target # incumbent
    /\ LET primary == CHOOSE p \in open_permits : TRUE
       IN /\ target # primary.target
          /\ LET att == next_permit_attempt
                 h   == next_holder
                 p   == [attempt       |-> att,
                         expect_epoch  |-> primary.expect_epoch,
                         expect_holder |-> primary.expect_holder,
                         target        |-> target]
                 op  == [attempt       |-> att,
                         expect_epoch  |-> primary.expect_epoch,
                         expect_holder |-> primary.expect_holder,
                         class         |-> "Permit",
                         holder        |-> h,
                         target        |-> target]
             IN /\ open_permits' = open_permits \cup {p}
                /\ authorized' = authorized \cup {p}
                /\ inflight' = inflight \cup {op}
                /\ next_permit_attempt' = next_permit_attempt + 1
                /\ next_holder' = next_holder + 1
                /\ UNCHANGED <<head, incumbent, next_continue_attempt, epoch_winners, consumed_predecessors>>

(*
 * Same-host Continue open (local restart or attached-stream janitor) naming
 * the head pair it observed before opening. Draws a fresh random holder.
 *)
SpawnContinue ==
    /\ head.epoch < MaxEpoch
    /\ Cardinality(inflight) < MaxInFlight
    /\ ~\E op \in inflight : op.class = "Continue" /\ op.expect_epoch = head.epoch
    /\ LET att == next_continue_attempt
           h   == next_holder
           op  == [attempt       |-> att,
                   expect_epoch  |-> head.epoch,
                   expect_holder |-> head.holder,
                   class         |-> "Continue",
                   holder        |-> h,
                   target        |-> incumbent]
       IN /\ inflight' = inflight \cup {op}
          /\ next_continue_attempt' = next_continue_attempt + 1
          /\ next_holder' = next_holder + 1
          /\ UNCHANGED <<head, incumbent, next_permit_attempt, open_permits,
                         epoch_winners, authorized, consumed_predecessors>>

(*
 * Concurrent objects@1 head CAS: checks ClaimAuth.expect BEFORE any write.
 * On match: installs (epoch + 1, fresh_holder) and writes ORIGIN per O1/O3.
 * On mismatch: returns Superseded with zero mutation to head.
 *)
ExecCas(op) ==
    /\ op \in inflight
    /\ inflight' = inflight \ {op}
    /\ IF op.expect_epoch = head.epoch /\ op.expect_holder = head.holder /\ head.epoch < MaxEpoch
       THEN LET new_ep == head.epoch + 1
                p_ep   == IF op.class = "Permit" THEN new_ep ELSE head.permit_epoch
                p_att  == IF op.class = "Permit" THEN op.attempt ELSE head.permit_attempt
            IN /\ head' = [
                      epoch          |-> new_ep,
                      holder         |-> op.holder,
                      origin_epoch   |-> new_ep,
                      origin_attempt |-> op.attempt,
                      origin_class   |-> op.class,
                      permit_epoch   |-> p_ep,
                      permit_attempt |-> p_att
                  ]
               /\ epoch_winners' = [epoch_winners EXCEPT ![new_ep] = @ \cup {op.holder}]
               /\ consumed_predecessors' = [consumed_predecessors EXCEPT ![op.expect_epoch] = @ \cup {new_ep}]
               /\ UNCHANGED <<incumbent, next_holder, next_permit_attempt,
                              next_continue_attempt, open_permits, authorized>>
       ELSE UNCHANGED <<head, incumbent, next_holder, next_permit_attempt,
                        next_continue_attempt, open_permits, epoch_winners,
                        authorized, consumed_predecessors>>

(*
 * Read-only settlement GET (FLT-2): settles open permits as Won or Dead once
 * head.epoch > expect_epoch without opening a writer or mutating head.
 *)
ReadSettle ==
    /\ open_permits # {}
    /\ \A p \in open_permits : head.epoch > p.expect_epoch
    /\ LET winners == {p \in open_permits :
                          head.permit_attempt = p.attempt /\ head.permit_epoch >= p.expect_epoch + 1}
       IN /\ IF winners # {}
             THEN incumbent' = (CHOOSE p \in winners : TRUE).target
             ELSE incumbent' = incumbent
          /\ open_permits' = {}
          /\ UNCHANGED <<head, next_holder, next_permit_attempt, next_continue_attempt,
                         inflight, epoch_winners, authorized, consumed_predecessors>>

Next ==
    \/ \E t \in Nodes : AuthPrimary(t)
    \/ \E t \in Nodes : AuthRecovery(t)
    \/ SpawnContinue
    \/ \E op \in inflight : ExecCas(op)
    \/ ReadSettle

Spec == Init /\ [][Next]_vars

(* ---- Safety Invariants ---- *)

TypeOK ==
    /\ head.epoch \in 1..MaxEpoch
    /\ head.origin_class \in Classes
    /\ incumbent \in Nodes
    /\ Cardinality(open_permits) <= 2

\* Rule O4: ORIGIN.epoch always equals HEAD.epoch.
OriginEpochMatchesHead ==
    head.origin_epoch = head.epoch

\* Fencing safety: at most one holder is ever installed at any epoch, and holders never repeat.
UniqueHolderPerEpoch ==
    /\ \A e \in 1..MaxEpoch : Cardinality(epoch_winners[e]) <= 1
    /\ \A e1, e2 \in 1..MaxEpoch : (e1 # e2) => (epoch_winners[e1] \cap epoch_winners[e2] = {})

\* Rule F1: every open permit shares the exact same (expect_epoch, expect_holder).
RuleF1SharedExpect ==
    \A p1, p2 \in open_permits :
        p1.expect_epoch = p2.expect_epoch /\ p1.expect_holder = p2.expect_holder

\* Every Permit winner on head corresponds to a previously authorized permit for (epoch - 1).
PermitWinnerWasAuthorized ==
    (head.origin_class = "Permit") =>
        \E p \in authorized :
            p.attempt = head.origin_attempt /\ p.expect_epoch + 1 = head.epoch

\* Rule O3: non-zero permit_attempt always refers to an authorized permit at permit_epoch <= head.epoch.
OriginLineagePreserved ==
    (head.permit_attempt # 0) =>
        /\ head.permit_epoch >= 2
        /\ head.permit_epoch <= head.epoch
        /\ \E p \in authorized :
               p.attempt = head.permit_attempt /\ p.expect_epoch + 1 = head.permit_epoch

\* Each predecessor epoch produces at most one successor epoch.
PredecessorConsumedAtMostOnce ==
    \A e \in 1..MaxEpoch : Cardinality(consumed_predecessors[e]) <= 1

=============================================================================
