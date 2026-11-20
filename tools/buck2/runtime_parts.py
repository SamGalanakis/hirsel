#!/usr/bin/env python3
"""Derive a self-contained, debug-stripped runtime archive and its compiler builtins.

Core runtime links always read the stripped archive. The builtins archive holds the archive's
`compiler_builtins` members in archive order: the integer and float helpers a
program library (which links no runtime) needs, the same implementations the
runtime archive gives an executable. Both are deterministic.
"""
import argparse
from pathlib import Path
import subprocess
import tempfile


def quote_mri(path):
    path = str(path)
    if any(c in path for c in '\n\r"'):
        raise ValueError("archive artifact path cannot be represented in MRI")
    return '"' + path + '"'


def bundle(ar, output, archives, objects, environment):
    # ADDLIB expands thin archives without losing identical member names.
    script = ["CREATE " + quote_mri(output)]
    script.extend("ADDLIB " + quote_mri(path) for path in archives)
    script.extend("ADDMOD " + quote_mri(path) for path in objects)
    script.extend(["SAVE", "END", ""])
    subprocess.run([ar, "-M"], input="\n".join(script), text=True, check=True, env=environment)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--llvm", required=True, help="the pinned LLVM tree")
    parser.add_argument("--archive", required=True)
    parser.add_argument("--stripped", required=True)
    parser.add_argument("--builtins")
    parser.add_argument("--native-archive", action="append", default=[])
    parser.add_argument("--native-object", action="append", default=[])
    args = parser.parse_args()
    tree = Path(args.llvm).resolve()
    environment = {"LD_LIBRARY_PATH": str(tree / "lib"), "PATH": "/usr/bin:/bin"}
    tool = lambda name: str(tree / "bin" / name)
    for output in filter(None, [args.stripped, args.builtins]):
        Path(output).parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="hirsel-runtime-bundle-") as scratch:
        source = args.archive
        if args.native_archive or args.native_object:
            source = Path(scratch) / "runtime.a"
            bundle(tool("llvm-ar"), source, [args.archive, *args.native_archive],
                   args.native_object, environment)
        subprocess.run([tool("llvm-objcopy"), "--strip-debug", source, args.stripped],
                       check=True, env=environment)
    if args.builtins:
        members = subprocess.check_output([tool("llvm-ar"), "t", args.stripped], text=True, env=environment).split()
        chosen = [m for m in members if m.startswith("compiler_builtins-")]
        if not chosen or len(set(chosen)) != len(chosen):
            raise SystemExit(f"{args.stripped}: {len(chosen)} compiler_builtins members, or duplicate names")
        with tempfile.TemporaryDirectory(prefix="hirsel-runtime-parts-") as scratch:
            subprocess.run([tool("llvm-ar"), "x", f"--output={scratch}", str(Path(args.stripped).resolve()), *chosen], check=True, env=environment)
            subprocess.run([tool("llvm-ar"), "rcsD", str(Path(args.builtins).resolve()), *chosen], check=True, cwd=scratch, env=environment)


if __name__ == "__main__":
    main()
