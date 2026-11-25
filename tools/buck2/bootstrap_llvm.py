#!/usr/bin/env python3
"""Materialize the pinned LLVM 23 tree as an ignored Buck2 toolchain input.

The locked bootstrap archive provides clang, the LLVM tools and headers, and
their stock shared library. This script adds the independently pinned ld.lld
and engine libLLVM and flattens symlinks (Buck2 hashes a link, not its target).

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
import io
import json
import os
import pathlib
import shutil
import stat
import tarfile
import subprocess
import sys
import tempfile
import urllib.request

import bootstrap_store


ROOT = pathlib.Path(__file__).resolve().parents[2]
LOCK = ROOT / "tools/native-toolchain/llvm-23.lock"
CXX_LOCK = pathlib.Path(__file__).with_name("llvm-cxx-headers-lock.json")
DEFAULT_OUTPUT = pathlib.Path(__file__).with_name("toolchains") / "llvm-files"
RECEIPT = ".hirsel-llvm.json"
LAYOUT = 10
REQUIRED = (
    "sysroot/usr/include/c++/13/vector",
    "sysroot/usr/include/features.h",
    "bin/clang",
    "bin/clang-23",
    "bin/llvm-config",
    "bin/ld.lld",
    "lib/libLLVM.so.23.1",
    "lib/libLLVM-23.so",
    "lib/libclang-cpp.so.23.1",
    "engine/libLLVM.so.23.1",
    "lib/clang/23/include/stddef.h",
    "include/llvm-c/Core.h",
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


def fetch(pin: dict, directory: pathlib.Path) -> pathlib.Path:
    path = directory / pin["name"]
    for url in pin["urls"]:
        try:
            # archive.ubuntu.com answers 403 to urllib's default Python-urllib agent.
            request = urllib.request.Request(url, headers={"User-Agent": "hirsel-bootstrap"})
            with urllib.request.urlopen(request, timeout=120) as response, path.open("wb") as out:
                shutil.copyfileobj(response, out)
        except OSError as error:
            print(f"{url}: {error}", file=sys.stderr)
            continue
        if path.stat().st_size == pin["size"] and hashlib.sha256(path.read_bytes()).hexdigest() == pin["sha256"]:
            return path
        print(f"{url}: size or SHA-256 differs from the lock", file=sys.stderr)
    raise SystemExit(f"could not fetch a verified {pin['name']}")


def fetch_built(record: str, directory: pathlib.Path) -> pathlib.Path:
    """Fetch one of the binaries built from the locked source archive."""
    records = {}
    for line in LOCK.read_text(encoding="utf-8").splitlines():
        fields = line.split()
        if fields and not fields[0].startswith("#"):
            records[fields[0]] = fields[1:]
    sha256, size, asset = records[record]
    pool = records["asset_pool"][0]
    return fetch({"name": asset, "urls": [f"{pool}/{asset}"], "size": int(size), "sha256": sha256}, directory)


def tool_version(path: pathlib.Path) -> str:
    env = os.environ.copy()
    env.pop("LD_LIBRARY_PATH", None)
    return subprocess.check_output([path, "--version"], env=env, text=True).strip()


def unpack_headers(deb: pathlib.Path, headers: pathlib.Path) -> None:
    # AlmaLinux 8's dpkg does not read Ubuntu's zstd-compressed header package.
    members = subprocess.check_output(["ar", "t", deb], text=True).splitlines()
    if "data.tar.zst" not in members:
        subprocess.run(["dpkg-deb", "-x", deb, headers], check=True)
        return
    compressed = subprocess.check_output(["ar", "p", deb, "data.tar.zst"])
    data = subprocess.check_output(["zstd", "-dq"], input=compressed)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as archive:
        archive.extractall(headers, filter="data")


def build(stage: pathlib.Path, wanted: dict) -> None:
    stage.mkdir()
    with tempfile.TemporaryDirectory(prefix="hirsel-llvm-") as raw:
        scratch = pathlib.Path(raw)
        bootstrap = scratch / "bootstrap"
        with tarfile.open(fetch_built("bootstrap_llvm", scratch)) as archive:
            archive.extractall(bootstrap, filter="data")
        place_tree(bootstrap, stage)
        linker = fetch_built("static_lld", scratch)
        linker.chmod(0o755)
        place(linker, stage / "bin/ld.lld")
        place(linker, stage / "bin/lld")
        place(fetch_built("engine_llvm", scratch), stage / "engine/libLLVM.so.23.1")
        headers = scratch / "cxx-headers"
        for pin in json.loads(CXX_LOCK.read_text())["debs"]:
            unpack_headers(fetch(pin, scratch), headers)
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
