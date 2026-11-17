"""RUNNER-SPEC v2.5 subscription model (subs22_model.py plus K8): two sessions, budgets, Reset, Ended, restart, branches.

Explicit-state BFS over every interleaving of: main commits (any order), a commit on a fork and its
adopt into main, journal flushes, the head's Release pushes (after durable), publisher rounds,
Subscribe from either session at any time (also between a commit and its batch), the wire
delivering queued frames to each client (a slow reader is a session whose frames are not
delivered), Reset handling, and at most one crash followed by reopen and resume.

Store keys: perm, row0, row1.  Queries (entries):
  Q1 reads perm; if perm, covers {row0, row1}, else returns nothing.  Read set {perm}.
  Qf reads row1; faults when row1 == 2 (a fault at a re-run), else returns {row1}.  Read set {row1}.
Session 0 subscribes Q1 and Qf; session 1 subscribes Q1.  Main commits: revoke, grant, w0
(row0=1), call (row1=2; session 0's call, its Outcome attached at that version).  Fork: fw
(row0=3 on a fork of main; never released), then adopt (row0=3 on main).
Budget: a session's queue holds at most B rows; more is Reset (Reset carries the dropped
frames' outcomes); the client re-subscribes after Reset.  Restart: the publisher starts at the
durable version of the new open; a resume token from the earlier open is Foreign for `changes`,
so the session gets Reset.

Checks:
  K1 after every frame at version u, each live subscription's client cache equals its query run
     from scratch over main at u (independent reference: no diff, no read sets);
  K2 frame versions a client receives never decrease within one connection;
  K3 at a crash no sent version exceeds the durable version;
  K4 the Outcome is in the first frame to session 0 at a version >= its commit, never earlier;
  K5 liveness: in a terminal state the Outcome of a durable call was delivered and every live
     subscription's cache equals its query at the final version;
  K6 a session's queue never holds more than B rows (slow-reader budget);
  K7 a fault in Qf ends only Qf (Q1 of the same session still follows);
  K8 (v2.5) at most one Outcome per send: the client sends the call once, and again after a reopen
     only if it has no Outcome, so a frame carrying the Outcome when the client already has one is a
     second Outcome for one send (subs22's client absorbed it). K5 is the at-least-once half.
"""
import sys
from collections import deque

INIT = (("perm", True), ("row0", 0), ("row1", 0))
MAIN = ("revoke", "grant", "w0", "call", "adopt")
OPS = {"revoke": ("perm", False), "grant": ("perm", True), "w0": ("row0", 1), "call": ("row1", 2),
       "adopt": ("row0", 3), "fw": ("row0", 3)}
SUBS = ((0, "Q1"), (0, "Qf"), (1, "Q1"))
B = 2

MUTANTS = {
    "none": "v2.5 rules",
    "subscribe_at_latest": "Subscribe runs the query at the newest pushed snapshot, labelled with the base",
    "shared_cache": "a later Subscribe copies another session's stored result for the same query; cover rows do not refresh it",
    "fault_ends_session": "a query fault ends every subscription of the session",
    "unbounded_queue": "slow reader: frames queue past the budget (a queue outside the session account)",
    "reset_drops_outcomes": "Reset drops the outcomes of the frames it replaces",
    "resume_foreign_as_empty": "after reopen, a resume token's Foreign `changes` is read as no change",
    "release_fork_commits": "the head releases a fork's commit to the main publisher",
    "outcome_never": "outcomes are never put in a frame (passes L6, needs L7)",
    "rerun_without_retract": "a re-run sends new rows but not the removal of old ones",
    "outcome_repeated": "the publisher puts the Outcome in every frame at a version >= its commit, not only the first",
}


def snap(order):
    s = dict(INIT)
    for c in order:
        k, v = OPS[c]; s[k] = v
    return s


def snap_ref(order):
    """The reference reads main only: a fork's write is never part of main."""
    return snap(tuple(c for c in order if c != "fw"))


def query(q, s):
    """-> (result dict, read set, cover) or None for a fault."""
    if q == "Q1":
        if not s["perm"]: return {}, frozenset({"perm"}), frozenset()
        return {"row0": s["row0"], "row1": s["row1"]}, frozenset({"perm"}), frozenset({"row0", "row1"})
    if s["row1"] == 2: return None
    return {"row1": s["row1"]}, frozenset({"row1"}), frozenset()


class Fail(Exception):
    pass


SCEN = {
    # name: (main commits, fork allowed, subscriptions, crashes allowed)
    "main_two_sessions": (("revoke", "grant", "w0", "call"), False, ((0, "Q1"), (0, "Qf"), (1, "Q1")), False),
    "restart_resume": (("revoke", "w0", "call"), False, ((0, "Q1"), (0, "Qf"), (1, "Q1")), True),
    "branch_adopt": (("w0", "call", "adopt"), True, ((0, "Q1"), (1, "Q1")), True),
    # session 0 subscribes to nothing: its only frames carry its call's outcome. Here a publisher
    # that never sends the outcome passes K4 (L6: no frame is ever late) and fails only K5 (L7).
    "outcome_only": (("w0", "call"), False, ((1, "Q1"),), False),
}


def explore_all(mutant, ref_self=False):
    global MAIN, SUBS
    tot = {"states": 0, "cuts": 0, "terminal": 0}; res = []
    for name, (main, fork_ok, subs, crash_ok) in SCEN.items():
        MAIN, SUBS = main, subs
        r = explore(mutant, ref_self, fork_ok, crash_ok)
        for k in tot: tot[k] += r[k]
        res.append((name, r))
    return tot, res


def explore(mutant, ref_self=False, fork_ok=True, crash_ok=True, machine=False):
    NS = len(SUBS)
    # state: order(main commits applied, in version order), fork(bool fw done), durable(int),
    #  pushed(int), base(int), subs(per SUBS idx: None | "ended" | (result, rs, cover)),
    #  queues(per session: tuple of frames), ccache(per SUBS idx: None | "ended" | frozenset items | "want"),
    #  clast(per session last version), outv(int|None delivered version of outcome), crashed(bool), conn(int)
    #  pend_out(bool outcome to resend after reopen)
    init = ((), not fork_ok, 0, 0, 0, (None,) * NS, ((), ()), ("want",) * NS, (0, 0), None, not crash_ok, 0, False)
    stats = {"states": 0, "cuts": 0, "terminal": 0}

    def vcall(order):
        return order.index("call") + 1 if "call" in order else None

    def check_client(order, se, u, ccache, outs, outv, subs_live):
        main = snap_ref(order[:u])
        for idx, (s, q) in enumerate(SUBS):
            if s != se: continue
            c = ccache[idx]
            if isinstance(c, frozenset):
                ref = query(q, main)
                if ref_self:
                    if isinstance(subs_live[idx], tuple) and dict(c) != dict(subs_live[idx][0]):
                        raise Fail(("K1(self) cache differs from the publisher's own result", q))
                    continue
                if ref is None:
                    raise Fail(("K1 live cache for a query that faults at", u, q))
                if dict(c) != ref[0]:
                    raise Fail(("K1 cache differs from reference", se, q, u, dict(c), ref[0]))

    def succs(st):
        (order, fork, durable, pushed, base, subs, queues, ccache, clast, outv, crashed, conn, pend_out) = st
        out = []
        # commits
        for c in MAIN:
            if c in order: continue
            if c == "adopt" and not fork: continue
            out.append((c, st[:0] + (order + (c,),) + st[1:]))
        if not fork and "adopt" in MAIN:
            if mutant == "release_fork_commits" and "fw" not in order:
                out.append(("fw", (order + ("fw",), True) + st[2:]))
            else:
                out.append(("fw", (order, True) + st[2:]))
        if durable < len(order):
            out.append(("flush", (order, fork, durable + 1) + st[3:]))
        if durable > pushed:
            out.append(("push", (order, fork, durable, durable) + st[4:]))
        # publisher round
        vc = vcall(order)
        if pushed > base or pend_out:
            u = pushed
            old, new = snap(order[:base]), snap(order[:u])
            changed = {k for k in new if new[k] != old[k]}
            nsubs = list(subs)
            frames = [dict(rows={}, outs=False, ended=[]), dict(rows={}, outs=False, ended=[])]
            for idx, (se, q) in enumerate(SUBS):
                sub = subs[idx]
                if not isinstance(sub, tuple): continue
                result, rs, cover = sub
                if changed & rs:
                    r = query(q, new)
                    if r is None:
                        nsubs[idx] = "ended"; frames[se]["ended"].append(idx)
                        if mutant == "fault_ends_session":
                            for j, (s2, _) in enumerate(SUBS):
                                if s2 == se and isinstance(nsubs[j], tuple):
                                    nsubs[j] = "ended"; frames[se]["ended"].append(j)
                        continue
                    nres = r[0]; old_r = dict(result)
                    for k in old_r:
                        if k not in nres and mutant != "rerun_without_retract": frames[se]["rows"][(idx, k)] = None
                    for k, v in nres.items():
                        if old_r.get(k) != v: frames[se]["rows"][(idx, k)] = v
                    nsubs[idx] = (frozenset(nres.items()), r[1], r[2])
                else:
                    hit = changed & cover
                    for k in hit: frames[se]["rows"][(idx, k)] = new[k]
                    if mutant != "shared_cache":
                        res = dict(result); res.update({k: new[k] for k in hit})
                        nsubs[idx] = (frozenset(res.items()), rs, cover)
            if mutant == "outcome_repeated" and vc is not None and vc <= u:
                frames[0]["outs"] = True
            if mutant != "outcome_never" and ((vc is not None and base < vc <= u) or pend_out):
                frames[0]["outs"] = True
            nq = list(queues); nc_subs = nsubs
            for se in (0, 1):
                f = frames[se]
                if not (f["rows"] or f["outs"] or f["ended"]): continue
                queued = sum(len(fr[2]) for fr in nq[se] if fr[0] == "B")
                if queued + len(f["rows"]) > B and mutant != "unbounded_queue":
                    keep_outs = f["outs"] or (mutant != "reset_drops_outcomes" and any(fr[3] for fr in nq[se]))
                    nq[se] = (("R", u, (), keep_outs, ()),)
                    nc_subs = [("dropped" if (SUBS[j][0] == se and isinstance(nc_subs[j], tuple)) else nc_subs[j]) for j in range(NS)]
                else:
                    nq[se] = nq[se] + (("B", u, tuple(sorted(f["rows"].items())), f["outs"], tuple(f["ended"])),)
            nsubs = [None if s_ == "dropped" else s_ for s_ in nc_subs]
            out.append(("round", (order, fork, durable, pushed, u, tuple(nsubs), tuple(nq), ccache, clast, outv, crashed, conn, False)))
        # Subscribe (client wants it, publisher has none for it)
        for idx, (se, q) in enumerate(SUBS):
            if ccache[idx] == "want" and subs[idx] is None:
                at = pushed if mutant == "subscribe_at_latest" else base
                r = query(q, snap(order[:at]))
                if mutant == "shared_cache":
                    for j, (s2, q2) in enumerate(SUBS):
                        if j != idx and q2 == q and isinstance(subs[j], tuple):
                            r = (dict(subs[j][0]), subs[j][1], subs[j][2]); break
                nsubs = list(subs); nq = list(queues)
                if r is None:
                    nsubs[idx] = "ended"
                    nq[se] = nq[se] + (("B", base, (), False, (idx,)),)
                else:
                    nsubs[idx] = (frozenset(r[0].items()), r[1], r[2])
                    rows = tuple(sorted(((idx, k), v) for k, v in r[0].items()))
                    # a full result replaces nothing; it is charged like any frame
                    queued = sum(len(fr[2]) for fr in nq[se] if fr[0] == "B")
                    if queued + len(rows) > B and mutant != "unbounded_queue":
                        nq[se] = (("R", base, (), any(fr[3] for fr in nq[se]), ()),)
                        nsubs = [(None if (SUBS[j][0] == se and isinstance(nsubs[j], tuple)) else nsubs[j]) for j in range(NS)]
                    else:
                        nq[se] = nq[se] + (("B", base, rows, False, ()),)
                cc = list(ccache); cc[idx] = "asked"
                out.append((f"subscribe{idx}", (order, fork, durable, pushed, base, tuple(nsubs), tuple(nq), tuple(cc), clast, outv, crashed, conn, pend_out)))
        # the wire delivers one frame to a client
        for se in (0, 1):
            if not queues[se]: continue
            kind, u, rows, outs, ended = queues[se][0]
            if u < clast[se]: raise Fail(("K2 frame version went back", se, u, clast[se]))
            cc = list(ccache)
            nov = outv
            if outs and se == 0:
                if vc is not None and u < vc: raise Fail(("K4 outcome before its version", u, vc))
                if outv is not None: raise Fail(("K8 a second Outcome for one send", "first at", outv, "again at", u))
                if nov is None: nov = u
            if kind == "R":
                for idx, (s2, _) in enumerate(SUBS):
                    if s2 == se and cc[idx] != "ended": cc[idx] = "want"
            else:
                for idx in ended: cc[idx] = "ended"
                for (idx, k), v in rows:
                    c = cc[idx]
                    d = dict(c) if isinstance(c, frozenset) else {}
                    if v is None: d.pop(k, None)
                    else: d[k] = v
                    if cc[idx] != "ended": cc[idx] = frozenset(d.items())
                if mutant == "fault_ends_session":
                    pass
            if se == 0 and vc is not None and nov is None and u >= vc and vc <= pushed and kind == "B" and conn == 0:
                raise Fail(("K4 first frame at >= the call's version lacks the outcome", u, vc))
            check_client(order, se, u, tuple(cc), outs, nov, subs)
            nq = list(queues); nq[se] = queues[se][1:]
            ncl = list(clast); ncl[se] = u
            out.append((f"wire{se}", (order, fork, durable, pushed, base, subs, tuple(nq), tuple(cc), tuple(ncl), nov, crashed, conn, pend_out)))
        # crash, reopen and resume (once)
        if not crashed:
            if max(clast) > durable or any(fr[1] > durable for qq in queues for fr in qq if False):
                out.append(("crash", "K3"))
            else:
                norder = order[:durable]
                v_new = durable
                nvc = vcall(norder)
                po = nvc is not None and outv is None and mutant != "outcome_never"   # the client re-sends; Duplicate, commitless
                cc = list(ccache)
                if mutant == "resume_foreign_as_empty":
                    nsubs = tuple((("dropped") if isinstance(s_, tuple) else s_) for s_ in subs)
                    # the publisher keeps the old results and diffs nothing: caches stay as they are
                    nsubs = tuple(((frozenset(dict(c).items()), query(q, snap(norder))[1] if query(q, snap(norder)) else frozenset(), query(q, snap(norder))[2] if query(q, snap(norder)) else frozenset())
                                   if isinstance(c, frozenset) else (None if c != "ended" else "ended"))
                                  for c, (s2, q) in zip(cc, SUBS))
                    nst = (norder, fork, durable, durable, v_new, nsubs, ((), ()), tuple(cc), (v_new, v_new), outv, True, conn + 1, po)
                else:
                    # resume token Foreign -> Reset: the client re-subscribes everything not ended
                    for idx in range(NS):
                        if cc[idx] != "ended": cc[idx] = "want"
                    nsubs = tuple("ended" if c == "ended" else None for c in cc)
                    nst = (norder, fork, durable, durable, v_new, nsubs, ((), ()), tuple(cc), (v_new, v_new), outv, True, conn + 1, po)
                out.append(("crash", nst))
        return out

    def terminal(st):
        (order, fork, durable, pushed, base, subs, queues, ccache, clast, outv, crashed, conn, pend_out) = st
        vc = vcall(order)
        if vc is not None and outv is None:
            raise Fail(("K5 liveness: the call's outcome never delivered",))
        fin = snap_ref(order)
        for idx, (se, q) in enumerate(SUBS):
            c = ccache[idx]
            if isinstance(c, frozenset):
                ref = query(q, fin)
                if ref is None or dict(c) != ref[0]:
                    raise Fail(("K5 final cache differs", se, q, dict(c), ref and ref[0]))
            if c == "ended" and query(q, fin) is not None and q == "Q1":
                raise Fail(("K7 Q1 ended by another query's fault", se))

    def check(st):
        queues = st[6]
        for se in (0, 1):
            if sum(len(fr[2]) for fr in queues[se] if fr[0] == "B") > B:
                raise Fail(("K6 queue past the budget", se))

    if machine:
        return dict(init=init, successors=succs, check=check, terminal=terminal)

    seen = {init: None}
    dq = deque([init])
    st = init
    try:
        while dq:
            st = dq.popleft(); stats["states"] += 1
            check(st)
            nx = succs(st)
            real = [x for x in nx if x[0] != "crash"]
            if not real:
                stats["terminal"] += 1; terminal(st)
            for label, ns in nx:
                if label == "crash":
                    stats["cuts"] += 1
                    if ns == "K3": raise Fail(("K3 a sent version exceeds the durable version at a crash",))
                if ns not in seen:
                    seen[ns] = (st, label); dq.append(ns)
    except Fail as e:
        trace = []; cur = st
        while seen.get(cur):
            prev, lab = seen[cur]; trace.append(lab); cur = prev
        return {"ok": False, "why": e.args[0], "trace": list(reversed(trace)), **stats}
    return {"ok": True, **stats}


if __name__ == "__main__":
    only = sys.argv[1:] or list(MUTANTS)
    for m in only:
        tot, res = explore_all(m)
        bad = [(n, r) for n, r in res if not r["ok"]]
        if m == "none":
            for n, r in res: print(f"   {n}: states={r['states']} cuts={r['cuts']} terminal={r['terminal']} {'ok' if r['ok'] else 'FAIL'}")
            print(f"[none] {MUTANTS[m]}: states={tot['states']} cuts={tot['cuts']} failures={len(bad)}")
            for n, r in bad: print("   FAIL", n, r["why"], " ".join(r["trace"][-20:]))
        else:
            print(f"[{m}] {MUTANTS[m]}: {'CAUGHT' if bad else 'NOT CAUGHT'}")
            for n, r in bad[:1]: print("    ", n, r["why"], " ".join(r["trace"][-14:]))
    if not sys.argv[1:]:
        # L6 without L7: the outcome-only scenario alone, with outcome_never.
        MAIN, SUBS = SCEN["outcome_only"][0], SCEN["outcome_only"][2]
        r = explore("outcome_never", False, False, False)
        print(f"[outcome_never, outcome_only scenario] {'CAUGHT by ' + r['why'][0][:2] if not r['ok'] else 'NOT CAUGHT'}: {r['why'] if not r['ok'] else ''}")


def export_obs(path):
    """Export counterexamples and 200 seeded maximal histories in the shared Obs vocabulary."""
    from export import export_subs
    return export_subs(path)
