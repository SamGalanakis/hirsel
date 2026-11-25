#!/usr/bin/env python3
"""Render docs/spec/ into /docs/spec/ (index + one page per chapter).

Imported by tools/site_docs.py so spec pages share the editorial docs shell,
Tree-sitter highlighting (with a plain fallback for intentional syntax-error
refusals or unbuilt grammar sketches), and release-tag alignment.
"""

from __future__ import annotations

import html
import re
import subprocess
from pathlib import Path

from site_docs_theme import GITHUB_REPO, render_page, render_sidebar

ROOT = Path(__file__).resolve().parents[1]
SPEC_DIR = ROOT / "docs" / "spec"


def list_spec_files(ref: str) -> list[str]:
    """Return sorted relative filenames (e.g. 'budgets.md') in docs/spec/ at ref."""
    if ref == "main":
        return sorted(p.name for p in SPEC_DIR.glob("*.md") if p.is_file())
    proc = subprocess.run(
        ["git", "ls-tree", "--name-only", ref, "docs/spec/"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return []
    names = []
    for line in proc.stdout.splitlines():
        name = line.strip().removeprefix("docs/spec/")
        if name.endswith(".md") and "/" not in name:
            names.append(name)
    return sorted(names)


def read_spec_file(ref: str, filename: str) -> str | None:
    if ref == "main":
        path = SPEC_DIR / filename
        return path.read_text(encoding="utf-8") if path.is_file() else None
    proc = subprocess.run(
        ["git", "show", f"{ref}:docs/spec/{filename}"],
        cwd=ROOT,
        capture_output=True,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout.decode("utf-8")


def extract_title(md_text: str, stem: str) -> str:
    for line in md_text.splitlines():
        s = line.strip()
        if s.startswith("# "):
            return s[2:].strip()
    return stem.replace("-", " ").title()


def order_chapters(filenames: list[str], index_md: str) -> list[str]:
    """Order spec chapters: those linked in index.md first (in index order), then the rest."""
    available = {f for f in filenames if f != "index.md"}
    ordered: list[str] = []
    seen: set[str] = set()
    for m in re.finditer(r"\]\(([a-z0-9_-]+\.md)(?:#[^)]*)?\)", index_md):
        name = m.group(1)
        if name in available and name not in seen:
            ordered.append(name)
            seen.add(name)
    for name in sorted(available):
        if name not in seen:
            ordered.append(name)
    return ordered


def rewrite_spec_href(url: str, ref: str, spec_files: set[str]) -> str:
    """Rewrite links inside docs/spec/*.md for rendered /docs/spec/ pages."""
    if "://" in url or url.startswith("#") or url.startswith("/"):
        # Also convert GitHub blob links to docs/spec/*.md into internal spec links.
        gh_spec = re.match(
            r"^https://github\.com/SamGalanakis/hirsel/blob/[^/]+/docs/spec/([a-z0-9_-]+\.md)(#.*)?$",
            url,
        )
        if gh_spec and gh_spec.group(1) in spec_files:
            fname = gh_spec.group(1)
            frag = gh_spec.group(2) or ""
            return f"/docs/spec/{frag}" if fname == "index.md" else f"/docs/spec/{fname[:-3]}/{frag}"
        return url

    path_part, hash_sep, frag = url.partition("#")
    anchor = f"#{frag}" if hash_sep else ""

    if path_part == "index.md":
        return f"/docs/spec/{anchor}"
    if path_part in spec_files:
        return f"/docs/spec/{path_part[:-3]}/{anchor}"
    if path_part.startswith("spec/") and path_part[5:] in spec_files:
        fname = path_part[5:]
        return f"/docs/spec/{anchor}" if fname == "index.md" else f"/docs/spec/{fname[:-3]}/{anchor}"
    if path_part.startswith("../../"):
        return f"{GITHUB_REPO}/blob/{ref}/{path_part[6:]}{anchor}"
    if path_part.startswith("../"):
        return f"{GITHUB_REPO}/blob/{ref}/docs/{path_part[3:]}{anchor}"
    return f"{GITHUB_REPO}/blob/{ref}/docs/spec/{path_part}{anchor}"


def build_spec_pages(
    *,
    ref: str,
    parser,
    query,
    docs_groups,
    render_markdown_fn,
    slug_fn,
) -> dict[str, str]:
    filenames = list_spec_files(ref)
    if not filenames:
        return {}

    spec_set = set(filenames)
    raw_docs: dict[str, str] = {}
    for fname in filenames:
        text = read_spec_file(ref, fname)
        if text is not None:
            raw_docs[fname] = text

    index_md = raw_docs.get("index.md", "# Hirsel Specification\n")
    chapter_files = order_chapters(list(raw_docs.keys()), index_md)
    chapters: list[tuple[str, str, str]] = []
    for fname in chapter_files:
        stem = fname[:-3]
        chapters.append((stem, extract_title(raw_docs[fname], stem), fname))

    href_rewriter = lambda url, _ref: rewrite_spec_href(url, ref, spec_set)
    pages: dict[str, str] = {}

    # Render /docs/spec/index.html
    idx_used: set[str] = set()
    idx_lead, idx_body, idx_toc, idx_banner = render_markdown_fn(
        index_md,
        parser=parser,
        query=query,
        ref=ref,
        label="docs/spec/index.md",
        used_ids=idx_used,
        href_rewriter=href_rewriter,
        tolerant_highlight=True,
        slug_fn=slug_fn,
    )
    chapter_cards = "".join(
        f'<a class="spec-card" href="/docs/spec/{html.escape(stem, quote=True)}/">'
        f'<span class="sc-title">{html.escape(title)}</span>'
        f'<span class="sc-file">docs/spec/{html.escape(fname)}</span></a>'
        for stem, title, fname in chapters
    )
    banner_html = f'<div class="release-note">{idx_banner}</div>' if idx_banner else ""
    index_body_html = (
        f"{banner_html}"
        f'<h2 id="chapters">All specification chapters ({len(chapters)})'
        f'<a class="anchor" href="#chapters" aria-label="Link to section">#</a></h2>'
        f'<div class="spec-grid">{chapter_cards}</div>'
        f"{idx_body}"
    )
    first_pager = (
        ("/docs/std/", "Standard library"),
        (f"/docs/spec/{chapters[0][0]}/", chapters[0][1]) if chapters else None,
    )
    pages["docs/spec/index.html"] = render_page(
        slug="/docs/spec/",
        title="Hirsel Specification Reference",
        description=f"Normative Hirsel specification across {len(chapters)} chapters: syntax, ownership, memory, budgets, capabilities, CLI, packages, and protocols.",
        eyebrow=f"Spec reference · {len(chapters)} chapters" + (f" · {ref}" if ref != "main" else ""),
        heading="Specification reference",
        lead=idx_lead or "The normative specification for Hirsel, its core language, runtime, CLI, packages, and protocols.",
        body_html=index_body_html,
        nav_key="spec",
        sidebar_html=render_sidebar(
            docs_groups,
            active_page="spec",
            page_toc=[("chapters", f"All chapters ({len(chapters)})"), *idx_toc],
            ref=ref,
            spec_chapters=[(s, t) for s, t, _ in chapters],
            active_spec=None,
        ),
        pager=first_pager,
        ref=ref,
        markdown_alternates=[],
    )

    # Render each /docs/spec/<stem>/index.html
    for pos, (stem, title, fname) in enumerate(chapters):
        md_text = raw_docs[fname]
        used_ids: set[str] = {slug_fn(title)}
        lead_html, body_html, toc, banner = render_markdown_fn(
            md_text,
            parser=parser,
            query=query,
            ref=ref,
            label=f"docs/spec/{fname}",
            used_ids=used_ids,
            href_rewriter=href_rewriter,
            tolerant_highlight=True,
            slug_fn=slug_fn,
        )
        source_url = f"{GITHUB_REPO}/blob/{ref}/docs/spec/{fname}"
        source_note = (
            f'<p class="spec-source">Source: <a href="{html.escape(source_url, quote=True)}">'
            f"<code>docs/spec/{html.escape(fname)}</code></a></p>"
        )
        full_body = (f'<div class="release-note">{banner}</div>' if banner else "") + source_note + body_html
        prev_p = ("/docs/spec/", "Spec overview") if pos == 0 else (f"/docs/spec/{chapters[pos - 1][0]}/", chapters[pos - 1][1])
        next_p = (f"/docs/spec/{chapters[pos + 1][0]}/", chapters[pos + 1][1]) if pos + 1 < len(chapters) else None
        pages[f"docs/spec/{stem}/index.html"] = render_page(
            slug=f"/docs/spec/{stem}/",
            title=f"{title} — Hirsel Specification",
            description=f"Hirsel specification chapter: {title} (docs/spec/{fname}).",
            eyebrow=f"Spec · docs/spec/{fname}" + (f" · {ref}" if ref != "main" else ""),
            heading=title,
            heading_id=slug_fn(title),
            lead=lead_html or f"Normative specification chapter <code>docs/spec/{html.escape(fname)}</code>.",
            body_html=full_body,
            nav_key="spec",
            sidebar_html=render_sidebar(
                docs_groups,
                active_page="spec",
                page_toc=toc,
                ref=ref,
                spec_chapters=[(s, t) for s, t, _ in chapters],
                active_spec=stem,
            ),
            pager=(prev_p, next_p),
            ref=ref,
            markdown_alternates=[],
        )

    return pages
