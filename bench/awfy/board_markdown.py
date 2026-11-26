"""Render the human-readable nightly goal board."""

import awfy_measure
import board_host
import board_size
from board_counters import EXPECTED as COUNTER_ROWS


def render(report, expected, density_goal):
    lines = ["# Hirsel goal board", "",
             "Complete" if report["complete"] else "Incomplete", "", *report["notes"], "", *board_host.markdown(report.get("host")),
             "| Goal | Row | Metric | H/R | H/G | Commit |",
             "| --- | --- | --- | ---: | ---: | --- |"]
    seconds = []
    for row in report["rows"]:
        if row["goal"] == density_goal or row["goal"] in COUNTER_ROWS:
            continue
        for metric in row["metrics"]:
            if metric == "seconds":
                values = [str(row["samples"].get(side, {}).get("seconds", ""))
                          for side in ("hirsel", "rust", "goose")]
                seconds.append("| " + " | ".join((row["goal"], row["row"], *values)) + " |")
                continue
            cells = []
            for side in ("rust", "goose"):
                value = row["ratios"][side].get(metric)
                cells.append(f"{value:.4f}×" if value is not None else "n/a")
            lines.append("| " + " | ".join((row["goal"], awfy_measure.row_label(row["row"]), metric, *cells,
                                              row["commit"][:8])) + " |")
    if seconds:
        lines += ["", "## Wall time per sample", "", "| Goal | Row | Hirsel (s) | Rust (s) | Goose (s) |",
                  "| --- | --- | ---: | ---: | ---: |", *seconds]
    if density := report.get("place_density"):
        lines += ["", f"## Place density, {density['places']} live Places", "", density["note"], "",
                  "| Metric | Per Place |", "| --- | ---: |"]
        lines += [f"| {metric['label']} | {metric['value']:,.2f} {metric['unit']} |"
                  for metric in density["metrics"].values()]
        lines += ["", "### Regression gate", "", *[f"- {line}" for line in density["comparison"]]]
    counters = [row for row in report["rows"] if row["goal"] in COUNTER_ROWS and row["status"] == "ok"]
    if counters:
        gates = {(gate["goal"], gate["row"], gate["metric"]): gate
                 for gate in report.get("counter_comparison", ())}
        lines += ["", "## Nightly counters", "", "| Goal | Row | Metric | Value | Limit | Verdict |",
                  "| --- | --- | --- | ---: | ---: | --- |"]
        for row in counters:
            goal, name = row["goal"], row["row"]
            for metric in row["metrics"]:
                gate = gates.get((goal, name, metric), {})
                limit = gate.get("limit")
                ceiling = f"{limit:,}" if limit is not None else ""
                value = row["samples"]["hirsel"][metric]
                lines.append(f"| {goal} | {name} | {metric} | {value:,} | {ceiling} | {gate.get('status', '')} |")
        if refusal := report.get("counter_refusal"):
            lines += ["", f"Counter comparison refused. {refusal}"]
    notes = [f"- {row['goal']}/{row['row']}. {row['notes']}"
             for row in report["rows"] if row.get("notes")]
    if notes:
        lines += ["", "## Runtime notes", "", *notes]
    if report["failures"]:
        lines += ["", "## Incomplete samples", ""]
        lines += [f"- {label}: {reason}" for label, reason in report["failures"].items()]
    missing = [f"{row['goal']}/{row['row']}" for row in expected if row["status"] == "missing"]
    if missing:
        lines += ["", "## Missing rows", "", *[f"- {label}" for label in missing]]
    if "exit_status" in report:
        lines += ["", f"Board exit status {report['exit_status']}"]
    comparison = report.get("instruction_comparison")
    if comparison:
        lines += ["", "## Nightly instructions and peak RSS comparison", "", comparison["status"], "",
                  *comparison["details"]]
    for gate in ("size", "checkbox"):
        if comparison := report.get(f"{gate}_comparison"):
            lines += ["", f"## Nightly {gate} comparison", "", *comparison]
        if gate == "size" and (census := report.get("size_census")):
            lines += board_size.census_markdown(census)
    return "\n".join(lines) + "\n"
