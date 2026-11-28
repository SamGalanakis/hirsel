-------------------------- MODULE StorePreviewHold --------------------------
EXTENDS StoreRetention

(*
 * STORE-SPEC §15.7 "Preview holds" over an object service whose only
 * condition is the ETag. Every archive-head write is a PUT issued against the
 * tag its issuer read and applied later, or never, by the service. The service
 * compares the tag and nothing else: it knows no deadline. The tag is a
 * function of the head's bytes, so it tells two heads apart exactly when they
 * differ: here in the incarnation the head was created with, or in rev.
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
 * An applied PUT drops the others, which name a replaced head:
 * HeadNeverRepeats shows no later head carries that tag.
 *
 * Recreate is every archive object erased, the head with them, and the
 * archive published again from the same unchanged source: the first head,
 * rev 1, no holds, a new incarnation. PUTs issued before it stay in flight.
 * It is one step: the request that creates a head is never in flight, which
 * is the spec's assumption that no copy of it arrives after the head is erased.
 * The step also folds the write that creates the empty head into the first
 * publications that follow it.
 *
 * An eraser outlives the head it wrote. Recreate moves every applied floor to
 * stale, tagged with the incarnation of the head that carried it, and
 * OldErase lets that eraser name any key by the ordinal rule and the survivor
 * set it computed (history is constant here, so the set is the same). Keys
 * live in a directory named by the incarnation. An eraser lists and deletes
 * only in the directory of the head its floor write installed, so an old one
 * names nothing the current head stores. The sink is the current
 * incarnation's directory. Older directories hold only garbage and are not
 * modelled. The model has no index key: a unit whose cursor starts again
 * stands for an index page whose ordinal starts again.
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
 *   "preview_no_incarnation" a head carries no incarnation, so a recreated
 *                            head repeats the bytes, the tag and the stamps
 *                            of the one it replaced.
 *   "preview_flat_keys"      every incarnation stores its keys in one
 *                            namespace, so an old eraser deletes by ordinal
 *                            what a later head lists (lane 3450b's law).
 * Watch names one behaviour whose first occurrence sets hit.
 *)
CONSTANTS LastTime, MaxPut, MaxWipe, Skew, Grace, Watch
VARIABLES holds, now, rev, inc, puts, flight, ever, mounted, expired, stale, hit

Children == 1..2
NoHold == [q |-> 0, until |-> 0, stamp |-> 0]
NoPut == [kind |-> "none", tag |-> 0, c |-> 0, q |-> 0, t |-> 0]
Idle == [auth |-> NoPut, arch |-> NoPut, op |-> NoPut]
FoundingBase(q) == IF Base(q) THEN q ELSE q-1
PreviewClosure(q) == Objects(archive.history, FoundingBase(q)..q)
NoMount == [inc |-> 0, stamp |-> 0]
\* What the service compares.
Tag == IF Mutant = "preview_no_incarnation" THEN rev ELSE <<inc, rev>>
InFlight(m) == m.kind # "none" /\ m.tag = Tag
\* An applied PUT is the next rev of the same incarnation.
Put == puts < MaxPut /\ rev' = rev + 1 /\ puts' = puts + 1 /\ inc' = inc
Seeing(name, cond) == hit \/ (Watch = name /\ cond)

\* What a child's reader accepts from one exact head read. The child's genesis
\* records the incarnation and the stamp, the rev that admitted the hold.
Serves(c) == /\ holds[c] # NoHold
             /\ \/ Mutant = "preview_unstamped"
                \/ /\ holds[c].stamp = mounted[c].stamp
                   /\ Mutant = "preview_no_incarnation" \/ inc = mounted[c].inc
             /\ Mutant = "preview_naive_renewal" => now < holds[c].until

First ==
    LET hist == <<HistoryUnit({1}, 2), HistoryUnit({2}, 3), HistoryUnit({1,3}, 4)>>
        objects == Objects(hist, 1..3)
    IN [ArchiveInitial EXCEPT !.history = hist, !.source = objects,
           !.sink = objects, !.uploaded = objects, !.through = 2,
           !.intervals = {<<1,2>>}, !.floor = 1]

Initial ==
    /\ archive = First
    /\ holds = [c \in Children |-> NoHold]
    /\ now = 0 /\ rev = 1 /\ inc = 1 /\ puts = 0 /\ flight = Idle
    /\ ever = [c \in Children |-> {}]
    /\ mounted = [c \in Children |-> NoMount]
    /\ expired = [c \in Children |-> FALSE]
    /\ stale = {}
    /\ hit = FALSE

Tick == /\ now < LastTime /\ now' = now + 1
        /\ UNCHANGED <<archive, holds, rev, inc, puts, flight, ever, mounted, expired, stale, hit>>

\* ---- the hold authority: issue against the head just read ----
IssueAdmit(c, q, t) ==
    /\ holds[c] = NoHold /\ q \in Named(archive)
    /\ flight' = [flight EXCEPT !.auth =
                     [kind |-> "admit", tag |-> Tag, c |-> c, q |-> q, t |-> t]]
    /\ UNCHANGED <<archive, holds, now, rev, inc, puts, ever, mounted, expired, stale, hit>>

IssueExtend(c, t) ==
    /\ holds[c] # NoHold /\ t > holds[c].until
    /\ Mutant = "preview_naive_renewal" => now < holds[c].until
    /\ flight' = [flight EXCEPT !.auth =
                     [kind |-> "extend", tag |-> Tag, c |-> c, q |-> 0, t |-> t]]
    /\ UNCHANGED <<archive, holds, now, rev, inc, puts, ever, mounted, expired, stale, hit>>

\* The service applies a PUT whose tag still matches. Every other PUT in
\* flight then names a replaced tag and can never apply.
ApplyAuth ==
    LET m == flight.auth
        c == m.c
        retiring == InFlight(flight.op) /\ flight.op.t >= holds[c].until + Grace
    IN /\ InFlight(m) /\ Put
       /\ holds' = [holds EXCEPT ![c] =
              IF m.kind = "admit" THEN [q |-> m.q, until |-> m.t, stamp |-> rev + 1]
              ELSE [@ EXCEPT !.until = m.t]]
       /\ ever' = IF m.kind = "admit"
                  THEN [ever EXCEPT ![c] = @ \cup {[inc |-> inc, stamp |-> rev + 1]}]
                  ELSE ever
       /\ flight' = Idle
       /\ hit' = \/ Seeing("readmit", m.kind = "admit" /\ mounted[c] # NoMount)
                 \/ Seeing("lapsedExtend", m.kind = "extend" /\ now >= holds[c].until)
                 \/ Seeing("extendOverRetire", m.kind = "extend" /\ retiring)
       /\ UNCHANGED <<archive, now, mounted, expired, stale>>

\* The authority publishes the child's genesis once, with an incarnation and a
\* stamp it read in the head. The hold may have left since.
Mount(c, s) ==
    /\ mounted[c] = NoMount /\ s \in ever[c]
    /\ mounted' = [mounted EXCEPT ![c] = s]
    /\ hit' = Seeing("staleMount", holds[c].stamp # s.stamp \/ inc # s.inc)
    /\ UNCHANGED <<archive, holds, now, rev, inc, puts, flight, ever, expired, stale>>

\* ---- the archiver: an ordinary head write that carries the holds ----
IssuePublish(q) ==
    /\ TargetUnit(archive, q) /\ Closure(archive.history, q) \subseteq archive.sink
    /\ flight' = [flight EXCEPT !.arch =
                     [kind |-> "publish", tag |-> Tag, c |-> 0, q |-> q, t |-> 0]]
    /\ UNCHANGED <<archive, holds, now, rev, inc, puts, ever, mounted, expired, stale, hit>>

ApplyPublish ==
    /\ InFlight(flight.arch) /\ Put
    /\ ArchivePublish(flight.arch.q)
    /\ flight' = Idle
    /\ hit' = Seeing("bumpOverFlight", InFlight(flight.op) \/ InFlight(flight.auth))
    /\ UNCHANGED <<holds, now, ever, mounted, expired, stale>>

\* ---- the expiry operator ----
\* One PUT retires every hold the sample s puts past its deadline and may move
\* the floor to f.
IssueExpire(f, s) ==
    /\ s <= now + Skew
    /\ f = archive.floor \/ (f \in Named(archive) /\ Base(f) /\ f > archive.floor)
    /\ flight' = [flight EXCEPT !.op =
                     [kind |-> "expire", tag |-> Tag, c |-> 0, q |-> f, t |-> s]]
    /\ UNCHANGED <<archive, holds, now, rev, inc, puts, ever, mounted, expired, stale, hit>>

ApplyExpire ==
    LET m == flight.op
        lapsed == {c \in Children : holds[c] # NoHold /\ m.t >= holds[c].until + Grace}
        kept == [c \in Children |-> IF c \in lapsed THEN NoHold ELSE holds[c]]
    IN /\ InFlight(m) /\ Put
       /\ lapsed # {} \/ m.q > archive.floor
       /\ IF m.q > archive.floor
          THEN /\ Mutant = "preview_no_clamp" \/
                    \A c \in Children : kept[c] # NoHold => FoundingBase(kept[c].q) >= m.q
               /\ ArchiveExpire(m.q)
          ELSE UNCHANGED archive
       /\ holds' = kept /\ flight' = Idle
       /\ hit' = Seeing("retireOverExtend", InFlight(flight.auth)
                            /\ flight.auth.kind = "extend" /\ flight.auth.c \in lapsed)
       /\ UNCHANGED <<now, ever, mounted, expired, stale>>

Erase ==
    /\ \E f \in 1..Current(archive), k \in archive.sink : ArchiveErase(f, k)
    /\ hit' = Seeing("eraseBesideHold", \E c, d \in Children : Serves(c) /\ expired[d])
    /\ UNCHANGED <<holds, now, rev, inc, puts, flight, ever, mounted, expired, stale>>

\* The directory an eraser lists and deletes in.
Dir(e) == IF Mutant = "preview_flat_keys" THEN inc ELSE e.inc

\* An eraser whose floor write applied to an erased head. It still holds the
\* floor and the survivors it verified, and it reads no head again.
OldErase ==
    \E e \in stale, k \in archive.sink :
        /\ k \notin Closure(archive.history, e.floor)
        /\ IF k[1] = "pack" THEN k[2] < archive.history[e.floor].frontier ELSE k[2] < e.floor
        /\ archive' = IF Dir(e) = inc THEN [archive EXCEPT !.sink = @ \ {k}] ELSE archive
        /\ hit' = Seeing("oldEraser", \E c \in Children :
                             holds[c] # NoHold /\ k \in PreviewClosure(holds[c].q))
        /\ UNCHANGED <<holds, now, rev, inc, puts, flight, ever, mounted, expired, stale>>

\* ---- physical deletion and recreation ----
\* A PUT issued against rev 1 of the erased head meets rev 1 of the new one.
Recreate ==
    /\ inc <= MaxWipe /\ puts < MaxPut
    /\ archive' = First /\ holds' = [c \in Children |-> NoHold]
    /\ rev' = 1 /\ inc' = inc + 1 /\ puts' = puts + 1
    /\ stale' = stale \cup {[inc |-> inc, floor |-> f] : f \in archive.expiredFloors}
    /\ hit' = Seeing("twinRefused", \E r \in DOMAIN flight :
                         flight[r].kind # "none" /\ flight[r].tag[2] = 1)
    /\ UNCHANGED <<now, flight, ever, mounted, expired>>

\* ---- a Preview child: an exact head read, and no clock ----
ReadHead(c) ==
    /\ mounted[c] # NoMount /\ ~expired[c]
    /\ ~Serves(c)
    /\ expired' = [expired EXCEPT ![c] = TRUE]
    /\ hit' = \/ Seeing("expiredSeen", TRUE)
              \/ Seeing("stampRepeated", holds[c].stamp = mounted[c].stamp)
    /\ UNCHANGED <<archive, holds, now, rev, inc, puts, flight, ever, mounted, stale>>

NextStep ==
    \/ Tick \/ ApplyAuth \/ ApplyPublish \/ ApplyExpire \/ Erase \/ OldErase \/ Recreate
    \/ \E c \in Children, q \in 2..3, t \in 1..LastTime : IssueAdmit(c, q, t)
    \/ \E c \in Children, t \in 1..LastTime : IssueExtend(c, t)
    \/ \E c \in Children : \E s \in ever[c] : Mount(c, s)
    \/ \E c \in Children : ReadHead(c)
    \/ \E q \in 1..3 : IssuePublish(q)
    \/ \E f \in 1..3, s \in 0..(LastTime + Skew) : IssueExpire(f, s)

state == <<vars, archive, holds, now, rev, inc, puts, flight, ever, mounted, expired, stale, hit>>
PreviewSpec == (Init /\ Initial) /\ [][NextStep /\ UNCHANGED vars]_state

\* A listed hold's boundary is restorable. No clock appears.
HeldReadable == \A c \in Children : holds[c] # NoHold =>
                    /\ holds[c].q \in Named(archive)
                    /\ PreviewClosure(holds[c].q) \subseteq archive.sink
\* A child that reported expiry is never served again.
NoResurrection == \A c \in Children : expired[c] => ~Serves(c)
\* Within one incarnation, no hold leaves the head before the true time
\* reaches its deadline. Erasing the archive breaks every promise it made.
PromiseKept ==
    [][\A c \in Children : (holds[c] # NoHold /\ holds'[c] = NoHold /\ inc' = inc) =>
                            now >= holds[c].until]_state
\* A head that was replaced never comes back: every write moves (inc, rev) up.
HeadNeverRepeats ==
    [][inc' > inc \/ (inc' = inc /\ rev' >= rev)]_state
\* While listed, a hold keeps its boundary and stamp and its deadline never
\* moves back.
HoldStable ==
    [][\A c \in Children : (holds[c] # NoHold /\ holds'[c] # NoHold) =>
          /\ holds'[c].q = holds[c].q /\ holds'[c].stamp = holds[c].stamp
          /\ holds'[c].until >= holds[c].until]_state
NoWitness == ~hit
=============================================================================
