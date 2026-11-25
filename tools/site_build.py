#!/usr/bin/env python3
"""Build the deployed site: site/ copied as is, with index.html's inline script, styles,
and JSON-LD minified.

The source stays readable; only the deployed copy is minified. Every file but index.html is
copied byte for byte (and when --release-tag TAG is passed, primer.md, cookbook.md,
api-index.md, llms.txt, llms-full.txt, and sitemap.xml are regenerated aligned to TAG).

Usage: tools/site_build.py <out-dir> [--release-tag TAG]
"""
import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ESBUILD = ["npx", "--yes", "esbuild@0.28.2"]
TOOLS = Path(__file__).resolve().parent
SITE = TOOLS.parent / "site"
JSONLD_TAG = '<script type="application/ld+json">'


def esbuild(code: str, loader: str) -> str:
    out = subprocess.run(
        [*ESBUILD, f"--loader={loader}", "--minify", "--charset=utf8"],
        input=code.encode(), capture_output=True, check=True,
    ).stdout.decode().rstrip("\n")
    if not out.strip():
        sys.exit(f"site_build: esbuild returned nothing for a {loader} block")
    return out


def splice(html: str, tag: str, loader: str) -> str:
    """Minify the body of every bare <tag>...</tag>, splicing by index (no regex replacement)."""
    parts, at = [], 0
    for m in re.finditer(rf"<{tag}>(.*?)</{tag}>", html, re.S):
        body = esbuild(m.group(1), loader)
        if f"</{tag}" in body.lower():
            sys.exit(f"site_build: minified {loader} contains </{tag}")
        parts += [html[at:m.start(1)], body]
        at = m.end(1)
    return "".join(parts) + html[at:]


def splice_jsonld(html: str) -> str:
    """Validate and minify every <script type="application/ld+json">...</script> block."""
    parts, at = [], 0
    pattern = re.compile(re.escape(JSONLD_TAG) + r"(.*?)</script>", re.S)
    for m in pattern.finditer(html):
        parsed = json.loads(m.group(1))
        body = json.dumps(parsed, separators=(",", ":"), ensure_ascii=False)
        if "</script" in body.lower():
            sys.exit("site_build: minified JSON-LD contains </script")
        parts += [html[at:m.start(1)], body]
        at = m.end(1)
    return "".join(parts) + html[at:]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("out_dir", type=Path)
    parser.add_argument("--release-tag", metavar="TAG",
                        help="Emit release-aligned docs for TAG into out_dir")
    args = parser.parse_args()
    out = args.out_dir
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(SITE, out)
    html = (SITE / "index.html").read_text(encoding="utf-8")
    all_scripts = len(re.findall(r"<script\b", html))
    bare_scripts = len(re.findall(r"<script>", html))
    jsonld_scripts = html.count(JSONLD_TAG)
    if all_scripts != bare_scripts + jsonld_scripts:
        sys.exit("site_build: index.html has an unsupported <script> with attributes; teach site_build about it")
    html = splice_jsonld(html)
    html = splice(html, "script", "js")
    html = splice(html, "style", "css")
    for m in re.finditer(r"<script>(.*?)</script>", html, re.S):
        with tempfile.NamedTemporaryFile("w", suffix=".js", encoding="utf-8") as f:
            f.write(m.group(1))
            f.flush()
            subprocess.run(["node", "--check", f.name], check=True)
    (out / "index.html").write_text(html, encoding="utf-8")
    if args.release_tag:
        subprocess.run(
            [sys.executable, str(TOOLS / "site_sync.py"),
             "--release-tag", args.release_tag, "--out-dir", str(out)],
            check=True,
        )


if __name__ == "__main__":
    main()
