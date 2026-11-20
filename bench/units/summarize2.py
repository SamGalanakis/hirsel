#!/usr/bin/env python3
"""Summarize Part 2 jsonl: median and min-max ns per message by shape and side."""
import json, statistics, sys, collections
rows = collections.defaultdict(list)
fails = collections.Counter()
for l in open(sys.argv[1]):
    r = json.loads(l)
    if r.get("discarded"):
        continue
    key = (r["shape"], r["name"])
    if not r["ok"]:
        fails[key] += 1
        continue
    rows[key].append(r["ns"] / r["n"])
for (shape, name) in sorted(set(rows) | set(fails)):
    v = rows.get((shape, name), [])
    med = statistics.median(v) if v else float("nan")
    lo, hi = (min(v), max(v)) if v else (float("nan"),) * 2
    print(f"{shape:18s} {name:24s} n={len(v)} fail={fails[(shape,name)]} med={med:9.0f} ns/msg  [{lo:.0f}-{hi:.0f}]")
