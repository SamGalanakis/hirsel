#!/usr/bin/env python3
"""Sync the primer, cookbook, std operation index, agent discovery files, and sitemap.

This writes:
- site/primer.md
- site/cookbook.md
- site/api-index.md
- site/llms.txt
- site/llms-full.txt
- site/sitemap.xml
- skills/hirsel/references/PRIMER.md
- the Markdown alternate link in site/index.html

Modes:
  --write                 Update all generated files in the repo checkout.
  --check                 Compare every generated file byte-for-byte; exit 1 if stale.
  --release-tag TAG --out-dir DIR
                          Emit release-aligned primer.md, cookbook.md, api-index.md,
                          llms.txt, llms-full.txt, and sitemap.xml into DIR using
                          TAG's docs/PRIMER.md and core/std/API-INDEX.md.

Neither mode runs hirsel: regenerate core/std/API-INDEX.md first with
core/tools/std_api_index.py --write.
"""

import argparse
import pathlib
import re
import subprocess
import sys

from site_spec import list_spec_files

ROOT = pathlib.Path(__file__).resolve().parents[1]
PRIMER = ROOT / "docs" / "PRIMER.md"
COOKBOOK = ROOT / "docs" / "COOKBOOK.md"
INDEX = ROOT / "core" / "std" / "API-INDEX.md"
SKILL = ROOT / "skills" / "hirsel" / "SKILL.md"
README = ROOT / "README.md"
HTML = ROOT / "site" / "index.html"

GITHUB = "https://github.com/SamGalanakis/hirsel/"
ALTERNATE = '  <link rel="alternate" type="text/markdown" href="/llms.txt">\n'
OLD_ALTERNATE = '  <link rel="alternate" type="text/markdown" href="/primer.md">\n'
ANCHOR = '  <link rel="canonical" href="https://hirsel.ai/">\n'
SITEMAP_LASTMOD = "2026-11-25"

SITEMAP_ENTRIES = (
    ("https://hirsel.ai/", "weekly", "1.0"),
    ("https://hirsel.ai/benchmarks/", "weekly", "0.9"),
    ("https://hirsel.ai/docs/", "weekly", "0.95"),
    ("https://hirsel.ai/docs/std/", "weekly", "0.85"),
    ("https://hirsel.ai/docs/spec/", "weekly", "0.85"),
    ("https://hirsel.ai/llms.txt", "weekly", "0.5"),
    ("https://hirsel.ai/llms-full.txt", "weekly", "0.5"),
)


def llms_header(ref: str) -> str:
    tag_note = (
        f"Published web docs align with release `{ref}`."
        if ref != "main"
        else "Repository docs track `main`."
    )
    return f"""# Hirsel

> Hirsel is a zero-GC native systems programming language with explicit capabilities, bounded memory and steps, deterministic runtime execution, and recorded replay.

**Authority rule:** Always run `hirsel primer` from your installed binary as the exact authority for your installed version. Use `hirsel explain RULE` for any diagnostic rule ID and `hirsel inspect --core --interface` / `hirsel inspect --std --interface` for exact function signatures. {tag_note}

## Quick start

```sh
curl -fsSL https://hirsel.ai/install.sh | sh   # Linux x86_64 (glibc 2.28+); Apple Silicon macOS in progress
hirsel new app && cd app
hirsel check
hirsel test
hirsel run
```

## Docs

- [Primer](https://hirsel.ai/primer.md): whole language in one page — syntax, ownership, capabilities, budgets, and the CLI loop
- [Cookbook](https://hirsel.ai/cookbook.md): tested task-shaped recipes (CLI, files, HTTP router, Store, tasks/units, `sim` testing & laws, `Inline`/`Simd`)
- [Standard library index](https://hirsel.ai/api-index.md): every `std` module and its operations
- [Specification reference](https://hirsel.ai/docs/spec/): normative chapters for syntax, ownership, memory, budgets, capabilities, CLI, packages, and protocols
- [Benchmarks](https://hirsel.ai/benchmarks/): goal-board results, losing rows, and methodology
- [Full agent bundle (llms-full.txt)](https://hirsel.ai/llms-full.txt): single-file bundle containing intro, primer, cookbook, std API index, and agent skill
- [Agent skill (SKILL.md)]({GITHUB}blob/{ref}/skills/hirsel/SKILL.md): iterative check/explain/inspect workflow for coding agents
- [Install script](https://hirsel.ai/install.sh): Linux x86_64 release installer
"""


def git_read(ref: str, rel_path: str) -> bytes | None:
    proc = subprocess.run(
        ["git", "show", f"{ref}:{rel_path}"],
        cwd=ROOT,
        capture_output=True,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout


def rewrite_github_links(text: str, ref: str) -> str:
    def repl(match: re.Match[str]) -> str:
        target = match[1]
        if "://" in target or target.startswith("#"):
            return match[0]
        path = target.split("#", 1)[0]
        kind = "tree" if path.endswith("/") else "blob"
        return f"]({GITHUB}{kind}/{ref}/{target})"

    return re.sub(r"\]\(([^)]+)\)", repl, text)


def rewrite_primer_links(primer: bytes, ref: str) -> bytes:
    spec_prefix = f"{GITHUB}blob/{ref}/docs/spec/".encode()
    primer = primer.replace(b"](spec/", b"](" + spec_prefix)
    if ref != "main":
        primer = primer.replace(
            f"{GITHUB}blob/main/docs/spec/".encode(),
            spec_prefix,
        )
    return primer


def stamp_release_banner(doc: bytes, banner: str) -> bytes:
    text = doc.decode("utf-8")
    lines = text.splitlines(keepends=True)
    if lines and lines[0].startswith("# "):
        return (lines[0] + "\n" + banner + "\n" + "".join(lines[1:])).encode("utf-8")
    return (banner + "\n\n" + text).encode("utf-8")


def rewrite_cookbook_links(cookbook: bytes, ref: str) -> bytes:
    text = cookbook.decode("utf-8")
    text = text.replace("](PRIMER.md)", "](https://hirsel.ai/primer.md)")
    text = text.replace("](spec/", f"]({GITHUB}blob/{ref}/docs/spec/")
    return text.encode("utf-8")


def rewrite_index_links(index: bytes, ref: str) -> bytes:
    std_prefix = f"{GITHUB}blob/{ref}/core/std/".encode()
    index = index.replace(b"](api/", b"](" + std_prefix + b"api/")
    return index.replace(b"](API.md)", b"](" + std_prefix + b"API.md)")


def readme_catalog(readme_text: str, ref: str) -> str:
    marker = "\n## Toolchain packages\n"
    if marker not in readme_text:
        return ""
    start = readme_text.index(marker)
    end = readme_text.index("\n## License\n", start)
    return rewrite_github_links(readme_text[start:end], ref)


def render_sitemap(ref: str = "main", lastmod: str = SITEMAP_LASTMOD) -> bytes:
    entries = list(SITEMAP_ENTRIES)
    for fname in list_spec_files(ref):
        if fname != "index.md" and fname.endswith(".md"):
            entries.append((f"https://hirsel.ai/docs/spec/{fname[:-3]}/", "weekly", "0.75"))
    rows = []
    for loc, freq, priority in entries:
        rows.append(
            "  <url>\n"
            f"    <loc>{loc}</loc>\n"
            f"    <lastmod>{lastmod}</lastmod>\n"
            f"    <changefreq>{freq}</changefreq>\n"
            f"    <priority>{priority}</priority>\n"
            "  </url>"
        )
    body = (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
        + "\n".join(rows)
        + "\n</urlset>\n"
    )
    return body.encode("utf-8")


def html(current: str) -> str:
    current = current.replace(OLD_ALTERNATE, "")
    if ALTERNATE in current:
        return current
    if current.count(ANCHOR) != 1:
        raise ValueError("site/index.html: expected one canonical link to place the alternate after")
    return current.replace(ANCHOR, ANCHOR + ALTERNATE)


def build_site_docs(ref: str = "main", lastmod: str = SITEMAP_LASTMOD) -> dict[str, bytes]:
    if ref == "main":
        primer_raw = PRIMER.read_bytes()
        cookbook_raw = COOKBOOK.read_bytes()
        index_raw = INDEX.read_bytes()
        skill_raw = SKILL.read_bytes()
        readme_text = README.read_text(encoding="utf-8")
        cookbook_ref = "main"
    else:
        primer_raw = git_read(ref, "docs/PRIMER.md")
        if primer_raw is None:
            raise ValueError(f"git ref {ref!r} has no docs/PRIMER.md")
        index_raw = git_read(ref, "core/std/API-INDEX.md")
        if index_raw is None:
            raise ValueError(f"git ref {ref!r} has no core/std/API-INDEX.md")
        primer_raw = stamp_release_banner(
            primer_raw,
            f"> Documentation for Hirsel `{ref}`. Always run `hirsel primer` from your installed binary as the exact authority for your installed version.",
        )
        cookbook_at_ref = git_read(ref, "docs/COOKBOOK.md")
        if cookbook_at_ref is not None:
            cookbook_raw = cookbook_at_ref
            cookbook_ref = ref
        else:
            cookbook_raw = stamp_release_banner(
                COOKBOOK.read_bytes(),
                f"> Note: this cookbook tracks `main`. Always run `hirsel primer` and `hirsel inspect --std --interface` from your installed `{ref}` binary for exact signatures.",
            )
            cookbook_ref = "main"
        skill_raw = git_read(ref, "skills/hirsel/SKILL.md")
        if skill_raw is None:
            raise ValueError(f"git ref {ref!r} has no skills/hirsel/SKILL.md")
        readme_raw = git_read(ref, "README.md")
        if readme_raw is None:
            raise ValueError(f"git ref {ref!r} has no README.md")
        readme_text = readme_raw.decode("utf-8")

    primer = rewrite_primer_links(primer_raw, ref)
    cookbook = rewrite_cookbook_links(cookbook_raw, cookbook_ref)
    index = rewrite_index_links(index_raw, ref)
    llms = (llms_header(ref) + readme_catalog(readme_text, ref)).encode("utf-8")

    skill_text = skill_raw.decode("utf-8").strip()
    llms_full = (
        llms.decode("utf-8").rstrip()
        + "\n\n---\n\n"
        + primer.decode("utf-8").rstrip()
        + "\n\n---\n\n"
        + cookbook.decode("utf-8").rstrip()
        + "\n\n---\n\n"
        + index.decode("utf-8").rstrip()
        + "\n\n---\n\n# Agent Skill (skills/hirsel/SKILL.md)\n\n"
        + skill_text
        + "\n"
    ).encode("utf-8")

    return {
        "primer.md": primer,
        "cookbook.md": cookbook,
        "api-index.md": index,
        "llms.txt": llms,
        "llms-full.txt": llms_full,
        "sitemap.xml": render_sitemap(ref=ref, lastmod=lastmod),
    }


def outputs() -> dict[pathlib.Path, bytes]:
    docs = build_site_docs(ref="main")
    primer_repo = PRIMER.read_bytes()
    out = {ROOT / "site" / name: data for name, data in docs.items()}
    out[ROOT / "skills" / "hirsel" / "references" / "PRIMER.md"] = primer_repo
    out[HTML] = html(HTML.read_text(encoding="utf-8")).encode("utf-8")
    return out


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--write", action="store_true")
    group.add_argument("--check", action="store_true")
    group.add_argument("--release-tag", metavar="TAG")
    parser.add_argument("--out-dir", type=pathlib.Path, metavar="DIR")
    parser.add_argument("--lastmod", default=SITEMAP_LASTMOD)
    args = parser.parse_args(argv)
    if args.release_tag and not args.out_dir:
        parser.error("--release-tag requires --out-dir")
    return args


def main() -> int:
    args = parse_args(sys.argv[1:])
    try:
        if args.release_tag:
            docs = build_site_docs(ref=args.release_tag, lastmod=args.lastmod)
            args.out_dir.mkdir(parents=True, exist_ok=True)
            for name, data in docs.items():
                (args.out_dir / name).write_bytes(data)
            return 0
        want = outputs()
    except (ValueError, OSError) as error:
        sys.stderr.write(str(error) + "\n")
        return 1
    stale = [
        path
        for path, data in want.items()
        if not path.is_file() or path.read_bytes() != data
    ]
    if args.write:
        for path in stale:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(want[path])
        return 0
    for path in stale:
        sys.stderr.write(
            f"{path.relative_to(ROOT)}: stale generated content; "
            "run python3 tools/site_sync.py --write\n"
        )
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
