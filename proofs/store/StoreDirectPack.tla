------------------------- MODULE StoreDirectPack ----------------------------
EXTENDS StoreRetention

(*
 * One blob row, a writer, a reader lease and a durable Pin or DeferredFree
 * root (both impose the same pack-membership obligation). history records the
 * immutable closure at each base/GROUP. A put introduces one fresh pack.
 * Erase projects a later deletion in the same replay suffix, with no new pack
 * dependency. A cut emits one ordinary pack and may adopt a direct pack.
 * Archive uploads, publication, budget release, floor writes and delayed
 * erases are the ordinary StoreRetention actions on this very same history.
 *)
VARIABLES next, pendingPack, value, holder, lease, candidate, seen
local == <<next, pendingPack, value, holder, lease, candidate, seen>>
Tag(s) == IF Track THEN s ELSE ""
One(p) == IF p = 0 THEN {} ELSE {p}
Q == Current(archive)
Covered == LastBase(archive.history)
Listed == archive.history[Covered].packs
Journal == Objects(archive.history, (Covered+1)..Q)
Roots == One(value) \cup One(holder)
Leased == {Pack(p) : p \in One(lease)}
Protected == SourceNeeded(archive) \cup Leased \cup {Pack(p) : p \in One(pendingPack)}

Start ==
    /\ archive = ArchiveInitial /\ next = 2 /\ pendingPack = 0 /\ value = 0
    /\ holder = 0 /\ lease = 0 /\ candidate = 0 /\ seen = ""

Reserve ==
    /\ Base(Q) /\ pendingPack = 0 /\ next < MaxPack
    /\ pendingPack' = next /\ candidate' = next /\ next' = next+1
    /\ UNCHANGED <<archive, value, holder, lease, seen>>

Seal ==
    /\ pendingPack # 0 /\ Pack(pendingPack) \notin archive.source
    /\ archive' = [archive EXCEPT !.source = @ \cup {Pack(pendingPack)}]
    /\ UNCHANGED local

Abort ==
    /\ pendingPack # 0 /\ pendingPack' = 0
    /\ UNCHANGED <<archive, next, value, holder, lease, candidate, seen>>

Commit ==
    /\ pendingPack # 0 /\ Base(Q)
    /\ Mutant = "direct_commit_unsealed" \/ Pack(pendingPack) \in archive.source
    /\ archive' = [archive EXCEPT
           !.history = Append(@, HistoryUnit({pendingPack}, next)),
           !.source = @ \cup {Unit(Q+1)}]
    /\ value' = pendingPack /\ pendingPack' = 0
    /\ UNCHANGED <<next, holder, lease, candidate, seen>>

EraseValue ==
    /\ value # 0 /\ value' = 0
    /\ UNCHANGED <<archive, next, pendingPack, holder, lease, candidate, seen>>

CaptureLease ==
    /\ value # 0 /\ lease = 0 /\ lease' = value
    /\ UNCHANGED <<archive, next, pendingPack, value, holder, candidate, seen>>

ReleaseLease ==
    /\ lease # 0 /\ lease' = 0
    /\ UNCHANGED <<archive, next, pendingPack, value, holder, candidate, seen>>

\* Capture/release a durable holder in the covering cut itself. Existing
\* holders may survive overwrite. A snapshot can be promoted at a new
\* location; A2 disallows introducing a previously delisted old ordinal.
CutWith(h) ==
    LET adopted == One(value) \cup One(h)
        hist == IF Base(Q) THEN Append(archive.history, HistoryUnit({}, next))
                ELSE archive.history
        groupFile == IF Base(Q) THEN {Unit(Q+1)} ELSE {}
    IN /\ pendingPack = 0 /\ next <= MaxPack /\ Len(hist) < 5
       /\ h \in {0, holder, value}
       /\ archive' = CoveredCut(archive, hist, adopted \cup {next}, next+1,
                                groupFile \cup {Pack(next)})
       /\ holder' = h /\ next' = next+1
       /\ seen' = IF adopted # {} THEN Tag("adopt") ELSE seen
       /\ UNCHANGED <<pendingPack, value, lease, candidate>>

\* Two events: a cleanup observed the sealed name while it was still private;
\* the owner may since have attached it. Re-evaluate under exclusion.
SweepCandidate ==
    /\ candidate # 0 /\ Pack(candidate) \in archive.source
    /\ pendingPack # candidate
    /\ Mutant = "direct_stale_abort" \/ Pack(candidate) \notin Protected
    /\ archive' = [archive EXCEPT !.source = @ \ {Pack(candidate)}]
    /\ candidate' = 0
    /\ UNCHANGED <<next, value, holder, pendingPack, lease, seen>>

Reopen ==
    LET base == Closure(archive.history, Covered) \cup Objects(archive.history, archive.hold)
        segments == {k \in Journal : k[1] = "unit"}
        packs == {k \in Journal : k[1] = "pack"}
        \* The early-sweep control deletes only pack files. The referencing
        \* journal segment remains intact and can still be replayed.
        remaining == RecoverFiles(archive.source, base \cup segments, packs)
    IN /\ archive' = [archive EXCEPT !.source = remaining]
       /\ seen' = CASE pendingPack # 0 /\ Pack(pendingPack) \in archive.source /\ Pack(pendingPack) \notin remaining -> Tag("abandoned")
                   [] value # 0 /\ value \notin Listed /\ Pack(value) \in remaining -> Tag("reopened")
                   [] OTHER -> seen
       /\ pendingPack' = 0 /\ candidate' = 0 /\ lease' = 0
       /\ next' = archive.history[Q].frontier
       /\ UNCHANGED <<value, holder>>

\* Restore the GROUP exactly, while the only published base predates its
\* pack and the source has not cut. The reader requires every byte at sink.
Restore ==
    /\ ~Base(Q) /\ Q \in Named(archive) /\ value # 0 /\ value \notin Listed
    /\ Objects(archive.history, Covered..Q) \subseteq archive.sink
    /\ seen' = Tag("restored")
    /\ UNCHANGED <<archive, next, pendingPack, value, holder, lease, candidate>>

\* A read-only follower captured a GROUP descriptor. Reclaim may have run
\* since. Publish only when the exact objects were obtained and verified.
Follow(q) ==
    LET got == Closure(archive.history, q) \cap archive.source
    IN /\ ~Base(q) /\ Unit(q) \in got
       /\ Mutant = "direct_follow_early" \/ Closure(archive.history, q) = got
       /\ seen' = IF Closure(archive.history, q) # got THEN "followerLost" ELSE Tag("follow")
       /\ UNCHANGED <<archive, next, value, holder, lease, candidate, pendingPack>>

ArchiveIO ==
    \/ \E q \in 1..Q : ArchivePublish(q) \/ (\E k \in archive.source : ArchiveStage(q,k))
    \/ ArchiveGap
    \/ \E f \in 1..Q : ArchiveExpire(f) \/ (\E k \in archive.sink : ArchiveErase(f,k))
    \/ \E k \in archive.source \ Protected : ArchiveReclaim(k)

DirectNext ==
    \/ Reserve \/ Seal \/ Abort \/ Commit \/ EraseValue \/ CaptureLease \/ ReleaseLease
    \/ \E h \in {0, holder, value} : CutWith(h)
    \/ SweepCandidate \/ Reopen \/ Restore
    \/ \E q \in 1..Q : Follow(q)
    \/ ArchiveIO /\ UNCHANGED local

DirectSpec == (Init /\ Start) /\ [][DirectNext /\ UNCHANGED vars]_<<vars, archive, local>>
DirectReadable ==
    /\ {Pack(p) : p \in Roots \cup One(lease)} \subseteq archive.source
    /\ Journal \subseteq archive.source
    /\ Roots \subseteq Listed \cup archive.history[Q].packs
    /\ \A p \in Listed \cup archive.history[Q].packs : p < next
FollowerReadable == seen # "followerLost"
NoDirectReopen == seen # "reopened"
NoDirectRestore == seen # "restored"
NoDirectAbandon == seen # "abandoned"
NoDirectAdopt == seen # "adopt"
=============================================================================
