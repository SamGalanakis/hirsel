#!/usr/bin/env python3
"""Materialize the pinned LLVM 23 tree as an ignored Buck2 toolchain input.

`tools/native-toolchain/install --unpack` verifies every locked `.deb` against
`tools/native-toolchain/llvm-23.lock`, unpacks it and runs the pin's `check`.
This script then lays the result out as one self-contained tree with no
symbolic links (Buck2 hashes a link, not its target):

    bin/                 every LLVM, clang and lld executable (RUNPATH $ORIGIN/../lib),
                         except ld.lld: the locked static PIE (`static_lld`),
                         and hirsel-clang, hirsel-clang++, hirsel-llvm-ar: wrappers
                         that locate their tools without PATH
    lib/                 libLLVM, libclang-cpp, the clang resource directory, and
                         the libraries they load that a minimal worker image lacks
                         (llvm-runtime-lock.json), with their own relative RUNPATH
    engine/              libLLVM.so.23.1: the compiler's own LLVM (`engine_llvm`, built
                         by build-libllvm), which first-party binaries load first
                         (tools/buck2/defs.bzl LLVM_RPATH); clang and the tools keep lib/'s
    include/llvm{,-c}/   the LLVM headers

The static LLVM component archives are left out: llvm-sys links libLLVM
dynamically (`force-dynamic`). Remote workers receive the tree as an action
input, so they need no LLVM installed.

Every file of the tree is this checkout's own inode (bootstrap_store `private`):
the compiler trusts these files by stat, so no other checkout may share one, and
a tree that does is stale.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import urllib.request

import bootstrap_store


ROOT = pathlib.Path(__file__).resolve().parents[2]
LOCK = ROOT / "tools/native-toolchain/llvm-23.lock"
CXX_LOCK = pathlib.Path(__file__).with_name("llvm-cxx-headers-lock.json")
RUNTIME_LOCK = pathlib.Path(__file__).with_name("llvm-runtime-lock.json")
INSTALL = ROOT / "tools/native-toolchain/install"
DEFAULT_OUTPUT = pathlib.Path(__file__).with_name("toolchains") / "llvm-files"
RECEIPT = ".hirsel-llvm.json"
LAYOUT = 9
REQUIRED = (
    "sysroot/usr/include/c++/13/vector",
    "sysroot/usr/include/features.h",
    "bin/clang",
    "bin/clang-23",
    "bin/llvm-config",
    "bin/ld.lld",
    "lib/libLLVM.so.23.1",
    "lib/libLLVM-23.so",
    "engine/libLLVM.so.23.1",
    "lib/libclang-cpp.so.23.1",
    "lib/clang/23/include/stddef.h",
    "include/llvm-c/Core.h",
    "lib/libedit.so.2",
    "lib/libxml2.so.2",
    "lib/libz3.so.4",
    "lib/libcurl.so.4",
    "bin/hirsel-clang",
    "bin/hirsel-clang++",
    "bin/hirsel-llvm-ar",
)

WRAPPER = """#!/bin/sh
tree="$(cd "${0%%/*}/.." && pwd)"
exec "$tree/bin/%s" "$@"
"""


def identity() -> dict:
    return {
        "layout": LAYOUT,
        "lock_sha256": hashlib.sha256(LOCK.read_bytes()).hexdigest(),
        "cxx_headers_lock_sha256": hashlib.sha256(CXX_LOCK.read_bytes()).hexdigest(),
        "runtime_lock_sha256": hashlib.sha256(RUNTIME_LOCK.read_bytes()).hexdigest(),
    }


def current(output: pathlib.Path, wanted: dict) -> bool:
    try:
        installed = json.loads((output / RECEIPT).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return False
    return installed == wanted and all((output / path).exists() for path in REQUIRED) and private(output)


def private(output: pathlib.Path) -> bool:
    """Whether no file of the tree shares its inode: a tree hardlinked with another checkout is stale."""
    for directory, _, names in os.walk(output):
        for name in names:
            status = os.lstat(os.path.join(directory, name))
            if stat.S_ISREG(status.st_mode) and status.st_nlink != 1:
                return False
    return True


def place(source: pathlib.Path, target: pathlib.Path) -> None:
    """Copy one resolved file, keeping only its executable bit."""
    target.parent.mkdir(parents=True, exist_ok=True)
    real = source.resolve(strict=True)
    if not real.is_file():
        raise SystemExit(f"LLVM tree entry is not a file: {source}")
    shutil.copyfile(real, target)
    target.chmod(0o755 if os.access(real, os.X_OK) else 0o644)


def place_tree(source: pathlib.Path, target: pathlib.Path) -> None:
    for path in sorted(source.rglob("*")):
        if path.is_dir():
            continue
        place(path, target / path.relative_to(source))


def fetch_deb(deb: dict, directory: pathlib.Path) -> pathlib.Path:
    path = directory / deb["name"]
    for url in deb["urls"]:
        try:
            with urllib.request.urlopen(url, timeout=120) as response, path.open("wb") as out:
                shutil.copyfileobj(response, out)
        except OSError as error:
            print(f"{url}: {error}", file=sys.stderr)
            continue
        if path.stat().st_size == deb["size"] and hashlib.sha256(path.read_bytes()).hexdigest() == deb["sha256"]:
            return path
        print(f"{url}: size or SHA-256 differs from the lock", file=sys.stderr)
    raise SystemExit(f"could not fetch a verified {deb['name']}")


def fetch_built(record: str, directory: pathlib.Path) -> pathlib.Path:
    """A locked asset built from the source archive: `static_lld`, the static-PIE
    ld.lld that replaces the deb's (build-static-lld), or `engine_llvm`, the
    compiler's libLLVM (build-libllvm)."""
    records = {}
    for line in LOCK.read_text(encoding="utf-8").splitlines():
        fields = line.split()
        if fields and not fields[0].startswith("#"):
            records[fields[0]] = fields[1:]
    sha256, size, asset = records[record]
    pool = records["deb_pool"][0]
    return fetch_deb({"name": asset, "urls": [f"{pool}/{asset}"], "size": int(size), "sha256": sha256}, directory)


def place_runtime_libraries(stage: pathlib.Path, scratch: pathlib.Path) -> None:
    lock = json.loads(RUNTIME_LOCK.read_text(encoding="utf-8"))
    if lock.get("schema") != 1:
        raise SystemExit("unsupported llvm-runtime-lock.json schema")
    unpacked = scratch / "runtime"
    for deb in lock["debs"]:
        subprocess.run(["dpkg-deb", "-x", fetch_deb(deb, scratch), unpacked], check=True)
    for path in sorted((unpacked / "usr/lib/x86_64-linux-gnu").iterdir()):
        # Sonames may be regular files (libz3) or links (libedit).
        if ".so." in path.name and path.is_file():
            place(path, stage / "lib" / path.name)


def relocate_tools(stage: pathlib.Path, scratch: pathlib.Path) -> None:
    lock = json.loads(RUNTIME_LOCK.read_text(encoding="utf-8"))
    patcher = scratch / "patchelf"
    subprocess.run(["dpkg-deb", "-x", fetch_deb(lock["patchelf"], scratch), patcher], check=True)
    libraries = {path.name for path in (stage / "lib").glob("*.so*")}
    tools = sorted(path for path in (stage / "bin").iterdir() if path.is_file())
    for path in tools + sorted((stage / "lib").glob("*.so*")):
        with path.open("rb") as source:
            if source.read(4) != b"\x7fELF":
                continue
        dynamic = subprocess.check_output(["readelf", "-d", path], env={**os.environ, "LC_ALL": "C"}, text=True)
        needed = set(re.findall(r"\(NEEDED\).*\[(.*?)\]", dynamic))
        existing = re.findall(r"\(RUNPATH\).*\[(.*?)\]", dynamic)
        if path.parent.name == "bin":
            if not needed:
                continue
            runpath = "$ORIGIN/../lib"
        else:
            if not needed & libraries or existing == ["$ORIGIN/../lib"]:
                continue
            runpath = "$ORIGIN"
        if existing != [runpath]:
            subprocess.run([patcher / "usr/bin/patchelf", "--set-rpath", runpath, path], check=True)


def tool_version(path: pathlib.Path) -> str:
    env = os.environ.copy()
    env.pop("LD_LIBRARY_PATH", None)
    return subprocess.check_output([path, "--version"], env=env, text=True).strip()


def build(stage: pathlib.Path, wanted: dict) -> None:
    stage.mkdir()
    with tempfile.TemporaryDirectory(prefix="hirsel-llvm-") as raw:
        unpacked = pathlib.Path(raw) / "root"
        subprocess.run([INSTALL, "--unpack", unpacked], check=True, stdout=sys.stderr)
        prefix = unpacked / "usr/lib/llvm-23"
        for path in sorted((prefix / "bin").iterdir()):
            place(path, stage / "bin" / path.name)
        linker = fetch_built("static_lld", pathlib.Path(raw))
        linker.chmod(0o755)
        place(linker, stage / "bin/ld.lld")
        place(fetch_built("engine_llvm", pathlib.Path(raw)), stage / "engine/libLLVM.so.23.1")
        for name in ("libLLVM.so.23.1", "libLLVM-23.so", "libclang-cpp.so.23.1"):
            place(prefix / "lib" / name, stage / "lib" / name)
        place_tree(prefix / "lib/clang", stage / "lib/clang")
        for name in ("llvm", "llvm-c"):
            place_tree(prefix / "include" / name, stage / "include" / name)
        place_runtime_libraries(stage, pathlib.Path(raw))
        relocate_tools(stage, pathlib.Path(raw))
        headers = pathlib.Path(raw) / "cxx-headers"
        for deb in json.loads(CXX_LOCK.read_text())["debs"]:
            subprocess.run(["dpkg-deb", "-x", fetch_deb(deb, pathlib.Path(raw)), headers], check=True)
        place_tree(headers / "usr/include", stage / "sysroot/usr/include")
    for name in ("clang", "clang++", "llvm-ar"):
        wrapper = stage / "bin" / f"hirsel-{name}"
        wrapper.write_text(WRAPPER % name, encoding="utf-8")
        wrapper.chmod(0o755)
    missing = [path for path in REQUIRED if not (stage / path).exists()]
    if missing:
        raise SystemExit("LLVM tree omits required paths: " + ", ".join(missing))
    for name in ("clang", "llvm-ar", "llvm-objcopy", "llvm-objdump", "llvm-config"):
        tool_version(stage / "bin" / name)
    (stage / RECEIPT).write_text(json.dumps(wanted, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def install(output: pathlib.Path, wanted: dict) -> None:
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".hirsel-llvm-", dir=output.parent) as raw:
        stage = pathlib.Path(raw) / "llvm-files"
        if not bootstrap_store.materialize(ROOT, "llvm", wanted, lambda tree: build(tree, wanted), stage,
                                           private=True):
            build(stage, wanted)
        old = output.with_name(output.name + ".old")
        if old.exists():
            shutil.rmtree(old)
        if output.exists():
            output.rename(old)
        stage.rename(output)
        if old.exists():
            shutil.rmtree(old)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--output", type=pathlib.Path, default=DEFAULT_OUTPUT)
    args = parser.parse_args()
    output = args.output.resolve()
    wanted = identity()
    if current(output, wanted):
        print(f"Buck2 LLVM tree is current: {output}")
        return 0
    if args.check:
        print(f"Buck2 LLVM tree is missing or stale: {output}", file=sys.stderr)
        return 1
    install(output, wanted)
    if not current(output, wanted):
        raise SystemExit("installed LLVM tree did not pass its receipt check")
    version = tool_version(output / "bin/llvm-config")
    print(f"installed LLVM {version}: {output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
