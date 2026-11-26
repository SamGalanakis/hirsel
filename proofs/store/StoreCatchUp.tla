---------------------------- MODULE StoreCatchUp ----------------------------
(*
 * Bounded model of export catch-up over a Tiered index (STORE-SPEC §3.5a
 * "Catch-up", §13.10a, §13.7 "An overlay H supersedes" (D-D7-5), §13.6a), and of
 * the holder's own journal recovery (§1.9, §3.4, §13.7 lazy open).
 *
 * OLD holds one table row r and one Tiered Many index X over it. X's entries are
 * the indexed values of r, so a correct X is {v} when Public and {} when
 * Backfilling(None). A COMMIT that changes r while X is Public carries X's ops,
 * each with the prior its journal op would carry: a delete of the old entry
 * (prior 1: the entry existed, with an empty payload) and an insert of the new
 * one (prior 0: absent). While X is Backfilling with the builder behind r, a
 * commit carries no X op. A demotion (`rebuild_index`, a walker or drift
 * demotion) and a build chunk are schema cuts: they write no journal frame, and
 * under an export lease each is a step that catch-up ships (§3.5a).
 *
 * Replay of a Tiered op puts it into an overlay over X's Cold root, a map from
 * entry to Put or Delete (`replay_tiered_op`); each op's prior is checked against
 * the root under the overlay (`replay_overlay_edit`). The child starts from the
 * copied MANIFEST and the journal prefix the copy recovered, so its overlay holds
 * the frames since OLD's last cut, and every round's frames join it.
 *
 * Rules:
 *   "v2157"  v2.157 as written. §3.5a ships a step as "the catalog records and
 *            collection rows it changed, each with the prior". For a Tiered index
 *            the changed collection row is X's root: a demotion's empty root, a
 *            chunk's root with the new entry. The child installs that root and,
 *            since nothing in §3.5a or §13.10a says otherwise, keeps its overlay
 *            on top (3150 F1).
 *   "v2161"  v2.161 (D-CU-1), the current text: a demotion step carries X's id as
 *            superseded, and `apply_steps` applies D-D7-5 to the child (drops its
 *            overlay of X, installs the empty root); a chunk step ships the entries
 *            it wrote, applied against their priors under the overlay. §13.7's
 *            premise covers holder reopen, and the prepare child is the exception.
 * Mutant names one seeded one-rule bug, or "none". README.md lists them.
 *)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS Vals, MaxWrites, MaxDemote, MaxCrash, Rules, Mutant

ASSUME Rules \in {"v2157", "v2161"}

\* Index ops. del: the entry must exist (prior 1). ins: the entry must be absent (prior 0).
Del(e) == [op |-> "del", e |-> e]
Ins(e) == [op |-> "ins", e |-> e]

\* Apply X ops to a plain tree, with the prior checks. A mismatch is Corrupt(Malformed).
RECURSIVE Apply(_, _)
Apply(ops, X) ==
    IF ops = <<>> THEN [ok |-> TRUE, x |-> X]
    ELSE LET o == Head(ops) IN
         IF (o.op = "del" /\ o.e \notin X) \/ (o.op = "ins" /\ o.e \in X)
         THEN [ok |-> FALSE, x |-> X]
         ELSE Apply(Tail(ops), IF o.op = "del" THEN X \ {o.e} ELSE X \cup {o.e})

\* All X ops of a sequence of frames, in order.
RECURSIVE OpsOf(_)
OpsOf(fs) == IF fs = <<>> THEN <<>> ELSE Head(fs).ops \o OpsOf(Tail(fs))

\* An overlay: puts and dels, disjoint. The view is the root under the overlay.
NoOverlay == [puts |-> {}, dels |-> {}]
View(root, ov) == (root \ ov.dels) \cup ov.puts

\* Replay ops into an overlay over root, checking each prior against the view.
RECURSIVE Overlay(_, _, _)
Overlay(ops, root, ov) ==
    IF ops = <<>> THEN [ok |-> TRUE, ov |-> ov]
    ELSE LET o == Head(ops)
             V == View(root, ov)
         IN IF (o.op = "del" /\ o.e \notin V) \/ (o.op = "ins" /\ o.e \in V)
            THEN [ok |-> FALSE, ov |-> ov]
            ELSE Overlay(Tail(ops), root,
                    IF o.op = "del" THEN [puts |-> ov.puts \ {o.e}, dels |-> ov.dels \cup {o.e}]
                    ELSE [puts |-> ov.puts \cup {o.e}, dels |-> ov.dels \ {o.e}])

Pick(S) == CHOOSE y \in S : TRUE

VARIABLES
    v, xs, x,        \* OLD's live head: r's value, X's state ("P" Public, "B" Backfilling(None)), X's entries
    base,            \* the MANIFEST's roots at the last cut: [v, xs, x, n]
    jn,              \* journal frames since the last cut: [t, v, ops]
    synced,          \* frames of jn made durable (Sync)
    n, acked,        \* commits made; commits whose durable ticket the caller saw resolve
    ovl,             \* the head's X is an overlay on base (after a reopen, until the next cut)
    hErr,            \* OLD's own replay or fold refused a valid store
    writes, demotes, crashes,
    lease,           \* the export lease is held
    log,             \* what the lease keeps after the copy, in version order: frames and steps
    shipped,         \* entries of log the child has applied
    child,           \* the child's copy: [v, xs, root, ov]
    cErr             \* the child refused a valid round (Corrupt)

vars == <<v, xs, x, base, jn, synced, n, acked, ovl, hErr, writes, demotes, crashes,
          lease, log, shipped, child, cErr>>

ChildX == View(child.root, child.ov)

Init ==
    /\ v = Pick(Vals) /\ xs = "P" /\ x = {v}
    /\ base = [v |-> v, xs |-> "P", x |-> {v}, n |-> 0]
    /\ jn = <<>> /\ synced = 0 /\ n = 0 /\ acked = 0
    /\ ovl = FALSE /\ hErr = FALSE
    /\ writes = 0 /\ demotes = 0 /\ crashes = 0
    /\ lease = FALSE /\ log = <<>> /\ shipped = 0
    /\ child = [v |-> v, xs |-> "P", root |-> {v}, ov |-> NoOverlay]
    /\ cErr = FALSE

Keep(L) == IF lease THEN L ELSE log

-----------------------------------------------------------------------------
\* A commit: r v -> w.
Write(w) ==
    /\ writes < MaxWrites /\ w # v
    /\ LET ops == IF xs = "P" THEN <<Del(v), Ins(w)>> ELSE <<>>
           f == [t |-> "frame", v |-> w, ops |-> ops]
       IN /\ v' = w
          /\ x' = Apply(ops, x).x
          /\ jn' = Append(jn, f)
          /\ log' = Keep(Append(log, f))
    /\ writes' = writes + 1
    /\ n' = n + 1
    \* Mutant ack_before_sync: the ticket resolves before the frame is durable.
    /\ acked' = IF Mutant = "ack_before_sync" THEN n + 1 ELSE acked
    /\ UNCHANGED <<xs, base, synced, ovl, hErr, demotes, crashes, lease, shipped, child, cErr>>

\* The journal task makes the frames durable and resolves their tickets.
Sync ==
    /\ synced < Len(jn)
    /\ synced' = Len(jn)
    /\ acked' = n
    /\ UNCHANGED <<v, xs, x, base, jn, n, ovl, hErr, writes, demotes, crashes, lease, log, shipped, child, cErr>>

\* A cut publishes the head and captures first_live past every frame, so jn
\* empties (§3.5 step 3). Every cut rotates the journal (§11.17 step 3).
CutRoots(nv, nxs, nx) ==
    /\ base' = [v |-> nv, xs |-> nxs, x |-> nx, n |-> n]
    /\ jn' = <<>> /\ synced' = 0 /\ acked' = n
    /\ ovl' = FALSE

\* Mutant lease_drops_segments: the lease keeps no segment a cut covered, so the
\* frames the child has not yet copied are gone.
DropCovered(L) == IF Mutant = "lease_drops_segments"
                  THEN SubSeq(L, 1, shipped) \o SelectSeq(SubSeq(L, shipped + 1, Len(L)), LAMBDA e : e.t = "step")
                  ELSE L

Checkpoint ==
    /\ CutRoots(v, xs, x)
    /\ log' = Keep(DropCovered(log))
    /\ UNCHANGED <<v, xs, x, n, hErr, writes, demotes, crashes, lease, shipped, child, cErr>>

\* A demotion: a schema cut whose H redefines X with an empty root it built
\* without reading the old one (§13.7 "Superseded"). The fold discards X's overlay.
\* Mutant fold_superseded: the cut folds a reopened overlay into the empty root.
Demote ==
    /\ demotes < MaxDemote /\ xs = "P"
    /\ LET folded == Apply(OpsOf(jn), {})
           nx == IF Mutant = "fold_superseded" /\ ovl THEN folded.x ELSE {}
           bad == Mutant = "fold_superseded" /\ ovl /\ ~folded.ok
           s == [t |-> "step", k |-> "B", root |-> {},
                 mode |-> IF Rules = "v2161" THEN "supersede" ELSE "root", rows |-> <<>>]
       IN /\ x' = nx /\ xs' = "B"
          /\ hErr' = (hErr \/ bad)
          /\ CutRoots(v, "B", nx)
          /\ log' = Keep(Append(DropCovered(log), s))
    /\ demotes' = demotes + 1
    /\ UNCHANGED <<v, n, writes, crashes, lease, shipped, child, cErr>>

\* A build chunk: the builder reaches r, writes v's entry and makes X Public. It
\* is a schema cut; a kept overlay folds first (§3.4 "An index_build_step chunk").
Build ==
    /\ xs = "B"
    /\ LET s == [t |-> "step", k |-> "P", root |-> {v},
                 mode |-> IF Rules = "v2161" THEN "chunk" ELSE "root", rows |-> <<Ins(v)>>]
       IN /\ x' = {v} /\ xs' = "P"
          /\ CutRoots(v, "P", {v})
          /\ log' = Keep(Append(DropCovered(log), s))
    /\ UNCHANGED <<v, n, hErr, writes, demotes, crashes, lease, shipped, child, cErr>>

\* Crash and reopen. The journal keeps every synced frame and possibly some of
\* the torn tail; open replays them as an overlay on the MANIFEST's X (§13.7),
\* with the prior checks. The export attempt dies with the process.
Crash ==
    /\ crashes < MaxCrash
    /\ \E k \in synced..Len(jn) :
        LET kept == SubSeq(jn, 1, k)
            r == Overlay(OpsOf(kept), base.x, NoOverlay)
            nv == IF k = 0 THEN base.v ELSE kept[k].v
        IN /\ v' = nv /\ xs' = base.xs /\ x' = View(base.x, r.ov)
           /\ hErr' = (hErr \/ ~r.ok)
           /\ jn' = kept /\ synced' = k
           /\ n' = base.n + k
           /\ ovl' = (k > 0)
    /\ crashes' = crashes + 1
    /\ lease' = FALSE /\ log' = <<>> /\ shipped' = 0
    /\ UNCHANGED <<base, acked, writes, demotes, cErr, child>>

-----------------------------------------------------------------------------
\* E1-E3: the lease settles the journal; the prepare opener reads the copied
\* MANIFEST and recovers the journal prefix as an overlay on it (§3.5a R1).
BeginExport ==
    /\ ~lease
    /\ LET r == Overlay(OpsOf(jn), base.x, NoOverlay) IN
       /\ child' = [v |-> v, xs |-> base.xs, root |-> base.x, ov |-> r.ov]
       /\ cErr' = ~r.ok
    /\ lease' = TRUE /\ log' = <<>> /\ shipped' = 0
    /\ UNCHANGED <<v, xs, x, base, jn, synced, n, acked, ovl, hErr, writes, demotes, crashes>>

\* The child applies one kept entry: a frame into its overlay, a step at its
\* version (§3.5a "Replay": before the first frame past it).
ApplyEntry(c, e) ==
    IF e.t = "frame"
    THEN LET r == Overlay(e.ops, c.root, c.ov)
         IN [ok |-> r.ok, c |-> [c EXCEPT !.v = e.v, !.ov = r.ov]]
    ELSE CASE e.mode = "root" ->        \* v2.157: install the shipped root under the overlay
                [ok |-> TRUE, c |-> [c EXCEPT !.xs = e.k, !.root = e.root]]
           [] e.mode = "supersede" ->   \* v2.161 D-CU-1: D-D7-5, the overlay is dropped
                [ok |-> TRUE, c |-> [c EXCEPT !.xs = e.k, !.root = {}, !.ov = NoOverlay]]
           [] e.mode = "chunk" ->       \* v2.161 D-CU-1: the chunk's entries, as replay applies a frame
                LET r == Overlay(e.rows, c.root, c.ov)
                IN [ok |-> r.ok, c |-> [c EXCEPT !.xs = e.k, !.ov = r.ov]]

RECURSIVE ApplyAll(_, _)
ApplyAll(c, es) ==
    IF es = <<>> THEN [ok |-> TRUE, c |-> c]
    ELSE LET r == ApplyEntry(c, Head(es)) IN
         IF ~r.ok THEN r ELSE ApplyAll(r.c, Tail(es))

\* A round: advance, copy the tail, catch_up_export over everything not yet shipped.
\* Mutant steps_after_tail: the round's steps are applied after its frames.
Round ==
    /\ lease /\ ~cErr /\ shipped < Len(log)
    /\ LET new == SubSeq(log, shipped + 1, Len(log))
           ord == IF Mutant = "steps_after_tail"
                  THEN SelectSeq(new, LAMBDA e : e.t = "frame") \o SelectSeq(new, LAMBDA e : e.t = "step")
                  ELSE new
           r == ApplyAll(child, ord)
       IN /\ child' = r.c
          /\ cErr' = ~r.ok
    /\ shipped' = Len(log)
    /\ UNCHANGED <<v, xs, x, base, jn, synced, n, acked, ovl, hErr, writes, demotes, crashes, lease, log>>

\* Install (after the last round, in the pause) or abort ends the attempt.
EndExport ==
    /\ lease /\ (cErr \/ shipped = Len(log))
    /\ lease' = FALSE /\ log' = <<>> /\ shipped' = 0
    /\ UNCHANGED <<v, xs, x, base, jn, synced, n, acked, ovl, hErr, writes, demotes, crashes, child, cErr>>

Next ==
    \/ \E w \in Vals : Write(w)
    \/ Sync
    \/ Checkpoint
    \/ Demote
    \/ Build
    \/ Crash
    \/ BeginExport
    \/ Round
    \/ EndExport

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Safety invariants. README.md maps each to its STORE-SPEC section.

\* C1 (§3.5a "Replay", §13.10a): a round never refuses a valid store as Corrupt.
NoFalseCorrupt == ~cErr

\* C2 (§3.5a, the demotion floor): a caught-up child installs exactly OLD's X and
\* X's state: no frame below a demotion survives it, none above it is missed.
InstallExact ==
    (lease /\ ~cErr /\ shipped = Len(log)) =>
        /\ ChildX = x
        /\ child.xs = xs
        /\ child.v = v

\* C3 (§1.2 index states, L68 (g)): Backfilling(None) holds no entry, at OLD and in
\* the child.
BackfillNoneEmpty ==
    /\ xs = "B" => x = {}
    /\ (lease /\ ~cErr /\ child.xs = "B") => ChildX = {}

\* C4 (§13.7, L68 (f)): OLD's own replay and fold never refuse a valid store.
HolderNoCorrupt == ~hErr

\* C5 (§1.9, §3.4): a recovered state is the state after a real commit prefix at or
\* beyond the last acknowledged durable ticket.
RecoveredPrefix == n >= acked

\* C6 (§13.7 "No epoch is needed"): "no live journal frame predates a
\* supersession". v2.157 states it of every replay. The child's overlay holds
\* every frame since OLD's last cut before the copy plus the lease's frames, so a
\* frame with an X op before a demotion step is live for the child. v2.161
\* scopes the premise to holder reopen, where jn is empty after every cut, and
\* makes the prepare child the exception (D-CU-1).
SupersedePremise ==
    Rules = "v2161" \/
    ~(lease /\ ~cErr /\
      \E j \in (shipped + 1)..Len(log) :
        /\ log[j].t = "step" /\ log[j].k = "B"
        /\ \/ child.ov # NoOverlay
           \/ \E i \in (shipped + 1)..(j - 1) : log[i].t = "frame" /\ log[i].ops # <<>>)

=============================================================================
