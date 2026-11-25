#!/usr/bin/env python3
"""Render Hirsel's human-facing HTML documentation pages with Tree-sitter highlighting.

Writes the left-sidebar documentation pages into a built site directory, reading the
primer.md, cookbook.md, api-index.md and install.sh already in it. tools/site_build.py runs
this on every build, so the pages are never committed and never go stale:
  - docs/index.html          Walkthrough (60-second quickstart + docs/PRIMER.md)
  - docs/cookbook/index.html 7 compiler-checked recipes (docs/COOKBOOK.md)
  - docs/std/index.html      Filterable Standard Library index (core/std/API-INDEX.md)
  - docs/install/index.html  Install & quickstart + inline install.sh source inspector

Usage: tools/site_docs.py --out-dir DIR [--release-tag TAG]
"""

from __future__ import annotations

import argparse
import html
import re
import tempfile
from pathlib import Path

from render_code_svg import ensure_binding, load_grammar, tokenize_lines
from site_docs_theme import COPY_ICON, GITHUB_REPO, OK_ICON, render_page


# Same earth-toned capture classes as site/index.html (tools/site_highlight.py).
CLASS = {
    "keyword": "k",
    "keyword.conditional": "k",
    "keyword.repeat": "k",
    "keyword.return": "k",
    "function": "d",
    "type.definition": "d",
    "type": "t",
    "type.builtin": "t",
    "constructor": "t",
    "attribute": "t",
    "string": "v",
    "character": "v",
    "number": "v",
    "number.float": "v",
    "boolean": "v",
    "operator": "o",
    "punctuation.bracket": "o",
    "punctuation.delimiter": "o",
    "comment": "c",
}

DOC_REF_COMMENT = re.compile(r"\s*<!--\s*doc-ref:[^>]*-->")
FENCE_RE = re.compile(r"^([ \t]*)(`{3,}|~{3,})([^\r\n]*)$")
HEADING_RE = re.compile(r"^(#{1,3})\s+(.*)$")
BULLET_RE = re.compile(r"^- (.*)$")
ORDERED_RE = re.compile(r"^(\d+)\. (.*)$")
TABLE_SEP_RE = re.compile(r"^\|?[\s:-]+\|[\s|:-]*$")
INLINE_RE = re.compile(
    r"(`[^`]+`|\*\*[^*]+\*\*|\*[^*\n]+\*|\[[^\]]+\]\([^)]+\))"
)

STARTER_MAIN = """\
pub fn main() -> Result[(), Exit] {
    Ok(())
}

pub fn twice(n: u64) -> u64 {
    n + n
}"""

STARTER_TESTS = """\
@test
fn test_twice() -> Result[(), ()] {
    if main.twice(21) == 42 {
        Ok(())
    } else {
        Error(())
    }
}"""


def slugify(text: str) -> str:
    plain = re.sub(r"`([^`]+)`", r"\1", text)
    plain = re.sub(r"[^a-zA-Z0-9]+", "-", plain).strip("-").lower()
    return plain or "section"


def rewrite_human_href(url: str, ref: str) -> str:
    if url in ("PRIMER.md", "https://hirsel.ai/primer.md", "/primer.md"):
        return "/docs/"
    if url in ("COOKBOOK.md", "https://hirsel.ai/cookbook.md", "/cookbook.md"):
        return "/docs/cookbook/"
    if url in ("https://hirsel.ai/api-index.md", "/api-index.md"):
        return "/docs/std/"
    if url in ("https://hirsel.ai/install.sh", "/install.sh"):
        return "/docs/install/"
    if url.startswith("spec/"):
        return f"{GITHUB_REPO}/blob/{ref}/docs/{url}"
    if url.startswith("api/") or url == "API.md":
        return f"{GITHUB_REPO}/blob/{ref}/core/std/{url}"
    return url


def render_inline(text: str, ref: str) -> str:
    text = DOC_REF_COMMENT.sub("", text)
    parts: list[str] = []
    pos = 0
    for m in INLINE_RE.finditer(text):
        if m.start() > pos:
            parts.append(html.escape(text[pos : m.start()], quote=False))
        tok = m.group(0)
        if tok.startswith("`"):
            body = tok[1:-1]
            # Short inline code never wraps mid-token (`--record`, `OWN-7`); long code may wrap at spaces.
            cls = ' class="w"' if len(body) > 28 else ""
            parts.append(f"<code{cls}>{html.escape(body, quote=False)}</code>")
        elif tok.startswith("**"):
            parts.append(f"<strong>{render_inline(tok[2:-2], ref)}</strong>")
        elif tok.startswith("*"):
            parts.append(f"<em>{render_inline(tok[1:-1], ref)}</em>")
        elif tok.startswith("["):
            split = tok.index("](")
            label = tok[1:split]
            href = rewrite_human_href(tok[split + 2 : -1], ref)
            parts.append(
                f'<a href="{html.escape(href, quote=True)}">{render_inline(label, ref)}</a>'
            )
        pos = m.end()
    if pos < len(text):
        parts.append(html.escape(text[pos:], quote=False))
    return "".join(parts)


def highlight_hirsel_html(lines: list[str], parser, query, label: str) -> tuple[str, bool]:
    ranges = tokenize_lines(parser, query, lines, CLASS, label=label)
    rendered_lines: list[str] = []
    has_refusal = False
    for line, spans in zip(lines, ranges):
        is_no = "// refused:" in line
        if is_no:
            has_refusal = True
        pieces: list[str] = []
        pos = 0
        for start, length, cls in spans:
            if start > pos:
                pieces.append(html.escape(line[pos:start], quote=False))
            pieces.append(
                f'<span class="{cls}">{html.escape(line[start : start + length], quote=False)}</span>'
            )
            pos = start + length
        if pos < len(line):
            pieces.append(html.escape(line[pos:], quote=False))
        inner = "".join(pieces)
        cls_attr = "ln no" if is_no else "ln"
        rendered_lines.append(f'<span class="{cls_attr}">{inner}</span>')
    return "\n".join(rendered_lines), has_refusal


def highlight_shell_html(lines: list[str]) -> str:
    rendered: list[str] = []
    for line in lines:
        if line.lstrip().startswith("#"):
            rendered.append(
                f'<span class="ln"><span class="c">{html.escape(line, quote=False)}</span></span>'
            )
            continue
        comment_idx = None
        in_sq = in_dq = False
        for i, ch in enumerate(line):
            if ch == "'" and not in_dq:
                in_sq = not in_sq
            elif ch == '"' and not in_sq:
                in_dq = not in_dq
            elif ch == "#" and not in_sq and not in_dq and (i == 0 or line[i - 1].isspace()):
                comment_idx = i
                break
        if comment_idx is not None:
            code_part = html.escape(line[:comment_idx], quote=False)
            comm_part = html.escape(line[comment_idx:], quote=False)
            rendered.append(
                f'<span class="ln">{code_part}<span class="c">{comm_part}</span></span>'
            )
        else:
            rendered.append(
                f'<span class="ln">{html.escape(line, quote=False)}</span>'
            )
    return "\n".join(rendered)


def render_code_card(
    lines: list[str],
    info: str,
    parser,
    query,
    label: str,
    *,
    is_output_hint: bool = False,
) -> str:
    tokens = info.strip().split()
    lang = tokens[0] if tokens else "text"
    extra = tokens[1:] if len(tokens) > 1 else []
    has_refusal = False
    is_output = False
    if lang == "hirsel":
        inner, has_refusal = highlight_hirsel_html(lines, parser, query, label)
        lang_label = "hirsel · runnable" if "run" in extra else "hirsel"
    elif lang in ("sh", "bash", "shell"):
        inner = highlight_shell_html(lines)
        lang_label = "sh"
    elif lang == "toml" or (lang == "text" and lines and lines[0].lstrip().startswith("[")):
        inner = "\n".join(
            f'<span class="ln">{html.escape(line, quote=False)}</span>'
            for line in lines
        )
        lang_label = "hirsel.toml"
    else:
        inner = "\n".join(
            f'<span class="ln">{html.escape(line, quote=False)}</span>'
            for line in lines
        )
        is_output = is_output_hint or lang == "output"
        lang_label = "output" if is_output else lang

    badge = '<span class="code-badge">refused by checker</span>' if has_refusal else ""
    copy_btn = (
        ""
        if is_output
        else (
            f'<button class="code-copy" type="button" aria-label="Copy code">'
            f'{COPY_ICON}{OK_ICON}<span class="txt">Copy</span></button>'
        )
    )
    extra_cls = " is-output" if is_output else ""
    return (
        f'<div class="code-block{extra_cls}">'
        f'<div class="code-bar"><span class="code-meta">'
        f'<span class="code-lang">{html.escape(lang_label)}</span>{badge}</span>{copy_btn}</div>'
        f"<pre><code>{inner}</code></pre></div>"
    )


def parse_table_row(line: str) -> list[str]:
    stripped = line.strip()
    if stripped.startswith("|"):
        stripped = stripped[1:]
    if stripped.endswith("|"):
        stripped = stripped[:-1]
    return [cell.strip() for cell in stripped.split("|")]


def render_markdown(
    md_text: str,
    *,
    parser,
    query,
    ref: str,
    label: str,
    extract_release_banner: bool = True,
) -> tuple[str, str, list[tuple[str, str]], str]:
    """Parse Markdown into (lead_html, body_html, toc, release_banner_html)."""
    lines = md_text.splitlines()
    idx = 0
    n = len(lines)

    # Skip H1 (rendered in hero-doc)
    while idx < n and not lines[idx].strip():
        idx += 1
    if idx < n and lines[idx].startswith("# "):
        idx += 1

    release_banner_html = ""
    while idx < n and not lines[idx].strip():
        idx += 1
    if extract_release_banner and idx < n and lines[idx].startswith("> "):
        bq_lines: list[str] = []
        while idx < n and lines[idx].startswith(">"):
            bq_lines.append(lines[idx][1:].lstrip())
            idx += 1
        release_banner_html = f"<p>{render_inline(' '.join(bq_lines), ref)}</p>"

    # First intro paragraph before the first ## becomes hero lead
    while idx < n and not lines[idx].strip():
        idx += 1
    lead_lines: list[str] = []
    while (
        idx < n
        and lines[idx].strip()
        and not HEADING_RE.match(lines[idx])
        and not FENCE_RE.match(lines[idx])
        and not lines[idx].startswith("> ")
        and not lines[idx].startswith("|")
        and not BULLET_RE.match(lines[idx])
    ):
        lead_lines.append(lines[idx].strip())
        idx += 1
    lead_html = render_inline(" ".join(lead_lines), ref)

    out: list[str] = []
    toc: list[tuple[str, str]] = []
    used_ids: set[str] = set()
    prev_was_runnable = False

    def next_id(raw: str) -> str:
        base = slugify(raw)
        cand = base
        k = 2
        while cand in used_ids:
            cand = f"{base}-{k}"
            k += 1
        used_ids.add(cand)
        return cand

    def read_fence(start_idx: int) -> tuple[str, list[str], int]:
        m = FENCE_RE.match(lines[start_idx])
        assert m is not None
        delim = m.group(2)
        info = m.group(3).strip()
        buf: list[str] = []
        j = start_idx + 1
        while j < n:
            end_m = FENCE_RE.match(lines[j])
            if end_m and end_m.group(2).startswith(delim) and not end_m.group(3).strip():
                return info, buf, j + 1
            buf.append(lines[j])
            j += 1
        return info, buf, j

    while idx < n:
        line = lines[idx]
        if not line.strip():
            idx += 1
            continue

        hm = HEADING_RE.match(line)
        if hm:
            level = len(hm.group(1))
            raw = hm.group(2).strip()
            hid = next_id(raw)
            rendered = render_inline(raw, ref)
            tag = f"h{min(level, 3)}"
            out.append(
                f'<{tag} id="{hid}">{rendered}'
                f'<a class="anchor" href="#{hid}" aria-label="Link to section">#</a></{tag}>'
            )
            if level == 2:
                toc.append((hid, rendered))
            prev_was_runnable = False
            idx += 1
            continue

        if FENCE_RE.match(line):
            fence_line = idx + 1
            info, buf, idx = read_fence(idx)
            is_out = prev_was_runnable and info.strip() in ("", "text")
            out.append(
                render_code_card(
                    buf,
                    info,
                    parser,
                    query,
                    f"{label}:{fence_line}",
                    is_output_hint=is_out,
                )
            )
            prev_was_runnable = info.strip() == "hirsel run"
            continue

        if line.strip() == "---":
            out.append("<hr>")
            prev_was_runnable = False
            idx += 1
            continue

        if line.startswith("> "):
            bq: list[str] = []
            while idx < n and lines[idx].startswith(">"):
                bq.append(lines[idx][1:].lstrip())
                idx += 1
            out.append(f"<blockquote><p>{render_inline(' '.join(bq), ref)}</p></blockquote>")
            prev_was_runnable = False
            continue

        if line.lstrip().startswith("|") and idx + 1 < n and TABLE_SEP_RE.match(lines[idx + 1].strip()):
            headers = parse_table_row(line)
            idx += 2
            rows: list[list[str]] = []
            while idx < n and lines[idx].lstrip().startswith("|"):
                rows.append(parse_table_row(lines[idx]))
                idx += 1
            ths = "".join(f"<th>{render_inline(h, ref)}</th>" for h in headers)
            trs = "".join(
                "<tr>" + "".join(f"<td>{render_inline(c, ref)}</td>" for c in r) + "</tr>"
                for r in rows
            )
            out.append(
                f'<div class="table-wrap"><table><thead><tr>{ths}</tr></thead>'
                f"<tbody>{trs}</tbody></table></div>"
            )
            prev_was_runnable = False
            continue

        if BULLET_RE.match(line):
            items: list[str] = []
            while idx < n:
                bm = BULLET_RE.match(lines[idx])
                if not bm:
                    break
                item_parts = [bm.group(1).strip()]
                idx += 1
                item_blocks: list[str] = []
                while idx < n:
                    cur = lines[idx]
                    if BULLET_RE.match(cur) or HEADING_RE.match(cur):
                        break
                    if not cur.strip():
                        # Look ahead: is a fenced code block immediately following for this bullet?
                        if idx + 1 < n and FENCE_RE.match(lines[idx + 1]):
                            idx += 1
                            while idx < n and FENCE_RE.match(lines[idx]):
                                fl = idx + 1
                                info, buf, idx = read_fence(idx)
                                item_blocks.append(
                                    render_code_card(buf, info, parser, query, f"{label}:{fl}")
                                )
                                while idx < n and not lines[idx].strip():
                                    idx += 1
                            break
                        idx += 1
                        break
                    if cur.startswith("  "):
                        item_parts.append(cur.strip())
                        idx += 1
                    else:
                        break
                item_html = f"<p>{render_inline(' '.join(item_parts), ref)}</p>" + "".join(item_blocks)
                items.append(f"<li>{item_html}</li>")
            out.append(f"<ul>{''.join(items)}</ul>")
            prev_was_runnable = False
            continue

        if ORDERED_RE.match(line):
            items = []
            while idx < n:
                om = ORDERED_RE.match(lines[idx])
                if not om:
                    break
                parts = [om.group(2).strip()]
                idx += 1
                while idx < n and lines[idx].startswith("  ") and lines[idx].strip():
                    parts.append(lines[idx].strip())
                    idx += 1
                items.append(f"<li>{render_inline(' '.join(parts), ref)}</li>")
            out.append(f"<ol>{''.join(items)}</ol>")
            prev_was_runnable = False
            continue

        # Paragraph
        para: list[str] = []
        while (
            idx < n
            and lines[idx].strip()
            and not HEADING_RE.match(lines[idx])
            and not FENCE_RE.match(lines[idx])
            and not BULLET_RE.match(lines[idx])
            and not ORDERED_RE.match(lines[idx])
            and not lines[idx].startswith("> ")
            and lines[idx].strip() != "---"
            and not (lines[idx].lstrip().startswith("|") and idx + 1 < n and TABLE_SEP_RE.match(lines[idx + 1].strip()))
        ):
            para.append(lines[idx].strip())
            idx += 1
        text = " ".join(para)
        if text == "Outcome: success.":
            out.append('<p class="outcome-note">Outcome: success.</p>')
        else:
            out.append(f"<p>{render_inline(text, ref)}</p>")
        prev_was_runnable = False

    return lead_html, "\n".join(out), toc, release_banner_html


def quickstart_section_html(parser, query) -> str:
    install_cmd = render_code_card(
        ["curl -fsSL https://hirsel.ai/install.sh | sh"],
        "sh",
        parser,
        query,
        "quickstart-install",
    )
    run_cmd = render_code_card(
        [
            "hirsel new hello && cd hello",
            "hirsel check     # type, ownership, capability & bound checks",
            "hirsel test      # run every @test in its own process",
            "hirsel run       # build native binary and execute",
        ],
        "sh",
        parser,
        query,
        "quickstart-run",
    )
    return (
        '<section class="quick" aria-labelledby="get-started">'
        '<div class="quick-head">'
        '<h2 id="get-started">Get started in 60 seconds</h2>'
        '<a href="/docs/install/">Install options &amp; source &rarr;</a>'
        "</div>"
        '<p class="step-label">1. Install the toolchain</p>'
        f"{install_cmd}"
        '<p class="plat">Linux x86_64 &middot; glibc 2.28+ &middot; Apple Silicon macOS in progress</p>'
        '<p class="step-label">2. Create, check, test, and run your first package</p>'
        f"{run_cmd}"
        '<div class="pillars">'
        '<div class="pillar"><h3>Explicit capabilities</h3>'
        "<p>Files, network, clocks, randomness, and stdout are ordinary parameters &mdash; never ambient globals.</p></div>"
        '<div class="pillar"><h3>Logical byte budgets</h3>'
        "<p>Every allocation charges against a deterministic limit. Overflow raises catchable <code>Capacity</code>.</p></div>"
        '<div class="pillar"><h3>Deterministic replay</h3>'
        "<p>Record any run externally with <code>--record</code> and replay it byte-for-byte with zero host grants.</p></div>"
        "</div>"
        "</section>"
    )


def build_walkthrough_page(primer_md: str, ref: str, parser, query) -> str:
    lead_html, body_html, toc, release_banner = render_markdown(
        primer_md, parser=parser, query=query, ref=ref, label="docs/PRIMER.md"
    )
    full_toc = [("get-started", "Get started in 60s"), *toc]
    full_body = quickstart_section_html(parser, query) + "\n" + body_html
    eyebrow = f"Walkthrough · {ref}" if ref != "main" else "Walkthrough"
    return render_page(
        slug="/docs/",
        title="Hirsel Walkthrough — Language Primer & Quickstart",
        description="Learn Hirsel in one readable walkthrough: explicit capabilities, value semantics, regions, logical byte and step budgets, and deterministic replay.",
        eyebrow=eyebrow,
        heading="Hirsel walkthrough",
        lead=lead_html,
        body_html=full_body,
        toc=full_toc,
        active_page="walkthrough",
        pager=(None, ("/docs/cookbook/", "Cookbook (7 recipes)")),
        ref=ref,
        markdown_alternate="/primer.md",
        release_banner_html=release_banner,
    )


def build_cookbook_page(cookbook_md: str, ref: str, parser, query) -> str:
    lead_html, body_html, toc, release_banner = render_markdown(
        cookbook_md, parser=parser, query=query, ref=ref, label="docs/COOKBOOK.md"
    )
    eyebrow = f"Cookbook · 7 checked recipes · {ref}" if ref != "main" else "Cookbook · 7 checked recipes"
    return render_page(
        slug="/docs/cookbook/",
        title="Hirsel Cookbook — Seven Compiler-Checked Recipes",
        description="Seven complete, compiler-checked Hirsel recipes: CLI config, confined files and byte bounds, HTTP routing, Store transactions, task units, sim testing, and SIMD loops.",
        eyebrow=eyebrow,
        heading="Hirsel cookbook",
        lead=lead_html,
        body_html=body_html,
        toc=toc,
        active_page="cookbook",
        pager=(("/docs/", "Walkthrough"), ("/docs/std/", "Standard library")),
        ref=ref,
        markdown_alternate="/cookbook.md",
        release_banner_html=release_banner,
    )


def build_std_page(api_index_md: str, ref: str, parser, query) -> str:
    lines = api_index_md.splitlines()
    modules: list[tuple[str, str, bool, str, str]] = []
    total_modules = 0
    total_extends = 0
    total_ops = 0
    related: list[tuple[str, str]] = []
    in_related = False

    row_re = re.compile(r"^\|\s*\[([^\]]+)\]\(([^)]+)\)\s*\|\s*(yes|)\s*\|\s*(\d+)\s*\|\s*(.*?)\s*\|$")
    total_re = re.compile(r"^\|\s*\*\*total\*\*\s*\|\s*\*\*(\d+)\s+of\s+(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*")
    rel_re = re.compile(r"^-\s*\[([^\]]+)\]\(([^)]+)\)$")

    for line in lines:
        s = line.strip()
        if s.startswith("## Related module contracts"):
            in_related = True
            continue
        if in_related:
            rm = rel_re.match(s)
            if rm:
                related.append((rm.group(1), rewrite_human_href(rm.group(2), ref)))
            continue
        m = row_re.match(s)
        if m:
            mod, href, ext, ops_count, ops = m.groups()
            modules.append((mod, rewrite_human_href(href, ref), ext == "yes", ops_count, ops))
            continue
        tm = total_re.match(s)
        if tm:
            total_extends, total_modules, total_ops = int(tm.group(1)), int(tm.group(2)), int(tm.group(3))

    if not total_modules:
        total_modules = len(modules)
        total_extends = sum(1 for item in modules if item[2])
        total_ops = sum(int(item[3]) for item in modules)

    rows_html: list[str] = []
    for mod, href, is_ext, ops_count, ops in modules:
        search_blob = html.escape(f"{mod} {ops}".lower(), quote=True)
        ext_html = '<span class="ext-badge">extends core</span>' if is_ext else ""
        rows_html.append(
            f'<tr data-mod="{html.escape(mod, quote=True)}" '
            f'data-ext="{"yes" if is_ext else "no"}" data-search="{search_blob}">'
            f'<td><a class="mod-link" href="{html.escape(href, quote=True)}">{html.escape(mod)}</a></td>'
            f"<td>{ext_html}</td>"
            f'<td class="ops-num">{ops_count}</td>'
            f'<td class="ops-cell">{html.escape(ops)}</td>'
            f"</tr>"
        )

    inspect_block = render_code_card(
        [
            "hirsel inspect --core --interface   # exact core signatures",
            "hirsel inspect --std --interface    # exact std signatures and core extensions",
        ],
        "sh",
        parser,
        query,
        "std-inspect",
    )

    related_items = "".join(
        f'<li><a href="{html.escape(href, quote=True)}"><code>{html.escape(name)}</code></a></li>'
        for name, href in related
    )
    overview_url = f"{GITHUB_REPO}/blob/{ref}/core/std/API.md"

    body_html = "\n".join(
        [
            '<h2 id="modules">All modules<a class="anchor" href="#modules" aria-label="Link to section">#</a></h2>',
            '<p>Modules are called directly by path (<code>std.json.parse</code>, <code>std.http.serve</code>) with no imports. '
            'Modules marked <strong>extends core</strong> merge into their core module name (for example, <code>list.sort</code>). '
            f'Read the <a href="{html.escape(overview_url, quote=True)}">API overview on GitHub</a> or click any module for its full contract.</p>',
            '<div class="std-controls">',
            f'  <input class="std-search" id="stdSearch" type="search" placeholder="Filter {total_modules} modules or {total_ops:,} operations (press /)" aria-label="Filter standard library modules and operations">',
            '  <div class="std-filters" role="group" aria-label="Module filter">',
            f'    <button class="pill-btn" type="button" data-filter="all" aria-pressed="true">All ({total_modules})</button>',
            f'    <button class="pill-btn" type="button" data-filter="ext" aria-pressed="false">Extends core ({total_extends})</button>',
            "  </div>",
            "</div>",
            f'<p class="std-count" id="stdCount">{total_modules} of {total_modules} modules shown</p>',
            '<div class="table-wrap"><table id="stdTable">'
            "<thead><tr><th>Module</th><th>Scope</th><th>Ops</th><th>Operations</th></tr></thead>"
            f"<tbody>{''.join(rows_html)}</tbody></table></div>",
            '<h2 id="related">Related module contracts<a class="anchor" href="#related" aria-label="Link to section">#</a></h2>',
            f"<ul>{related_items}</ul>",
            '<h2 id="inspect">Exact signatures from your binary<a class="anchor" href="#inspect" aria-label="Link to section">#</a></h2>',
            "<p>Your installed toolchain prints the exact declarations and types it compiles against:</p>",
            inspect_block,
        ]
    )

    toc = [
        ("modules", f"All modules ({total_modules})"),
        ("related", "Related contracts"),
        ("inspect", "CLI inspection"),
    ]
    eyebrow = f"Standard library · {total_modules} modules · {total_ops:,} ops"
    if ref != "main":
        eyebrow += f" · {ref}"
    return render_page(
        slug="/docs/std/",
        title="Hirsel Standard Library — Module & Operation Index",
        description=f"Browse all {total_modules} Hirsel standard library modules and {total_ops:,} operations with instant filtering and links to every module contract.",
        eyebrow=eyebrow,
        heading="Standard library",
        lead=(
            f"Every <code>std</code> module and operation in Hirsel ({total_modules} modules, "
            f"{total_extends} core extensions, {total_ops:,} operations). Filter live below or run "
            "<code class=\"w\">hirsel inspect --std --interface</code> for exact local signatures."
        ),
        body_html=body_html,
        toc=toc,
        active_page="std",
        pager=(("/docs/cookbook/", "Cookbook"), ("/docs/install/", "Install & quickstart")),
        ref=ref,
        markdown_alternate="/api-index.md",
    )


def build_install_page(install_sh: str, ref: str, parser, query) -> str:
    tag_example = ref if ref != "main" else "v0.1.0-pre"
    install_lines = len(install_sh.splitlines())

    one_liner = render_code_card(
        ["curl -fsSL https://hirsel.ai/install.sh | sh"],
        "sh",
        parser,
        query,
        "install-one-liner",
    )
    pin_cmd = render_code_card(
        [
            f"# Pin a specific release tag",
            f"HIRSEL_VERSION={tag_example} curl -fsSL https://hirsel.ai/install.sh | sh",
            "",
            "# Custom install directory (default: $HOME/.hirsel)",
            "HIRSEL_INSTALL_DIR=/opt/hirsel curl -fsSL https://hirsel.ai/install.sh | sh",
            "",
            "# Skip editing shell rc files (.bashrc / .zshrc / fish)",
            "curl -fsSL https://hirsel.ai/install.sh | sh -s -- --no-modify-path",
        ],
        "sh",
        parser,
        query,
        "install-options",
    )
    uninstall_cmd = render_code_card(
        ["curl -fsSL https://hirsel.ai/install.sh | sh -s -- --uninstall"],
        "sh",
        parser,
        query,
        "install-uninstall",
    )
    quickstart_cmd = render_code_card(
        [
            "hirsel new hello && cd hello",
            "hirsel check     # fast incremental check",
            "hirsel test      # runs tests.hirsel in isolated processes",
            "hirsel run       # compiles and runs main.hirsel",
            "hirsel build --release -o hello",
        ],
        "sh",
        parser,
        query,
        "install-quickstart",
    )
    starter_main_card = render_code_card(
        STARTER_MAIN.splitlines(), "hirsel", parser, query, "install-starter-main"
    )
    starter_test_card = render_code_card(
        STARTER_TESTS.splitlines(), "hirsel", parser, query, "install-starter-tests"
    )
    source_card = render_code_card(
        install_sh.splitlines(), "sh", parser, query, "install-sh-source"
    )

    body_html = "\n".join(
        [
            '<h2 id="install">Install the toolchain<a class="anchor" href="#install" aria-label="Link to section">#</a></h2>',
            "<p>Downloads the latest signed release bundle, verifies its SHA-256 checksum against <code>SHA256SUMS</code>, "
            "and links <code>hirsel</code> into <code>~/.hirsel/bin</code> without touching system packages:</p>",
            one_liner,
            '<div class="info-grid">',
            '  <div class="info-card"><h3>Supported today</h3><p>Linux <code>x86_64</code> with <code>glibc 2.28+</code>. Self-contained native toolchain including Cranelift, LLVM, and <code>ld.lld</code>.</p></div>',
            '  <div class="info-card"><h3>In progress</h3><p>Apple Silicon macOS (<code>aarch64-apple-darwin</code>) native release binaries are in progress. Linux hosts on musl are not yet supported.</p></div>',
            "</div>",
            '<h2 id="options">Version pinning &amp; uninstall<a class="anchor" href="#options" aria-label="Link to section">#</a></h2>',
            "<p>Pin a release, choose an install prefix, or opt out of shell PATH edits with environment variables and flags:</p>",
            pin_cmd,
            "<p>To remove every installed version and revert PATH entries cleanly:</p>",
            uninstall_cmd,
            '<h2 id="first-project">Your first project<a class="anchor" href="#first-project" aria-label="Link to section">#</a></h2>',
            "<p><code>hirsel new</code> scaffolds a ready-to-run package with <code>hirsel.toml</code>, "
            "<code>main.hirsel</code>, and <code>tests.hirsel</code>:</p>",
            quickstart_cmd,
            "<h3>Generated <code>main.hirsel</code></h3>",
            starter_main_card,
            "<h3>Generated <code>tests.hirsel</code></h3>",
            starter_test_card,
            "<p>Next, follow the <a href=\"/docs/\">Hirsel walkthrough</a> to add explicit capabilities "
            "(<code>Stdout</code>, <code>Dir</code>, <code>Net</code>), byte budgets, and recorded replay.</p>",
            '<h2 id="source">Inspect the installer<a class="anchor" href="#source" aria-label="Link to section">#</a></h2>',
            "<p>Prefer to read the shell script before running it? The full installer served at "
            "<code class=\"w\">https://hirsel.ai/install.sh</code> is shown verbatim below.</p>",
            f'<details class="source-drawer"><summary><span>Inspect <code>install.sh</code> source</span>'
            f'<span class="hint">{install_lines} lines &middot; POSIX sh</span></summary>{source_card}</details>',
        ]
    )

    toc = [
        ("install", "Install the toolchain"),
        ("options", "Version pinning &amp; uninstall"),
        ("first-project", "Your first project"),
        ("source", "Inspect install.sh"),
    ]
    eyebrow = f"Install · {ref}" if ref != "main" else "Install & Quickstart"
    return render_page(
        slug="/docs/install/",
        title="Install Hirsel — Toolchain Installer & Quickstart",
        description="Install the Hirsel native compiler toolchain on Linux x86_64, pin versions, uninstall cleanly, and run your first Hirsel project in 60 seconds.",
        eyebrow=eyebrow,
        heading="Install Hirsel",
        lead=(
            "One self-contained native toolchain with Cranelift, LLVM, simulator, profiler, and deterministic replay. "
            "Verified by SHA-256 and installed under <code>~/.hirsel</code>."
        ),
        body_html=body_html,
        toc=toc,
        active_page="install",
        pager=(("/docs/std/", "Standard library"), ("/docs/", "Start the walkthrough")),
        ref=ref,
    )


def build_docs_pages(
    *,
    primer_md: str,
    cookbook_md: str,
    api_index_md: str,
    install_sh: str,
    ref: str,
    parser,
    query,
) -> dict[str, str]:
    return {
        "docs/index.html": build_walkthrough_page(primer_md, ref, parser, query),
        "docs/cookbook/index.html": build_cookbook_page(cookbook_md, ref, parser, query),
        "docs/std/index.html": build_std_page(api_index_md, ref, parser, query),
        "docs/install/index.html": build_install_page(install_sh, ref, parser, query),
    }


def main() -> None:
    parser_cli = argparse.ArgumentParser(description=__doc__)
    parser_cli.add_argument("--out-dir", type=Path, metavar="DIR", required=True)
    parser_cli.add_argument("--release-tag", metavar="TAG")
    args = parser_cli.parse_args()
    ensure_binding(__file__)
    src = args.out_dir
    with tempfile.TemporaryDirectory(prefix="site-docs-") as temp:
        _grammar, ts_parser, ts_query = load_grammar(temp)
        pages = build_docs_pages(
            primer_md=(src / "primer.md").read_text(encoding="utf-8"),
            cookbook_md=(src / "cookbook.md").read_text(encoding="utf-8"),
            api_index_md=(src / "api-index.md").read_text(encoding="utf-8"),
            install_sh=(src / "install.sh").read_text(encoding="utf-8"),
            ref=args.release_tag or "main",
            parser=ts_parser,
            query=ts_query,
        )
    for rel, content in pages.items():
        dest = args.out_dir / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(content, encoding="utf-8")


if __name__ == "__main__":
    main()
