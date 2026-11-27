--------------------------- MODULE StoreRetention ---------------------------
(*
 * Bounded model of Hirsel Store Tiered pack retention (STORE-SPEC §1.9, §3.5,
 * §13.5 "Promoting a historical Snapshot", §13.6 deadlists, the probe and
 * Tiered-idle cuts, §13.9 anchors, §13.10a the export lease).
 *
 * Holders are a pin, an optional anchor and an optional live-head fork of main
 * (WithFork), whose row files kills into lineage holder cells (§13.6 "Deadlist
 * cells"), so cells can exist in a Tiered-idle generation (review 3177 F3).
 * The drain is optional, and a cut can drop its probe progress (review 3177 F2).
 *
 * One Tiered collection of NKeys rows. Each row is one leaf frame named by its
 * stamp. A Cold reference is (pack, PACK_INDEX row, stamp). A cut is one atomic
 * action that runs the steps of the mandatory candidate in §13.6a order: H, the
 * candidate queue's added rows, the promotion, T2 emission, then AP16 item 3's
 * probe, the drain and publication. Crash and reopen can happen between any two
 * actions. A crash inside a cut is the state before the rename (§3.6), which is
 * the same as no cut.
 *
 * Rules selects the reading of the spec under check:
 *   "v2157"         v2.157 as written: an idle step advances the cursor's row count
 *                   and a later reading probe resumes there ("Progress").
 *   "v2157_restart" v2.157 with the only other reading: a reading probe restarts an
 *                   idle-advanced cursor at row 0, which "Progress" forbids.
 *   "v2164"         the current text: v2.161 (D-TI-1) as amended by v2.164 (review
 *                   3177 F1-F3). An idle cut reads nothing, never installs or moves a
 *                   cursor, and takes every listed class-3/4 pack in sequence order,
 *                   selected or not, retiring each whole and deleting its lineage cells
 *                   (budget 1 takes one pack); idle is decided over installed plus added
 *                   queue rows; promotion copies a frame of a pack an idle cut retired
 *                   through the Snapshot's lease.
 *   "count3157"     review 3157's alternative, which v2.161 rejects as sound but
 *                   costlier: idle progress is a count apart from the probe cursor,
 *                   and the first non-idle cut restarts it at row 0, as a third reset.
 * Mutant names one seeded one-rule bug, or "none". README.md lists them.
 *)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS NKeys, MaxStamp, MaxPack, MaxCrash, Rules, Mutant, WithAnchors, WithExport, WithFork, Track

ASSUME Rules \in {"v2157", "v2157_restart", "count3157", "v2164"}

Keys == 1..NKeys
Packs == 1..MaxPack
Stamps == 1..MaxStamp

\* One record shape for every reference, so TLC compares them freely.
\* k = "C": a Cold frame at PACK_INDEX row i of pack p, with stamp s.
\* k = "H": a Hot page with stamp s (no location yet; T2 emits it).
\* k = "M": a Memory page, or no row; never a class-3/4 frame.
Cold(p, i, s) == [k |-> "C", p |-> p, i |-> i, s |-> s]
Hot(s) == [k |-> "H", p |-> 0, i |-> 0, s |-> s]
Mem == [k |-> "M", p |-> 0, i |-> 0, s |-> 0]
MemRefs == [x \in Keys |-> Mem]

NoRoot == [on |-> FALSE, refs |-> MemRefs, w |-> 0]
NoFork == [on |-> FALSE, w |-> 0, res |-> "M", refs |-> MemRefs]
NoCursor == [on |-> FALSE, rows |-> 0, known |-> 0, idle |-> FALSE]

MinOf(S) == CHOOSE x \in S : \A y \in S : x <= y
Max(S) == CHOOSE x \in S : \A y \in S : x >= y

\* The stamps of S in ascending order.
RECURSIVE Sorted(_)
Sorted(S) == IF S = {} THEN <<>> ELSE <<MinOf(S)>> \o Sorted(S \ {MinOf(S)})

VARIABLES
    head,       \* the writer's live main head: [res, refs]; may hold Hot pages
    gen,        \* the published MANIFEST generation (durable)
    packRows,   \* pack -> PACK_INDEX rows (stamps); immutable once written
    files,      \* pack files present on disk
    nextPack,
    nextStamp,
    cursor,     \* writer memory: probe cursor per pack (rows probed; known = rows whose offset is known)
    snap,       \* writer memory: one process-local Snapshot with its lease
    export,     \* writer memory: the export lease and the image it copied
    acked,      \* pin names whose pin the caller saw acknowledged (caller knowledge, survives crashes)
    pending,    \* ack_before_publish mutant only: a pin acked but not yet cut
    fault,      \* a promotion read a frame whose file was gone
    rewind,     \* what the last cut did to a probe cursor: "none", "idle" or "other"
    ev,         \* tags of what the last cut did: every tag when Track (the cover runs),
                \* else only "idleNoRetire" (IdleProgress), so baselines add no states
    crashes

vars == <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, export,
          acked, pending, fault, rewind, ev, crashes>>

-----------------------------------------------------------------------------
\* Holder roots of a generation (§13.6 "The probe": participating roots).

RootRefs(root) == IF root.on THEN {root.refs[x] : x \in Keys} ELSE {}

DurableRefs(g) ==
    {g.hrefs[x] : x \in Keys} \cup RootRefs(g.pin) \cup RootRefs(g.anchor) \cup RootRefs(g.fork)
    \cup g.queue

DurableColdRefs(g) == {r \in DurableRefs(g) : r.k = "C"}

\* §13.6 "Holders": the holder watermarks of main's chain in a generation. The pin
\* holds at its Snapshot's capture watermark; the fork row's parent side at the
\* row's watermark. Anchors are not cell holders here (cells are filed for pins and
\* forks only).
Watermarks(pinOn, pinW, forkOn, forkW) ==
    (IF pinOn THEN {pinW} ELSE {}) \cup (IF forkOn THEN {forkW} ELSE {})

\* §13.6 "Tiered-idle cuts": no holder root has a Tiered descriptor, the candidate
\* queue is empty and there is no anchor. A pin is always of a Tiered Snapshot here;
\* the fork's child is a branch head with its own residency.
IsIdleGen(g) == g.hres # "T" /\ ~g.pin.on /\ ~(g.fork.on /\ g.fork.res = "T")
                /\ g.queue = {} /\ ~g.anchor.on

-----------------------------------------------------------------------------
Init ==
    /\ head = [res |-> "T", refs |-> [x \in Keys |-> Hot(x)]]
    /\ gen = [hres |-> "T", hrefs |-> MemRefs, pin |-> NoRoot, queue |-> {},
              anchor |-> NoRoot, fork |-> NoFork, cells |-> {},
              listed |-> {}, live |-> [p \in Packs |-> {}]]
    /\ packRows = [p \in Packs |-> <<>>]
    /\ files = {}
    /\ nextPack = 1
    /\ nextStamp = NKeys + 1
    /\ cursor = [p \in Packs |-> NoCursor]
    /\ snap = [on |-> FALSE, refs |-> MemRefs, leased |-> {}, w |-> 0]
    /\ export = [on |-> FALSE, lease |-> {}, listed |-> {}, copied |-> FALSE, image |-> {}]
    /\ acked = {}
    /\ pending = FALSE
    /\ fault = FALSE
    /\ rewind = "none"
    /\ ev = {}
    /\ crashes = 0

-----------------------------------------------------------------------------
\* Commits and Snapshots (between cuts).

Write(x) ==
    /\ head.res = "T"
    /\ nextStamp <= MaxStamp
    /\ head' = [head EXCEPT !.refs[x] = Hot(nextStamp)]
    /\ nextStamp' = nextStamp + 1
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<gen, packRows, files, nextPack, cursor, snap, export, acked, pending, fault, crashes>>

\* §13.5 holder leases: a Snapshot that reaches Cold slots leases its generation;
\* AP16's Remove defers a pack while a lease names a generation that lists it.
Capture ==
    /\ head.res = "T"
    /\ ~snap.on
    /\ snap' = [on |-> TRUE, refs |-> head.refs, leased |-> gen.listed, w |-> nextStamp]
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, export, acked, pending, fault, crashes>>

DropSnap ==
    /\ snap.on
    /\ snap' = [snap EXCEPT !.on = FALSE, !.leased = {}]
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, export, acked, pending, fault, crashes>>

\* Mutant ack_before_publish: `pin` answers before its cut publishes (§1.9 says the
\* ticket is the cut's generation).
RequestPin ==
    /\ Mutant = "ack_before_publish"
    /\ snap.on /\ ~gen.pin.on /\ ~pending
    /\ pending' = TRUE
    /\ acked' = acked \cup {"s"}
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, export, fault, crashes>>

-----------------------------------------------------------------------------
\* The cut. op is the holder change H; sel is a pack newly chosen for a
\* compaction probe (0 for none). Selection by compact_ratio or repair_cuts is
\* left free: any listed pack without a probe can be chosen. dr says whether the
\* drain runs: it is optional (§13.6a T8 step 4 runs it "unless its own allocation
\* is refused", and free_budget bounds it; review 3177 F2 of the model brief).
\* pg says whether the cut keeps its probe progress: a refused allocation or the
\* mandatory-only retry drops the probe step, its relocation and the Tiered-idle
\* step alike, and the installed cursors stand (§13.6 "Tiered-idle cuts", AP16
\* item 3).
\*
\* op "fork" is a live-head fork of main (§1.9): the child's head shares main's
\* roots, and the fork row holds main's chain at the row's watermark (§13.6
\* "Holders": the fork's parent side). A fork of a Memory head has no Tiered
\* descriptor, so a generation can be Tiered-idle while the row's watermark files
\* drained frames into holder cells (review 3177 F2 trace A, F3). op "unfork" is
\* `delete_branch` of the child: its Cold roots that no other holder shares go to
\* the candidate queue as added rows (§13.6 "Deferred frees"), and the row goes
\* with it (ghost rows are abstracted: the model has one branch id per queue row).

Ops == {"plain", "pin", "unpin", "toMemory", "toTiered", "archive", "unarchive",
        "fork", "unfork"}

\* v2.164: the packs a Tiered-idle cut retires with probe_budget = 1. It takes
\* listed class-3/4 packs in sequence order and finishes one it starts, so one
\* budget row takes exactly the lowest-numbered listed pack (§13.6 "A Tiered-idle
\* cut decides whole packs"). Mutant idle_selected_only keeps v2.161's wording
\* before review 3177 F2: only a pack a probe already selected.
IdleTake(listed, cur) ==
    LET cand == IF Mutant = "idle_selected_only"
                THEN {p \in listed : cur[p].on} ELSE listed
    IN IF cand = {} THEN {} ELSE {MinOf(cand)}

Cut(op, sel, dr, pg) ==
    /\ (op = "pin") => (snap.on /\ ~gen.pin.on /\ (Mutant = "ack_before_publish" => pending))
    /\ (op = "unpin") => gen.pin.on
    /\ (op = "toMemory") => head.res = "T"
    /\ (op = "toTiered") => (head.res = "M" /\ nextStamp + NKeys - 1 <= MaxStamp)
    /\ (op = "archive") => (WithAnchors /\ ~gen.anchor.on /\ head.res = "T")
    /\ (op = "unarchive") => gen.anchor.on
    /\ (op = "fork") => (WithFork /\ ~gen.fork.on)
    /\ (op = "unfork") => gen.fork.on
    /\ sel \in {0} \cup {p \in gen.listed : ~cursor[p].on}
    /\ (~pg => sel = 0)
    \* Only branch where the choice can matter: a skipped drain needs an installed
    \* row, and a dropped probe step needs a listed pack.
    /\ (~dr => gen.queue # {})
    /\ (~pg => gen.listed # {})
    /\ LET
        \* ---- H (AP16 item 0) ----
        hRes == CASE op = "toMemory" -> "M" [] op = "toTiered" -> "T" [] OTHER -> head.res
        hRefs0 == CASE op = "toMemory" -> MemRefs
                    [] op = "toTiered" -> [x \in Keys |-> Hot(nextStamp + x - 1)]
                    [] OTHER -> head.refs
        stampOut == IF op = "toTiered" THEN nextStamp + NKeys ELSE nextStamp
        pinOn == CASE op = "pin" -> TRUE [] op = "unpin" -> FALSE [] OTHER -> gen.pin.on
        pinW == IF op = "pin" THEN snap.w ELSE gen.pin.w
        anchorOn == CASE op = "archive" -> TRUE [] op = "unarchive" -> FALSE [] OTHER -> gen.anchor.on
        anchorRefs0 == IF op = "archive" THEN head.refs ELSE gen.anchor.refs
        \* A live-head fork shares main's head (before H, which changes nothing else here).
        forkOn == CASE op = "fork" -> TRUE [] op = "unfork" -> FALSE [] OTHER -> gen.fork.on
        forkRes == IF op = "fork" THEN head.res ELSE gen.fork.res
        forkRefs0 == IF op = "fork" THEN head.refs ELSE gen.fork.refs
        forkW == IF op = "fork" THEN nextStamp ELSE gen.fork.w
        \* ---- the candidate queue's added rows (T8 step 3) ----
        \* §13.8: Tiered -> Memory hands the old Cold root to the queue.
        addMem == IF op = "toMemory" THEN {r \in {head.refs[x] : x \in Keys} : r.k = "C"} ELSE {}
        \* `delete_branch`: the child's released Cold roots no published holder shares.
        keepRefs == {hRefs0[x] : x \in Keys}
                    \cup (IF pinOn THEN {gen.pin.refs[x] : x \in Keys} ELSE {})
                    \cup (IF anchorOn THEN {anchorRefs0[x] : x \in Keys} ELSE {})
        addFork == IF op = "unfork"
                   THEN {r \in {gen.fork.refs[x] : x \in Keys} : r.k = "C" /\ r \notin keepRefs}
                   ELSE {}
        added == addMem \cup addFork
        qPart == gen.queue \cup added            \* installed rows plus this cut's added rows
        \* ---- Tiered-idle: a property of the generation this cut publishes ----
        \* §13.6 "Tiered-idle cuts": no holder root (branch head, pin, import pin)
        \* has a Tiered descriptor, the candidate queue (installed rows and this cut's
        \* added rows) is empty, and there is no anchor. Mutant idle_predicts_drain is
        \* review 3177 F1's unsound reading: idle when the drain is predicted to empty
        \* the queue (no added row and at most one installed row, free_budget 1),
        \* decided before the drain runs. Mutant idle_ignores_forks drops the child.
        queueEmpty == IF Mutant = "idle_predicts_drain"
                      THEN added = {} /\ Cardinality(gen.queue) <= 1
                      ELSE qPart = {}
        idleGen == /\ hRes # "T"
                   /\ (Mutant # "idle_ignores_pins" => ~pinOn)
                   /\ (Mutant # "idle_ignores_forks" => ~(forkOn /\ forkRes = "T"))
                   /\ queueEmpty
                   /\ ~anchorOn
        v2164Idle == Rules = "v2164" /\ idleGen
        \* count3157 (and the restart reading) drop an idle-advanced cursor at a non-idle cut.
        resetIdle == Rules \in {"count3157", "v2157_restart"} /\ ~idleGen
        cur0 == [p \in Packs |->
                    IF resetIdle /\ cursor[p].on /\ cursor[p].idle
                    THEN [cursor[p] EXCEPT !.rows = 0, !.known = 0, !.idle = FALSE]
                    ELSE cursor[p]]
        \* ---- promotion (§13.5 steps 2-3) ----
        lookup == {hRefs0[x] : x \in Keys} \cup qPart
                  \cup (IF anchorOn THEN {anchorRefs0[x] : x \in Keys} ELSE {})
                  \cup (IF forkOn THEN {forkRefs0[x] : x \in Keys} ELSE {})
        Found(s) == {r \in lookup : r.k \in {"C", "H"} /\ r.s = s}
        \* A PagePtr carries (pack, offset, len, checksum, stamp), never a PACK_INDEX row
        \* (§11 "Pages", AP20). So "behind the cursor" is decided by the cursor's offset,
        \* which only a reading probe knows (`known`).
        Behind(r) == cur0[r.p].on /\ r.i <= cur0[r.p].known
        Promote(r) ==
            IF r.k = "M" THEN r
            ELSE IF Found(r.s) # {} THEN CHOOSE f \in Found(r.s) : TRUE
            ELSE IF r.k = "H" THEN r
            ELSE IF r.p \in gen.listed /\ (Mutant = "inplace_behind_cursor" \/ ~Behind(r)) THEN r
            ELSE Hot(r.s)                         \* copied with its stamp (into the fresh pack)
        pinRefs1 == IF op = "pin" THEN [x \in Keys |-> Promote(snap.refs[x])] ELSE gen.pin.refs
        copyFault == op = "pin" /\ \E x \in Keys :
                        LET r == snap.refs[x] IN
                        r.k = "C" /\ Found(r.s) = {} /\ r.p \notin gen.listed /\ r.p \notin files
        \* ---- T2: emit every Hot page of every published root into the fresh pack ----
        roots1 == {hRefs0[x] : x \in Keys}
                  \cup (IF pinOn THEN {pinRefs1[x] : x \in Keys} ELSE {})
                  \cup (IF anchorOn THEN {anchorRefs0[x] : x \in Keys} ELSE {})
                  \cup (IF forkOn THEN {forkRefs0[x] : x \in Keys} ELSE {})
        emitStamps == {r.s : r \in {y \in roots1 : y.k = "H"}}
        emitSeq == Sorted(emitStamps)
        fresh == nextPack
        EmitIdx(s) == CHOOSE j \in 1..Len(emitSeq) : emitSeq[j] = s
        Seal(r) == IF r.k = "H" THEN Cold(fresh, EmitIdx(r.s), r.s) ELSE r
        hRefs2 == [x \in Keys |-> Seal(hRefs0[x])]
        pinRefs2 == [x \in Keys |-> Seal(pinRefs1[x])]
        anchorRefs2 == [x \in Keys |-> Seal(anchorRefs0[x])]
        forkRefs2 == [x \in Keys |-> Seal(forkRefs0[x])]
        \* ---- AP16 item 3: one probe step (probe_budget = 1, shared in pack order) ----
        named == {hRefs2[x] : x \in Keys}
                 \cup (IF pinOn THEN {pinRefs2[x] : x \in Keys} ELSE {})
                 \cup (IF anchorOn THEN {anchorRefs2[x] : x \in Keys} ELSE {})
                 \cup (IF forkOn THEN {forkRefs2[x] : x \in Keys} ELSE {})
                 \cup (IF Mutant = "probe_ignores_queue" THEN {} ELSE qPart)
        \* v2.164: an idle cut never installs a cursor (§13.6 "No cursor ever covers
        \* an unread prefix").
        cur1 == IF sel # 0 /\ ~v2164Idle
                THEN [cur0 EXCEPT ![sel] = [on |-> TRUE, rows |-> 0, known |-> 0, idle |-> FALSE]]
                ELSE cur0
        active == IF pg /\ ~v2164Idle THEN {p \in Packs : cur1[p].on} ELSE {}
        tp == IF active = {} THEN 0 ELSE MinOf(active)
        row == IF tp = 0 THEN 0 ELSE cur1[tp].rows + 1
        loc == IF tp = 0 THEN Mem ELSE Cold(tp, row, packRows[tp][row])
        matched == tp # 0 /\ ~idleGen /\ loc \in named   \* an idle step reads nothing
        copyRow == Len(emitSeq) + 1
        Move(r) == IF matched /\ r = loc THEN Cold(fresh, copyRow, loc.s) ELSE r
        hRefs3 == [x \in Keys |-> Move(hRefs2[x])]
        pinRefs3 == [x \in Keys |-> Move(pinRefs2[x])]
        anchorRefs3 == [x \in Keys |-> Move(anchorRefs2[x])]
        forkRefs3 == [x \in Keys |-> Move(forkRefs2[x])]
        finishes == tp # 0 /\ row = Len(packRows[tp])
        idleRetired == IF pg /\ v2164Idle THEN IdleTake(gen.listed, cur0) ELSE {}
        \* ---- §13.6 "Deferred frees": drain one installed row this cut did not move ----
        eligible == {r \in gen.queue : Move(r) = r}
        drained == IF ~dr \/ eligible = {} THEN {} ELSE {CHOOSE r \in eligible : TRUE}
        queueOut == {Move(r) : r \in qPart} \ drained
        \* ---- accounting: `live` and cells are advisory (§13.6 "What drifts") ----
        holderOut == {hRefs3[x] : x \in Keys}
                     \cup (IF pinOn THEN {pinRefs3[x] : x \in Keys} ELSE {})
                     \cup (IF anchorOn THEN {anchorRefs3[x] : x \in Keys} ELSE {})
                     \cup (IF forkOn THEN {forkRefs3[x] : x \in Keys} ELSE {})
        replaced == {r \in {gen.hrefs[x] : x \in Keys} : r.k = "C" /\ r \notin holderOut /\ r \notin added}
        killed == {r \in drained \cup replaced : r \notin holderOut}
        \* §13.6 "Filing a kill" against B_new: dead when no holder watermark is
        \* above the stamp, else into the newest holder's cell.
        wOld == Watermarks(gen.pin.on, gen.pin.w, gen.fork.on, gen.fork.w)
        wNew == Watermarks(pinOn, pinW, forkOn, forkW)
        filedCells == {[k |-> Max(wNew), p |-> r.p, s |-> r.s] :
                         r \in {y \in killed : \E w \in wNew : w > y.s}}
        filedDead == {y \in killed : ~\E w \in wNew : w > y.s}
        \* §13.6 "Dropping": a holder cell whose watermark left B moves to the next
        \* lower holder watermark j, or is dead when there is none or s >= j.
        Below(k) == {w \in wNew : w < k}
        dropDead == {c \in gen.cells : c.k \notin wNew /\ (Below(c.k) = {} \/ c.s >= Max(Below(c.k)))}
        dropMoved == {[c EXCEPT !.k = Max(Below(c.k))] :
                        c \in {d \in gen.cells : d.k \notin wNew /\ d \notin dropDead}}
        cellsDropped == {c \in gen.cells : c.k \in wNew} \cup dropMoved \cup filedCells
        \* §13.6 "Relocation moves cells": a moved frame's cells follow it.
        cellsMoved == {IF matched /\ c.p = loc.p /\ c.s = loc.s THEN [c EXCEPT !.p = fresh] ELSE c :
                         c \in cellsDropped}
        needFresh == emitStamps # {} \/ matched
        freshRows == emitSeq \o (IF matched THEN <<loc.s>> ELSE <<>>)
        DeadAt(p) == {r.s : r \in {y \in filedDead : y.p = p}} \cup {c.s : c \in {d \in dropDead : d.p = p}}
        live1 == [p \in Packs |->
                    IF needFresh /\ p = fresh THEN {freshRows[j] : j \in 1..Len(freshRows)}
                    ELSE {s \in gen.live[p] :
                            /\ s \notin DeadAt(p)
                            /\ ~(matched /\ loc.p = p /\ loc.s = s)}]
        \* ---- retirement: only by a finished probe (Fable F6) or a Tiered-idle
        \* cut (D-TI-1), plus the mutant's rule ----
        zeroLive == IF Mutant = "zero_live_retires"
                    THEN {p \in gen.listed : gen.live[p] = {} /\ Len(packRows[p]) > 0}
                    ELSE {}
        retired == (IF finishes THEN {tp} ELSE {}) \cup idleRetired \cup zeroLive
        \* §13.6 "Probe completion and retirement" (and T8 step 7): the cut that
        \* retires a pack deletes every cell naming it. Mutant idle_keeps_cells skips
        \* the deletion at a Tiered-idle retirement (review 3177 F3).
        cellsOut == {c \in cellsMoved :
                        c.p \notin retired
                        \/ (Mutant = "idle_keeps_cells" /\ c.p \in idleRetired)}
        cur2 == [p \in Packs |->
                    IF p \in retired THEN NoCursor
                    ELSE IF p # tp THEN cur1[p]
                    ELSE IF idleGen THEN [cur1[p] EXCEPT !.rows = row, !.idle = TRUE]
                    ELSE [cur1[p] EXCEPT !.rows = row, !.known = row]]
        \* §13.6 "Progress": did this cut probe a row behind an installed cursor?
        rew == IF tp # 0 /\ cursor[tp].on /\ row <= cursor[tp].rows
               THEN (IF cursor[tp].idle THEN "idle" ELSE "other") ELSE "none"
        \* What the cut did, for the cover properties and IdleProgress.
        noRetire == pg /\ v2164Idle /\ gen.listed # {} /\ idleRetired = {}
        tags == (IF noRetire THEN {"idleNoRetire"} ELSE {})
                \cup (IF idleRetired # {} THEN {"idleRetire"} ELSE {})
                \cup (IF \E p \in idleRetired : ~cursor[p].on THEN {"idleUnselected"} ELSE {})
                \cup (IF \E p \in idleRetired : cursor[p].on THEN {"idleCursor"} ELSE {})
                \cup (IF \E c \in gen.cells : c.p \in idleRetired THEN {"idleCells"} ELSE {})
                \cup (IF ~pg /\ Rules = "v2164" /\ idleGen /\ gen.listed # {} THEN {"idleDropped"} ELSE {})
                \cup (IF gen.queue # {} /\ queueOut = {} /\ ~idleGen THEN {"drainNotIdle"} ELSE {})
                \cup (IF ~dr /\ eligible # {} THEN {"drainSkipped"} ELSE {})
                \cup (IF addFork # {} THEN {"unforkQueued"} ELSE {})
                \cup (IF filedCells # {} THEN {"cellFiled"} ELSE {})
                \cup (IF dropMoved # {} THEN {"cellMoved"} ELSE {})
                \cup (IF idleGen /\ (pinOn \/ forkOn) /\ cellsOut # {} THEN {"idleHolderCells"} ELSE {})
       IN
        /\ (needFresh => nextPack <= MaxPack)
        /\ (v2164Idle => sel = 0)
        /\ gen' = [hres |-> hRes, hrefs |-> hRefs3,
                   pin |-> IF pinOn THEN [on |-> TRUE, refs |-> pinRefs3, w |-> pinW] ELSE NoRoot,
                   queue |-> queueOut,
                   anchor |-> IF anchorOn THEN [on |-> TRUE, refs |-> anchorRefs3, w |-> 0] ELSE NoRoot,
                   fork |-> IF forkOn THEN [on |-> TRUE, w |-> forkW, res |-> forkRes, refs |-> forkRefs3]
                            ELSE NoFork,
                   cells |-> cellsOut,
                   listed |-> (gen.listed \ retired) \cup (IF needFresh THEN {fresh} ELSE {}),
                   live |-> live1]
        /\ head' = [res |-> hRes, refs |-> hRefs3]
        /\ packRows' = IF needFresh THEN [packRows EXCEPT ![fresh] = freshRows] ELSE packRows
        /\ files' = IF needFresh THEN files \cup {fresh} ELSE files
        /\ nextPack' = IF needFresh THEN nextPack + 1 ELSE nextPack
        /\ nextStamp' = stampOut
        /\ cursor' = cur2
        /\ fault' = (fault \/ copyFault)
        /\ rewind' = rew
        /\ ev' = IF Track THEN tags ELSE (IF noRetire THEN {"idleNoRetire"} ELSE {})
        /\ acked' = CASE op = "pin" -> acked \cup {"s"}
                      [] op = "unpin" -> acked \ {"s"}
                      [] OTHER -> acked
        /\ pending' = (IF op = "pin" THEN FALSE ELSE pending)
        /\ UNCHANGED <<snap, export, crashes>>
-----------------------------------------------------------------------------
\* AP16 Remove: delete a delisted pack's file once no lease names it.
RemoveFile(p) ==
    /\ p \in files \ gen.listed
    /\ \/ Mutant = "remove_ignores_lease"
       \/ /\ ~(snap.on /\ p \in snap.leased)
          /\ ~(export.on /\ p \in export.lease)
    /\ files' = files \ {p}
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, nextPack, nextStamp, cursor, snap, export, acked, pending, fault, crashes>>

\* §13.10a (v2.157, D-ZL-2): the export lease names every listed class-3/4 pack.
BeginExport ==
    /\ WithExport /\ ~export.on
    /\ LET lease == IF Mutant = "positive_live_lease"
                    THEN {p \in gen.listed : gen.live[p] # {}}
                    ELSE gen.listed
       IN export' = [on |-> TRUE, lease |-> lease, listed |-> gen.listed, copied |-> FALSE, image |-> {}]
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, acked, pending, fault, crashes>>

\* The copy task copies each leased pack byte for byte while the store serves.
CopyDone ==
    /\ export.on /\ ~export.copied
    /\ export' = [export EXCEPT !.copied = TRUE, !.image = export.lease \cap files]
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, acked, pending, fault, crashes>>

ReleaseExport ==
    /\ export.on /\ export.copied
    /\ export' = [on |-> FALSE, lease |-> {}, listed |-> {}, copied |-> FALSE, image |-> {}]
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, acked, pending, fault, crashes>>

\* Crash and reopen: writer memory is lost (cursors, Snapshots and their leases,
\* the export attempt). Open removes every delisted pack (§3.5 step 5). The head's
\* commits are in the journal and come back through replay (§13.7).
Crash ==
    /\ crashes < MaxCrash
    /\ crashes' = crashes + 1
    /\ cursor' = [p \in Packs |-> NoCursor]
    /\ snap' = [on |-> FALSE, refs |-> MemRefs, leased |-> {}, w |-> 0]
    /\ export' = [on |-> FALSE, lease |-> {}, listed |-> {}, copied |-> FALSE, image |-> {}]
    /\ pending' = FALSE
    /\ files' = files \cap gen.listed
    /\ rewind' = "none" /\ ev' = {}
    /\ UNCHANGED <<head, gen, packRows, nextPack, nextStamp, acked, fault>>

Next ==
    \/ \E x \in Keys : Write(x)
    \/ Capture
    \/ DropSnap
    \/ RequestPin
    \/ \E op \in Ops, sel \in {0} \cup Packs, dr \in BOOLEAN, pg \in BOOLEAN : Cut(op, sel, dr, pg)
    \/ \E p \in Packs : RemoveFile(p)
    \/ BeginExport
    \/ CopyDone
    \/ ReleaseExport
    \/ Crash

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
\* Safety invariants. README.md maps each to its STORE-SPEC section.

\* R1 (§13.6 "The probe", §13.5 step 2 "Resolution frees nothing", §1.9): every
\* frame a durable holder names (head, pin or fork, anchor, queue row) is in a
\* listed pack whose file exists, at the row that holds its stamp.
RetainReachable ==
    \A r \in DurableColdRefs(gen) :
        /\ r.p \in gen.listed
        /\ r.p \in files
        /\ r.i \in 1..Len(packRows[r.p])
        /\ packRows[r.p][r.i] = r.s

\* R2 (§13.5 holder leases, AP16 Remove): an open reader's frames stay readable,
\* and a promotion never reads a removed frame.
SnapReadable ==
    /\ ~fault
    /\ snap.on => \A x \in Keys : snap.refs[x].k = "C" => snap.refs[x].p \in files

\* R3 (§13.10a v2.157): every pack the copied MANIFEST lists is in the image.
ImageComplete == export.copied => export.listed \subseteq export.image

\* R4 (§1.9, §3.5 step 6): an acknowledged pin survives every crash.
AckedDurable == \A n \in acked : gen.pin.on

\* P1 (§13.6 "Tiered-idle cuts", I-TI): a Tiered-idle generation reaches no class-3/4 frame.
IdleReachesNoFrame == IsIdleGen(gen) => DurableColdRefs(gen) = {}

\* P2 (§13.6 "A probe across cuts"): no participating root of the published
\* generation names a frame before an unfinished compaction probe's cursor.
ProbeClaim ==
    \A p \in Packs : cursor[p].on =>
        \A r \in DurableColdRefs(gen) : r.p = p => r.i > cursor[p].rows

\* P3 (§13.6 "Progress"): only a crash or a compact_ratio reselection resets a
\* cursor. count3157 adds a third reset, an idle-advanced cursor at a non-idle cut.
Progress == rewind = "none" \/ (Rules = "count3157" /\ rewind = "idle")

\* P4 (§13.6 "Probe completion and retirement", "A Tiered-idle cut decides whole
\* packs", T8 step 7): no lineage cell names a pack that has left the list. A cut
\* that retires a pack deletes its cells; one that kept them would later subtract
\* from a pack nothing lists (review 3177 F3).
CellsListed == \A c \in gen.cells : c.p \in gen.listed

\* P5 (§13.6 "Progress", v2.164; review 3177 F2): a Tiered-idle cut that keeps its
\* progress and lists a class-3/4 pack retires at least one, selected or not.
IdleProgress == Rules # "v2164" \/ "idleNoRetire" \notin ev

-----------------------------------------------------------------------------
\* Reachability witnesses. check_proofs.py requires TLC to violate each one: a
\* violation is a trace in which the behaviour happens, so the fold is not vacuous.
NoIdleRetire        == "idleRetire" \notin ev        \* an idle cut retires a pack whole
NoIdleUnselected    == "idleUnselected" \notin ev    \* ... one no probe selected (v2.164, F2)
NoIdleCursor        == "idleCursor" \notin ev        \* ... one a reading probe had a cursor in
NoIdleCells         == "idleCells" \notin ev         \* ... one lineage cells named (F3)
NoIdleHolderCells   == "idleHolderCells" \notin ev   \* an idle generation with a holder and cells (F3)
NoIdleDropped       == "idleDropped" \notin ev       \* a refused idle step retires nothing
NoDrainNotIdle      == "drainNotIdle" \notin ev      \* the drain empties the queue at a non-idle cut (F1)
NoDrainSkipped      == "drainSkipped" \notin ev      \* the drain does not run (F2)
NoUnforkQueued      == "unforkQueued" \notin ev      \* delete_branch hands Cold roots to the queue
NoCellFiled         == "cellFiled" \notin ev         \* a kill is filed into a holder cell
NoCellMoved         == "cellMoved" \notin ev         \* a dropped boundary moves a cell down

=============================================================================
