#!/usr/bin/env python3
"""The operation index of core/std/API.md, generated from core/std/*.hirsel.

A module extends core when core/core has a file of the same name (LANG-7). An operation is a
top-level `pub fn`. With --write the table in API.md is replaced; with --check the script exits 1
when API.md's table differs from the files.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
STD = ROOT / "core" / "std"
CORE = ROOT / "core" / "core"
API = STD / "API.md"
HEADER = "| Module | Extends | Ops | Operations |"
PUB_FN = re.compile(r"^pub fn ([a-z_][a-z0-9_]*)", re.M)


def table() -> str:
    core = {p.stem for p in CORE.glob("*.hirsel")}
    rows = [HEADER, "|---|---|---|---|"]
    extends = total = modules = 0
    for path in sorted(STD.glob("*.hirsel")):
        ops = PUB_FN.findall(path.read_text())
        ext = path.stem in core
        extends += ext
        modules += 1
        total += len(ops)
        rows.append(f"| {path.stem} | {'yes' if ext else ''} | {len(ops)} | {', '.join(ops)} |")
    rows.append(f"| **total** | **{extends} of {modules}** | **{total}** | |")
    return "\n".join(rows) + "\n"


def split(text: str) -> tuple[str, str, str]:
    start = text.index(HEADER)
    end = text.index("\n", text.index("| **total** |", start)) + 1
    return text[:start], text[start:end], text[end:]


def main() -> int:
    mode = sys.argv[1] if len(sys.argv) > 1 else ""
    want = table()
    before, have, after = split(API.read_text())
    if mode == "--write":
        API.write_text(before + want + after)
        return 0
    if mode == "--check":
        if have != want:
            sys.stderr.write("core/std/API.md: operation index is stale; run core/tools/std_api_index.py --write\n")
            return 1
        return 0
    sys.stdout.write(want)
    return 0


if __name__ == "__main__":
    sys.exit(main())
