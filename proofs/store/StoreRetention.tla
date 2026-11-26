--------------------------- MODULE StoreRetention ---------------------------
(*
 * Bounded model of Hirsel Store Tiered pack retention (STORE-SPEC §1.9, §3.5,
 * §13.5 "Promoting a historical Snapshot", §13.6 deadlists, the probe and
 * Tiered-idle cuts, §13.9 anchors, §13.10a the export lease).
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
 *   "v2161"         v2.161 (D-TI-1), the current text: an idle cut reads nothing,
 *                   never installs or moves a cursor, and retires the first selected
 *                   pack whole (budget 1 takes one pack); promotion copies a frame of
 *                   a pack an idle cut retired through the Snapshot's lease.
 *   "count3157"     review 3157's alternative, which v2.161 rejects as sound but
 *                   costlier: idle progress is a count apart from the probe cursor,
 *                   and the first non-idle cut restarts it at row 0, as a third reset.
 * Mutant names one seeded one-rule bug, or "none". README.md lists them.
 *)
EXTENDS Naturals, Sequences, FiniteSets, TLC

CONSTANTS NKeys, MaxStamp, MaxPack, MaxCrash, Rules, Mutant, WithAnchors, WithExport

ASSUME Rules \in {"v2157", "v2157_restart", "count3157", "v2161"}

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

NoRoot == [on |-> FALSE, refs |-> MemRefs]
NoCursor == [on |-> FALSE, rows |-> 0, known |-> 0, idle |-> FALSE]

MinOf(S) == CHOOSE x \in S : \A y \in S : x <= y

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
    crashes

vars == <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, export,
          acked, pending, fault, rewind, crashes>>

-----------------------------------------------------------------------------
\* Holder roots of a generation (§13.6 "The probe": participating roots).

RootRefs(root) == IF root.on THEN {root.refs[x] : x \in Keys} ELSE {}

DurableRefs(g) ==
    {g.hrefs[x] : x \in Keys} \cup RootRefs(g.pin) \cup RootRefs(g.anchor) \cup g.queue

DurableColdRefs(g) == {r \in DurableRefs(g) : r.k = "C"}

\* §13.6 "Tiered-idle cuts": no holder root has a Tiered descriptor, the candidate
\* queue is empty and there is no anchor. A pin is always of a Tiered Snapshot here.
IsIdleGen(g) == g.hres # "T" /\ ~g.pin.on /\ g.queue = {} /\ ~g.anchor.on

-----------------------------------------------------------------------------
Init ==
    /\ head = [res |-> "T", refs |-> [x \in Keys |-> Hot(x)]]
    /\ gen = [hres |-> "T", hrefs |-> MemRefs, pin |-> NoRoot, queue |-> {},
              anchor |-> NoRoot, listed |-> {}, live |-> [p \in Packs |-> {}]]
    /\ packRows = [p \in Packs |-> <<>>]
    /\ files = {}
    /\ nextPack = 1
    /\ nextStamp = NKeys + 1
    /\ cursor = [p \in Packs |-> NoCursor]
    /\ snap = [on |-> FALSE, refs |-> MemRefs, leased |-> {}]
    /\ export = [on |-> FALSE, lease |-> {}, listed |-> {}, copied |-> FALSE, image |-> {}]
    /\ acked = {}
    /\ pending = FALSE
    /\ fault = FALSE
    /\ rewind = "none"
    /\ crashes = 0

-----------------------------------------------------------------------------
\* Commits and Snapshots (between cuts).

Write(x) ==
    /\ head.res = "T"
    /\ nextStamp <= MaxStamp
    /\ head' = [head EXCEPT !.refs[x] = Hot(nextStamp)]
    /\ nextStamp' = nextStamp + 1
    /\ rewind' = "none"
    /\ UNCHANGED <<gen, packRows, files, nextPack, cursor, snap, export, acked, pending, fault, crashes>>

\* §13.5 holder leases: a Snapshot that reaches Cold slots leases its generation;
\* AP16's Remove defers a pack while a lease names a generation that lists it.
Capture ==
    /\ head.res = "T"
    /\ ~snap.on
    /\ snap' = [on |-> TRUE, refs |-> head.refs, leased |-> gen.listed]
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, export, acked, pending, fault, crashes>>

DropSnap ==
    /\ snap.on
    /\ snap' = [snap EXCEPT !.on = FALSE, !.leased = {}]
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, export, acked, pending, fault, crashes>>

\* Mutant ack_before_publish: `pin` answers before its cut publishes (§1.9 says the
\* ticket is the cut's generation).
RequestPin ==
    /\ Mutant = "ack_before_publish"
    /\ snap.on /\ ~gen.pin.on /\ ~pending
    /\ pending' = TRUE
    /\ acked' = acked \cup {"s"}
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, export, fault, crashes>>

-----------------------------------------------------------------------------
\* The cut. op is the holder change H; sel is a pack newly chosen for a
\* compaction probe (0 for none). Selection by compact_ratio or repair_cuts is
\* left free: any listed pack without a probe can be chosen.

Ops == {"plain", "pin", "unpin", "toMemory", "toTiered", "archive", "unarchive"}

Cut(op, sel) ==
    /\ (op = "pin") => (snap.on /\ ~gen.pin.on /\ (Mutant = "ack_before_publish" => pending))
    /\ (op = "unpin") => gen.pin.on
    /\ (op = "toMemory") => head.res = "T"
    /\ (op = "toTiered") => (head.res = "M" /\ nextStamp + NKeys - 1 <= MaxStamp)
    /\ (op = "archive") => (WithAnchors /\ ~gen.anchor.on /\ head.res = "T")
    /\ (op = "unarchive") => gen.anchor.on
    /\ sel \in {0} \cup {p \in gen.listed : ~cursor[p].on}
    /\ LET
        \* ---- H (AP16 item 0) ----
        hRes == CASE op = "toMemory" -> "M" [] op = "toTiered" -> "T" [] OTHER -> head.res
        hRefs0 == CASE op = "toMemory" -> MemRefs
                    [] op = "toTiered" -> [x \in Keys |-> Hot(nextStamp + x - 1)]
                    [] OTHER -> head.refs
        stampOut == IF op = "toTiered" THEN nextStamp + NKeys ELSE nextStamp
        \* §13.8: Tiered -> Memory hands the old Cold root to the queue.
        added == IF op = "toMemory" THEN {r \in {head.refs[x] : x \in Keys} : r.k = "C"} ELSE {}
        qPart == gen.queue \cup added            \* installed rows plus this cut's added rows
        pinOn == CASE op = "pin" -> TRUE [] op = "unpin" -> FALSE [] OTHER -> gen.pin.on
        anchorOn == CASE op = "archive" -> TRUE [] op = "unarchive" -> FALSE [] OTHER -> gen.anchor.on
        anchorRefs0 == IF op = "archive" THEN head.refs ELSE gen.anchor.refs
        \* ---- Tiered-idle: a property of the generation this cut publishes ----
        idleGen == /\ hRes # "T"
                   /\ (Mutant # "idle_ignores_pins" => ~pinOn)
                   /\ qPart = {}
                   /\ ~anchorOn
        \* count3157 (and the restart reading) drop an idle-advanced cursor at a non-idle cut.
        resetIdle == Rules \in {"count3157", "v2157_restart"} /\ ~idleGen
        cur0 == [p \in Packs |->
                    IF resetIdle /\ cursor[p].on /\ cursor[p].idle
                    THEN [cursor[p] EXCEPT !.rows = 0, !.known = 0, !.idle = FALSE]
                    ELSE cursor[p]]
        \* ---- promotion (§13.5 steps 2-3) ----
        lookup == {hRefs0[x] : x \in Keys} \cup qPart
                  \cup (IF anchorOn THEN {anchorRefs0[x] : x \in Keys} ELSE {})
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
        emitStamps == {r.s : r \in {y \in roots1 : y.k = "H"}}
        emitSeq == Sorted(emitStamps)
        fresh == nextPack
        EmitIdx(s) == CHOOSE j \in 1..Len(emitSeq) : emitSeq[j] = s
        Seal(r) == IF r.k = "H" THEN Cold(fresh, EmitIdx(r.s), r.s) ELSE r
        hRefs2 == [x \in Keys |-> Seal(hRefs0[x])]
        pinRefs2 == [x \in Keys |-> Seal(pinRefs1[x])]
        anchorRefs2 == [x \in Keys |-> Seal(anchorRefs0[x])]
        \* ---- AP16 item 3: one probe step (probe_budget = 1, shared in pack order) ----
        named == {hRefs2[x] : x \in Keys}
                 \cup (IF pinOn THEN {pinRefs2[x] : x \in Keys} ELSE {})
                 \cup (IF anchorOn THEN {anchorRefs2[x] : x \in Keys} ELSE {})
                 \cup (IF Mutant = "probe_ignores_queue" THEN {} ELSE qPart)
        cur1 == IF sel # 0 THEN [cur0 EXCEPT ![sel] = [on |-> TRUE, rows |-> 0, known |-> 0, idle |-> FALSE]]
                ELSE cur0
        active == {p \in Packs : cur1[p].on}
        tp == IF active = {} THEN 0 ELSE MinOf(active)
        row == IF tp = 0 THEN 0 ELSE cur1[tp].rows + 1
        loc == IF tp = 0 THEN Mem ELSE Cold(tp, row, packRows[tp][row])
        matched == tp # 0 /\ ~idleGen /\ loc \in named   \* an idle step reads nothing
        copyRow == Len(emitSeq) + 1
        Move(r) == IF matched /\ r = loc THEN Cold(fresh, copyRow, loc.s) ELSE r
        hRefs3 == [x \in Keys |-> Move(hRefs2[x])]
        pinRefs3 == [x \in Keys |-> Move(pinRefs2[x])]
        anchorRefs3 == [x \in Keys |-> Move(anchorRefs2[x])]
        \* v2161: an idle cut takes the first selected pack and retires it whole.
        finishes == tp # 0 /\ (row = Len(packRows[tp]) \/ (Rules = "v2161" /\ idleGen))
        \* ---- §13.6 "Deferred frees": drain one installed row this cut did not move ----
        eligible == {r \in gen.queue : Move(r) = r}
        drained == IF eligible = {} THEN {} ELSE {CHOOSE r \in eligible : TRUE}
        queueOut == {Move(r) : r \in qPart} \ drained
        \* ---- accounting: `live` is advisory (§13.6 "What drifts") ----
        holderOut == {hRefs3[x] : x \in Keys}
                     \cup (IF pinOn THEN {pinRefs3[x] : x \in Keys} ELSE {})
                     \cup (IF anchorOn THEN {anchorRefs3[x] : x \in Keys} ELSE {})
        replaced == {r \in {gen.hrefs[x] : x \in Keys} : r.k = "C" /\ r \notin holderOut /\ r \notin added}
        killedDead == {r \in drained \cup replaced : r \notin holderOut}
        needFresh == emitStamps # {} \/ matched
        freshRows == emitSeq \o (IF matched THEN <<loc.s>> ELSE <<>>)
        live1 == [p \in Packs |->
                    IF needFresh /\ p = fresh THEN {freshRows[j] : j \in 1..Len(freshRows)}
                    ELSE {s \in gen.live[p] :
                            /\ ~\E r \in killedDead : r.p = p /\ r.s = s
                            /\ ~(matched /\ loc.p = p /\ loc.s = s)}]
        \* ---- retirement: only by a finished probe (Fable F6), plus the mutant's rule ----
        zeroLive == IF Mutant = "zero_live_retires"
                    THEN {p \in gen.listed : gen.live[p] = {} /\ Len(packRows[p]) > 0}
                    ELSE {}
        retired == (IF finishes THEN {tp} ELSE {}) \cup zeroLive
        cur2 == [p \in Packs |->
                    IF p \in retired THEN NoCursor
                    ELSE IF p # tp THEN cur1[p]
                    ELSE IF idleGen THEN [cur1[p] EXCEPT !.rows = row, !.idle = TRUE]
                    ELSE [cur1[p] EXCEPT !.rows = row, !.known = row]]
        \* §13.6 "Progress": did this cut probe a row behind an installed cursor?
        rew == IF tp # 0 /\ cursor[tp].on /\ row <= cursor[tp].rows
               THEN (IF cursor[tp].idle THEN "idle" ELSE "other") ELSE "none"
       IN
        /\ (needFresh => nextPack <= MaxPack)
        /\ gen' = [hres |-> hRes, hrefs |-> hRefs3,
                   pin |-> IF pinOn THEN [on |-> TRUE, refs |-> pinRefs3] ELSE NoRoot,
                   queue |-> queueOut,
                   anchor |-> IF anchorOn THEN [on |-> TRUE, refs |-> anchorRefs3] ELSE NoRoot,
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
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, nextPack, nextStamp, cursor, snap, export, acked, pending, fault, crashes>>

\* §13.10a (v2.157, D-ZL-2): the export lease names every listed class-3/4 pack.
BeginExport ==
    /\ WithExport /\ ~export.on
    /\ LET lease == IF Mutant = "positive_live_lease"
                    THEN {p \in gen.listed : gen.live[p] # {}}
                    ELSE gen.listed
       IN export' = [on |-> TRUE, lease |-> lease, listed |-> gen.listed, copied |-> FALSE, image |-> {}]
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, acked, pending, fault, crashes>>

\* The copy task copies each leased pack byte for byte while the store serves.
CopyDone ==
    /\ export.on /\ ~export.copied
    /\ export' = [export EXCEPT !.copied = TRUE, !.image = export.lease \cap files]
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, acked, pending, fault, crashes>>

ReleaseExport ==
    /\ export.on /\ export.copied
    /\ export' = [on |-> FALSE, lease |-> {}, listed |-> {}, copied |-> FALSE, image |-> {}]
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, files, nextPack, nextStamp, cursor, snap, acked, pending, fault, crashes>>

\* Crash and reopen: writer memory is lost (cursors, Snapshots and their leases,
\* the export attempt). Open removes every delisted pack (§3.5 step 5). The head's
\* commits are in the journal and come back through replay (§13.7).
Crash ==
    /\ crashes < MaxCrash
    /\ crashes' = crashes + 1
    /\ cursor' = [p \in Packs |-> NoCursor]
    /\ snap' = [on |-> FALSE, refs |-> MemRefs, leased |-> {}]
    /\ export' = [on |-> FALSE, lease |-> {}, listed |-> {}, copied |-> FALSE, image |-> {}]
    /\ pending' = FALSE
    /\ files' = files \cap gen.listed
    /\ rewind' = "none"
    /\ UNCHANGED <<head, gen, packRows, nextPack, nextStamp, acked, fault>>

Next ==
    \/ \E x \in Keys : Write(x)
    \/ Capture
    \/ DropSnap
    \/ RequestPin
    \/ \E op \in Ops, sel \in {0} \cup Packs : Cut(op, sel)
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

=============================================================================
