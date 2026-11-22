#!/usr/bin/env python3
"""Copy docs/PRIMER.md and the std operation index to the site and the skill.

docs/PRIMER.md is the one source. This writes site/primer.md, site/api-index.md,
site/llms.txt, skills/hirsel/references/PRIMER.md and the Markdown alternate link in
site/index.html. --write owns those outputs; --check compares every byte and exits 1
when any is stale. Neither runs hirsel: regenerate core/std/API-INDEX.md first with
core/tools/std_api_index.py --write.
"""

import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
PRIMER = ROOT / "docs" / "PRIMER.md"
INDEX = ROOT / "core" / "std" / "API-INDEX.md"
GITHUB_STD = b"https://github.com/samgalanakis/hirsel/blob/main/core/std/"
HTML = ROOT / "site" / "index.html"
ALTERNATE = '  <link rel="alternate" type="text/markdown" href="/primer.md">\n'
ANCHOR = '  <link rel="canonical" href="https://hirsel.ai/">\n'
LLMS = """# Hirsel

> Hirsel is a native systems language for coding agents, with explicit capabilities and byte budgets.

The primer is the whole language in one page; with the installed `hirsel` binary it is
enough to write, check and test working programs.

## Docs

- [Primer](https://hirsel.ai/primer.md): syntax, ownership, capabilities and the command-line loop
- [Standard library index](https://hirsel.ai/api-index.md): every std module and its operations
- [Install script](https://hirsel.ai/install.sh): Linux x86_64 release installer
"""


def html(current: str) -> str:
    if ALTERNATE in current:
        return current
    if current.count(ANCHOR) != 1:
        raise ValueError("site/index.html: expected one canonical link to place the alternate after")
    return current.replace(ANCHOR, ANCHOR + ALTERNATE)


def outputs() -> dict:
    primer = PRIMER.read_bytes()
    index = INDEX.read_bytes().replace(b"](api/", b"](" + GITHUB_STD + b"api/")
    index = index.replace(b"](API.md)", b"](" + GITHUB_STD + b"API.md)")
    return {
        ROOT / "site" / "primer.md": primer,
        ROOT / "skills" / "hirsel" / "references" / "PRIMER.md": primer,
        ROOT / "site" / "api-index.md": index,
        ROOT / "site" / "llms.txt": LLMS.encode(),
        HTML: html(HTML.read_text()).encode(),
    }


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    if mode not in ("--write", "--check"):
        sys.stderr.write("usage: tools/site_sync.py --write | --check\n")
        return 2
    try:
        want = outputs()
    except (ValueError, OSError) as error:
        sys.stderr.write(str(error) + "\n")
        return 1
    stale = [path for path, data in want.items()
             if not path.is_file() or path.read_bytes() != data]
    if mode == "--write":
        for path in stale:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(want[path])
        return 0
    for path in stale:
        sys.stderr.write(f"{path.relative_to(ROOT)}: stale copy of the primer or std index; "
                         "run python3 tools/site_sync.py --write\n")
    return 1 if stale else 0


if __name__ == "__main__":
    sys.exit(main())
