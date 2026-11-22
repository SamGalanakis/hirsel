#!/usr/bin/env python3
"""Build the deployed site: site/ copied as is, with index.html's inline script and styles minified.

The source stays readable; only the Pages artifact is minified. Every file but index.html is
copied byte for byte. Usage: tools/site_build.py <out-dir>
"""
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ESBUILD = ["npx", "--yes", "esbuild@0.28.2"]
SITE = Path(__file__).resolve().parent.parent / "site"


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


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: site_build.py <out-dir>")
    out = Path(sys.argv[1])
    if out.exists():
        shutil.rmtree(out)
    shutil.copytree(SITE, out)
    html = (SITE / "index.html").read_text(encoding="utf-8")
    if len(re.findall(r"<script\b", html)) != len(re.findall(r"<script>", html)):
        sys.exit("site_build: index.html has a <script> with attributes; teach site_build about it")
    html = splice(html, "script", "js")
    html = splice(html, "style", "css")
    for m in re.finditer(r"<script>(.*?)</script>", html, re.S):
        with tempfile.NamedTemporaryFile("w", suffix=".js", encoding="utf-8") as f:
            f.write(m.group(1))
            f.flush()
            subprocess.run(["node", "--check", f.name], check=True)
    (out / "index.html").write_text(html, encoding="utf-8")


if __name__ == "__main__":
    main()
