--------------------------- MODULE TraceFleetMove ---------------------------
(*
 * Replay one Place's SM1 trace against FleetMove (docs/spec/fleet.md FLT-8, T1).
 * check_proofs.py validates the capture and gives each full holder ID a local
 * integer name before passing the rows to TLC.
 *)
EXTENDS FleetMove, Json, IOUtils

TraceLog == ndJsonDeserialize(IOEnv.TRACE_FILE)

VARIABLES l, holders

traceVars == <<vars, l, holders>>

TraceInit ==
    /\ Init
    /\ l = 1
    /\ holders = [h \in {head.holder} |-> TraceLog[1].state.holder]

Logline == TraceLog[l]

MatchClaim(claim, op) ==
    /\ claim.attempt = op.attempt
    /\ claim.class.tag = op.class
    /\ claim.expect.epoch = op.expect_epoch
    /\ claim.expect.holder = holders[op.expect_holder]

MatchNewClaim(claim) ==
    \E op \in inflight' \ inflight : MatchClaim(claim, op)

MatchAction ==
    LET event == Logline.event
    IN \/ /\ event.tag = "Sm1AuthPrimary"
          /\ AuthPrimary(event.value[1])
          /\ MatchNewClaim(event.value[2])
       \/ /\ event.tag = "Sm1AuthRecovery"
          /\ AuthRecovery(event.value[1])
          /\ MatchNewClaim(event.value[2])
       \/ /\ event.tag = "Sm1SpawnContinue"
          /\ SpawnContinue
          /\ MatchNewClaim(event.value)
       \/ /\ event.tag = "Sm1ExecCas"
          /\ \E op \in inflight :
                /\ MatchClaim(event.value[1], op)
                /\ ExecCas(op)
                /\ event.value[2] = (head'.epoch > head.epoch)
       \/ /\ event.tag = "Sm1ReadSettle"
          /\ ReadSettle

(* The model allocates before CAS. The simulator exposes the winning ID only
 * after CAS. Bind on first install and enforce a bijection over seen holders.
 *)
MatchHolder(observed) ==
    IF head'.holder \in DOMAIN holders
    THEN /\ holders[head'.holder] = observed
         /\ UNCHANGED holders
    ELSE /\ \A h \in DOMAIN holders : holders[h] # observed
         /\ holders' = [h \in DOMAIN holders \cup {head'.holder} |->
                            IF h = head'.holder THEN observed ELSE holders[h]]

MatchState ==
    LET state == Logline.state
    IN /\ MatchHolder(state.holder)
       /\ head'.epoch = state.epoch
       /\ head'.origin_epoch = state.origin_epoch
       /\ head'.permit_attempt = state.permit_attempt
       /\ incumbent' = state.incumbent
       /\ Cardinality(open_permits') = state.open_count

TraceNext ==
    IF l <= Len(TraceLog)
    THEN /\ MatchAction
         /\ MatchState
         /\ l' = l + 1
    ELSE UNCHANGED traceVars

TraceSpec == TraceInit /\ [][TraceNext]_traceVars

=============================================================================
