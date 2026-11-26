#!/usr/bin/env python3
"""Type-check Rust for the Apple targets on Linux, without linking.

Use the pinned rustc and std installed by tools/buck2/prepare.py. The macOS
check covers workspace tests except crates that need a target LLVM prefix.
The iOS checks cover the crates ready for each triple. The C tools are `true`
and HIRSEL_METADATA_CHECK skips the LLVM engine.

`--if-changed` checks only when a Rust or Cargo file differs from origin/main.
"""

import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
RUSTC = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
DARWIN_EXCLUDED = ('hirsel-llvm', 'hirsel-core-program-tests')
TARGET_CRATES = {
    'aarch64-apple-darwin': None,  # Workspace tests, with DARWIN_EXCLUDED.
    'aarch64-apple-ios': ('hirsel-core-layout',),
    'aarch64-apple-ios-sim': ('hirsel-core-layout',),
}


def rust_changed():
    base = subprocess.check_output(['git', 'merge-base', 'HEAD', 'origin/main'], cwd=ROOT, text=True).strip()
    specs = ['*.rs', '**/Cargo.toml', 'Cargo.lock', '.cargo/config.toml']
    changed = subprocess.check_output(['git', 'diff', '--name-only', base, '--', *specs], cwd=ROOT, text=True)
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', '--', *specs],
                                        cwd=ROOT, text=True)
    return bool(changed.strip() or untracked.strip())


def check_target(target, crates):
    sysroot = RUSTC.parent.parent
    if not (sysroot / 'lib/rustlib' / target / 'lib').is_dir():
        print(f'apple check: run python3 tools/buck2/prepare.py for the pinned std for {target}',
              file=sys.stderr)
        return 1
    target_dir = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'apple-check'
    triple = target.replace('-', '_')
    # Rustup's library path can make the pinned rustc infer Rustup's sysroot.
    flags = '\x1f'.join(('-Awarnings', '--sysroot', str(sysroot)))
    env = dict(os.environ, RUSTC=str(RUSTC), CARGO_TARGET_DIR=str(target_dir), HIRSEL_METADATA_CHECK='1',
               CARGO_ENCODED_RUSTFLAGS=flags, CRATE_CC_NO_DEFAULTS='1',
               **{f'CC_{triple}': 'true', f'CXX_{triple}': 'true', f'AR_{triple}': 'true'})
    command = ['cargo', 'check', '--locked', '--quiet', '--target', target, '--keep-going']
    if crates is None:
        command.extend(('--workspace', '--tests'))
        for crate in DARWIN_EXCLUDED:
            command.extend(('--exclude', crate))
    else:
        for crate in crates:
            command.extend(('--package', crate))
    print(f'apple check: {target}', flush=True)
    status = subprocess.run(command, cwd=ROOT, env=env).returncode
    if status:
        print(f'apple check: compilation failed for {target}', file=sys.stderr)
    return status


def check():
    status = 0
    for target, crates in TARGET_CRATES.items():
        status = check_target(target, crates) or status
    return status


def main():
    if '--if-changed' in sys.argv[1:] and not rust_changed():
        return 0
    return check()


if __name__ == '__main__':
    sys.exit(main())
