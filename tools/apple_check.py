#!/usr/bin/env python3
"""Type-check Rust for the Apple targets on Linux, without linking.

Use the pinned rustc and std installed by tools/buck2/prepare.py. The macOS
check covers workspace tests except crates that need a target LLVM prefix.
The iOS checks cover the embed runtime slot and its Cargo dependencies.
The C tools are `true` and HIRSEL_METADATA_CHECK skips the LLVM engine.

`--if-changed` checks for Rust, Cargo or product changes since origin/main.
"""

from concurrent.futures import ThreadPoolExecutor
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
RUSTC = ROOT / 'tools/buck2/toolchains/rust-files/bin/rustc'
DARWIN_EXCLUDED = ('hirsel-llvm', 'hirsel-core-program-tests')

sys.path.insert(0, str(ROOT / 'tools/toolchain-dist'))
import hcfg

PRODUCT = ROOT / 'tools/toolchain-dist/product.hcfg'


def apple_targets():
    product = hcfg.product(PRODUCT)
    slots = product['runtime_slots']
    targets = {'aarch64-apple-darwin': None}  # Workspace tests, with DARWIN_EXCLUDED.
    for target in ('aarch64-apple-ios', 'aarch64-apple-ios-sim'):
        (slot,) = product['runtime_targets'][target]['slots']
        targets[target] = slots[slot]
    return targets


def rust_changed():
    base = subprocess.check_output(['git', 'merge-base', 'HEAD', 'origin/main'], cwd=ROOT, text=True).strip()
    specs = ['*.rs', '**/Cargo.toml', 'Cargo.lock', '.cargo/config.toml', 'tools/toolchain-dist/product.hcfg']
    changed = subprocess.check_output(['git', 'diff', '--name-only', base, '--', *specs], cwd=ROOT, text=True)
    untracked = subprocess.check_output(['git', 'ls-files', '--others', '--exclude-standard', '--', *specs],
                                        cwd=ROOT, text=True)
    return bool(changed.strip() or untracked.strip())


def check_target(target, slot):
    sysroot = RUSTC.parent.parent
    if not (sysroot / 'lib/rustlib' / target / 'lib').is_dir():
        print(f'apple check: run python3 tools/buck2/prepare.py for the pinned std for {target}',
              file=sys.stderr)
        return 1
    target_dir = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / 'apple-check' / target
    triple = target.replace('-', '_')
    # Rustup's library path can make the pinned rustc infer Rustup's sysroot.
    flags = '\x1f'.join(('-Awarnings', '--sysroot', str(sysroot)))
    env = dict(os.environ, RUSTC=str(RUSTC), CARGO_TARGET_DIR=str(target_dir), HIRSEL_METADATA_CHECK='1',
               CARGO_ENCODED_RUSTFLAGS=flags, CRATE_CC_NO_DEFAULTS='1',
               **{f'CC_{triple}': 'true', f'CXX_{triple}': 'true', f'AR_{triple}': 'true'})
    command = ['cargo', 'check', '--locked', '--quiet', '--target', target, '--keep-going']
    if slot is None:
        command.extend(('--workspace', '--tests'))
        for crate in DARWIN_EXCLUDED:
            command.extend(('--exclude', crate))
    else:
        command.extend(('--package', slot['crate_name'], '--no-default-features'))
        if slot['features']:
            command.extend(('--features', ','.join(slot['features'])))
    print(f'apple check: {target}', flush=True)
    status = subprocess.run(command, cwd=ROOT, env=env).returncode
    if status:
        print(f'apple check: compilation failed for {target}', file=sys.stderr)
    return status


def check():
    targets = apple_targets()
    with ThreadPoolExecutor(max_workers=len(targets)) as pool:
        statuses = [pool.submit(check_target, target, slot) for target, slot in targets.items()]
        return max(status.result() for status in statuses)


def main():
    if '--if-changed' in sys.argv[1:] and not rust_changed():
        return 0
    return check()


if __name__ == '__main__':
    sys.exit(main())
