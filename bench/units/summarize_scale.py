#!/usr/bin/env python3
"""Render the measurements from scale.py as Markdown tables."""

import argparse
from itertools import combinations
import json
from pathlib import Path

from scale import REC_CELLS, REC_SIDES, SCALE_CELLS, SCALE_SIDES


def read_rows(data):
    for path in sorted(data.glob("*.jsonl")):
        with path.open() as source:
            for line in source:
                if line.endswith("\n"):
                    yield json.loads(line)


def fmt(x, places=0):
    return "—" if x is None else f"{x:,.{places}f}"


def ms(ns):
    return fmt(ns / 1e6, 3) if ns is not None else "—"


def table(header):
    print("| " + " | ".join(header) + " |")
    print("| " + " | ".join("---" for _ in header) + " |")


def row(*cells):
    print("| " + " | ".join(map(str, cells)) + " |")


def number(record, name):
    return (record or {}).get(name)


def load_number(record, name):
    return ((record or {}).get("load") or {}).get(name)


def resources(trial):
    rss = number(trial, "peak_rss_kib")
    stats = number(trial, "ustats") or {}
    return (
        fmt(rss / 1024, 1) if rss is not None else "—",
        fmt(number(trial, "peak_threads_sampled")),
        fmt(number(trial, "server_measured_cpu_us_per_turn")),
        fmt(number(trial, "server_cpu_us_per_turn")),
        fmt(number(trial, "steals_per_s")),
        fmt(stats.get("units")),
    )


def search_trials(rows, cell, side, group):
    return [r for r in rows if r.get("cell_name") == cell and r.get("side") == side
            and r.get("group") == group and r.get("phase") == "search" and "trial" in r]


def result(rows, cell, side, group):
    trials = search_trials(rows, cell, side, group)
    summary = next((r for r in rows if r.get("summary") and r["cell_name"] == cell
                    and r["side"] == side and r["group"] == group), None)
    if not trials:
        return None, None, "Pending" if summary is None else "Incomplete"
    rate = number(summary, "max_rate")
    if rate is not None:
        trial = next(r for r in trials if r["search_rate"] == rate and r["passed"])
        return trial, trial["rate"], "Pass"
    lowest = min(trials, key=lambda r: r["rate"])
    if summary is None or summary.get("incomplete"):
        passed = max((r for r in trials if r["passed"]), key=lambda r: r["rate"], default=None)
        return passed or lowest, None, "Incomplete"
    return lowest, None, "No pass"


def best(rows, cell, side):
    rates = [result(rows, cell, side, group)[1] for group in (1, 2)]
    return max((r for r in rates if r is not None), default=None), sum(r is not None for r in rates)


def finished(rows, cell, sides):
    summaries = {
        (r["side"], r["group"])
        for r in rows if r.get("summary") and r["cell_name"] == cell and not r.get("incomplete")
    }
    return all((s, g) in summaries for s in sides for g in (1, 2))


def fairness_trial(rows, side, hot):
    return next((r for r in rows if r.get("phase") == "fairness" and r.get("side") == side
                 and "trial" in r and r["cell"]["hot"] == hot), None)


def complete(trial):
    load = (trial or {}).get("load")
    return bool(load and trial["server_rc"] == trial["load_rc"] == 0
                and load["completed"] == load["issued"]
                and load["late"] == load["failures"] == load["timeouts"] == 0)


def fair(rows, side):
    cold, hot = (fairness_trial(rows, side, v) for v in (False, True))
    if not cold or not hot:
        return None
    return complete(cold) and complete(hot) and hot["load"]["p99_ns"] <= min(5_000_000, 2 * cold["load"]["p99_ns"])


def verdicts(rows, n):
    high = f"A1-{n}"
    u0, n0 = best(rows, "A1-256", "untied")
    u, nu = best(rows, high, "untied")
    t, nt = best(rows, high, "tokiow8")
    s, ns = best(rows, high, "S3k7")
    if all((u0, u, t, s)):
        repeat = " At least one side had a single passing search." if min(n0, nu, nt, ns) == 1 else ""
        rel = f"At {n:,} sessions untied reached {u / u0:.3f}× its 256-session rate and {u / t:.3f}× tokiow8. S3k7 reached {s / u:.3f}× untied."
        exclusion = s >= 1.25 * u
        if u < 0.9 * u0 or u < 0.75 * t:
            state = "Threshold missed."
        elif exclusion and fair(rows, "S3k7") is None:
            state = "Rate thresholds met. S3k7 fairness is pending."
        elif exclusion and fair(rows, "S3k7"):
            state = "Pool not supported at scale under the S3k7 rule."
        else:
            state = "Holds."
        print(f"- **Scale.** {state} {rel}{repeat}")
    else:
        print("- **Scale.** Pending comparable passing rates.")

    untied = fair(rows, "untied")
    if untied is None:
        print("- **Fairness.** Pending paired runs.")
    else:
        by_side = ", ".join(f"{s} {'passed' if fair(rows, s) else 'failed'}"
                            for s in SCALE_SIDES[1:] if fair(rows, s) is not None)
        controls = f" {by_side}." if by_side else ""
        print(f"- **Fairness.** {'Holds' if untied else 'Threshold missed'} for untied.{controls} One pair per side.")

    ratios = []
    details = []
    sparse = False
    for cell in REC_CELLS:
        candidate, count = best(rows, cell, "untied")
        control, control_count = max((best(rows, cell, side) for side in REC_SIDES[1:4]),
                                     key=lambda item: item[0] or 0)
        ratio = candidate / control if candidate and control else None
        ratios.append(ratio)
        fraction = REC_CELLS[cell]["f"]
        if ratio is not None:
            details.append(f"At f={fraction}, untied reached {ratio:.2f}× the best recorded control.")
        elif finished(rows, cell, REC_SIDES[:4]):
            if candidate is None:
                details.append(f"At f={fraction}, every untied trial missed the pass rule.")
            if control is None:
                details.append(f"At f={fraction}, all three recorded controls missed the pass rule.")
        sparse |= count == 1 or control_count == 1
    if any(ratio is not None and ratio < 1.25 for ratio in ratios):
        state = "Threshold missed."
    elif all(ratios):
        state = "Supported."
    elif all(finished(rows, cell, REC_SIDES[:4]) for cell in REC_CELLS):
        state = "The 1.25× rule remains unverified across both cells."
    else:
        state = "Pending complete searches in both cells."
    measurement = f" {' '.join(details)}" if details else ""
    repeat = " At least one rate rests on a single passing search." if sparse and details else ""
    print(f"- **Recorded fan-out.** {state}{measurement}{repeat}")


def scale_tables(rows):
    for cell_name, cell in SCALE_CELLS.items():
        print(f"\n## Base load at {cell['sessions']:,} sessions, f=0\n")
        table(("Side", "Search", "Result", "Best pass or lowest tried (turns/s)", "p99 (ms)",
               "Peak RSS (MiB)", "Peak sampled threads", "CPU µs/turn after warmup (est.)",
               "CPU µs/turn incl. setup", "Steals/s incl. setup", "Units"))
        for side in SCALE_SIDES:
            for group in (1, 2):
                trial, rate, state = result(rows, cell_name, side, group)
                row(side, group, state, fmt(rate if rate is not None else number(trial, "rate")),
                    ms(load_number(trial, "p99_ns")), *resources(trial))
        print()
        for first, second in combinations(SCALE_SIDES, 2):
            a, b = best(rows, cell_name, first)[0], best(rows, cell_name, second)[0]
            if a and b:
                gap = 1 - min(a, b) / max(a, b)
                if gap < 0.25:
                    print(f"- {first} and {second} tie under the 25% rule.")
                else:
                    faster, slower = (first, second) if a > b else (second, first)
                    print(f"- {slower} is {gap * 100:.1f}% below {faster}.")


def fairness_table(rows, n):
    print(f"\n## Hot sessions at {n:,} sessions\n")
    table(("Side", "Hot", "Open turns/s", "Non-hot p50 (ms)", "Non-hot p99 (ms)",
           "Non-hot max (ms)", "Max ready wait during load (ms)", "Max ready wait during drain (ms)", "Hot turns/s", "Complete",
           "Peak RSS (MiB)", "Peak sampled threads", "CPU µs/turn after warmup (est.)",
           "CPU µs/turn incl. setup", "Steals/s incl. setup", "Units"))
    for side in SCALE_SIDES:
        for hot in (False, True):
            trial = fairness_trial(rows, side, hot)
            wait = number(trial, "queue_wait_max_ns")
            upper = number(trial, "queue_wait_overlap_max_ns")
            wait_text = ms(wait)
            if upper is not None and upper != wait:
                wait_text += " to " + ms(upper)
            end = load_number(trial, "end_ns")
            after = max((delay for t, delay in number(trial, "queue_wait_buckets") or [] if t >= end), default=None)
            row(side, "Yes" if hot else "No", fmt(number(trial, "rate")),
                *(ms(load_number(trial, col)) for col in ("p50_ns", "p99_ns", "max_ns")),
                wait_text, ms(after), fmt(load_number(trial, "hot_turns_per_s"), 1),
                "Yes" if complete(trial) else "No" if trial else "—", *resources(trial))


def recorded_tables(rows):
    for cell_name, cell in REC_CELLS.items():
        print(f"\n## Full recording, 16 parallel tool calls per turn, f={cell['f']}\n")
        table(("Side", "Search", "Result", "Best pass or lowest tried (turns/s)",
               "p99 (ms)", "Recorded bytes/turn", "Source"))
        for side in REC_SIDES:
            for group in (1, 2):
                trial, rate, state = result(rows, cell_name, side, group)
                stats = number(trial, "ustats") or {}
                written, source = stats.get("tape_bytes"), "Writer"
                if written is None:
                    written, source = number(trial, "tape_disk_bytes"), "File size"
                completed = load_number(trial, "completed")
                row(side, group, state, fmt(rate if rate is not None else number(trial, "rate")),
                    ms(load_number(trial, "p99_ns")),
                    fmt(written / completed if written is not None and completed else None),
                    source if written is not None and completed else "—")


def main():
    args = argparse.ArgumentParser(description=__doc__)
    args.add_argument("data", type=Path)
    args.add_argument("--sessions", type=int, default=8192)
    args.add_argument("--attribution", type=Path)
    args.add_argument("--method", type=Path)
    args.add_argument("--tip", required=True)
    opt = args.parse_args()
    rows = [r for r in read_rows(opt.data) if not r.get("discarded")]
    print("# Session scaling and recorded fan-out\n")
    verdicts(rows, opt.sessions)
    print("\nThe tables use clean timing holds. Numbers come from the measured runs except where marked as estimates.")
    scale_tables(rows)
    fairness_table(rows, opt.sessions)
    recorded_tables(rows)
    if opt.attribution:
        print("\n## Attribution\n")
        print(opt.attribution.read_text().strip())
    if opt.method:
        print("\n" + opt.method.read_text().strip())
    print(f"\nBranch `t3/mc-units-scale`, tip `{opt.tip}`.")


if __name__ == "__main__":
    main()
