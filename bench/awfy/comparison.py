"""Qualification and rendering for the opt-in board comparisons."""
import math
import re
import shlex

import awfy_measure
import receipt

VERSION = 1
RUNTIME_SUITES = ("AWFY original", "AWFY pool/index", "LangArena")
RUNTIME_MEMORY = (*RUNTIME_SUITES, "All matched runtime rows")
SPECTRALNORM = "CLBG::Spectralnorm"
SPECTRALNORM_CHECKSUM = 1051192944
SIZE_ROW = "AWFY original-14 harness"
CHECKBOX_ROWS = ("memory-subscribed", "memory-unsubscribed")
COMPILE_FIELDS = ("instructions", "cycles", "seconds", "max_rss_kib")
CHECKBOX_FIELDS = ("server_rss_bytes", "server_peak_rss_bytes")
BUILD_ROWS = {
    "compile": (
        ("Dev compile", "LangArena cold", "arena-dev", "cold"),
        ("Dev compile", "LangArena one-body edit", "arena-dev", "edit"),
        ("Release compile", "LangArena cold", "arena-release", "cold"),
        ("Release compile", "LangArena one-body edit", "arena-release", "edit"),
        ("Release compile", "AWFY original-14 cold", "awfy-release", "cold"),
        ("Release compile", "AWFY original-14 one-body edit", "awfy-release", "edit"),
    ),
    "check": (
        ("Check", "LangArena fresh", "arena-check", "cold"),
        ("Check", "LangArena one-body edit", "arena-check", "edit"),
    ),
}
FAMILIES = (*BUILD_ROWS, "runtime-memory", "image-size", "checkbox", "units")


def positive(value):
    if not isinstance(value, (int, float, str)) or isinstance(value, bool):
        return False
    try:
        number = float(value)
    except (ValueError, OverflowError):
        return False
    return math.isfinite(number) and number > 0


def divide(left, right):
    if positive(left) and positive(right):
        value = float(left) / float(right)
        if positive(value):
            return value


def spectralnorm_marker(side, checksum):
    name = re.escape(SPECTRALNORM)
    if side == "hirsel":
        return name + f" checksum={checksum} expected={checksum} status=OK"
    return name + r": OK in [0-9.]+s"


def compile_oracle(sample, side, recipe, mode):
    if recipe == "arena-check":
        return sample.get("front_only") is True and sample.get("rebuilt") is True
    if sample.get("rebuilt") is not True:
        return False
    results = sample.get("verification", {})
    if recipe == "awfy-release":
        if results.keys() != set(awfy_measure.EXPECTED["AWFY original"]):
            return False
        for name, result in results.items():
            marker = re.escape(name) + r": iterations=1 runtime: \d+us" if side == "rust" else "verified"
            if mode == "edit":
                marker += " board-edit"
            if not awfy_measure.matches_output(result, marker):
                return False
        return True
    checksum = SPECTRALNORM_CHECKSUM + (mode == "edit")
    result = results.get(SPECTRALNORM)
    return bool(result and result.get("expected_checksum") == checksum
                and awfy_measure.matches_output(result, spectralnorm_marker(side, checksum)))


def checkbox_oracle(sample, mode):
    return (sample.get("returncode") == 0 and sample.get("complete") is True
            and sample.get("requested_connections") == sample.get("connections") == 64
            and sample.get("subscribed") is (mode == "memory-subscribed")
            and sample.get("seeded_chunks") == 1954 and positive(sample.get("seed_version"))
            and "setup_error" in sample and sample["setup_error"] is None)


def sample_issue(sample, commit, mode, fields, counters=()):
    if not sample:
        return "missing arm"
    if sample.get("commit") != commit:
        return "stale revision"
    if sample.get("mode") != mode:
        return "mismatched mode"
    if sample.get("status") != "ok":
        return sample.get("status") or "missing status"
    if any(not positive(sample.get(field)) for field in fields):
        return "missing or invalid metric"
    if any(not receipt.counter_ready(sample, event + ":u") for event in counters):
        return "inactive counter"


def result_row(goal, name, fields, qualified, samples, issues, peers=("rust", "goose")):
    ratios = {
        peer: {field: value for field in fields if (value := divide(
            qualified.get("hirsel", {}).get(field), qualified.get(peer, {}).get(field))) is not None}
        for peer in peers
    }
    return {"goal": goal, "row": name, "metrics": fields, "samples": samples, "ratios": ratios,
            "status": "ok" if not issues and all(len(values) == len(fields) for values in ratios.values())
            else "incomplete", "issues": issues}


def compare_row(goal, name, samples, commit, mode, fields, *, oracle, peers=("rust", "goose"), counters=()):
    qualified, issues = {}, {}
    for side in ("hirsel", *peers):
        sample = samples.get(side)
        issue = sample_issue(sample, commit, mode, fields, counters)
        if issue is None and not oracle(side, sample):
            issue = "workload or rebuild oracle failed"
        if issue:
            issues[side] = issue
        else:
            qualified[side] = sample
    row = result_row(goal, name, fields, qualified, samples, issues, peers)
    row["mode"] = mode
    return row


def runtime_memory(runtime, commit, suite):
    cases = [(goal, name) for goal in RUNTIME_SUITES if suite in (goal, RUNTIME_MEMORY[-1])
             for name in awfy_measure.EXPECTED[goal]]
    peers = ("rust", "goose")
    logs = {peer: [] for peer in peers}
    issues = {}
    for goal, name in cases:
        key = f"{goal}/{name}"
        samples = runtime.get(key, {})
        pair = compare_row(goal, name, samples, commit, "runtime", ("max_rss_kib",),
                           oracle=lambda _, sample: sample.get("verified") is True, counters=("instructions",))
        for side, issue in pair["issues"].items():
            issues[f"{key}/{side}"] = issue
        for peer in peers:
            if ratio := pair["ratios"][peer].get("max_rss_kib"):
                logs[peer].append(math.log(ratio))
    expected = len(cases)
    ratios = {peer: {"geomean_peak_rss": math.exp(math.fsum(values) / expected)}
              if len(values) == expected else {} for peer, values in logs.items()}
    return {"goal": "Peak RSS", "row": suite, "metrics": ("geomean_peak_rss",), "samples": {},
            "ratios": ratios, "coverage": {peer: (len(values), expected) for peer, values in logs.items()},
            "status": "ok" if all(len(values) == expected for values in logs.values()) else "incomplete",
            "issues": issues}


def rows(evidence, units_rows):
    commit = evidence["commit"]
    observed = evidence["samples"]
    output = []
    for family in evidence["families"]:
        for goal, name, recipe, mode in BUILD_ROWS.get(family, ()):
            samples = observed.get(f"{recipe}/{mode}", {})
            output.append(compare_row(goal, name, samples, commit, f"{recipe}/{mode}", COMPILE_FIELDS,
                                      oracle=lambda side, sample: compile_oracle(sample, side, recipe, mode),
                                      counters=("instructions", "cycles")))
        if family == "runtime-memory":
            output.extend(runtime_memory(evidence["runtime"], commit, suite) for suite in RUNTIME_MEMORY)
        if family == "image-size":
            details = evidence.get("size", {})
            oracle = (details.get("anchor_present") is True
                      and details.get("verified_workloads") == sorted(awfy_measure.EXPECTED["AWFY original"])
                      and details.get("strip_version", "").startswith("GNU strip")
                      and details.get("strip_flags") == list(awfy_measure.STRIP_FLAGS))
            output.append(compare_row("Size", SIZE_ROW, observed.get("size", {}), commit, "stripped",
                                      ("bytes",), oracle=lambda *_: oracle))
        if family == "checkbox":
            for mode in CHECKBOX_ROWS:
                samples = observed.get(mode, {})
                output.append(compare_row("Checkbox", f"{mode}, 64 clients", samples, commit, mode,
                                          CHECKBOX_FIELDS, peers=("rust",), oracle=lambda _, sample:
                                          checkbox_oracle(sample, mode)
                                          and evidence.get("checkbox_anchor_present") is True))
        if family == "units":
            output.extend(units_rows)
    return output


def summary(evidence, units_rows):
    families = evidence["families"]
    if (evidence["version"] != VERSION or not families or len(families) != len(set(families))
            or not set(families).issubset(FAMILIES)):
        raise ValueError("invalid comparison version or family selection")
    result = rows(evidence, units_rows)
    selection_complete = bool(result) and all(row["status"] == "ok" for row in result) and not evidence["failures"]
    return evidence | {"rows": result, "selection_complete": selection_complete,
                       "complete": selection_complete and set(families) == set(FAMILIES)}


def markdown(report):
    def show(value, suffix=""):
        return f"{float(value):,.6g}{suffix}" if positive(value) else "—"

    scope = "full" if set(report["families"]) == set(FAMILIES) else "selected families"
    lines = ["# Hirsel comparison board", "",
             f"Revision `{report['commit']}` · {scope} · "
             + ("complete" if report["selection_complete"] else "incomplete"), "",
             f"Families: {', '.join(report['families'])}.",
             f"Build CPUs: {report['build_cpus']}. Measurement CPUs: {report['measure_cpus']}. "
             "Build cap: 8 GiB. Process-tree measurement cap: 2 GiB.",
             "Cycles and elapsed time describe one observation per configuration.", "",
             f"Command: `{shlex.join(report['command'])}`.", ""]
    lines += [f"- {peer} pin `{pin}`" for peer, pin in report["pins"].items()]
    if units := report.get("units"):
        lines += [f"- Units {units['status']}. {units.get('reason', '')}"]
        if path := units.get("path"):
            lines += [f"- Units evidence [{units['source']}](<{path}>)."]
    if size := report.get("size"):
        lines += [f"- Image strip `{size['strip_version']}` with `{' '.join(size['strip_flags'])}`."]
    lines += ["", "| Goal | Row | Metric | Hirsel | Rust/Tokio | Goose | H/R or H/T | H/Goose | Status |",
              "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |"]
    for row in report["rows"]:
        samples = row["samples"]
        peer = "tokio" if "tokio" in row["ratios"] else "rust"
        for metric in row["metrics"]:
            values = [show(samples.get(arm, {}).get(metric)) for arm in ("hirsel", peer, "goose")]
            ratios = [show(row["ratios"].get(arm, {}).get(metric), "×") for arm in (peer, "goose")]
            coverage = row.get("coverage")
            status = row["status"]
            if coverage:
                status += " (" + ", ".join(f"{arm} {matched}/{total}"
                                           for arm, (matched, total) in coverage.items()) + ")"
            lines.append("| " + " | ".join((row["goal"], awfy_measure.row_label(row["row"]),
                                             metric, *values, *ratios, status)) + " |")
    lines += ["", "## Qualification", ""]
    for row in report["rows"]:
        if row["issues"] and row["issues"].get("export") != report.get("units", {}).get("reason"):
            lines.extend(f"- {row['goal']} / {row['row']} / {arm}. {reason}"
                         for arm, reason in row["issues"].items())
        if row.get("notes"):
            lines.append(f"- {row['goal']} / {row['row']}. {row['notes']}")
        for side, sample in row["samples"].items():
            if cache := sample.get("cache"):
                library = sample.get("cranelift_library_objects_written")
                observed = ""
                if library is not None and (parts := sample.get("compiled_parts")) is not None:
                    unit = "unit" if parts == 1 else "units"
                    observed = (f" Cranelift compiled {parts} {unit} across the app and libraries. "
                                f"{library} native objects were added to the published library store.")
                lines.append(f"- {row['goal']} / {row['row']} / {side}. {cache}.{observed} "
                             f"Command `{sample.get('command_line', '')}`.")
    lines.extend(f"- {where}. {reason}" for where, reason in report["failures"].items())
    return "\n".join(lines) + "\n"
