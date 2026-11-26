--------------------------- MODULE TraceFleetMove ---------------------------
(*
 * TLA+/TLC trace-validation wrapper over FleetMove (docs/spec/fleet.md FLT-8, T1).
 * Reads a canonical SM1 NDJSON trace emitted by `std.sim.trace.ndjson`
 * (`seed`, `index`, `event: {tag, value}`, `actions`, `state`) via CommunityModules Json
 * and checks that every recorded kernel/sim transition is admitted by FleetMove.
 *)
EXTENDS FleetMove, Json, IOUtils, Sequences

TraceFile ==
    IF "TRACE_FILE" \in DOMAIN IOEnv THEN IOEnv.TRACE_FILE ELSE "sample_trace.ndjson"

TraceLog == ndJsonDeserialize(TraceFile)

VARIABLE l

traceVars == <<vars, l>>

TraceInit ==
    /\ Init
    /\ l = 1

Logline == TraceLog[l]

MatchAuthPrimary ==
    /\ Logline.event.tag = "Sm1AuthPrimary"
    /\ AuthPrimary(Logline.event.value)
    /\ head'.epoch = Logline.state.epoch
    /\ head'.permit_attempt = Logline.state.permit_attempt
    /\ incumbent' = Logline.state.incumbent
    /\ l' = l + 1

MatchAuthRecovery ==
    /\ Logline.event.tag = "Sm1AuthRecovery"
    /\ AuthRecovery(Logline.event.value)
    /\ head'.epoch = Logline.state.epoch
    /\ head'.permit_attempt = Logline.state.permit_attempt
    /\ incumbent' = Logline.state.incumbent
    /\ l' = l + 1

MatchSpawnContinue ==
    /\ Logline.event.tag = "Sm1SpawnContinue"
    /\ SpawnContinue
    /\ head'.epoch = Logline.state.epoch
    /\ head'.permit_attempt = Logline.state.permit_attempt
    /\ l' = l + 1

MatchExecCas ==
    /\ Logline.event.tag = "Sm1ExecCas"
    /\ \E op \in inflight :
           /\ op.attempt = Logline.event.value[1]
           /\ op.expect_epoch = Logline.event.value[2]
           /\ ExecCas(op)
           /\ head'.epoch = Logline.state.epoch
           /\ head'.origin_epoch = Logline.state.origin_epoch
           /\ head'.permit_attempt = Logline.state.permit_attempt
    /\ l' = l + 1

MatchReadSettle ==
    /\ Logline.event.tag = "Sm1ReadSettle"
    /\ ReadSettle
    /\ incumbent' = Logline.state.incumbent
    /\ Cardinality(open_permits') = Logline.state.open_count
    /\ l' = l + 1

TraceNext ==
    IF l <= Len(TraceLog)
    THEN \/ MatchAuthPrimary
         \/ MatchAuthRecovery
         \/ MatchSpawnContinue
         \/ MatchExecCas
         \/ MatchReadSettle
    ELSE UNCHANGED traceVars

TraceSpec == TraceInit /\ [][TraceNext]_traceVars

\* When TLC finishes without deadlock and reaches Len(TraceLog) + 1, the SM1 trace conformed.
TraceAccepted ==
    (l <= Len(TraceLog)) \/ (head.origin_epoch = head.epoch)

=============================================================================
