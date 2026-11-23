"""RUNNER-SPEC subscription and output-gate model.

The original K1–K5 explorer models journal delivery. The shared-views explorer
checks L55–L57 with per-session writer cuts and a publisher barrier.

Explicit-state exploration of every interleaving of six commits (in any order), journal flushes,
the head's push to the publisher, publisher rounds, one Subscribe and at most one crash.

Store keys: perm (bool), ptr (0/1), row0, row1.  The query (an entry) reads perm; if allowed it
reads ptr and covers row[ptr].  Its read set is {perm} or {perm, ptr}; its Cover is {row[ptr]}.
Commits: revoke, grant, move (ptr=1), w0 (row0=1), w1 (row1=1), call (row1=2, this session's call:
its Outcome is attached at that commit's version).

v2 rules modelled (RUNNER-SPEC v2 section 4):
  G1 the head pushes (version, snapshot, outcomes) to the publisher only after the version is durable;
  G2 a publisher round drains every pushed entry, diffs its base snapshot against the newest one,
     re-runs a subscription's query when a changed key is in its read set (sending the difference of
     the old and new results), else sends the changed keys inside its Cover; outcomes drained in the
     round go in that round's batch; the batch carries the newest version;
  G3 Subscribe runs the query at the publisher's base and sends the full result at that version.
Checks:
  K1 after every batch at version u the client cache equals the query re-run from scratch at u
     (independent reference follower: full evaluation, no diff, no read sets);
  K2 batch versions never decrease;  K3 at a crash no sent version exceeds the durable version;
  K4 an Outcome is in the first batch at version >= its commit version, never earlier (bounded delay);
  K5 liveness: with no crash, every terminal state has the Outcome delivered and the cache equal to
     the query at the final version.
"""
import sys

INIT = {"perm": True, "ptr": 0, "row0": 0, "row1": 0}
COMMITS = ("revoke", "grant", "move", "w0", "w1", "call")
OPS = {"revoke": ("perm", False), "grant": ("perm", True), "move": ("ptr", 1),
       "w0": ("row0", 1), "w1": ("row1", 1), "call": ("row1", 2)}
MUTANTS = {
    "none": "v2 rules",
    "saved_cover": "never re-run the query (v1 section 4)",
    "partial_read_set": "re-run only when perm changes (ptr read not recorded)",
    "publish_on_commit": "head pushes at commit, not at durable (v1 R1)",
    "outcome_late": "an outcome waits one more round than its version needs",
    "rerun_without_retract": "re-run sends the new rows but not the removal of old ones",
}

def snap(order, k):
    s = dict(INIT)
    for c in order[:k]:
        key, val = OPS[c]; s[key] = val
    return s

def query(s):
    if not s["perm"]: return {}, frozenset({"perm"})
    key = "row%d" % s["ptr"]
    return {key: s[key]}, frozenset({"perm", "ptr"})

def explore(mutant):
    seen = set(); stats = {"states": 0, "terminal": 0, "paths": 0}
    sys.setrecursionlimit(100000)
    class F(Exception): pass
    first = []

    def batch(st, u, rows, outs):
        (order, durable, pushed, base, sub, cache, last, outv, held, crashed, maxsent) = st
        if u < last: raise F(("K2 version went back", u, last))
        cache = dict(cache)
        for k, v in rows.items():
            if v is None: cache.pop(k, None)
            else: cache[k] = v
        if sub is not None and cache != query(snap(order, u))[0]:
            raise F(("K1 cache differs from reference at", u, cache, query(snap(order, u))[0], order))
        vc = order.index("call") + 1 if "call" in order else None
        got = outv
        if outs:
            if u < vc: raise F(("K4 outcome before its version", u, vc))
            got = u
        elif vc is not None and outv is None and u >= vc and vc <= pushed:
            raise F(("K4 first batch at >= version lacks the outcome", u, vc))
        return frozenset(cache.items()), u, got, max(maxsent, u)

    def dfs(st, trace):
        key = st
        if key in seen: return
        seen.add(key); stats["states"] += 1
        (order, durable, pushed, base, sub, cache, last, outv, held, crashed, maxsent) = st
        succ = []
        if crashed:
            stats["terminal"] += 1; return
        for c in COMMITS:
            if c not in order:
                succ.append((c, (order + (c,), durable, pushed, base, sub, cache, last, outv, held, crashed, maxsent)))
        if durable < len(order):
            succ.append(("flush", (order, durable + 1, pushed, base, sub, cache, last, outv, held, crashed, maxsent)))
        limit = len(order) if mutant == "publish_on_commit" else durable
        if limit > pushed:
            succ.append(("push", (order, durable, limit, base, sub, cache, last, outv, held, crashed, maxsent)))
        if pushed > base or held:
            u = pushed
            vc = order.index("call") + 1 if "call" in order else None
            outs = vc is not None and outv is None and base < vc <= u
            nheld = False
            if mutant == "outcome_late" and outs and not held:
                outs, nheld = False, True
            elif held: outs = True
            rows = {}; nsub = sub
            if sub is not None:
                result, rs, cover = sub
                old, new = snap(order, base), snap(order, u)
                changed = {k for k in new if new[k] != old[k]}
                watch = {"perm"} if mutant == "partial_read_set" else rs
                if changed & watch and mutant != "saved_cover":
                    nres, nrs = query(new)
                    for k in dict(result):
                        if k not in nres and mutant != "rerun_without_retract": rows[k] = None
                    for k, v in nres.items():
                        if dict(result).get(k) != v: rows[k] = v
                    nsub = (frozenset(nres.items()), nrs, frozenset(nres))
                else:
                    for k in changed & cover: rows[k] = new[k]
                    nres = dict(result); nres.update({k: new[k] for k in changed & cover})
                    nsub = (frozenset(nres.items()), rs, cover)
            nst = (order, durable, pushed, u, nsub, cache, last, outv, nheld, crashed, maxsent)
            if rows or outs:
                try:
                    c2, l2, o2, m2 = batch(nst, u, rows, outs)
                except F as e:
                    raise F((e.args[0], trace + ["round"]))
                nst = (order, durable, pushed, u, nsub, c2, l2, o2, nheld, crashed, m2)
            succ.append(("round", nst))
        if sub is None:
            res, rs = query(snap(order, base))
            nsub = (frozenset(res.items()), rs, frozenset(res))
            nst = (order, durable, pushed, base, nsub, cache, last, outv, held, crashed, maxsent)
            try:
                c2, l2, o2, m2 = batch(nst, base, dict(res), False)
            except F as e:
                raise F((e.args[0], trace + ["subscribe"]))
            succ.append(("subscribe", (order, durable, pushed, base, nsub, c2, l2, o2, held, crashed, m2)))
        if not crashed:
            if maxsent > durable:
                succ.append(("crash", None))
            else:
                succ.append(("crash", (order, durable, pushed, base, sub, cache, last, outv, held, True, maxsent)))
        real = [s for s in succ if s[0] != "crash"]
        if not real:
            stats["terminal"] += 1
            if outv is None or (sub is not None and dict(cache) != query(snap(order, len(order)))[0]):
                raise F(("K5 liveness", trace))
        for label, nxt in succ:
            if nxt is None: raise F(("K3 sent a version the crash lost", trace + ["crash"]))
            dfs(nxt, trace + [label])

    init = ((), 0, 0, 0, None, frozenset(), 0, None, False, False, 0)
    try:
        dfs(init, [])
    except F as e:
        return {"ok": False, "why": e.args[0]}
    return {"ok": True, **stats}

def main():
    from shared_views import MUTANTS as VIEW_MUTANTS, Violation, check_shared_views, failed_read_barrier

    if len(sys.argv) == 2 and sys.argv[1] in MUTANTS:
        # The original model's fault injections remain selectable.
        name = sys.argv[1]
        result = explore(name)
        print(name, result)
        return int(result["ok"] != (name == "none"))
    if len(sys.argv) != 1:
        raise SystemExit("usage: subs_model.py [original-mutant]")

    legacy = explore("none")
    if not legacy["ok"]:
        raise SystemExit(f"original K1–K5 model failed: {legacy['why']}")
    print(f"K1–K5: {legacy['states']} states, {legacy['terminal']} terminals")
    count = check_shared_views() + failed_read_barrier()
    print(f"L55–L57: {count} writer schedules, zero violations")
    for mutant in VIEW_MUTANTS:
        try:
            check_shared_views(mutant)
        except Violation as e:
            print(f"{mutant}: {e.law}, {len(e.trace)} steps, {e}")
            print("  " + " → ".join(e.trace))
        else:
            raise SystemExit(f"{mutant} did not violate L55–L57")
    return 0


if __name__ == "__main__":
    sys.exit(main())
