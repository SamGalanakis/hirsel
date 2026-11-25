#!/usr/bin/env python3
"""Generate site/benchmarks/board-data.json and site/benchmarks/index.html.

Regenerate from a local Hirsel goal-board run:
  python3 tools/site_benchmarks.py \
    --from-board ~/.local/share/hirsel-board/runs/<run>/board/board.json \
    --current-board /workspace/code/hirsel-briefs/CURRENT-BOARD.md

Regenerate HTML from committed site/benchmarks/board-data.json:
  python3 tools/site_benchmarks.py --write
  python3 tools/site_benchmarks.py --check
"""

from __future__ import annotations

import argparse
import html
import json
import math
import re
import sys
from pathlib import Path

from site_docs_theme import (
    AGENT_COMMENT,
    COPY_ICON,
    DOCS_CSS,
    DOCS_JS,
    GITHUB_ICON,
    GITHUB_REPO,
    OK_ICON,
    ORIGIN,
    RAM_MARK_SVG,
    TOPO_ACCENT_SVG,
)

ROOT = Path(__file__).resolve().parents[1]
DATA_PATH = ROOT / "site" / "benchmarks" / "board-data.json"
HTML_PATH = ROOT / "site" / "benchmarks" / "index.html"

BENCH_CSS = """
main.bench{max-width:1120px;margin:0 auto;padding:38px 36px 88px}
.bench-status{
  margin:22px 0 28px;padding:18px 20px;border-radius:12px;
  background:var(--paper);border:1px solid var(--rule-strong);border-left:4px solid var(--red);
}
.bench-status.passed{border-left-color:var(--moss)}
.status-head{display:flex;align-items:center;flex-wrap:wrap;gap:10px 14px;margin:0 0 10px}
.status-badge{
  display:inline-flex;align-items:center;gap:6px;padding:4px 10px;border-radius:999px;
  font:600 12px/1 var(--mono);letter-spacing:.03em;text-transform:uppercase;
  background:var(--red-soft);color:var(--red);border:1px solid rgba(203,39,31,.28);
}
.bench-status.passed .status-badge{background:rgba(79,106,46,.12);color:var(--moss);border-color:rgba(79,106,46,.3)}
.status-meta{font:500 13px/1.4 var(--mono);color:var(--mute)}
.bench-status p{margin:0 0 10px;max-width:78ch;font-size:14.5px;line-height:1.58}
.bench-status p:last-child{margin-bottom:0}
.reg-list{margin:8px 0 0;padding-left:20px;font-size:14px;line-height:1.55;color:var(--body)}
.reg-list li{margin:0 0 4px}
.reg-list code{font-family:var(--mono);font-size:.88em}

.kpi-grid{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:14px;margin:24px 0 32px}
.kpi{
  padding:18px 20px;border-radius:12px;background:var(--paper);border:1px solid var(--rule);
  display:flex;flex-direction:column;gap:6px;
}
.kpi .k-label{font:600 11.5px/1.2 var(--mono);letter-spacing:.06em;text-transform:uppercase;color:var(--mute)}
.kpi .k-val{font-family:var(--display);font-weight:400;font-size:32px;line-height:1.05;letter-spacing:-.03em;color:var(--ink)}
.kpi .k-val.good{color:var(--moss)}
.kpi .k-val.warn{color:var(--red)}
.kpi .k-sub{font-size:13.5px;line-height:1.45;color:var(--mute)}

.bench-section{margin:40px 0 0}
.bench-section h2{
  margin:0 0 14px;padding-bottom:10px;border-bottom:1px solid var(--rule);
  font-family:var(--display);font-weight:400;font-synthesis:none;
  font-size:24px;line-height:1.18;letter-spacing:-.022em;color:var(--ink);
}
.bench-section h3{margin:22px 0 8px;font:600 16.5px/1.3 var(--sans);color:var(--ink)}
.bench-section p{margin:0 0 13px;max-width:76ch;font-size:15.5px;line-height:1.62;color:var(--body)}
.bench-section ul{margin:0 0 16px;padding-left:22px;max-width:76ch}
.bench-section li{margin:0 0 7px;font-size:15px;line-height:1.56}

.trails-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin:16px 0 24px}
.trail-card{
  padding:16px 18px;border-radius:10px;background:var(--cream);
  border:1px solid var(--rule-strong);border-left:3px solid var(--umber);
}
.trail-card .t-name{font:600 14.5px/1.3 var(--mono);color:var(--ink);margin:0 0 4px}
.trail-card .t-stat{font:600 13.5px/1.3 var(--mono);color:var(--red);margin:0 0 6px}
.trail-card p{margin:0;font-size:13.5px;line-height:1.5;color:var(--mute)}

.meth-grid{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;margin:16px 0 24px}
.meth-card{padding:16px 18px;border-radius:10px;background:var(--paper);border:1px solid var(--rule)}
.meth-card h3{margin:0 0 8px;font:600 15px/1.3 var(--sans);color:var(--ink)}
.meth-card p{margin:0 0 8px;font-size:14px;line-height:1.55;color:var(--body)}
.meth-card p:last-child{margin-bottom:0}
.meth-card ul{margin:0;padding-left:18px;font-size:13.5px;line-height:1.5;color:var(--body)}
.meth-card li{margin:0 0 4px}

.bench-controls{display:flex;align-items:center;gap:10px;flex-wrap:wrap;margin:18px 0 14px}
.bench-search{
  flex:1 1 220px;min-width:0;padding:8px 13px;border-radius:8px;
  border:1px solid var(--rule-strong);background:var(--cream);color:var(--ink);
  font:400 14px/1.3 var(--sans);
}
.bench-filters{display:flex;gap:6px;flex-wrap:wrap}
.bench-count{font:500 12.5px/1 var(--mono);color:var(--mute);margin:0 0 12px}

.num{font:500 13px/1.4 var(--mono);text-align:right;white-space:nowrap}
.num.good{color:var(--moss);font-weight:600}
.num.bad{color:var(--red);font-weight:600}
.num.neutral{color:var(--mute)}
.bar-cell{width:136px;min-width:110px}
.ratio-bar{position:relative;height:8px;border-radius:999px;background:var(--ground);overflow:hidden}
.ratio-bar .fill{position:absolute;top:0;bottom:0;left:0;border-radius:999px}
.ratio-bar .fill.good{background:var(--moss)}
.ratio-bar .fill.bad{background:var(--red)}
.ratio-bar .mid{position:absolute;top:0;bottom:0;left:50%;width:1.5px;background:rgba(17,17,15,.35)}
.suite-tag{
  display:inline-block;padding:2px 7px;border-radius:999px;background:var(--ground);
  font:500 11px/1.3 var(--mono);color:var(--ink);white-space:nowrap;
}
.row-name{font:500 13.5px/1.4 var(--mono);color:var(--ink)}
.metric-name{font:400 12.5px/1.4 var(--mono);color:var(--mute);white-space:nowrap}
.bench-foot{
  margin-top:40px;padding-top:18px;border-top:1px solid var(--rule);
  display:flex;align-items:center;justify-content:space-between;flex-wrap:wrap;gap:10px;
  font-size:13px;color:var(--mute);
}
.bench-foot a{color:var(--mute);text-decoration:none;border-bottom:1px solid var(--rule)}
.bench-foot a:hover{color:var(--ink);border-bottom-color:var(--ink)}
.bench-foot code{font-family:var(--mono);font-size:.9em}

@media (max-width:960px){
  header.top .nav{display:flex}
  main.bench{padding:28px 20px 72px}
  .kpi-grid{grid-template-columns:repeat(2,minmax(0,1fr))}
  .trails-grid,.meth-grid{grid-template-columns:1fr}
}
@media (max-width:640px){
  header.top{padding:0 12px;gap:8px}
  header.top .brand{gap:6px}
  header.top .brand .wm{font-size:18px}
  header.top .hill-link,header.top .nav .hide-sm{display:none}
  header.top .nav{gap:10px}
  header.top .nav a{font-size:13px}
  header.top .right{gap:6px}
  main.bench{padding:20px 14px 60px}
  .kpi-grid{grid-template-columns:1fr}
  .bar-cell{display:none}
}
"""

BENCH_JS = """
const bSearch = document.getElementById('benchSearch');
if (bSearch) {
  const rows = Array.from(document.querySelectorAll('#benchTable tbody tr[data-suite]'));
  const countEl = document.getElementById('benchCount');
  const btns = Array.from(document.querySelectorAll('.bench-filters .pill-btn'));
  let suite = 'all';
  const apply = () => {
    const q = bSearch.value.trim().toLowerCase();
    let shown = 0;
    for (const r of rows) {
      const sOk = suite === 'all' || (suite === 'trails' ? r.dataset.trails === 'yes' : r.dataset.suite === suite);
      const qOk = !q || r.dataset.search.includes(q);
      const show = sOk && qOk;
      r.hidden = !show;
      if (show) shown++;
    }
    if (countEl) countEl.textContent = shown + ' of ' + rows.length + ' metric rows shown';
  };
  bSearch.addEventListener('input', apply);
  btns.forEach(b => b.addEventListener('click', () => {
    suite = b.dataset.suite;
    btns.forEach(x => x.setAttribute('aria-pressed', x === b ? 'true' : 'false'));
    apply();
  }));
}
"""


def _geomean(vals: list[float]) -> float:
    return math.exp(sum(math.log(v) for v in vals) / len(vals))


def extract_board_data(board_json_path: Path, current_board_path: Path | None) -> dict:
    raw = json.loads(board_json_path.read_text(encoding="utf-8"))
    status_meta = {
        "passed": raw.get("exit_status") == 0,
        "complete": bool(raw.get("complete", False)),
        "exit_status": raw.get("exit_status"),
        "commit": raw["commit"],
        "commit_short": raw["commit"][:8],
        "board_utc": "unknown",
        "attempt_utc": "unknown",
        "freshness_utc": "unknown",
        "vopr_exit": None,
        "laws_exit": None,
        "boat_exit": None,
        "commits_behind_main": None,
    }
    if current_board_path and current_board_path.is_file():
        cb = current_board_path.read_text(encoding="utf-8")
        if m := re.search(r"Freshness as of ([0-9-]+ [0-9:]+ UTC)", cb):
            status_meta["freshness_utc"] = m.group(1)
        if m := re.search(r"\*\*Passed\*\*\s*·\s*(\w+)", cb):
            status_meta["passed"] = m.group(1).lower() == "yes"
        if m := re.search(r"\*\*Latest attempt\*\*.*?·\s*([0-9-]+ [0-9:]+ UTC)", cb):
            status_meta["attempt_utc"] = m.group(1)
        if m := re.search(r"\*\*Last complete board\*\*.*?·\s*([0-9-]+ [0-9:]+ UTC).*?·\s*(\d+)\s+commits behind", cb):
            status_meta["board_utc"] = m.group(1)
            status_meta["commits_behind_main"] = int(m.group(2))
        if m := re.search(r"Boat exit (\d+)\s*·\s*VOPR exit (\d+)\s*·\s*Laws exit (\d+)", cb):
            status_meta["boat_exit"] = int(m.group(1))
            status_meta["vopr_exit"] = int(m.group(2))
            status_meta["laws_exit"] = int(m.group(3))

    regressions: list[str] = []
    for line in raw.get("instruction_comparison", {}).get("details", []):
        if "FAIL" in line:
            regressions.append(line.strip())
    for line in raw.get("size_comparison", []):
        if "FAIL" in line:
            regressions.append(f"size/{line.strip()}")
    for line in raw.get("checkbox_comparison", []):
        if "FAIL" in line:
            regressions.append(f"checkbox/{line.strip()}")

    flat_rows: list[dict] = []
    by_gm: dict[tuple[str, str], list[float]] = {}
    for row in raw["rows"]:
        goal = row["goal"]
        name = row["row"]
        for metric in row["metrics"]:
            hr = row.get("ratios", {}).get("rust", {}).get(metric)
            hg = row.get("ratios", {}).get("goose", {}).get(metric)
            higher_better = metric == "toggles_per_second"
            if hr is not None:
                by_gm.setdefault((goal, metric), []).append(hr)
            h_sample = row.get("samples", {}).get("hirsel", {}).get(metric)
            r_sample = row.get("samples", {}).get("rust", {}).get(metric)
            flat_rows.append(
                {
                    "goal": goal,
                    "row": name,
                    "metric": metric,
                    "higher_is_better": higher_better,
                    "h_over_r": round(hr, 4) if hr is not None else None,
                    "h_over_g": round(hg, 4) if hg is not None else None,
                    "hirsel": h_sample,
                    "rust": r_sample,
                }
            )

    geomeans: dict[str, dict] = {}
    for (goal, metric), vals in by_gm.items():
        higher_better = metric == "toggles_per_second"
        wins = sum(1 for v in vals if (v >= 1.0 if higher_better else v <= 1.0))
        geomeans[f"{goal} | {metric}"] = {
            "goal": goal,
            "metric": metric,
            "count": len(vals),
            "geomean_h_over_r": round(_geomean(vals), 4),
            "hirsel_le_rust": wins,
        }

    return {
        "_comment": (
            "Generated by tools/site_benchmarks.py from a Hirsel goal-board run "
            "(~/.local/share/hirsel-board/runs/<run>/board/board.json and "
            "/workspace/code/hirsel-briefs/CURRENT-BOARD.md). "
            "Regenerate with: python3 tools/site_benchmarks.py --from-board <board.json> --current-board <CURRENT-BOARD.md>"
        ),
        "status": status_meta,
        "regressions": regressions,
        "compiler_versions": raw.get("compiler_versions", {}),
        "pins": raw.get("pins", {}),
        **({"host": raw["host"]} if "host" in raw else {}),
        "runtime_notes": [
            "LangArena/Matmul::T4: Hirsel submits 4 row blocks to one worker for the process run. Rust creates and drops a 4-worker Rayon pool each iteration. Goose starts 4 workers at construction and joins them at checksum time. All workers share the measurement CPU.",
            "LangArena/Matmul::T8: Hirsel submits 8 row blocks to one worker for the process run. Rust creates and drops an 8-worker Rayon pool each iteration. Goose starts 8 workers at construction and joins them at checksum time. All workers share the measurement CPU.",
            "LangArena/Matmul::T16: Hirsel submits 16 row blocks to one worker for the process run. Rust creates and drops a 16-worker Rayon pool each iteration. Goose starts 16 workers at construction and joins them at checksum time. All workers share the measurement CPU.",
        ],
        "geomeans": geomeans,
        "rows": flat_rows,
    }


def _fmt_ratio(v: float | None) -> str:
    return f"{v:.2f}×" if v is not None else "n/a"


def _fmt_ratio4(v: float | None) -> str:
    return f"{v:.4f}×" if v is not None else "n/a"


def _is_trailing(row: dict) -> bool:
    hr = row["h_over_r"]
    if hr is None:
        return False
    return hr < 1.0 if row["higher_is_better"] else hr > 1.0


def render_html(data: dict) -> str:
    st = data["status"]
    gms = data["geomeans"]
    passed = bool(st["passed"])
    commit_short = st["commit_short"]
    board_date = st["board_utc"] if st["board_utc"] != "unknown" else st["attempt_utc"]

    awfy_orig = gms["AWFY original | instructions"]
    awfy_pool = gms["AWFY pool/index | instructions"]
    la_sec = gms["LangArena | seconds"]
    la_insn = gms["LangArena | instructions"]
    comp_insn = gms["Compile | instructions"]

    def find_row(goal: str, name: str, metric: str) -> dict:
        for r in data["rows"]:
            if r["goal"] == goal and r["row"] == name and r["metric"] == metric:
                return r
        raise KeyError(f"missing benchmark row {goal}/{name}/{metric}")

    comp_cold_s = find_row("Compile", "LangArena cold", "seconds")
    comp_edit_s = find_row("Compile", "LangArena edit", "seconds")
    comp_cold_i = find_row("Compile", "LangArena cold", "instructions")
    comp_edit_i = find_row("Compile", "LangArena edit", "instructions")
    json_gen_s = find_row("LangArena", "Json::Generate", "seconds")
    json_gen_i = find_row("LangArena", "Json::Generate", "instructions")
    log_p_s = find_row("LangArena", "Etc::LogParser", "seconds")
    log_p_i = find_row("LangArena", "Etc::LogParser", "instructions")
    cb_tput = find_row("Checkbox", "converge, 64 clients", "toggles_per_second")
    cb_sub = find_row("Checkbox", "memory-subscribed, 64 clients", "server_rss_bytes")
    cb_unsub = find_row("Checkbox", "memory-unsubscribed, 64 clients", "server_rss_bytes")

    reg_items = "".join(f"<li><code>{html.escape(item)}</code></li>" for item in data["regressions"])
    gate_parts = []
    if st["boat_exit"] is not None:
        gate_parts.append(f"Boat exit {st['boat_exit']}")
    if st["vopr_exit"] is not None:
        gate_parts.append(f"VOPR exit {st['vopr_exit']}")
    if st["laws_exit"] is not None:
        gate_parts.append(f"Laws exit {st['laws_exit']}")
    gate_line = " · ".join(gate_parts)

    status_class = "bench-status passed" if passed else "bench-status"
    status_label = "Passed: yes" if passed else "Passed: no"
    status_copy = (
        "All board gates passed on this snapshot."
        if passed
        else (
            f"The latest complete goal-board run on commit <code>{html.escape(commit_short)}</code> "
            f"({html.escape(board_date)}) reported <strong>Passed: no</strong> (board exit status "
            f"<code>{st['exit_status']}</code>; {html.escape(gate_line)}). "
            "The numbers below are published as measured with these open regressions:"
        )
    )

    suites = []
    for r in data["rows"]:
        if r["goal"] not in suites:
            suites.append(r["goal"])

    filter_btns = ['<button class="pill-btn" type="button" data-suite="all" aria-pressed="true">All</button>']
    for s in suites:
        filter_btns.append(
            f'<button class="pill-btn" type="button" data-suite="{html.escape(s, quote=True)}" aria-pressed="false">'
            f"{html.escape(s)}</button>"
        )
    filter_btns.append(
        '<button class="pill-btn" type="button" data-suite="trails" aria-pressed="false">Trailing Rust</button>'
    )

    table_rows: list[str] = []
    for r in data["rows"]:
        hr = r["h_over_r"]
        hg = r["h_over_g"]
        trails = _is_trailing(r)
        cls = "bad" if trails else "good"
        # Bar maps 0..2.0x linearly with 1.0x at 50%; clamped to [4%, 100%].
        pct = 0.0 if hr is None else max(4.0, min(100.0, (hr / 2.0) * 100.0))
        note = " (higher is better)" if r["higher_is_better"] else " (1 sample)" if r["metric"] == "seconds" else ""
        search_blob = html.escape(f"{r['goal']} {r['row']} {r['metric']}".lower(), quote=True)
        table_rows.append(
            f'<tr data-suite="{html.escape(r["goal"], quote=True)}" '
            f'data-trails="{"yes" if trails else "no"}" data-search="{search_blob}">'
            f'<td><span class="suite-tag">{html.escape(r["goal"])}</span></td>'
            f'<td class="row-name">{html.escape(r["row"])}</td>'
            f'<td class="metric-name">{html.escape(r["metric"])}{html.escape(note)}</td>'
            f'<td class="num {cls}">{html.escape(_fmt_ratio4(hr))}</td>'
            f'<td class="bar-cell"><div class="ratio-bar" aria-hidden="true">'
            f'<span class="fill {cls}" style="width:{pct:.1f}%"></span><span class="mid"></span></div></td>'
            f'<td class="num neutral">{html.escape(_fmt_ratio4(hg))}</td>'
            f"</tr>"
        )

    cv = data.get("compiler_versions", {})
    cv_items = "".join(
        f"<li><code>{html.escape(k)}</code>: {html.escape(v)}</li>"
        for k, v in cv.items()
    )
    host = data.get("host")
    if host:
        host_items = "".join(
            f"<li><code>{html.escape(item)}</code></li>"
            for item in (
                f"CPU model: {host['cpu_model']}",
                f"Kernel: {host['kernel']}",
                f"SMT: {host['smt']['control']} (active {'yes' if host['smt']['active'] else 'no'})",
                *(
                    f"CPU {cpu}: governor {facts['governor']} ({facts['driver']}"
                    + (f", {facts['energy_preference']}" if facts.get("energy_preference") else "")
                    + "), boost "
                    + {True: "on", False: "off"}.get(facts["boost"], "unknown")
                    + f", SMT siblings {facts['smt_siblings']}"
                    for cpu, facts in host["cpus"].items()
                ),
            )
        )
        host_card = f"<p>Measured on CPUs {html.escape(', '.join(map(str, host['measure_cpus'])))}:</p><ul>{host_items}</ul>"
    else:
        host_card = "<p>This board run predates host recording, so its CPU model, kernel, governor, boost and SMT state are not published.</p>"
    matmul_items = "".join(
        f"<li>{html.escape(note)}</li>" for note in data.get("runtime_notes", [])
    )

    top_nav_html = "".join(
        f'<a href="{href}"'
        + (' class="hide-sm"' if hide_sm else "")
        + (' aria-current="page"' if active else "")
        + f">{label}</a>"
        for href, label, active, hide_sm in (
            ("/docs/", "Learn", False, False),
            ("/docs/#cookbook", "Cookbook", False, True),
            ("/docs/std/", "Std", False, True),
            ("/docs/spec/", "Spec", False, False),
            ("/benchmarks/", "Benchmarks", True, False),
        )
    )

    canonical = f"{ORIGIN}/benchmarks/"
    title = "Hirsel Benchmarks — Goal Board & Methodology"
    desc = (
        f"Hirsel vs Rust benchmark results and methodology from commit {commit_short} ({board_date}), "
        "including AWFY, LangArena, compile time, Checkbox server metrics, losing rows, and current board status."
    )
    jsonld = {
        "@context": "https://schema.org",
        "@type": "TechArticle",
        "headline": title,
        "description": desc,
        "url": canonical,
        "inLanguage": "en",
        "isPartOf": {"@id": f"{ORIGIN}/#website"},
        "about": {"@id": f"{ORIGIN}/#language"},
    }

    return "\n".join(
        [
            "<!doctype html>",
            AGENT_COMMENT,
            f"<!-- Generated by tools/site_benchmarks.py from site/benchmarks/board-data.json (commit {commit_short}, {board_date}) -->",
            '<html lang="en">',
            "<head>",
            '<meta charset="utf-8">',
            '<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">',
            f"  <title>{html.escape(title)}</title>",
            f'  <meta name="description" content="{html.escape(desc, quote=True)}">',
            '  <meta name="theme-color" content="#FBF6EC">',
            '  <link rel="icon" href="/favicon.ico" sizes="any">',
            '  <link rel="icon" href="/favicon.svg" type="image/svg+xml">',
            '  <link rel="apple-touch-icon" href="/apple-touch-icon.png">',
            f'  <link rel="canonical" href="{canonical}">',
            '  <link rel="alternate" type="text/markdown" href="/llms.txt">',
            '  <meta property="og:type" content="article">',
            '  <meta property="og:site_name" content="Hirsel">',
            f'  <meta property="og:title" content="{html.escape(title, quote=True)}">',
            f'  <meta property="og:description" content="{html.escape(desc, quote=True)}">',
            f'  <meta property="og:url" content="{canonical}">',
            f'  <meta property="og:image" content="{ORIGIN}/og-image.png">',
            '  <script type="application/ld+json">',
            json.dumps(jsonld, indent=2, ensure_ascii=False),
            "  </script>",
            '  <link rel="preload" href="/fonts/archivo-black-latin.woff2" as="font" type="font/woff2" crossorigin>',
            '  <link rel="preload" href="/fonts/inter-latin.woff2" as="font" type="font/woff2" crossorigin>',
            "<style>",
            DOCS_CSS.strip(),
            BENCH_CSS.strip(),
            "</style>",
            "</head>",
            "<body>",
            '<header class="top">',
            f'  <a class="brand" href="/" aria-label="Hirsel home">{RAM_MARK_SVG}<span class="wm wmfont">hirsel</span></a>',
            f'  <nav class="nav" aria-label="Site">{top_nav_html}</nav>',
            '  <div class="right">',
            '    <a class="hill-link" href="/"><span class="dot" aria-hidden="true"></span>Overview</a>',
            f'    <button class="pill-copy" id="copyAgent" type="button" title="Copy Hirsel docs and install links for coding agents" aria-live="polite">{COPY_ICON}{OK_ICON}<span class="lbl"><span class="idle">Copy for agents</span><span class="done" aria-hidden="true">Copied</span></span></button>',
            f'    <a class="gh" href="{GITHUB_REPO}" aria-label="Source on GitHub" title="Source on GitHub">{GITHUB_ICON}</a>',
            "  </div>",
            "</header>",
            '<main class="bench">',
            '  <div class="hero-doc">',
            f"    {TOPO_ACCENT_SVG}",
            f'    <p class="eyebrow"><span class="pip" aria-hidden="true"></span>Benchmarks · commit {html.escape(commit_short)} · {html.escape(board_date)}</p>',
            "    <h1>Benchmarks</h1>",
            '    <p class="lead">Measured ratios of Hirsel against Rust (and Goose where a peer exists) from the nightly goal board. Lower is better except Checkbox throughput (<code>toggles_per_second</code>, higher is better). Every suite, losing row, and methodology caveat is shown below.</p>',
            "  </div>",
            f'  <section class="{status_class}" aria-label="Board status">',
            f'    <div class="status-head"><span class="status-badge">{html.escape(status_label)}</span>'
            f'<span class="status-meta">Commit {html.escape(commit_short)} · {html.escape(board_date)}</span></div>',
            f"    <p>{status_copy}</p>",
            f"    <ul class=\"reg-list\">{reg_items}</ul>" if reg_items else "",
            "  </section>",
            '  <section class="kpi-grid" aria-label="Suite geomeans">',
            f'    <div class="kpi"><span class="k-label">AWFY original · instructions</span><span class="k-val good">{_fmt_ratio(awfy_orig["geomean_h_over_r"])}</span><span class="k-sub">geomean vs Rust · {awfy_orig["hirsel_le_rust"]} of {awfy_orig["count"]} rows &le; Rust</span></div>',
            f'    <div class="kpi"><span class="k-label">AWFY pool/index · instructions</span><span class="k-val good">{_fmt_ratio(awfy_pool["geomean_h_over_r"])}</span><span class="k-sub">geomean vs Rust · {awfy_pool["hirsel_le_rust"]} of {awfy_pool["count"]} rows &le; Rust</span></div>',
            f'    <div class="kpi"><span class="k-label">LangArena · seconds</span><span class="k-val good">{_fmt_ratio(la_sec["geomean_h_over_r"])}</span><span class="k-sub">geomean vs Rust · {la_sec["hirsel_le_rust"]} of {la_sec["count"]} rows &le; Rust</span></div>',
            f'    <div class="kpi"><span class="k-label">LangArena · instructions</span><span class="k-val good">{_fmt_ratio(la_insn["geomean_h_over_r"])}</span><span class="k-sub">geomean vs Rust · {la_insn["hirsel_le_rust"]} of {la_insn["count"]} rows &le; Rust</span></div>',
            f'    <div class="kpi"><span class="k-label">Compile · seconds</span><span class="k-val">{_fmt_ratio(comp_edit_s["h_over_r"])} / {_fmt_ratio(comp_cold_s["h_over_r"])}</span><span class="k-sub">warm edit / cold vs Rust (instructions geomean {_fmt_ratio(comp_insn["geomean_h_over_r"])})</span></div>',
            f'    <div class="kpi"><span class="k-label">Checkbox · 64 clients</span><span class="k-val warn">{_fmt_ratio(cb_tput["h_over_r"])} / {_fmt_ratio(cb_unsub["h_over_r"])}</span><span class="k-sub">throughput (higher better) / unsubscribed RSS ({_fmt_ratio(cb_sub["h_over_r"])} subscribed RSS)</span></div>',
            "  </section>",
            '  <section class="bench-section" id="losing-rows">',
            "    <h2>Where Hirsel trails Rust</h2>",
            "    <p>Headline geomeans hide real gaps. These are the largest rows where Hirsel is slower or uses more memory than the Rust peer on this board run:</p>",
            '    <div class="trails-grid">',
            f'      <div class="trail-card"><p class="t-name">LangArena / Json::Generate</p><p class="t-stat">{_fmt_ratio(json_gen_s["h_over_r"])} seconds · {_fmt_ratio(json_gen_i["h_over_r"])} instructions vs Rust</p><p>Largest runtime loss on the board; Hirsel JSON serialization does roughly twice the work of the Rust port.</p></div>',
            f'      <div class="trail-card"><p class="t-name">LangArena / Etc::LogParser</p><p class="t-stat">{_fmt_ratio(log_p_s["h_over_r"])} seconds · {_fmt_ratio(log_p_i["h_over_r"])} instructions vs Rust</p><p>Regex/log-parsing hot loop executes 73% more instructions and 31% more wall time than Rust.</p></div>',
            f'      <div class="trail-card"><p class="t-name">Compile instructions (LangArena)</p><p class="t-stat">{_fmt_ratio(comp_cold_i["h_over_r"])} cold · {_fmt_ratio(comp_edit_i["h_over_r"])} warm edit ({_fmt_ratio(comp_insn["geomean_h_over_r"])} geomean)</p><p>While warm edit wall time is {_fmt_ratio(comp_edit_s["h_over_r"])} Rust, cold compile wall time is {_fmt_ratio(comp_cold_s["h_over_r"])} and total compiler instructions remain higher on both.</p></div>',
            f'      <div class="trail-card"><p class="t-name">Checkbox server (64 clients)</p><p class="t-stat">{_fmt_ratio(cb_tput["h_over_r"])} throughput (lower) · {_fmt_ratio(cb_unsub["h_over_r"])} unsubscribed RSS</p><p>Hirsel delivers 82% of Rust throughput (7,162 vs 8,766 toggles/s) and uses 2.24&times; Rust RSS for unsubscribed sessions (200 vs 89 KiB/session), though subscribed RSS is {_fmt_ratio(cb_sub["h_over_r"])} Rust.</p></div>',
            "    </div>",
            "  </section>",
            '  <section class="bench-section" id="methodology">',
            "    <h2>Methodology &amp; caveats</h2>",
            '    <div class="meth-grid">',
            '      <div class="meth-card"><h3>Checks kept on</h3><p>Hirsel keeps <strong>bounds checks</strong>, <strong>integer overflow checks</strong>, <strong>logical byte admission</strong>, and <strong>step charges</strong> (wherever a step bound can be current) enabled. The Rust and Goose peers check bounds only.</p><p>Runtime instruction counts measure user-space CPU work with all PMU counters 100% active. Seconds use <code>perf duration_time</code> around the whole process (startup, setup, warmups, measured iterations, verification, exit).</p><p>Every <code>seconds</code> row is <strong>a single sample per side</strong>: no repeats, so no minimum, maximum or spread is published for it. The instruction counts are the measurement the nightly gate checks.</p></div>',
            f'      <div class="meth-card"><h3>Bench host</h3>{host_card}</div>',
            '      <div class="meth-card"><h3>Compiler build (no BOLT)</h3><p>The nightly board builds the Hirsel compiler with <code>DIST_BOLT=off</code>, so its Compile rows measure a compiler <strong>without BOLT</strong>. Shipped releases are built with <code>--bolt</code>: the compiler is BOLT-optimized on a profile from the release std and cold LangArena builds, the same LangArena workload the Compile rows use. The board never measures that release binary.</p></div>',
            '      <div class="meth-card"><h3>Libraries, layouts &amp; verification</h3><p>The ports use <strong>different libraries and data layouts</strong>; ratios compare full implementations rather than identical IR. LangArena runs the pinned upstream <code>run.js</code> fixture with upstream checksums, and each AWFY row verifies its reference result internally.</p><p>Compile rows keep dependencies built, use one Cargo codegen unit with release incremental enabled, and edit Spectralnorm&rsquo;s checksum for the warm rebuild.</p></div>',
            f'      <div class="meth-card"><h3>Matmul threading notes</h3><ul>{matmul_items}</ul></div>',
            f'      <div class="meth-card"><h3>Pinned toolchain &amp; peers</h3><ul>{cv_items}</ul></div>',
            "    </div>",
            "  </section>",
            '  <section class="bench-section" id="all-rows">',
            f'    <h2>All {len(data["rows"])} board metrics</h2>',
            "    <p>Ratios show <code>Hirsel / Rust</code> (<code>H/R</code>) and <code>Hirsel / Goose</code> (<code>H/G</code>). The bar places <code>1.00&times;</code> at the center line; green means Hirsel matches or beats Rust, red means Hirsel trails Rust.</p>",
            '    <div class="bench-controls">',
            '      <input class="bench-search" id="benchSearch" type="search" placeholder="Filter by suite, benchmark, or metric" aria-label="Filter benchmark rows">',
            f'      <div class="bench-filters" role="group" aria-label="Suite filter">{"".join(filter_btns)}</div>',
            "    </div>",
            f'    <p class="bench-count" id="benchCount">{len(data["rows"])} of {len(data["rows"])} metric rows shown</p>',
            '    <div class="table-wrap"><table id="benchTable">'
            "<thead><tr><th>Suite</th><th>Benchmark</th><th>Metric</th><th class=\"num\">H / Rust</th><th class=\"bar-cell\">vs 1.0&times;</th><th class=\"num\">H / Goose</th></tr></thead>"
            f"<tbody>{''.join(table_rows)}</tbody></table></div>",
            "  </section>",
            '  <footer class="bench-foot">',
            '    <span>Data file: <code>site/benchmarks/board-data.json</code> &middot; Regenerated by <code>tools/site_benchmarks.py</code></span>',
            f'    <span><a href="/">Overview</a> &middot; <a href="/docs/">Docs</a> &middot; <a href="/docs/spec/">Spec</a> &middot; <a href="{GITHUB_REPO}">GitHub</a></span>',
            "  </footer>",
            "</main>",
            "<script>",
            DOCS_JS.strip(),
            BENCH_JS.strip(),
            "</script>",
            "</body>",
            "</html>",
            "",
        ]
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true", help="Render HTML from board-data.json")
    mode.add_argument("--check", action="store_true", help="Check HTML matches board-data.json")
    mode.add_argument("--from-board", type=Path, metavar="BOARD_JSON", help="Refresh board-data.json and HTML from a board run")
    parser.add_argument("--current-board", type=Path, metavar="MD", help="Optional CURRENT-BOARD.md for freshness/pass status")
    args = parser.parse_args()

    if args.from_board:
        data = extract_board_data(args.from_board, args.current_board)
        DATA_PATH.parent.mkdir(parents=True, exist_ok=True)
        DATA_PATH.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        HTML_PATH.write_text(render_html(data), encoding="utf-8")
        return 0

    data = json.loads(DATA_PATH.read_text(encoding="utf-8"))
    rendered = render_html(data)
    if args.write:
        HTML_PATH.parent.mkdir(parents=True, exist_ok=True)
        HTML_PATH.write_text(rendered, encoding="utf-8")
        return 0

    if not HTML_PATH.is_file() or HTML_PATH.read_text(encoding="utf-8") != rendered:
        sys.stderr.write("site/benchmarks/index.html: stale; run python3 tools/site_benchmarks.py --write\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
