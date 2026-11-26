---------------------------- MODULE StoreJanitor ----------------------------
(*
 * Bounded model of host-stream retirement (STORE-SPEC §12.11.8 "Retirement, GC
 * and the janitor"): the release rule that lets `low` pass a batch.
 *
 * One stream of batches 1..MaxBatch and one Place X. A batch holds X's frames
 * when it has an INDEX row for X (`rows`). X's head names this stream as `live`
 * from some seq (0: X's live stream is another one), plus `sealed` entries
 * [from, cutoff]. A Place cut that moves X to another stream seals its live range;
 * a cut that settles X folds the batches into packs and drops its sealed entries.
 * The service knows X's head only through the summary X last reported (`obs`,
 * with the seq the stream had reached when it was observed). The election and
 * pin rebuild are abstracted: a crash forgets the summary.
 *)
EXTENDS Naturals, FiniteSets

CONSTANTS MaxBatch, Mutant

Seqs == 1..MaxBatch
Range(f, c) == [from |-> f, cutoff |-> c]

VARIABLES
    seq,      \* the next batch the stream creates
    rows,     \* batches with an INDEX row for X
    live,     \* X's head: this stream's live ref from seq `live` (0 = none)
    sealed,   \* X's head: sealed entries on this stream
    obs,      \* the head summary X last reported: [at, live, sealed]; at = 0 is none
    low       \* every batch below low is deleted

vars == <<seq, rows, live, sealed, obs, low>>

Contains(lv, sl, b) == (lv # 0 /\ lv <= b) \/ (\E e \in sl : e.from <= b /\ b <= e.cutoff)

Init ==
    /\ seq = 1 /\ rows = {} /\ live = 0 /\ sealed = {}
    /\ obs = [at |-> 0, live |-> 0, sealed |-> {}]
    /\ low = 1

\* A batch with X's frames (X's head names this stream), or without.
Batch(withX) ==
    /\ seq <= MaxBatch
    /\ withX => live # 0
    /\ rows' = IF withX THEN rows \cup {seq} ELSE rows
    /\ seq' = seq + 1
    /\ UNCHANGED <<live, sealed, obs, low>>

\* X's head starts naming this stream as live.
Attach ==
    /\ live = 0 /\ seq <= MaxBatch
    /\ live' = seq
    /\ UNCHANGED <<seq, rows, sealed, obs, low>>

\* An open-time cut moves X to another stream and seals its range here.
Seal ==
    /\ live # 0
    /\ sealed' = IF live <= seq - 1 THEN sealed \cup {Range(live, seq - 1)} ELSE sealed
    /\ live' = 0
    /\ UNCHANGED <<seq, rows, obs, low>>

\* A cut settles X: its frames are in packs, its sealed entries go.
Settle ==
    /\ sealed' = {}
    /\ live' = IF live # 0 THEN seq ELSE 0
    /\ UNCHANGED <<seq, rows, obs, low>>

\* X reports its head (§12.11.12).
Report ==
    /\ obs' = [at |-> seq, live |-> live, sealed |-> sealed]
    /\ UNCHANGED <<seq, rows, live, sealed, low>>

\* The release rule: low passes b only once X, if b has X's INDEX row, has a head
\* observed after the row was written in which neither live nor a sealed entry
\* contains b.
\* Mutant release_ignores_sealed: the rule reads only `live`.
\* Mutant stale_summary: the rule takes a head observed before the row was written.
Releasable(b) ==
    b \in rows =>
        /\ (Mutant = "stale_summary" \/ obs.at > b)
        /\ ~Contains(obs.live, IF Mutant = "release_ignores_sealed" THEN {} ELSE obs.sealed, b)

RaiseLow ==
    /\ \E L \in (low + 1)..seq :
        /\ \A b \in low..(L - 1) : Releasable(b)
        /\ low' = L
    /\ UNCHANGED <<seq, rows, live, sealed, obs>>

\* A restart forgets the summaries; pins are rebuilt from durable state.
Crash ==
    /\ obs' = [at |-> 0, live |-> 0, sealed |-> {}]
    /\ UNCHANGED <<seq, rows, live, sealed, low>>

Next ==
    \/ \E w \in BOOLEAN : Batch(w)
    \/ Attach \/ Seal \/ Settle \/ Report \/ RaiseLow \/ Crash

Spec == Init /\ [][Next]_vars

\* J1 (§12.11.8 Invariants, the release rule): no batch X's head contains is deleted.
HeadBatchesKept == \A b \in rows : Contains(live, sealed, b) => b >= low

=============================================================================
