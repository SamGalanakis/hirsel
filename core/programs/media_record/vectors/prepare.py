"""Prepare the media_record vector workspace.

Usage: prepare.py WORK

Builds WORK/ as its own Hirsel workspace (a path dependency must be a
workspace member): media_record and the packages it depends on, copied
from core/programs, with vectors/dump.hirsel and the law rig
(drive.hirsel, frames.hirsel) as media_record's only tests, writing to
WORK/out.
"""

import json
from pathlib import Path
import re
import shutil
import sys


def closure(programs, name, seen):
    if name in seen:
        return
    seen.add(name)
    manifest = (programs / name / "hirsel.toml").read_text()
    for dep in re.findall(r'path = "\.\./([A-Za-z0-9_]+)"', manifest):
        closure(programs, dep, seen)


def main():
    work = Path(sys.argv[1]).resolve()
    vectors = Path(__file__).resolve().parent
    package = vectors.parent
    programs = package.parent
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    members = set()
    closure(programs, "media_record", members)
    members = sorted(members)
    (work / "hirsel.toml").write_text("[workspace]\nmembers = " + json.dumps(members) + "\n")
    for name in members:
        if name != "media_record":
            shutil.copytree(programs / name, work / name, ignore=shutil.ignore_patterns("hirsel.lock", "target", "buck-out"))
    target = work / "media_record"
    target.mkdir()
    for source in package.glob("*.hirsel"):
        if source.name != "laws.hirsel":
            shutil.copyfile(source, target / source.name)
    shutil.copyfile(vectors / "dump.hirsel", target / "dump.hirsel")
    out = work / "out"
    out.mkdir()
    base = (package / "hirsel.toml").read_text().split("\n[tests]")[0].rstrip("\n") + "\n"
    tests = (vectors / "test.toml").read_text().replace('"out"', json.dumps(str(out)))
    (target / "hirsel.toml").write_text(base + tests)


if __name__ == "__main__":
    main()
