#!/usr/bin/env python3
"""Verify internal HTML links and release-tag GitHub links in a built site directory.

Checks:
1. Every `https://github.com/SamGalanakis/hirsel/(blob|tree)/<ref>/<path>` URL in
   `llms.txt` and `llms-full.txt` exists in git at `<ref>`.
2. Every internal `href` in every `.html` file under `--site-dir` resolves to an
   existing file (or `<dir>/index.html`) inside `--site-dir`, and any `#fragment`
   exists on the target page (except interactive walk views on `/`).

Usage:
  python3 tools/site_check_links.py --site-dir _site [--release-tag TAG]
"""

from __future__ import annotations

import argparse
import html.parser
import posixpath
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GH_LINK_RE = re.compile(
    r"https://github\.com/SamGalanakis/hirsel/(blob|tree)/([^/\s)\"'>]+)/([^\s)\"'>#]+)"
)
# Interactive JS walk view hashes on site/index.html that are handled by script.
INDEX_WALK_HASHES = {
    "hill",
    "safe",
    "parallel",
    "memory",
    "bounded",
    "deterministic",
    "authority",
    "local",
    "fast",
    "meaning",
}


class HtmlLinkCollector(html.parser.HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.hrefs: list[str] = []
        self.ids: set[str] = set()

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attr_map = dict(attrs)
        if eid := attr_map.get("id"):
            self.ids.add(eid)
        if tag in ("a", "link") and (href := attr_map.get("href")):
            self.hrefs.append(href)


def git_path_exists(ref: str, rel_path: str) -> bool:
    clean = rel_path.rstrip("/")
    if not clean:
        return True
    proc = subprocess.run(
        ["git", "cat-file", "-e", f"{ref}:{clean}"],
        cwd=ROOT,
        capture_output=True,
    )
    return proc.returncode == 0


def check_llms_github_links(site_dir: Path, release_tag: str | None) -> tuple[int, list[str]]:
    errors: list[str] = []
    checked: dict[tuple[str, str], bool] = {}
    total = 0

    for name in ("llms.txt", "llms-full.txt"):
        path = site_dir / name
        if not path.is_file():
            errors.append(f"{name}: missing file")
            continue
        text = path.read_text(encoding="utf-8")
        for m in GH_LINK_RE.finditer(text):
            kind, ref, rel_path = m.group(1), m.group(2), m.group(3).rstrip(".,;:")
            if release_tag and ref != release_tag and ref != "main":
                continue
            target_ref = release_tag if (release_tag and ref == release_tag) else ref
            total += 1
            key = (target_ref, rel_path)
            if key not in checked:
                if target_ref == "main" and release_tag is None:
                    checked[key] = (ROOT / rel_path.rstrip("/")).exists()
                else:
                    checked[key] = git_path_exists(target_ref, rel_path)
            if not checked[key]:
                errors.append(
                    f"{name}: dead GitHub {kind} link at {target_ref}: {rel_path}"
                )
    return total, sorted(set(errors))


def resolve_site_file(site_dir: Path, current_rel: Path, target_path: str) -> Path | None:
    if target_path.startswith("/"):
        norm = posixpath.normpath(target_path).lstrip("/")
    else:
        base_dir = current_rel.parent.as_posix()
        joined = posixpath.join("" if base_dir == "." else base_dir, target_path)
        norm = posixpath.normpath(joined).lstrip("/")

    if not norm or norm == ".":
        cand = site_dir / "index.html"
        return cand if cand.is_file() else None

    direct = site_dir / norm
    if direct.is_file():
        return direct
    if direct.is_dir() and (direct / "index.html").is_file():
        return direct / "index.html"
    return None


def check_internal_html_links(site_dir: Path) -> tuple[int, list[str]]:
    html_files = sorted(site_dir.rglob("*.html"))
    parsed: dict[Path, HtmlLinkCollector] = {}
    for p in html_files:
        col = HtmlLinkCollector()
        col.feed(p.read_text(encoding="utf-8"))
        parsed[p] = col

    errors: list[str] = []
    total = 0
    for p, col in parsed.items():
        rel = p.relative_to(site_dir)
        for href in col.hrefs:
            if href.startswith(("http://", "https://", "mailto:", "javascript:", "data:")):
                continue
            total += 1
            path_part, has_frag, frag = href.partition("#")
            path_part = path_part.split("?", 1)[0]
            if not path_part:
                target = p
            else:
                target = resolve_site_file(site_dir, rel, path_part)
                if target is None:
                    errors.append(f"{rel.as_posix()}: dead internal link {href!r}")
                    continue
            if has_frag and frag and target in parsed:
                target_rel = target.relative_to(site_dir).as_posix()
                if frag not in parsed[target].ids:
                    if target_rel == "index.html" and frag in INDEX_WALK_HASHES:
                        continue
                    errors.append(
                        f"{rel.as_posix()}: missing anchor #{frag} in {target_rel} (from {href!r})"
                    )
    return total, sorted(set(errors))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--site-dir", type=Path, required=True, metavar="DIR")
    parser.add_argument("--release-tag", metavar="TAG", default=None)
    args = parser.parse_args()

    gh_count, gh_errors = check_llms_github_links(args.site_dir, args.release_tag)
    html_count, html_errors = check_internal_html_links(args.site_dir)
    all_errors = [*gh_errors, *html_errors]
    if all_errors:
        for err in all_errors:
            sys.stderr.write(f"link-check: {err}\n")
        sys.stderr.write(
            f"link-check: {len(all_errors)} broken link(s) "
            f"(checked {gh_count} GitHub links and {html_count} internal HTML links)\n"
        )
        return 1
    print(
        f"link-check: ok ({gh_count} GitHub links, {html_count} internal HTML links"
        + (f", ref={args.release_tag}" if args.release_tag else "")
        + ")"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
