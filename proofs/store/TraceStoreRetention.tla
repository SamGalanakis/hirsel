------------------------- MODULE TraceStoreRetention -------------------------
(*
 * Trace validation of the Store crash sim against StoreRetention (the way
 * proofs/fleet/TraceFleetMove.tla checks Fleet). The trace is the NDJSON
 * test_store_retention_trace prints (core/programs/store/
 * store_retention_trace_laws.hirsel), with pack sequence numbers renumbered
 * 1..n in sequence order by check_proofs.py, which also writes the cfg.
 *
 * Each row must be one model action whose result equals what the sim observed:
 *   cut      Cut(op, sel, dr, pg) for some arguments: the published generation's
 *            residency, listed class-3/4 packs and the packs its deferred-free
 *            rows name equal the row's (publication), and the cut leaves the
 *            row's files plus the ones the call removed;
 *   remove   RemoveFile(p), matched apart from the cut that preceded it
 *            (removal): p is delisted and no Snapshot lease names it;
 *   capture  Capture, leasing exactly the row's packs;
 *   drop     DropSnap of a Snapshot that leased the row's packs;
 *   reopen   Crash, then the installed lists and files open left.
 * The model's own Cut decides whether a cut may retire a pack; nothing in a row
 * says why the store did what it did, so the store's idle decision cannot
 * certify itself. Writes are not rows: Init's Hot head stands for the rows
 * written before the first cut, which is all the fixture writes.
 *
 * The trace conforms iff TLC reports TraceUnfinished violated (l passed the
 * last row). Otherwise the deepest state matched rows 1..depth-1 and row
 * `depth` is the first row no model action admits; check_proofs.py names it.
 *)
EXTENDS StoreRetention, Json, IOUtils

TraceFile ==
    IF "TRACE_FILE" \in DOMAIN IOEnv THEN IOEnv.TRACE_FILE ELSE "traces/retention-trace-green.ndjson"

TraceLog == ndJsonDeserialize(TraceFile)

VARIABLE l

traceVars == <<vars, l>>

Row == TraceLog[l]

Set(s) == {s[i] : i \in DOMAIN s}

\* The packs a generation's deferred-free rows name.
QueuePacks(g) == {r.p : r \in {q \in g.queue : q.k = "C"}}

TraceInit ==
    /\ Init
    /\ l = 1

MatchCut ==
    /\ Row.kind = "cut"
    /\ \E op \in Ops, sel \in {0} \cup Packs, dr \in BOOLEAN, pg \in BOOLEAN : Cut(op, sel, dr, pg)
    /\ gen'.hres = Row.hres
    /\ gen'.listed = Set(Row.listed)
    /\ QueuePacks(gen') = Set(Row.queue)
    /\ files' = Set(Row.files) \cup Set(Row.removed)
    /\ l' = l + 1

MatchRemove ==
    /\ Row.kind = "remove"
    /\ RemoveFile(Row.pack)
    /\ l' = l + 1

MatchCapture ==
    /\ Row.kind = "capture"
    /\ Capture
    /\ snap'.leased = Set(Row.leased)
    /\ l' = l + 1

MatchDrop ==
    /\ Row.kind = "drop"
    /\ snap.leased = Set(Row.leased)
    /\ DropSnap
    /\ l' = l + 1

MatchReopen ==
    /\ Row.kind = "reopen"
    /\ Crash
    /\ gen.listed = Set(Row.listed)
    /\ QueuePacks(gen) = Set(Row.queue)
    /\ files' = Set(Row.files)
    /\ l' = l + 1

TraceNext ==
    /\ l <= Len(TraceLog)
    /\ \/ MatchCut
       \/ MatchRemove
       \/ MatchCapture
       \/ MatchDrop
       \/ MatchReopen

TraceSpec == TraceInit /\ [][TraceNext]_traceVars

\* Violated exactly when every row was matched: the trace conforms.
TraceUnfinished == l <= Len(TraceLog)

=============================================================================
