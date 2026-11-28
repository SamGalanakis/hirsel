------------------------- MODULE StorePreviewPack ---------------------------
EXTENDS StorePreviewHold

(*
 * Pack 2 belongs only to the held GROUP. Pack 3 is present at the sink but
 * is outside that boundary. The parent and child can both store good sealed
 * bytes. A row's locator is untrusted input; GET and frame verification alone
 * do not grant authority to use it. Model one child cut at a time; the
 * ordinary hold model checks concurrent writers/erasers at larger bounds.
 *)
VARIABLES row, captured, ownFile, answer, childErase
childVars == <<row, captured, ownFile, answer, childErase>>
Parent == 10
Child == 20
Genesis == [parent |-> Parent, inc |-> 1, child |-> Child, stamp |-> 1, q |-> 2]
Held == inc = Genesis.inc /\ holds[1].stamp = Genesis.stamp /\ holds[1].q = Genesis.q
Ref(owner, directory, pack) == [owner |-> owner, dir |-> directory, pack |-> pack]
Good == Ref(Parent, Genesis.inc, 2)
LocalRef == Ref(Child, 0, 4)
Empty == Ref(0,0,0)
Inputs == {Good, Ref(Parent, 1, 3), Ref(Child, 1, 2), Ref(Parent, 2, 2)}

ChildInit ==
    /\ archive = First /\ inc = 1
    /\ holds = [c \in Children |-> IF c = 1 THEN [q |-> 2, until |-> 1, stamp |-> 1] ELSE NoHold]
    /\ row = Empty /\ captured = Empty /\ ownFile = FALSE /\ answer = "idle" /\ childErase = {}

Validate(r) ==
    /\ Held /\ r.owner = Genesis.parent /\ r.dir = Genesis.inc
    /\ Mutant = "preview_unbound_locator" \/ Pack(r.pack) \in PreviewClosure(Genesis.q)

OpenRow(r) ==
    /\ row = Empty /\ Validate(r)
    /\ row' = r
    /\ UNCHANGED <<archive, inc, holds, captured, ownFile, answer, childErase>>

\* Capture before a copy-on-write cut. A foreign Snapshot still reads through
\* its captured resolver. Reclamation has delete authority on child keys only.
ChildCapture ==
    /\ captured = Empty /\ row # Empty /\ captured' = row
    /\ UNCHANGED <<archive, inc, holds, row, ownFile, answer, childErase>>

ChildDropCapture ==
    /\ captured # Empty /\ captured' = Empty
    /\ UNCHANGED <<archive, inc, holds, row, ownFile, answer, childErase>>

Compact ==
    /\ Held /\ row = Good /\ Pack(row.pack) \in archive.sink
    /\ ownFile' = TRUE /\ row' = LocalRef
    /\ UNCHANGED <<archive, inc, holds, captured, answer, childErase>>

DropRow ==
    /\ row # Empty /\ row' = Empty
    /\ UNCHANGED <<archive, inc, holds, captured, ownFile, answer, childErase>>

Collect ==
    LET old == IF Mutant = "preview_foreign_delete" /\ row = LocalRef /\ captured # Good
               THEN {Pack(Good.pack)} ELSE {}
    IN /\ archive' = [archive EXCEPT !.sink = @ \ old]
       /\ childErase' = childErase \cup old
       /\ ownFile' = (ownFile /\ (row = LocalRef \/ captured = LocalRef))
       /\ answer' = IF answer = "snapshotAfterCopy" /\ ownFile /\ ~ownFile'
                        /\ Pack(Good.pack) \in archive'.sink
                     THEN "localReclaimed" ELSE answer
       /\ UNCHANGED <<row, captured, inc, holds>>

\* Outside damage is allowed here. A foreign NotFound asks the head again.
\* Store the exact-read result at this step so later expiry cannot change it.
Damage ==
    /\ Pack(2) \in archive.sink
    /\ archive' = [archive EXCEPT !.sink = @ \ {Pack(2)}]
    /\ UNCHANGED <<inc, holds, childVars>>

Read(r) ==
    /\ r \in {row, captured} \ {Empty}
    /\ answer' = CASE ~Held -> "expired"
                  [] r = LocalRef /\ ownFile -> "local"
                  [] r = Good /\ Pack(2) \in archive.sink ->
                        IF row = LocalRef /\ r = captured THEN "snapshotAfterCopy" ELSE "parent"
                  [] OTHER -> "lost"
    /\ UNCHANGED <<archive, inc, holds, row, captured, ownFile, childErase>>

Release ==
    /\ Held /\ holds' = [holds EXCEPT ![1] = NoHold]
    /\ UNCHANGED <<archive, inc, childVars>>

ChildNext ==
    \/ \E r \in Inputs : OpenRow(r)
    \/ ChildCapture \/ ChildDropCapture \/ Compact \/ DropRow \/ Collect \/ Damage \/ Release
    \/ \E r \in {row, captured} : Read(r)

fixed == <<vars, now, rev, puts, flight, ever, mounted, expired, stale, hit>>
Frozen == /\ Init /\ now = 0 /\ rev = 1 /\ puts = 0 /\ flight = Idle
          /\ ever = [c \in Children |-> {}] /\ mounted = [c \in Children |-> NoMount]
          /\ expired = [c \in Children |-> FALSE] /\ stale = {} /\ hit = FALSE
ForeignSpec == (Frozen /\ ChildInit) /\ [][ChildNext /\ UNCHANGED fixed]_<<state, childVars>>
ForeignBound == {row,captured} \subseteq {Empty, LocalRef, Good}
ChildDeleteAuthority == childErase = {}
LocalReadable == LocalRef \in {row, captured} => ownFile
NoForeignCopy == answer # "localReclaimed"
NoForeignLost == answer # "lost"
NoForeignExpired == answer # "expired"
=============================================================================
