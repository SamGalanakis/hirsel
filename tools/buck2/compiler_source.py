"""The driver build script's inputs: what compiler_source.rs hashes.

crates/hirsel-core-driver/src/compiler_source.rs names the compiler's source:
its PINS, its EXTRA directories and its crate closure (path dependencies,
normal and build), each without its SKIPPED directories. The build script
hashes them, so Buck must lay out exactly those files.
"""

from __future__ import annotations

import pathlib
import re

SOURCE = "crates/hirsel-core-driver/src/compiler_source.rs"
# The build script's own sources, and the library sources it bundles.
SOURCES = ["crates/hirsel-core-driver/build.rs", SOURCE]
BUNDLED = ["core/core/**", "core/std/**/*.hirsel", "core/programs/store/*.hirsel", "core/programs/runner/*.hirsel",
           "core/programs/store/hirsel.toml", "core/programs/runner/hirsel.toml", "core/programs/runner/frame-schema.bin"]


def lists(reads, root: pathlib.Path) -> tuple[list[str], list[str], list[str]]:
    """compiler_source.rs's PINS, EXTRA and SKIPPED string arrays."""
    text = reads.text(root / SOURCE)

    def strings(name: str) -> list[str]:
        body = re.search(rf"const {name}: \[&str; \d+\] = \[(.*?)\];", text, re.S)
        assert body, f"{SOURCE} has no {name}"
        return re.findall(r'"([^"\\\n]+)"', body.group(1))
    return strings("PINS"), strings("EXTRA"), strings("SKIPPED")


def closure(root: pathlib.Path, start: str, nodes: dict, packages: dict, members) -> list[str]:
    """The workspace crates `start` compiles from, as compiler_source::closure
    finds them, as repository paths."""
    seen, pending = set(), [start]
    while pending:
        member = pending.pop()
        if member in seen:
            continue
        seen.add(member)
        pending.extend(dep["pkg"] for dep in nodes[member]["deps"]
                       if dep["pkg"] in members and any(k["kind"] in (None, "build") for k in dep["dep_kinds"]))
    return sorted(str(pathlib.Path(packages[m]["manifest_path"]).parent.relative_to(root)) for m in seen)


def build_script(reads, root: pathlib.Path, start: str, nodes: dict, packages: dict, members):
    """The build script's sources, input globs, their excludes, and its
    input_paths: a root's glob stops at a Buck package under it, which exports
    its files as `compiler-source`."""
    pins, extra, skipped = lists(reads, root)
    roots = closure(root, start, nodes, packages, members) + extra
    globs = BUNDLED + pins + [f"{r}/**" for r in roots]
    exclude = ["core/std/tests/**"] + [f"{r}/{skip}/**" for r in roots for skip in skipped]
    paths = {}
    for r in roots:
        for package in reads.glob(root / r, "**/BUCK"):
            directory = str(package.parent.relative_to(root))
            paths[directory] = f"//{directory}:compiler-source"
    return SOURCES, globs, exclude, paths
