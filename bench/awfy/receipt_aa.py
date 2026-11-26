"""Compare two paired A/A sampling runs without a performance gate."""
from decimal import Decimal
from statistics import median

from receipt import INSTRUCTION_LIMIT, candidate_identity, change, compare, cycle_count, match_rows, read


def compare_command(args):
    if args.aa:
        before, after = read(args.baseline), read(args.candidate)
        if any(run.get("comparison") in ("A/A", "A/B") for run in (before, after)):
            if args.threshold is not None:
                raise ValueError("paired A/A reports a trend and does not take --threshold")
            return compare_aa_runs(before, after)
    limit = INSTRUCTION_LIMIT if args.threshold is None else args.threshold
    return compare(args.baseline, args.candidate, limit, aa=args.aa)


def compare_aa_runs(before, after):
    keys = before["rows"].keys()
    count = before["pairs"]
    cpu = before["cpu"]
    if (any(run["comparison"] != "A/A" or run["failures"] for run in (before, after))
            or not keys or type(count) is not int or count < 1 or type(cpu) is not int
            or (after["rows"].keys(), after["pairs"], after["cpu"], after["lock"]) !=
            (keys, count, cpu, before["lock"])):
        raise ValueError("paired A/A requires two complete runs with identical rows, pair counts, CPU and lock")
    for key in sorted(keys):
        left, right = (run["rows"][key] for run in (before, after))
        if len(left) != count or len(right) != count:
            raise ValueError(f"{key}: incomplete A/A pairs")
        anchor = left[0]["base"]
        for pairs in (left, right):
            for pair in pairs:
                if sorted(pair["order"]) != ["base", "candidate"]:
                    raise ValueError(f"{key}: invalid A/A arm order")
                for side in ("base", "candidate"):
                    row = pair[side]
                    cycle_count(key, row)
                    if (row["key"] != key or row["affinity"] != [cpu]
                            or candidate_identity(row) != candidate_identity(anchor)):
                        raise ValueError(f"{key}: A/A requires identical candidate hashes and CPU")
                    match_rows(key, anchor, row)
        for event in ("instructions", "cycles"):
            counts = [[pair[side]["sample"][event] for pair in pairs for side in ("base", "candidate")]
                      for pairs in (left, right)]
            a, b = (median(map(Decimal, samples)) for samples in counts)
            print(f"{key} {event}: run medians {a:,.1f} -> {b:,.1f} ({change(a, b):+.3f}%), "
                  f"ranges [{min(counts[0]):,}, {max(counts[0]):,}] and [{min(counts[1]):,}, {max(counts[1]):,}]")
    return 0
