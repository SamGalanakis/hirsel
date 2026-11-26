#!/usr/bin/env python3
"""Type-check the workspace for aarch64-apple-darwin on Linux, without linking.

`cargo check --target aarch64-apple-darwin --tests` (rustc --emit=metadata) with the
pinned rustc, whose Darwin std tools/buck2/prepare.py installs. A Darwin-only item that
does not compile, or a Linux-only libc item reached from a Darwin build, is red here
rather than on a Mac. Nothing is linked and no C is compiled: the C compiler and
archiver are `true`, and hirsel-llvm-lazy's build script skips its engine under
HIRSEL_METADATA_CHECK, because the macOS SDK and engine are not on this host.
hirsel-llvm and hirsel-core-program-tests are left out: hirsel-llvm's build script
needs an LLVM prefix for the target. Errors fail; warnings are not shown.

`--if-changed` checks only when a Rust or Cargo file differs from origin/main.
"""

import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
TARGET = 'aarch64-apple-darwin'
RUSTC = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
EXCLUDED = ('hirsel-llvm', 'hirsel-core-program-tests')


def rust_changed():
    base = subprocess.check_output(['git', 'merge-base', 'HEAD', 'origin/main'], cwd=ROOT, text=True).strip()
    specs = ['*.rs', '**/Cargo.toml', 'Cargo.lock', '.cargo/config.toml']
    changed = subprocess.check_output(['git', 'diff', '--name-only', base, '--', *specs], cwd=ROOT, text=True)
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', '--', *specs],
                                        cwd=ROOT, text=True)
    return bool(changed.strip() or untracked.strip())


def check():
    sysroot = RUSTC.parent.parent
    if not (sysroot / 'lib/rustlib' / TARGET / 'lib').is_dir():
        raise SystemExit('darwin check: run python3 tools/buck2/prepare.py for the pinned Darwin std')
    target_dir = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'darwin-check'
    triple = TARGET.replace('-', '_')
    # Rustup's library path can make the pinned rustc infer Rustup's sysroot.
    flags = '\x1f'.join(('-Awarnings', '--sysroot', str(sysroot)))
    env = dict(os.environ, RUSTC=str(RUSTC), CARGO_TARGET_DIR=str(target_dir), HIRSEL_METADATA_CHECK='1',
               CARGO_ENCODED_RUSTFLAGS=flags,
               **{f'CC_{triple}': 'true', f'CXX_{triple}': 'true', f'AR_{triple}': 'true'})
    command = ['cargo', 'check', '--locked', '--quiet', '--target', TARGET, '--workspace', '--tests', '--keep-going',
               *(flag for crate in EXCLUDED for flag in ('--exclude', crate))]
    status = subprocess.run(command, cwd=ROOT, env=env).returncode
    if status:
        print(f'darwin check: the workspace does not compile for {TARGET}', file=sys.stderr)
    return status


def main():
    if '--if-changed' in sys.argv[1:] and not rust_changed():
        return 0
    return check()


if __name__ == '__main__':
    sys.exit(main())
