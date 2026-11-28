----------------------- MODULE StoreFollowerCapture -------------------------
EXTENDS Naturals

(*
 * Directory (log@1) follower capture retention (STORE-SPEC §13.5 "Followers").
 * One writer and one follower are separate processes over one store
 * directory. A MANIFEST identity is (generation, digest). The digest is a
 * publish counter, so a writer that lost an unsynced rename and publishes the
 * same generation again writes a different identity.
 *
 * Writer: a cut syncs its new packs, renames MANIFEST and syncs the directory
 * as separate steps (AP16). A power cut between the rename and the sync keeps
 * either MANIFEST. Remove unlinks unlisted packs only after the directory sync.
 * Erase is damage outside the store: a listed pack of the current generation
 * disappears.
 *
 * Follower: a capture reads MANIFEST, syncs the directory (O1) and requires
 * the same identity again. A Cold read stages its value (a cache hit or a pack
 * open, which can observe NotFound), then takes the witness by opening MANIFEST
 * by name, and only then releases a result. The cache is one set for the
 * pin map, the pager cache and a projector image: all three are hits that
 * outlive a read. The follower's own sync_dir changes shared durable state,
 * and it can fail. A writer crash does not touch follower state.
 *)
CONSTANTS Mutant, MaxPub, MaxPack, MaxCrash, MaxErase

VARIABLES
    visible,   \* MANIFEST identity selected by name in the directory
    durable,   \* MANIFEST identity a power cut keeps
    inv,       \* digest -> listed packs
    files,     \* pack files present
    pubs, nextPack, crashes, erased,
    cap,       \* the follower Snapshot's captured identity
    floor,     \* highest generation the follower has confirmed durable
    cache,     \* packs with warmed bytes (pin map, pager cache, projector image)
    cp, cand,  \* capture phase and candidate
    rd, rp, rg, wit, out, outG,
    start, hit, nf, lost, bySync  \* ghosts the protocol never reads

vars == <<visible, durable, inv, files, pubs, nextPack, crashes, erased,
          cap, floor, cache, cp, cand, rd, rp, rg, wit, out, outG,
          start, hit, nf, lost, bySync>>
writerVars == <<visible, durable, inv, files, pubs, nextPack, crashes, erased, lost>>
followerVars == <<cap, floor, cache, cp, cand, rd, rp, rg, wit, out, outG, start, hit, nf, bySync>>

Id(g, d) == [gen |-> g, dig |-> d]
None == Id(0, 0)
Max(a, b) == IF a > b THEN a ELSE b

Init ==
    /\ visible = Id(1, 1) /\ durable = Id(1, 1)
    /\ inv = [d \in 1..MaxPub |-> IF d = 1 THEN {1} ELSE {}]
    /\ files = {1} /\ pubs = 1 /\ nextPack = 2 /\ crashes = 0 /\ erased = 0
    /\ cap = None /\ floor = 0 /\ cache = {} /\ cp = "idle" /\ cand = None
    /\ rd = "idle" /\ rp = 0 /\ rg = 0 /\ wit = None /\ out = "none" /\ outG = 0
    /\ start = 0 /\ hit = FALSE /\ nf = FALSE /\ lost = FALSE /\ bySync = FALSE

---------------------------------------------------------------------------
\* Writer.

\* A cut either keeps the pack list (the pack survives a newer generation) or
\* writes a new pack and lists only it. The new pack is synced before rename.
Cut(keep) ==
    /\ pubs < MaxPub /\ visible = durable
    /\ keep \/ nextPack <= MaxPack
    /\ LET d == pubs + 1
           listed == IF keep THEN inv[visible.dig] ELSE {nextPack}
       IN /\ inv' = [inv EXCEPT ![d] = listed]
          /\ files' = files \cup listed
          /\ visible' = Id(visible.gen + 1, d)
          /\ pubs' = d
          /\ nextPack' = IF keep THEN nextPack ELSE nextPack + 1
    /\ UNCHANGED <<durable, crashes, erased, lost>> /\ UNCHANGED followerVars

\* The cut's own directory sync, or the writer's O1 on restart.
WriterSync ==
    /\ visible # durable /\ durable' = visible
    /\ UNCHANGED <<visible, inv, files, pubs, nextPack, crashes, erased, lost>>
    /\ UNCHANGED followerVars

\* AP16 Remove: only after the directory sync, only unlisted packs.
Remove(p) ==
    /\ visible = durable /\ p \in files /\ p \notin inv[visible.dig]
    /\ files' = files \ {p}
    /\ UNCHANGED <<visible, durable, inv, pubs, nextPack, crashes, erased, lost>>
    /\ UNCHANGED followerVars

\* Out-of-band damage to a pack the current generation lists.
Erase(p) ==
    /\ erased < MaxErase /\ p \in files /\ p \in inv[visible.dig]
    /\ files' = files \ {p} /\ erased' = erased + 1
    /\ UNCHANGED <<visible, durable, inv, pubs, nextPack, crashes, lost>>
    /\ UNCHANGED followerVars

\* A power cut after a rename and before its directory sync keeps either
\* MANIFEST. Packs synced before the rename stay as leftovers.
Crash ==
    /\ crashes < MaxCrash /\ visible # durable
    /\ \E v \in {visible, durable} :
         /\ visible' = v /\ durable' = v
         /\ lost' = (lost \/ v # visible)
    /\ crashes' = crashes + 1
    /\ UNCHANGED <<inv, files, pubs, nextPack, erased>> /\ UNCHANGED followerVars

---------------------------------------------------------------------------
\* Follower capture, follower_floor included: read, O1, read again.

CapRead ==
    /\ rd = "idle" /\ cp = "idle"
    /\ cand' = visible /\ cp' = "read"
    /\ UNCHANGED <<cap, floor, cache, rd, rp, rg, wit, out, outG, start, hit, nf, bySync>>
    /\ UNCHANGED writerVars

CapSync ==
    /\ cp = "read"
    /\ \/ /\ cp' = "synced" /\ durable' = visible
       \/ /\ cp' = "idle" /\ UNCHANGED durable          \* Io(e), nothing published
    /\ UNCHANGED <<cap, floor, cache, cand, rd, rp, rg, wit, out, outG, start, hit, nf, bySync>>
    /\ UNCHANGED <<visible, inv, files, pubs, nextPack, crashes, erased, lost>>

CapConfirm ==
    /\ cp = "synced" /\ cp' = "idle"
    /\ cap' = IF visible = cand THEN cand ELSE cap       \* changed: restart
    /\ floor' = IF visible = cand THEN Max(floor, cand.gen) ELSE floor
    /\ UNCHANGED <<cache, cand, rd, rp, rg, wit, out, outG, start, hit, nf, bySync>>
    /\ UNCHANGED writerVars

---------------------------------------------------------------------------
\* A Cold-dependent read of pack p of the capture.

Begin(p) ==
    /\ cap # None /\ rd = "idle" /\ cp = "idle" /\ p \in inv[cap.dig]
    /\ rd' = "begun" /\ rp' = p /\ rg' = cap.gen /\ start' = durable.gen
    /\ out' = "none" /\ outG' = 0 /\ hit' = FALSE /\ nf' = FALSE /\ bySync' = FALSE
    /\ UNCHANGED <<cap, floor, cache, cp, cand, wit>> /\ UNCHANGED writerVars

\* Stage privately: a hit, a pack read that fills the cache, or NotFound.
Stage ==
    /\ rd = "begun"
    /\ CASE rp \in cache ->
              /\ hit' = TRUE /\ UNCHANGED <<cache, nf>>
              /\ IF Mutant = "cache_skips_witness"
                 THEN rd' = "idle" /\ out' = "value" /\ outG' = rg
                 ELSE rd' = "staged" /\ UNCHANGED <<out, outG>>
         [] rp \in files ->
              /\ rd' = "staged" /\ cache' = cache \cup {rp}
              /\ UNCHANGED <<hit, nf, out, outG>>
         [] OTHER ->
              /\ rd' = "notfound" /\ nf' = TRUE
              /\ UNCHANGED <<cache, hit, out, outG>>
    /\ UNCHANGED <<cap, floor, cp, cand, rp, rg, wit, start, bySync>> /\ UNCHANGED writerVars

\* The witness: open MANIFEST by name after staging (after the failed open on
\* NotFound) and decide.
Witness ==
    /\ rd \in {"staged", "notfound"}
    /\ IF floor > cap.gen
       THEN rd' = "idle" /\ out' = "reclaimed" /\ outG' = floor /\ UNCHANGED <<wit, floor>>
       ELSE CASE visible = cap ->
                   /\ rd' = "idle" /\ out' = IF nf THEN "absent" ELSE "value"
                   /\ outG' = cap.gen /\ UNCHANGED <<wit, floor>>
              [] visible.gen > cap.gen ->
                   IF Mutant = "reclaim_before_sync"
                   THEN /\ rd' = "idle" /\ out' = "reclaimed" /\ outG' = visible.gen
                        /\ floor' = visible.gen /\ UNCHANGED wit
                   ELSE /\ rd' = "witsync" /\ wit' = visible
                        /\ UNCHANGED <<out, outG, floor>>
              [] OTHER ->
                   /\ rd' = "idle" /\ out' = "integrity" /\ outG' = visible.gen
                   /\ UNCHANGED <<wit, floor>>
    /\ UNCHANGED <<cap, cache, cp, cand, rp, rg, start, hit, nf, bySync>> /\ UNCHANGED writerVars

\* O1 for a changed generation: sync the directory, which can fail.
WitSync ==
    /\ rd = "witsync"
    /\ \/ /\ rd' = "witsynced" /\ durable' = visible
          /\ bySync' = (bySync \/ visible # durable) /\ UNCHANGED <<out, outG>>
       \/ /\ rd' = "idle" /\ out' = "io" /\ outG' = 0
          /\ UNCHANGED <<durable, bySync>>
    /\ UNCHANGED <<cap, floor, cache, cp, cand, rp, rg, wit, start, hit, nf>>
    /\ UNCHANGED <<visible, inv, files, pubs, nextPack, crashes, erased, lost>>

\* Reopen and require the same identity, or restart the witness.
WitConfirm ==
    /\ rd = "witsynced"
    /\ IF visible = wit
       THEN /\ rd' = "idle" /\ out' = "reclaimed" /\ outG' = wit.gen
            /\ floor' = Max(floor, wit.gen)
       ELSE /\ rd' = IF nf THEN "notfound" ELSE "staged"
            /\ UNCHANGED <<out, outG, floor>>
    /\ UNCHANGED <<cap, cache, cp, cand, rp, rg, wit, start, hit, nf, bySync>>
    /\ UNCHANGED writerVars

Next ==
    \/ \E keep \in BOOLEAN : Cut(keep)
    \/ WriterSync \/ Crash
    \/ \E p \in 1..MaxPack : Remove(p) \/ Erase(p) \/ Begin(p)
    \/ CapRead \/ CapSync \/ CapConfirm
    \/ Stage \/ Witness \/ WitSync \/ WitConfirm

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
\* Invariants.

\* No Cold-dependent read returns data from a generation that was already
\* superseded by a durable MANIFEST when the read began.
NoReclaimedRead == out = "value" => rg >= start

\* Reclaimed(G) names a newer generation that is durable, now and in every
\* later state. So a missing pack at the captured generation, or a successor
\* seen only before the directory barrier, is never reported as Reclaimed.
NoFalseReclaim == out = "reclaimed" => outG > rg /\ outG <= durable.gen

\* A capture is always of a durable MANIFEST.
CaptureDurable == cap # None => cap.gen <= durable.gen

---------------------------------------------------------------------------
\* Reachability witnesses: each must be violated. One per harness case.

\* follower_laws (L66): a warm reader gets Reclaimed(G) though its pack survives.
NoWarmReclaim == ~(out = "reclaimed" /\ hit /\ rp \in files)
\* follower_laws (L66): a cold reader, pack survives, Reclaimed(G).
NoColdReclaim == ~(out = "reclaimed" /\ ~hit /\ ~nf /\ rp \in files)
\* tiered_open_laws (L66): a pack erased at the current g is Corrupt(Absent).
NoAbsent == out # "absent"
\* tiered_open_laws (L66): NotFound after G published and Remove ran is Reclaimed(G).
NoRemovedReclaim == ~(out = "reclaimed" /\ nf)
\* follower_crash_laws (L66): a lost unsynced rename leaves the capture readable.
NoLostRenameRead == ~(out = "value" /\ lost)
\* follower_crash_laws (L66): the follower's own O1 made the successor durable.
NoSyncedReclaim == ~(out = "reclaimed" /\ bySync)
\* follower_crash_laws (L66): a failed O1 returns no floor.
NoWitIo == out # "io"
=============================================================================
