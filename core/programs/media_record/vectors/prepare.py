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

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tools/toolchain-dist"))
import hcfg


def closure(programs, name, seen):
    if name in seen:
        return
    seen.add(name)
    manifest = hcfg.read(programs / name / "hirsel.hcfg", "pkg_v1.Package")
    for _, binding in manifest["dependencies"]:
        if binding["case"] == "Path":
            closure(programs, (programs / name / binding["fields"]["path"]).resolve().relative_to(programs).as_posix(), seen)


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
    hcfg.workspace(work, members)
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
    hcfg.vector_package(package, vectors, target, out)


if __name__ == "__main__":
    main()
