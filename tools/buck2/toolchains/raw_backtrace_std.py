#!/usr/bin/env python3
"""Compile the runtime's std against the pinned sysroot with trace-only panics."""
from pathlib import Path
import os
import shutil
import subprocess
import sys
import tempfile


def main():
    sysroot, sources, image = map(lambda name: Path(name).resolve(), sys.argv[1:4])
    target, output = sys.argv[4:]
    rustc = sysroot / "bin/rustc"
    libraries = sysroot / "lib/rustlib" / target / "lib"
    dependencies = (
        "addr2line", "alloc", "cfg_if", "core", "hashbrown", "libc", "miniz_oxide",
        "object", "panic_abort", "panic_unwind", "rustc_demangle", "std_detect", "unwind",
    )
    externs = []
    for name in dependencies:
        metadata, = libraries.glob(f"lib{name}-*.rmeta")
        externs.extend(("--extern", f"{name}={metadata}"))

    version = subprocess.check_output([rustc, "-vV"], text=True)
    commit, = (line.split()[1] for line in version.splitlines() if line.startswith("commit-hash: "))
    with tempfile.TemporaryDirectory(prefix="hirsel-std-") as temporary:
        library = Path(temporary) / "library"
        shutil.copytree(sources, library, copy_function=shutil.copyfile)
        backtrace = library / "std/src/sys/backtrace.rs"
        shutil.copyfile(image, backtrace.with_name("hirsel_panic_image.rs"))
        original = backtrace.read_text()
        marker = '    writeln!(fmt, "stack backtrace:")?;\n'
        if original.count(marker) != 1:
            raise SystemExit("Rust's backtrace formatter changed. Recheck the image header insertion.")
        backtrace.write_text(original.replace(marker, marker + '''    #[path = "hirsel_panic_image.rs"]
    mod hirsel_panic_image;
    hirsel_panic_image::write(fmt)?;
'''))
        subprocess.run([
            rustc, library / "std/src/lib.rs", "--crate-name", "std", "--edition=2024",
            "--crate-type=rlib", "--target", target, "--sysroot", sysroot,
            f"--remap-path-prefix={library}=/rustc/{commit}/library",
            "-Zforce-unstable-if-unmarked", "-Copt-level=3", "-Ccodegen-units=1",
            "-Cembed-bitcode=yes", "-Aunfulfilled-lint-expectations",
            "--cfg", "backtrace_in_libstd", "--cfg", 'feature="backtrace"',
            "--cfg", 'feature="backtrace-trace-only"', "--cfg", 'feature="panic-unwind"',
            "-L", f"dependency={libraries}", *externs, "-o", output,
        ], env={**os.environ, "RUSTC_BOOTSTRAP": "1", "STD_ENV_ARCH": target.split("-")[0]},
            check=True)


if __name__ == "__main__":
    main()
