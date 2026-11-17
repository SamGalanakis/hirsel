"""RUNNER-SPEC v2.5 calls model: sessions, head, gate, Release FIFO and publisher.

v2.5 (from calls22_model.py; review 1042, runner-build-1033 08:50): the session and the client are
now separate. The session keeps its own owed sends (one per Call frame, each with its own deadline
timer) and emits wire rows; the client only counts, per (session, stream, p), the sends it made,
the rows it received and the sends it lost when its session ended. In v2.2 the session gave a
head frame only to requests the client still had "inflight", so a second Outcome for one send could
not be expressed. New checks:
  C9 exactly one Outcome per send, from the client's counts per (session, stream, p):
     at every state, rows + lost <= sends (at most one);
     at every terminal state (no step but crash/poison is enabled, nothing poisoned), rows + lost
     == sends (at least one). A non-terminal state with rows + lost < sends proves nothing: a row may
     still come. The search is finite (bounded sends, re-sends, crashes); a history that never
     reaches a terminal state is not checked for the at-least-once half, and the state graph has
     no cycles (sends, versions and epochs only grow), so every maximal history ends in one.
  C6b the session's owed sends plus its held positions with no owed send stay within 1+q
     (a joined send takes a slot; v2.5 A2).
  C10 a body run never starts at a position whose reply the store holds (L3's model analogue).


Explicit-state BFS over every interleaving of client sends, session steps (validation, slots, the
session's own deadline timer), head steps (H1-H4), journal flushes (group commit, prefix order),
journal poison, the head's Release pushes, publisher steps and process crashes (lose volatile
store ops, head, channels and sessions; clients re-send every unanswered request).

Bounded dimensions (each scenario turns some on): two streams A and B (two principals), positions
0 and 1, two sessions (connections), run plans ok / err (Aborted with reply) / fault (Failed, then
a reply-only commit) / refuse (store Refused after admit) / hang (a body that never returns: the
head task stops), malformed arguments, the session slot limit 1+q, deadlines, one re-send after
Abandoned or Refused(Overloaded), upgrade with every entry gone after a restart, poison.

v2.2 rules (RUNNER-SPEC v2.2 sections 2 and 3):
  V1 the session answers what it decides itself (Malformed, Overloaded for a full slot table, and
     Abandoned at the deadline) at once, in a Batch at its last sent version;
  V2 the session forwards a position at most once while the head holds it (local join), and a
     forwarded position keeps its slot until the head's answer for it arrives, even after Abandoned;
  V3 the head answers a commitless outcome (Duplicate, Refused(HistoryUnavailable) below latest,
     Refused(Overloaded)) by pushing
     it at once in a snapshot-less Release, behind every earlier Release on the same FIFO;
  V4 a committed outcome is pushed in a Release only after durable (H3); on a durable error
     (poison) the head answers nothing and the process exits;
  V5 a gone key (after upgrade) is admitted and replied Failed(Gone) in a reply-only commit.
Checks:
  C1 durable effects per position <= 1;
  C3 a delivered reply-bearing outcome equals the position's durable reply (so it is durable);
     HistoryUnavailable only below a durably replied position (RUNNER-SPEC v2.11.4);
  C5 an outcome reporting the commit at version c arrives in a frame at version >= c, and the frame
     versions a session sends never decrease;
  C6 the head holds at most 1+q requests from one session (the queue is inside the session account);
  C7 a refusal records nothing: a malformed request's position is never admitted;
  C8 (gone scenario) no stream is left admitted-unreplied at p when p+1 was sent and p's request
     was answered with something other than Abandoned;
  C4 liveness: in a terminal state every request has an answer.
"""
import sys
from collections import deque

STREAMS = 2
KEYS = 4  # (stream, p) -> stream*2 + p

def key(s, p): return s * 2 + p

MUTANTS = {
    "none": "v2.5 rules",
    "deadline_rides_release": "v2.1 H3: the deadline answer waits for the next commit's Release (review finding 1)",
    "direct_commitless": "the head answers Duplicate/HistoryUnavailable straight to the session, not behind the Release FIFO",
    "answer_before_durable": "H3 dropped: a committed outcome is pushed at commit",
    "queue_outside_account": "the session frees the slot when it sends Abandoned, not when the head answers",
    "poison_answers": "on a durable error the head pushes the outcome anyway",
    "gone_refused_at_session": "v1 upgrade: a gone key is Refused(NoEntry) at the session, no reply",
    "decode_after_admit": "the head decodes the argument after admit",
    "store_v22_admit": "S7 dropped: admit(latest+1) while latest is unreplied",
    "drop_joiner": "a second session's send of the running position is dropped at the head",
    "release_at_commit_version": "a Release carries its own commit version, not the group's durable version (the first v2.2 run's bug)",
    "late_completion_resent": "the session forwards every head frame to the client, owed or not (R0 at 4a7fe8a9, review 1042)",
    "one_for_joined": "the session gives one row for a head answer however many sends of the position it owes",
    "join_outside_account": "a joined send takes no slot (bdf27482's form; unbounded per-send timers)",
    "rerun_on_duplicate": "the head re-runs the body on Duplicate when the stored reply is Failed (L3's wrong implementation)",
}

# requests: (session, stream, p, kind, depends_on)   kind: ok | bad
SCENARIOS = {
    # the review's counterexample: p=1 sent while p=0 runs; p=0 may never return
    "review_deadline": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 1, "ok", None)),
                            plans=("ok", "hang"), q=1, crashes=0, poison=0, gone=False),
    "two_sessions_two_streams": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 1, "ok", 0), (1, 1, 0, "ok", None)),
                                     plans=("ok",), q=1, crashes=0, poison=0, gone=False),
    "cross_session_crash": dict(reqs=((0, 0, 0, "ok", None), (1, 0, 0, "ok", None), (1, 1, 0, "ok", None)),
                                plans=("ok", "err"), q=1, crashes=1, poison=0, gone=False),
    "out_of_order_two_sessions": dict(reqs=((0, 0, 0, "ok", None), (1, 0, 1, "ok", None)),
                                      plans=("ok", "fault"), q=1, crashes=2, poison=0, gone=False),
    "refusals": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 1, "ok", None), (0, 1, 0, "bad", None)),
                     plans=("ok", "refuse", "fault"), q=0, crashes=1, poison=0, gone=False),
    "slots_and_deadline": dict(reqs=((0, 0, 0, "ok", None), (0, 1, 0, "ok", None)),
                               plans=("ok",), q=0, crashes=0, poison=0, gone=False),
    "upgrade_gone": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 1, "ok", 0)),
                         plans=("ok",), q=1, crashes=1, poison=0, gone=True),
    # two sessions send one position with no deadline: a joiner must be answered by the worker itself
    "joiners": dict(reqs=((0, 0, 0, "ok", None), (1, 0, 0, "ok", None)),
                    plans=("ok",), q=1, crashes=0, poison=0, gone=False, deadlines=False),
    "poison": dict(reqs=((0, 0, 0, "ok", None), (1, 1, 0, "ok", None)),
                   plans=("ok",), q=1, crashes=1, poison=1, gone=False),
    # v2.5: p1 alone, then p0 after p1's Outcome, then p2 (1033's scenario; review 1042)
    "abandoned_no_resend": dict(reqs=((0, 0, 1, "ok", None), (0, 0, 0, "ok", 0), (0, 0, 2, "ok", 1)),
                                plans=("ok",), q=1, crashes=0, poison=0, gone=False),
    # v2.5: one session sends one position twice before an Outcome (two sends, one run)
    "same_session_double_send": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 0, "ok", None)),
                                     plans=("ok", "fault"), q=1, crashes=1, poison=0, gone=False),
    "same_session_join_nodeadline": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 0, "ok", None)),
                                         plans=("ok",), q=1, crashes=0, poison=0, gone=False, deadlines=False),
    # v2.5 A2: with q = 0 a joined send needs a second slot, so it is Refused(Overloaded) while the first is owed
    "join_overflow": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 0, "ok", None)),
                          plans=("ok",), q=0, crashes=0, poison=0, gone=False),
    # v2.5 C10: a Failed p re-sent runs once
    "failed_resent": dict(reqs=((0, 0, 0, "ok", None), (0, 0, 0, "ok", 0)),
                          plans=("fault",), q=1, crashes=1, poison=0, gone=False),
}

RESENDABLE = ("Abandoned", "Refused(Overloaded)")
REPLY_BEARING = ("ok", "err", "fault", "gone")


class Fail(Exception):
    pass


def explore(name, mutant, machine=False):
    sc = SCENARIOS[name]
    reqs, plans, q, maxc, maxp, gone = sc["reqs"], sc["plans"], sc["q"], sc["crashes"], sc["poison"], sc["gone"]
    n = len(reqs)
    nsess = 1 + max(r[0] for r in reqs)
    limit = 1 + q
    groups = sorted({(r[0], key(r[1], r[2])) for r in reqs})
    gof = [groups.index((r[0], key(r[1], r[2]))) for r in reqs]
    ng = len(groups)

    # store: durable per stream (latest, st, rep, repver); durrep per key (rep, ver); vol ops
    def view(dur, vol):
        d = list(dur)
        for op in vol:
            k, s, p, rep, ver = op[0], op[1], op[2], op[3], op[4]
            d[s] = (p, "A", None, None) if k == "admit" else (p, "R", rep, ver)
        return tuple(d)

    def admit(sv, p):
        latest, st, rep, rv = sv
        if p == latest + 1:
            if st == "A" and mutant != "store_v22_admit":
                return ("Rejected", latest)
            return ("Admitted",)
        if p == latest and st == "R": return ("Duplicate", rep, rv)
        if p == latest and st == "A": return ("Abandoned",)
        return ("Rejected", latest)

    # state fields
    # 0 dur, 1 durrep, 2 eff, 3 vol, 4 ver(last assigned), 5 dver, 6 poisoned,
    # 7 inbox, 8 workers(per stream), 9 queues(per stream), 10 lastrel,
    # 11 relq, 12 base, 13 sessq(per session), 14 fwd(per session), 15 lastv(per session),
    # 16 client, 17 resends, 18 crashes, 19 poisons, 20 epoch, 21 admitted_keys,
    # 22 owed (per session: the request indices whose send the session owes; session-internal),
    # 23 sent, 24 rows, 25 lost (per (session, key) group: the client's own counts)
    init = (((-1, "N", None, None),) * STREAMS, ((None, None),) * KEYS, (0,) * KEYS, (), 0, 0, False,
            (), (None,) * STREAMS, ((),) * STREAMS, 0,
            (), 0, ((),) * nsess, (frozenset(),) * nsess, (0,) * nsess,
            ("unsent",) * n, (0,) * n, 0, 0, 0, frozenset(),
            (frozenset(),) * nsess, (0,) * ng, (0,) * ng, (0,) * ng)

    stats = {"states": 0, "cuts": 0, "terminal": 0}

    def sset(st, **kw):
        names = ("dur", "durrep", "eff", "vol", "ver", "dver", "poisoned", "inbox", "workers", "queues", "lastrel",
                 "relq", "base", "sessq", "fwd", "lastv", "client", "resends", "crashes", "poisons", "epoch", "admitted",
                 "owed", "sent", "rows", "lost")
        l = list(st)
        for k, v in kw.items(): l[names.index(k)] = v
        return tuple(l)

    def tup_set(t, i, v):
        l = list(t); l[i] = v; return tuple(l)

    def push(st, ver, entries):
        """Head pushes a Release (ver None = snapshot-less, commitless outcomes)."""
        lastrel = st[10] if ver is None else max(st[10], ver)
        return sset(st, relq=st[11] + ((ver, entries),), lastrel=lastrel)

    def direct(st, entries):
        """Mutant direct_commitless: frames to the session at the session's last version."""
        sq = list(st[13])
        for (se, s, p, out, c) in entries:
            sq[se] = sq[se] + ((None, s, p, out, c),)
        return sset(st, sessq=tuple(sq))

    def commitless(st, entries):
        return direct(st, entries) if mutant == "direct_commitless" else push(st, None, entries)

    def end_worker(st, s):
        return sset(st, workers=tup_set(st[8], s, None),
                    queues=tup_set(st[9], s, tuple((se, p, g, False) for (se, p, g, pk) in st[9][s])))

    def owed_of(owed_se, se, k):
        return [i for i in owed_se if key(reqs[i][1], reqs[i][2]) == k]

    def used(fwd_se, owed_se, se):
        return sum(max(1, len(owed_of(owed_se, se, k))) for k in fwd_se)

    def wire(st, se, k, o, target=None):
        """One Outcome row reaches the client of session se for key k. The client matches it to
        the send it names (an Abandoned row: the client knows its own deadlines) or to its oldest
        open send of the position; with no open send the row is counted and matched to nothing."""
        client, rows = st[16], st[24]
        g = groups.index((se, k))
        cands = [i for i in range(n) if gof[i] == g and client[i] == "inflight"]
        if target is not None and target in cands:
            cands = [target]
        if cands:
            client = tup_set(client, cands[0], o)
        return sset(st, client=client, rows=tup_set(rows, g, rows[g] + 1))

    def sent_one(st, i):
        g = gof[i]
        return sset(st, sent=tup_set(st[23], g, st[23][g] + 1))

    def succs(st):
        (dur, durrep, eff, vol, ver, dver, poisoned, inbox, workers, queues, lastrel,
         relq, base, sessq, fwd, lastv, client, resends, crashes, poisons, epoch, admitted,
         owed, sent, rows, lost) = st
        out = []
        sv = view(dur, vol)
        # ---- clients and sessions
        for i, (se, s, p, kind, dep) in enumerate(reqs):
            c = client[i]
            if c == "unsent" and (dep is None or client[dep] not in ("unsent", "inflight")):
                k = key(s, p)
                # one Call frame: the client counts the send and holds it open
                st1 = sent_one(sset(st, client=tup_set(client, i, "inflight")), i)
                if kind == "bad" and mutant != "decode_after_admit":
                    out.append((f"send{i}_malformed", wire(st1, se, k, "Refused(Malformed)", i)))
                    continue
                g = gone and epoch >= 1
                if g and mutant == "gone_refused_at_session":
                    out.append((f"send{i}_noentry", wire(st1, se, k, "Refused(NoEntry)", i)))
                    continue
                u = used(fwd[se], owed[se], se)
                if k in fwd[se]:
                    # a local join (V2): a second owed send of a held position takes a slot (v2.5 A2);
                    # a re-send of a held position with no owed send reuses its slot
                    need = u + (1 if owed_of(owed[se], se, k) and mutant != "join_outside_account" else 0)
                    if need > limit:
                        out.append((f"send{i}_full", wire(st1, se, k, "Refused(Overloaded)", i)))
                    else:
                        out.append((f"send{i}_join_local", sset(st1, owed=tup_set(owed, se, owed[se] | {i}))))
                elif u + 1 > limit:
                    out.append((f"send{i}_full", wire(st1, se, k, "Refused(Overloaded)", i)))
                else:
                    out.append((f"send{i}_fwd", sset(st1, owed=tup_set(owed, se, owed[se] | {i}),
                                                     fwd=tup_set(fwd, se, fwd[se] | {k}),
                                                     inbox=inbox + ((se, s, p, g, kind == "bad"),))))
            if c in RESENDABLE and resends[i] < 1:
                out.append((f"resend{i}", sset(st, client=tup_set(client, i, "unsent"), resends=tup_set(resends, i, resends[i] + 1))))
            # the session's own deadline timer for this send (V1; each send its own, v2.5 A1)
            if i in owed[se] and mutant != "deadline_rides_release" and sc.get("deadlines", True):
                nf = fwd
                if mutant == "queue_outside_account":
                    nf = tup_set(fwd, se, fwd[se] - {key(s, p)})
                st1 = sset(st, owed=tup_set(owed, se, owed[se] - {i}), fwd=nf)
                out.append((f"deadline{i}", wire(st1, se, key(s, p), "Abandoned", i)))
        # session delivers a frame from the publisher (or a direct frame)
        for se in range(nsess):
            if sessq[se]:
                (fv, s, p, o, cv) = sessq[se][0]
                v = lastv[se] if fv is None else fv
                if v < lastv[se]:
                    raise Fail(("C5 session frame version went back", v, lastv[se]))
                k = key(s, p)
                ow = owed_of(owed[se], se, k)
                nrows = len(ow)
                if mutant == "late_completion_resent": nrows = max(1, nrows)
                if mutant == "one_for_joined": nrows = min(1, nrows)
                if nrows and cv is not None and v < cv:
                    raise Fail(("C5 outcome reporting version", cv, "delivered in a frame at", v))
                st1 = sset(st, sessq=tup_set(sessq, se, sessq[se][1:]), lastv=tup_set(lastv, se, v),
                           fwd=tup_set(fwd, se, fwd[se] - {k}), owed=tup_set(owed, se, owed[se] - set(ow)))
                for _ in range(nrows):
                    st1 = wire(st1, se, k, o)
                out.append((f"deliver_s{se}", st1))
        # ---- publisher
        if relq:
            (rv, entries) = relq[0]
            nb = base if rv is None else max(base, rv)
            if rv is not None and rv < base:
                raise Fail(("publisher version went back", rv, base))
            sq = list(sessq)
            for (se, s, p, o, cv) in entries:
                sq[se] = sq[se] + ((nb, s, p, o, cv),)
            out.append(("publish", sset(st, relq=relq[1:], base=nb, sessq=tuple(sq))))
        # ---- head (stops entirely while a body hangs)
        hung = any(w is not None and w[1] == "hung" for w in workers)
        if not hung:
            if inbox:
                (se, s, p, g, bad) = inbox[0]
                w = workers[s]
                rest = inbox[1:]
                qs = tup_set(queues, s, tuple((a, b, c_, False) for (a, b, c_, pk) in queues[s]))  # an arrival clears parks
                if w is not None and w[0] == p:
                    if mutant == "drop_joiner" and se not in w[2]:
                        out.append(("head_drop", sset(st, inbox=rest)))
                    else:
                        nw = (w[0], w[1], w[2] + ((se,) if se not in w[2] else ()), w[3], w[4], w[5])
                        out.append(("head_join", sset(st, inbox=rest, workers=tup_set(workers, s, nw), queues=qs)))
                elif w is not None:
                    out.append(("head_queue", sset(st, inbox=rest, queues=tup_set(qs, s, qs[s] + ((se, p, g, False),)))))
                else:
                    out.append(("head_start", sset(st, inbox=rest, queues=qs,
                                                   workers=tup_set(workers, s, (p, "start", (se,), g, None, bad)))))
            for s in range(STREAMS):
                if workers[s] is None:
                    for qi, (se, p, g, pk) in enumerate(queues[s]):
                        if not pk:
                            nq = queues[s][:qi] + queues[s][qi + 1:]
                            out.append((f"head_take{s}", sset(st, queues=tup_set(queues, s, nq),
                                                               workers=tup_set(workers, s, (p, "start", (se,), g, None, False)))))
                            break
            for s in range(STREAMS):
                w = workers[s]
                if w is None: continue
                (p, ph, waiters, g, val, bad) = w
                k = key(s, p)
                if poisoned and ph != "await":
                    continue
                if ph == "start":
                    a = admit(sv[s], p)
                    if a[0] == "Admitted":
                        nver = ver + 1
                        out.append((f"admit{s}{p}", sset(st, vol=vol + (("admit", s, p, None, nver),), ver=nver,
                                                         admitted=admitted | {k},
                                                         workers=tup_set(workers, s, (p, "run", waiters, g, None, bad)))))
                    elif a[0] == "Abandoned":
                        out.append((f"abandoned{s}{p}", sset(st, workers=tup_set(workers, s, (p, "run", waiters, g, None, bad)))))
                    elif a[0] == "Duplicate" and mutant == "rerun_on_duplicate" and a[1] == "fault":
                        out.append((f"rerun{s}{p}", sset(st, workers=tup_set(workers, s, (p, "run", waiters, g, None, bad)))))
                    elif a[0] == "Duplicate":
                        st2 = commitless(st, tuple((se, s, p, a[1], a[2]) for se in waiters))
                        out.append((f"dup{s}{p}", end_worker(st2, s)))
                    elif p < a[1]:
                        st2 = commitless(st, tuple((se, s, p, "HistoryUnavailable", durrep[k][1]) for se in waiters))
                        out.append((f"history_unavailable{s}{p}", end_worker(st2, s)))
                    else:  # park behind the stream's FIFO
                        nq = queues[s] + tuple((se, p, g, True) for se in waiters)
                        out.append((f"park{s}{p}", sset(st, workers=tup_set(workers, s, None), queues=tup_set(queues, s, nq))))
                elif ph == "run":
                    if g:
                        out.append((f"gone{s}{p}", sset(st, workers=tup_set(workers, s, (p, "reply_only", waiters, g, "gone", bad)))))
                        continue
                    for plan in plans:
                        if plan in ("ok", "err"):
                            nver = ver + 1
                            out.append((f"commit{s}{p}{plan}", sset(st, vol=vol + (("reply", s, p, plan, nver, plan == "ok"),), ver=nver,
                                                                   workers=tup_set(workers, s, (p, "await", waiters, g, (plan, nver), bad)))))
                        elif plan == "fault":
                            out.append((f"fault{s}{p}", sset(st, workers=tup_set(workers, s, (p, "reply_only", waiters, g, "fault", bad)))))
                        elif plan == "refuse":
                            st2 = commitless(st, tuple((se, s, p, "Refused(Overloaded)", None) for se in waiters))
                            out.append((f"refused{s}{p}", end_worker(st2, s)))
                        elif plan == "hang":
                            out.append((f"hang{s}{p}", sset(st, workers=tup_set(workers, s, (p, "hung", waiters, g, None, bad)))))
                elif ph == "reply_only":
                    nver = ver + 1
                    out.append((f"replyonly{s}{p}", sset(st, vol=vol + (("reply", s, p, val, nver, False),), ver=nver,
                                                         workers=tup_set(workers, s, (p, "await", waiters, g, (val, nver), bad)))))
                elif ph == "await":
                    rep, cv = val
                    ok = dver >= cv
                    if ok or mutant == "answer_before_durable" or (poisoned and mutant == "poison_answers"):
                        # H3: the Release carries the durable version the group reached (>= cv), so
                        # Releases leave the head in version order whatever order workers finish in
                        rv = (cv if mutant == "release_at_commit_version" else dver) if ok else max(cv, lastrel)
                        st2 = push(st, rv, tuple((se, s, p, rep, cv) for se in waiters))
                        out.append((f"answer{s}{p}", end_worker(st2, s)))
        # ---- journal
        if vol and not poisoned:
            op = vol[0]
            ndur = view(dur, (op,))
            ndr, neff = durrep, eff
            if op[0] == "reply":
                k = key(op[1], op[2])
                ndr = tup_set(durrep, k, (op[3], op[4]))
                if op[5]: neff = tup_set(eff, k, eff[k] + 1)
            out.append(("flush", sset(st, dur=ndur, durrep=ndr, eff=neff, vol=vol[1:], dver=op[4])))
        if vol and not poisoned and poisons < maxp:
            out.append(("poison", sset(st, poisoned=True, poisons=poisons + 1)))

        def restart(st, label):
            # every session ends: the client counts each open send lost and re-sends after reconnecting
            nl = list(lost)
            for i, x in enumerate(client):
                if x == "inflight": nl[gof[i]] += 1
            cl = tuple("unsent" if x == "inflight" else x for x in client)
            return (label, sset(st, vol=(), ver=dver, poisoned=False, inbox=(), workers=(None,) * STREAMS,
                                queues=((),) * STREAMS, lastrel=dver, relq=(), base=dver, sessq=((),) * nsess,
                                fwd=(frozenset(),) * nsess, lastv=(dver,) * nsess, client=cl, epoch=epoch + 1,
                                crashes=crashes + (label == "crash"), owed=(frozenset(),) * nsess, lost=tuple(nl)))
        if poisoned and mutant != "poison_answers":
            out.append(restart(st, "poison_exit"))
        if crashes < maxc:
            out.append(restart(st, "crash"))
        return out

    def check(st):
        (dur, durrep, eff, vol, ver, dver, poisoned, inbox, workers, queues, lastrel,
         relq, base, sessq, fwd, lastv, client, resends, crashes, poisons, epoch, admitted,
         owed, sent, rows, lost) = st
        if any(e > 1 for e in eff): raise Fail(("C1 effect twice",))
        for g in range(ng):
            if rows[g] + lost[g] > sent[g]:
                raise Fail(("C9 more Outcomes than sends", groups[g], "sent", sent[g], "rows", rows[g], "lost", lost[g]))
        for se in range(nsess):
            if used(fwd[se], owed[se], se) > limit or len(owed[se]) > limit:
                raise Fail(("C6b session slots (owed sends, held positions) above 1+q", se, sorted(owed[se]), sorted(fwd[se])))
        sv = view(dur, vol)
        for s in range(STREAMS):
            w = workers[s]
            if w is not None and w[1] == "run" and sv[s][0] == w[0] and sv[s][1] == "R":
                raise Fail(("C10 a body run starts at a position whose reply the store holds", (s, w[0])))
        for i, (se, s, p, kind, dep) in enumerate(reqs):
            c = client[i]; k = key(s, p)
            if c in REPLY_BEARING and durrep[k][0] != c:
                raise Fail(("C3 answer", c, "for", (s, p), "but durable reply", durrep[k][0]))
            if c == "HistoryUnavailable" and durrep[k][0] is None:
                raise Fail(("C3 HistoryUnavailable for a position never replied", (s, p)))
            if kind == "bad" and k in admitted:
                raise Fail(("C7 a malformed request's position was admitted", (s, p)))
        held = [0] * nsess
        for (se, *_r) in inbox: held[se] += 1
        for s in range(STREAMS):
            for (se, *_r) in queues[s]: held[se] += 1
            if workers[s] is not None:
                for se in workers[s][2]: held[se] += 1
        if any(h > limit for h in held): raise Fail(("C6 head holds more than 1+q requests of a session", held))

    def terminal_check(st):
        client, dur = st[16], st[0]
        sent, rows, lost = st[23], st[24], st[25]
        for g in range(ng):
            if rows[g] + lost[g] != sent[g]:
                raise Fail(("C9 rows + lost != sends at a terminal state" if rows[g] + lost[g] > sent[g] else "C9 a send with no Outcome at a terminal state", groups[g], "sent", sent[g], "rows", rows[g], "lost", lost[g]))
        for i, c in enumerate(client):
            if c in ("unsent", "inflight"): raise Fail(("C4 liveness: request", i, "never answered"))
        if gone:
            for s in range(STREAMS):
                latest, stt = dur[s][0], dur[s][1]
                if stt == "A":
                    sent_next = any(rs == s and rp == latest + 1 for (_, rs, rp, _, _) in reqs)
                    asked = [client[i] for i, (_, rs, rp, _, _) in enumerate(reqs) if rs == s and rp == latest]
                    if sent_next and asked and all(a != "Abandoned" for a in asked):
                        raise Fail(("C8 gone position left admitted without a reply", (s, latest), asked))

    if machine:
        return dict(init=init, successors=succs, check=check, terminal=terminal_check, reqs=reqs, groups=groups)

    seen = {init: None}
    dq = deque([init])
    try:
        while dq:
            st = dq.popleft()
            stats["states"] += 1
            check(st)
            nx = succs(st)
            real = [x for x in nx if x[0] not in ("crash", "poison", "poison_exit")]
            if not real and not st[6]:
                stats["terminal"] += 1
                terminal_check(st)
            for label, ns in nx:
                if label in ("crash", "poison_exit"): stats["cuts"] += 1
                if ns not in seen:
                    seen[ns] = (st, label)
                    dq.append(ns)
    except Fail as e:
        trace = []
        cur = st
        while seen.get(cur):
            prev, lab = seen[cur]; trace.append(lab); cur = prev
        return {"ok": False, "why": e.args[0], "trace": list(reversed(trace)), **stats}
    return {"ok": True, **stats}


if __name__ == "__main__":
    args = sys.argv[1:]
    scen = [a[2:] for a in args if a.startswith("s=")]
    only = [a for a in args if not a.startswith("s=")] or list(MUTANTS)
    for m in only:
        tot = {"states": 0, "cuts": 0, "terminal": 0}; caught = []
        for name in (scen or sorted(SCENARIOS, key=lambda x: x in ("two_sessions_two_streams", "cross_session_crash") and (1 + (x == "cross_session_crash")))):
            r = explore(name, m)
            for k in tot: tot[k] += r[k]
            if not r["ok"]: caught.append((name, r["why"], r["trace"][-16:]))
            if caught and m != "none": break  # a mutant stops at its first failing scenario
            if m == "none":
                print(f"   {name}: states={r['states']} cuts={r['cuts']} terminal={r['terminal']} {'ok' if r['ok'] else 'FAIL'}")
        if m == "none":
            print(f"[none] {MUTANTS[m]}: states={tot['states']} cuts={tot['cuts']} failures={len(caught)}")
            for c in caught: print("   FAIL", c)
        else:
            print(f"[{m}] {MUTANTS[m]}: {'CAUGHT' if caught else 'NOT CAUGHT'}")
            for c in caught[:1]: print("    ", c[0], c[1], " ".join(c[2]))


def export_obs(path):
    """Export counterexamples and 200 seeded maximal histories in the shared Obs vocabulary."""
    from export import export_calls
    return export_calls(path)
