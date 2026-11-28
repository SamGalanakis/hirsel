-------------------------- MODULE StorePreviewHold --------------------------
EXTENDS StoreRetention

(*
 * ArchiveExpire publishes a floor and ArchiveErase can run much later.
 * Cursor 2 is a journal unit following base 1. Base 3 shares a pack with
 * base 1. The model checks two child identities, each admitted at most once,
 * against every renewal, time advance, expiry plan, floor CAS and erase.
 * The floor CAS competes on the same revision as admission and renewal.
 * Renew is the specification's atomic time check and publication. An object
 * service that only tests the ETag does not perform the time check: a PUT
 * issued before the deadline can apply after a reader has observed expiry.
 *)
VARIABLES leases, now, revision, plan
Children == 1..2
LastTime == 3
Empty == [boundary |-> 0, until |-> 0]
Active(h) == h.boundary # 0 /\ now < h.until
FoundingBase(q) == IF Base(q) THEN q ELSE q-1
PreviewClosure(q) == Objects(archive.history, FoundingBase(q)..q)

Initial ==
    LET hist == <<{1}, {1,2}>>
        objects == Objects(hist, 1..3)
    IN /\ archive = [ArchiveInitial EXCEPT !.history = hist, !.source = objects,
                       !.sink = objects, !.uploaded = objects, !.through = 3,
                       !.intervals = {<<1,3>>}, !.floor = 1]
       /\ leases = [c \in Children |-> Empty]
       /\ now = 0 /\ revision = 1 /\ plan = 0

Admit(c, q, t) ==
    /\ leases[c] = Empty /\ q \in Named(archive) /\ t > now
    /\ leases' = [leases EXCEPT ![c] = [boundary |-> q, until |-> t]]
    /\ revision' = revision + 1
    /\ UNCHANGED <<archive, now, plan>>

Renew(c, t) ==
    /\ Active(leases[c]) /\ t > leases[c].until
    /\ leases' = [leases EXCEPT ![c].until = t]
    /\ revision' = revision + 1
    /\ UNCHANGED <<archive, now, plan>>

Tick == /\ now < LastTime /\ now' = now + 1
        /\ UNCHANGED <<archive, leases, revision, plan>>

PlanExpiry ==
    /\ archive.floor = 1
    /\ \A c \in Children : Active(leases[c]) => FoundingBase(leases[c].boundary) >= 3
    /\ plan' = revision
    /\ UNCHANGED <<archive, leases, now, revision>>

PublishFloor ==
    /\ plan = revision /\ ArchiveExpire(3)
    /\ revision' = revision + 1 /\ plan' = 0
    /\ UNCHANGED <<leases, now>>

Erase == /\ \E k \in archive.sink : ArchiveErase(3, k)
         /\ UNCHANGED <<leases, now, revision, plan>>

NextStep ==
    \/ \E c \in Children, q \in 1..3, t \in 1..LastTime : Admit(c,q,t)
    \/ \E c \in Children, t \in 1..LastTime : Renew(c,t)
    \/ Tick \/ PlanExpiry \/ PublishFloor \/ Erase

state == <<vars, archive, leases, now, revision, plan>>
PreviewSpec == (Init /\ Initial) /\ [][NextStep /\ UNCHANGED vars]_state
HeldReadable == \A c \in Children : Active(leases[c]) =>
                    /\ leases[c].boundary \in Named(archive)
                    /\ PreviewClosure(leases[c].boundary) \subseteq archive.sink
ExpiredStaysExpired ==
    [][\A c \in Children : (leases[c] # Empty /\ ~Active(leases[c])) =>
                            leases'[c] = leases[c]]_state
=============================================================================
