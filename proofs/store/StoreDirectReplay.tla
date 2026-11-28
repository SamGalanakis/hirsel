------------------------ MODULE StoreDirectReplay ---------------------------
EXTENDS Naturals, Sequences, FiniteSets

(*
 * A fixed BLOB stamp is already sealed when a journal op names it. Ordinary
 * replay may allocate a different number of page stamps than the live writer.
 * The already captured prefix can grow, bringing a stamp that the last
 * private replay had legitimately used. Model either a full recovery or that
 * incremental follower. A re-base replaces private state atomically.
 *)
CONSTANTS Mutant, MaxOps
VARIABLES log, at, limit, pages, blobs, rebased
state == <<log, at, limit, pages, blobs, rebased>>
MaxOf(s) == CHOOSE n \in s : \A x \in s : x <= n
Fixed(i,j) == {log[k] : k \in {k \in i..j : log[k] # 0}}
Init == /\ log = <<>> /\ at = 0 /\ limit = 1 /\ pages = {} /\ blobs = {} /\ rebased = FALSE

\* In this fixture the live writer uses one stamp per op. Zero denotes an
\* ordinary edit; a direct op records its exact stamp in the journal.
Write(v) ==
    /\ Len(log) < MaxOps /\ v \in {0, Len(log)+1} /\ log' = Append(log,v)
    /\ UNCHANGED <<at, limit, pages, blobs, rebased>>

Replay(to) ==
    LET new == Fixed(at+1,to)
        again == new \cap pages # {} /\ Mutant # "direct_follower_stamp_alias"
        start == IF again THEN 1 ELSE at+1
        fixed == Fixed(start,to)
        oldPages == IF again THEN {} ELSE pages
        draw == MaxOf({IF again THEN 1 ELSE limit}
                      \cup (IF Mutant = "direct_replay_no_reserve" THEN {} ELSE {b+1 : b \in fixed}))
        edits == Cardinality({k \in start..to : log[k] = 0})
    IN /\ to \in (at+1)..Len(log)
       /\ \E n \in edits..(2*edits) :
            /\ pages' = oldPages \cup (draw..(draw+n-1))
            /\ limit' = draw+n
       /\ blobs' = Fixed(1,to) /\ at' = to /\ rebased' = again /\ UNCHANGED log

Crash ==
    /\ at' = 0 /\ limit' = 1 /\ pages' = {} /\ blobs' = {} /\ rebased' = FALSE
    /\ UNCHANGED log

Next == (\E v \in {0,Len(log)+1} : Write(v)) \/ (\E to \in 1..Len(log) : Replay(to)) \/ Crash
Spec == Init /\ [][Next]_state
StampDisjoint == pages \cap blobs = {}
CounterCovers == \A s \in pages \cup blobs : s < limit
NoReplayRebase == ~rebased
=============================================================================
