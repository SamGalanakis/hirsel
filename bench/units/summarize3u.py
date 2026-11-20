#!/usr/bin/env python3
"""Summarize part3u.jsonl: best passing rate per cell, side and search group,
latency at that rate, the verdict ratios and untied counters per turn."""
import collections, json, statistics, sys

CELL_ORDER = [("base", 0.0, False), ("base", 1.0, False), ("par", 0.5, False), ("par", 0.1, False),
              ("base", 0.0, True), ("par", 0.5, True)]
SIDES = ["untied", "untied-oo", "S3k7", "tokiow8"]


def key(c):
    return (c["variant"], c["f"], c["rec"])


def main(paths):
    runs, summ = [], []
    for p in paths:
        for line in open(p):
            r = json.loads(line)
            (summ if r.get("summary") else runs).append(r)
    # A search is (cell, side, group); its best is the highest passing clean rate.
    searches = collections.defaultdict(list)
    for r in runs:
        if r.get("discarded") or r.get("infra_retry"):
            continue
        searches[(key(r["cell"]), r["side"], r["group"])].append(r)
    stats = collections.Counter()
    for r in runs:
        stats["runs"] += 1
        stats["discarded"] += bool(r.get("discarded"))
        stats["infra_retry"] += bool(r.get("infra_retry"))
        stats["aborted"] += bool(r.get("aborted"))
        stats["checksum_failures"] += (r.get("load") or {}).get("failures", 0) or 0
        u = r.get("ustats")
        if u:
            stats["untied_runs"] += 1
            stats["token_acquires"] += int(u.get("token_acquires", 0))
    print("integrity:", dict(stats))
    best = {}
    for (ck, side, g), rs in sorted(searches.items(), key=lambda kv: str(kv[0])):
        ok = [r for r in rs if r["pass"]]
        top = max(ok, key=lambda r: r["rate"]) if ok else None
        lo = min(r["rate"] for r in rs)
        best.setdefault((ck, side), []).append((g, top, lo, len(rs)))
    print()
    print("| cell | side | best passing rate per search | searches | p50 / p99 ms at best | CPU µs/turn at best |")
    print("|---|---|---|---|---|---|")
    medians = {}
    for ck in CELL_ORDER:
        for side in SIDES:
            if (ck, side) not in best:
                continue
            items = best[(ck, side)]
            parts, rates = [], []
            lat = []
            cpu = []
            for g, top, lo, n in items:
                if top:
                    parts.append(f"{top['rate']:.0f} (g{g}, {n} runs)")
                    rates.append(top["rate"])
                    L = top["load"]
                    lat.append(f"{L['p50_ns'] / 1e6:.2f} / {L['p99_ns'] / 1e6:.2f}")
                    cpu.append(f"{top['server_cpu_s'] / L['completed'] * 1e6:.0f}")
                else:
                    parts.append(f"never passed (searched down to {lo:.0f}) (g{g})")
                    rates.append(0)
            medians[(ck, side)] = max(rates) if rates else None
            name = f"{ck[0]} f={ck[1]}{' rec' if ck[2] else ''}"
            print(f"| {name} | {side} | {'; '.join(parts)} | {len(items)} | {'; '.join(lat)} | {'; '.join(cpu)} |")
    print()
    print("best of searches; ratios untied / other:")
    for ck in CELL_ORDER:
        u = medians.get((ck, "untied"))
        s3 = medians.get((ck, "S3k7"))
        tk = medians.get((ck, "tokiow8"))
        f = lambda a, b: f"{a / b:.2f}" if a is not None and b else "-"
        print(f"  {ck}: untied={u} S3k7={s3} tokio={tk}  u/S3={f(u, s3)} u/tokio={f(u, tk)}")
    rec_off = medians.get((("base", 0.0, False), "untied"))
    rec_on = medians.get((("base", 0.0, True), "untied"))
    if rec_off and rec_on is not None:
        print(f"  recording cost untied base f=0: {1 - rec_on / rec_off:.1%}")
    rec_off = medians.get((("par", 0.5, False), "untied"))
    rec_on = medians.get((("par", 0.5, True), "untied"))
    if rec_off and rec_on is not None:
        print(f"  recording cost untied par f=0.5: {1 - rec_on / rec_off:.1%}")
    print()
    print("untied counters at the best passing run of each search (per turn unless noted):")
    for ck in CELL_ORDER:
        for g, top, lo, n in best.get((ck, "untied"), []):
            if not top or "ustats" not in top:
                continue
            u = {k: int(v) for k, v in top["ustats"].items() if v.isdigit()}
            t = top["load"]["completed"]
            dur = top["wall"]
            print(f"  {ck} g{g} rate={top['rate']:.0f}: cpu={top['server_cpu_s'] / t * 1e6:.0f}us "
                  f"sys={top['server_sys_s'] / t * 1e6:.0f}us gpolls={u['gpolls'] / t:.1f} "
                  f"gpoll_blocks={u['gpoll_blocks'] / t:.2f} unit_epoll_waits={u['unit_epoll_waits'] / t:.1f} "
                  f"os_wakes={u['os_wakes'] / t:.2f} steals/s={u['steals'] / dur:.0f} steals/turn={u['steals'] / t:.2f} "
                  f"resumes={u['resumes'] / t:.1f} lifo={u['lifo'] / t:.2f} units={u['units']} "
                  f"create_us={u['create_ns_avg'] / 1e3:.1f} tape_MB={top.get('tape_bytes', 0) / 1e6:.0f} "
                  f"tape_B/turn={top.get('tape_bytes', 0) / t:.0f}")


main(sys.argv[1:])
