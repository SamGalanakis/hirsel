-------------------------- MODULE StorePreviewHold --------------------------
EXTENDS StoreRetention

(*
 * STORE-SPEC §15.7 "Preview holds" over an object service whose only
 * condition is the ETag. Every archive-head write is a PUT issued against the
 * tag its issuer read and applied later, or never, by the service. The service
 * compares the tag and nothing else: it knows no deadline.
 *
 * A hold in the durable head retains its boundary until a head write removes
 * it. The deadline only says when a writer may remove it. A Preview child uses
 * no clock: it reports expiry when an exact head read no longer lists its
 * hold. Extension is one PUT that is valid whenever the head it replaces
 * lists the hold.
 *
 * Cursor 2 is a journal unit following base 1. Base 3 shares a pack with
 * base 1 and is uploaded but unpublished at the start, so the archiver's
 * publication is an ordinary head write that carries the holds. Two children
 * are admitted, extended, retired and admitted again in any order against
 * time, the archiver, the expiry operator and erases under a published floor.
 *
 * Issuers keep nothing between steps. Each decides from the head it reads and
 * learns an outcome only by reading again, so losing a process changes no
 * behaviour: its PUTs stay in flight and its successor reads the same head.
 * The authority, the archiver and the operator each have one PUT in flight. A
 * newer one replaces it, because two PUTs against one tag cannot both apply.
 *
 * The operator reads the head and samples its clock in separate steps, in
 * either order, and its clock may run ahead of true time by Skew. A PUT
 * against a replaced tag never applies, so what matters is the sample, which
 * may be arbitrarily old: any value up to now + Skew. It retires a hold only
 * when the sample is at or past the deadline plus Grace.
 *
 * Mutant names one broken rule, or "none":
 *   "preview_naive_renewal"  the lane 3435 single-CAS design: the authority
 *                            checks the clock before it sends an extension and
 *                            a child also reports expiry at the deadline.
 *   "preview_no_clamp"       a floor write ignores the holds it keeps.
 *   "preview_unstamped"      a child accepts any hold under its identity.
 * Watch names one behaviour whose first occurrence sets hit.
 *)
CONSTANTS LastTime, MaxRev, Skew, Grace, Watch
VARIABLES holds, now, tag, flight, ever, mounted, expired, hit

Children == 1..2
NoHold == [q |-> 0, until |-> 0, stamp |-> 0]
NoPut == [kind |-> "none", tag |-> 0, c |-> 0, q |-> 0, t |-> 0]
Idle == [auth |-> NoPut, arch |-> NoPut, op |-> NoPut]
FoundingBase(q) == IF Base(q) THEN q ELSE q-1
PreviewClosure(q) == Objects(archive.history, FoundingBase(q)..q)
InFlight(m) == m.kind # "none" /\ m.tag = tag
Seeing(name, cond) == hit \/ (Watch = name /\ cond)

\* What a child's reader accepts from one exact head read. The stamp is the
\* head revision that admitted the hold, which the child's genesis records.
Serves(c) == /\ holds[c] # NoHold
             /\ Mutant = "preview_unstamped" \/ holds[c].stamp = mounted[c]
             /\ Mutant = "preview_naive_renewal" => now < holds[c].until

Initial ==
    LET hist == <<{1}, {1,2}>>
        objects == Objects(hist, 1..3)
    IN /\ archive = [ArchiveInitial EXCEPT !.history = hist, !.source = objects,
                       !.sink = objects, !.uploaded = objects, !.through = 2,
                       !.intervals = {<<1,2>>}, !.floor = 1]
       /\ holds = [c \in Children |-> NoHold]
       /\ now = 0 /\ tag = 1 /\ flight = Idle
       /\ ever = [c \in Children |-> {}]
       /\ mounted = [c \in Children |-> 0]
       /\ expired = [c \in Children |-> FALSE]
       /\ hit = FALSE

Tick == /\ now < LastTime /\ now' = now + 1
        /\ UNCHANGED <<archive, holds, tag, flight, ever, mounted, expired, hit>>

\* ---- the hold authority: issue against the head just read ----
IssueAdmit(c, q, t) ==
    /\ holds[c] = NoHold /\ q \in Named(archive)
    /\ flight' = [flight EXCEPT !.auth =
                     [kind |-> "admit", tag |-> tag, c |-> c, q |-> q, t |-> t]]
    /\ UNCHANGED <<archive, holds, now, tag, ever, mounted, expired, hit>>

IssueExtend(c, t) ==
    /\ holds[c] # NoHold /\ t > holds[c].until
    /\ Mutant = "preview_naive_renewal" => now < holds[c].until
    /\ flight' = [flight EXCEPT !.auth =
                     [kind |-> "extend", tag |-> tag, c |-> c, q |-> 0, t |-> t]]
    /\ UNCHANGED <<archive, holds, now, tag, ever, mounted, expired, hit>>

\* The service applies a PUT whose tag still matches. Every other PUT in
\* flight then names a replaced tag and can never apply.
ApplyAuth ==
    LET m == flight.auth
        c == m.c
        retiring == InFlight(flight.op) /\ flight.op.t >= holds[c].until + Grace
    IN /\ InFlight(m) /\ tag < MaxRev
       /\ holds' = [holds EXCEPT ![c] =
              IF m.kind = "admit" THEN [q |-> m.q, until |-> m.t, stamp |-> tag + 1]
              ELSE [@ EXCEPT !.until = m.t]]
       /\ ever' = IF m.kind = "admit" THEN [ever EXCEPT ![c] = @ \cup {tag + 1}] ELSE ever
       /\ tag' = tag + 1 /\ flight' = Idle
       /\ hit' = \/ Seeing("readmit", m.kind = "admit" /\ mounted[c] # 0)
                 \/ Seeing("lapsedExtend", m.kind = "extend" /\ now >= holds[c].until)
                 \/ Seeing("extendOverRetire", m.kind = "extend" /\ retiring)
       /\ UNCHANGED <<archive, now, mounted, expired>>

\* The authority publishes the child's genesis once, with a stamp it read in
\* the head. The hold may have left since.
Mount(c, s) ==
    /\ mounted[c] = 0 /\ s \in ever[c]
    /\ mounted' = [mounted EXCEPT ![c] = s]
    /\ hit' = Seeing("staleMount", holds[c].stamp # s)
    /\ UNCHANGED <<archive, holds, now, tag, flight, ever, expired>>

\* ---- the archiver: an ordinary head write that carries the holds ----
IssuePublish(q) ==
    /\ TargetUnit(archive, q) /\ Closure(archive.history, q) \subseteq archive.sink
    /\ flight' = [flight EXCEPT !.arch =
                     [kind |-> "publish", tag |-> tag, c |-> 0, q |-> q, t |-> 0]]
    /\ UNCHANGED <<archive, holds, now, tag, ever, mounted, expired, hit>>

ApplyPublish ==
    /\ InFlight(flight.arch) /\ tag < MaxRev
    /\ ArchivePublish(flight.arch.q)
    /\ tag' = tag + 1 /\ flight' = Idle
    /\ hit' = Seeing("bumpOverFlight", InFlight(flight.op) \/ InFlight(flight.auth))
    /\ UNCHANGED <<holds, now, ever, mounted, expired>>

\* ---- the expiry operator ----
\* One PUT retires every hold the sample s puts past its deadline and may move
\* the floor to f.
IssueExpire(f, s) ==
    /\ s <= now + Skew
    /\ f = archive.floor \/ (f \in Named(archive) /\ Base(f) /\ f > archive.floor)
    /\ flight' = [flight EXCEPT !.op =
                     [kind |-> "expire", tag |-> tag, c |-> 0, q |-> f, t |-> s]]
    /\ UNCHANGED <<archive, holds, now, tag, ever, mounted, expired, hit>>

ApplyExpire ==
    LET m == flight.op
        lapsed == {c \in Children : holds[c] # NoHold /\ m.t >= holds[c].until + Grace}
        kept == [c \in Children |-> IF c \in lapsed THEN NoHold ELSE holds[c]]
    IN /\ InFlight(m) /\ tag < MaxRev
       /\ lapsed # {} \/ m.q > archive.floor
       /\ IF m.q > archive.floor
          THEN /\ Mutant = "preview_no_clamp" \/
                    \A c \in Children : kept[c] # NoHold => FoundingBase(kept[c].q) >= m.q
               /\ ArchiveExpire(m.q)
          ELSE UNCHANGED archive
       /\ holds' = kept /\ tag' = tag + 1 /\ flight' = Idle
       /\ hit' = Seeing("retireOverExtend", InFlight(flight.auth)
                            /\ flight.auth.kind = "extend" /\ flight.auth.c \in lapsed)
       /\ UNCHANGED <<now, ever, mounted, expired>>

Erase ==
    /\ \E f \in 1..Current(archive), k \in archive.sink : ArchiveErase(f, k)
    /\ hit' = Seeing("eraseBesideHold", \E c, d \in Children : Serves(c) /\ expired[d])
    /\ UNCHANGED <<holds, now, tag, flight, ever, mounted, expired>>

\* ---- a Preview child: an exact head read, and no clock ----
ReadHead(c) ==
    /\ mounted[c] # 0 /\ ~expired[c]
    /\ ~Serves(c)
    /\ expired' = [expired EXCEPT ![c] = TRUE]
    /\ hit' = Seeing("expiredSeen", TRUE)
    /\ UNCHANGED <<archive, holds, now, tag, flight, ever, mounted>>

NextStep ==
    \/ Tick \/ ApplyAuth \/ ApplyPublish \/ ApplyExpire \/ Erase
    \/ \E c \in Children, q \in 2..3, t \in 1..LastTime : IssueAdmit(c, q, t)
    \/ \E c \in Children, t \in 1..LastTime : IssueExtend(c, t)
    \/ \E c \in Children, s \in 2..MaxRev : Mount(c, s)
    \/ \E c \in Children : ReadHead(c)
    \/ \E q \in 1..3 : IssuePublish(q)
    \/ \E f \in 1..3, s \in 0..(LastTime + Skew) : IssueExpire(f, s)

state == <<vars, archive, holds, now, tag, flight, ever, mounted, expired, hit>>
PreviewSpec == (Init /\ Initial) /\ [][NextStep /\ UNCHANGED vars]_state

\* A listed hold's boundary is restorable. No clock appears.
HeldReadable == \A c \in Children : holds[c] # NoHold =>
                    /\ holds[c].q \in Named(archive)
                    /\ PreviewClosure(holds[c].q) \subseteq archive.sink
\* A child that reported expiry is never served again.
NoResurrection == \A c \in Children : expired[c] => ~Serves(c)
\* No hold leaves the head before the true time reaches its deadline.
PromiseKept ==
    [][\A c \in Children : (holds[c] # NoHold /\ holds'[c] = NoHold) =>
                            now >= holds[c].until]_state
\* While listed, a hold keeps its boundary and stamp and its deadline never
\* moves back.
HoldStable ==
    [][\A c \in Children : (holds[c] # NoHold /\ holds'[c] # NoHold) =>
          /\ holds'[c].q = holds[c].q /\ holds'[c].stamp = holds[c].stamp
          /\ holds'[c].until >= holds[c].until]_state
NoWitness == ~hit
=============================================================================
