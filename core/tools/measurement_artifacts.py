#!/usr/bin/env python3
"""Build/identify the release compiler and shipping runtime used for measurements."""
import argparse
import contextlib
import hashlib
import json
from pathlib import Path
import subprocess
import sys

from std_native import wait_load

ROOT = Path(__file__).resolve().parents[2]
COMPILER_TARGET = '//:hirsel-core-driver-bin-hirsel-core'
RUNTIME_TARGET = '//:hirsel-core-rt-archives'
PLATFORM = 'x86_64-unknown-linux-gnu'
RUNTIME_VIEW = 'buck-out/v2/art/root/*/__hirsel-core-rt-archives__/hirsel-core-rt-archives/' + PLATFORM + '/libhirsel_core_rt.a'


def identity(compiler, runtime, compiler_profile, runtime_profile=None, diagnostic=False):
    """Reject non-shipping archives, including ordinary O3 staticlibs."""
    roots = {ROOT, *(parent for parent in runtime.absolute().parents
                     if (parent / 'buck-out/v2/art/root').is_dir())}
    canonical = runtime.resolve()
    shipping = any(path.is_file() and path.resolve() == canonical
                   for root in roots for path in root.glob(RUNTIME_VIEW))
    profile = 'shipping-release-fat-lto' if shipping else runtime_profile or 'unknown'
    if not diagnostic and (compiler_profile != 'release' or not shipping):
        raise ValueError('ratios require a release compiler and the shipping fat-LTO runtime; '
                         'run python3 core/tools/measurement_artifacts.py; '
                         'use --diagnostic explicitly for other artifacts')
    return {'compiler_build_profile': compiler_profile,
            'compiler_sha256': hashlib.sha256(compiler.read_bytes()).hexdigest(),
            'runtime_archive_profile': profile,
            'runtime_archive_sha256': hashlib.sha256(runtime.read_bytes()).hexdigest(),
            'diagnostic': diagnostic}


def build():
    """Buck locates cached outputs or builds exactly the canonical targets."""
    with contextlib.redirect_stdout(sys.stderr):
        wait_load()
    command = [str(ROOT / '.buck2/bin/buck2'), 'build', '--target-platforms',
               '//tools/buck2:release', COMPILER_TARGET, RUNTIME_TARGET, '--show-json-output']
    completed = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, text=True, check=True)
    outputs = json.loads(completed.stdout)
    compiler = ROOT / outputs['root' + COMPILER_TARGET]
    runtime = ROOT / outputs['root' + RUNTIME_TARGET] / PLATFORM / 'libhirsel_core_rt.a'
    return compiler, runtime


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--runtime', type=Path)
    p.add_argument('--compiler-profile', choices=['release', 'checked', 'unknown'], default='unknown')
    p.add_argument('--runtime-profile', choices=['ordinary-release', 'checked', 'unknown'])
    p.add_argument('--diagnostic', action='store_true')
    args = p.parse_args()
    if bool(args.compiler) != bool(args.runtime):
        p.error('supply both --compiler and --runtime, or neither to build/locate them')
    try:
        if args.compiler is None:
            args.compiler, args.runtime = build()
            args.compiler_profile = 'release'
        result = identity(args.compiler, args.runtime, args.compiler_profile,
                          args.runtime_profile, args.diagnostic)
        result.update(compiler=str(args.compiler.absolute()), runtime=str(args.runtime.absolute()),
                      compiler_target=COMPILER_TARGET, runtime_archive_target=RUNTIME_TARGET,
                      target=PLATFORM)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        p.error(str(error))
    print(json.dumps(result, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
