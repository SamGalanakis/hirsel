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
        source = backtrace.read_text()
        signature = "unsafe fn _print_fmt(fmt: &mut fmt::Formatter<'_>, print_fmt: PrintFmt) -> fmt::Result {"
        next_item = "/// Fixed frame used to clean the backtrace with `RUST_BACKTRACE=1`."
        before, found_signature, body = source.partition(signature)
        formatter, found_next_item, after = body.partition(next_item)
        if not (found_signature and found_next_item and 'feature="backtrace-trace-only"' in formatter.replace(" ", "")):
            raise SystemExit("Rust's trace-only formatter changed. Recheck the native panic renderer.")
        before = before.replace(
            "use crate::backtrace_rs::{self, BacktraceFmt, BytesOrWideString, PrintFmt};",
            "use crate::backtrace_rs::{BytesOrWideString, PrintFmt};",
        ).replace("use crate::{env, fmt, io};", "use crate::{fmt, io};")
        after = after.replace("backtrace_rs::set_image_base(", "crate::backtrace_rs::set_image_base(")
        backtrace.write_text(before + signature + '''
    #[path = "hirsel_panic_image.rs"]
    mod hirsel_panic_image;
    // SAFETY: the caller holds the backtrace lock.
    unsafe { hirsel_panic_image::write(fmt, print_fmt) }
}

''' + next_item + after)
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
