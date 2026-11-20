#!/usr/bin/env python3
"""Summarize Part 3 searches: max passing rate per side and cell, with the
paired S3 / S1-best ratio per group and the declared decision rule.

S3 supported: every group's ratio >= 1.25. S1 supported: every group's
ratio < 1.25 / RES (RES = the search's resolution, 1.25**0.25), so a 25% S3
advantage is ruled out even at the search's worst-case error. Otherwise open.
Usage: summarize3.py DATA.jsonl...
"""
import json, statistics, sys
from collections import defaultdict

RES = 1.25 ** 0.25


def key(c):
    return (c["variant"], c["f"], c["rec"])


def fmt(v):
    if not v:
        return "-"
    v = sorted(v)
    return f"{statistics.median(v):.0f} [{v[0]:.0f}-{v[-1]:.0f}]" if len(v) > 1 else f"{v[0]:.0f}"


def main():
    rates = defaultdict(lambda: defaultdict(dict))  # cell -> side -> group -> rate
    runs = defaultdict(lambda: [0, 0, 0, 0])  # side -> runs, failures(checksum), timeouts, discarded
    for p in sys.argv[1:]:
        for line in open(p):
            r = json.loads(line)
            if r.get("summary"):
                rates[key(r["cell"])][r["side"]][(p, r["group"])] = r["max_rate"] or 0
            else:
                t = runs[r["side"]]
                t[0] += 1
                L = r.get("load") or {}
                t[1] += L.get("failures", 0)
                t[2] += L.get("timeouts", 0)
                t[3] += bool(r.get("discarded"))
    sides_all = sorted({s for c in rates.values() for s in c})
    print("| variant | f | recording | " + " | ".join(sides_all) + " | S3/S1-best per group | decision |")
    print("|---" * (len(sides_all) + 5) + "|")
    for cell in sorted(rates):
        bys = rates[cell]
        groups = sorted({g for s in bys.values() for g in s})
        ratios = []
        for g in groups:
            s1 = max((bys[s][g] for s in bys if s.startswith("S1") and g in bys[s]), default=None)
            s3 = max((bys[s][g] for s in bys if s.startswith("S3") and g in bys[s]), default=None)
            if s1 and s3:
                ratios.append(s3 / s1)
        if not ratios:
            dec = "-"
        elif min(ratios) >= 1.25:
            dec = "S3 supported"
        elif max(ratios) < 1.25 / RES:
            dec = "S1 supported"
        else:
            dec = "open"
        near = any(abs(r - 1.25) / 1.25 < 0.03 for r in ratios)
        cols = [fmt(list(bys[s].values())) if s in bys else "-" for s in sides_all]
        rs = ", ".join(f"{r:.2f}" for r in ratios)
        print(f"| {cell[0]} | {cell[1]} | {'on' if cell[2] else 'off'} | " + " | ".join(cols)
              + f" | {rs} | {dec}{' (within 3% of 1.25: A/A needed)' if near else ''} |")
    print()
    print("| side | runs | checksum failures | timeouts (turns) | runs discarded (cc1plus) |")
    print("|---|---|---|---|---|")
    for s in sorted(runs):
        t = runs[s]
        print(f"| {s} | {t[0]} | {t[1]} | {t[2]} | {t[3]} |")


main()
