"""Render the human-readable nightly goal board."""

import board_host
import board_size


def render(report, expected, density_goal):
    lines = ["# Hirsel goal board", "",
             "Complete" if report["complete"] else "Incomplete", "", *report["notes"], "", *board_host.markdown(report.get("host")),
             "| Goal | Row | Metric | H/R | H/G | Commit |",
             "| --- | --- | --- | ---: | ---: | --- |"]
    for row in report["rows"]:
        if row["goal"] == density_goal:
            continue
        for metric in row["metrics"]:
            cells = []
            for side in ("rust", "goose"):
                value = row["ratios"][side].get(metric)
                cells.append(f"{value:.4f}×" if value is not None else "n/a")
            lines.append("| " + " | ".join((row["goal"], row["row"], metric, *cells,
                                              row["commit"][:8])) + " |")
    if density := report.get("place_density"):
        lines += ["", f"## Place density, {density['places']} live Places", "", density["note"], "",
                  "| Metric | Per Place |", "| --- | ---: |"]
        lines += [f"| {metric['label']} | {metric['value']:,.2f} {metric['unit']} |"
                  for metric in density["metrics"].values()]
        lines += ["", "### Regression gate", "", *[f"- {line}" for line in density["comparison"]]]
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
        lines += ["", "## Nightly instruction comparison", "", comparison["status"], "",
                  *comparison["details"]]
    for gate in ("size", "checkbox"):
        if comparison := report.get(f"{gate}_comparison"):
            lines += ["", f"## Nightly {gate} comparison", "", *comparison]
        if gate == "size" and (census := report.get("size_census")):
            lines += board_size.census_markdown(census)
    return "\n".join(lines) + "\n"
