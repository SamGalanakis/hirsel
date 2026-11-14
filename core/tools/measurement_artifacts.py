#!/usr/bin/env python3
"""Build, pin and identify the release compiler and shipping runtime used for measurements.

Samples execute the pinned copy, never the live path. Buck's output paths are
shared by several build configurations, so a later build in the same checkout
silently replaces the live compiler or archive. This tool copies both
artifacts to content-addressed, read-only files under the checkout's
buck-out/measurement/pinned/v1/sha256/ (each name carries
its profile and sha256) and reports those copies as `compiler` and `runtime`:
the only paths a harness executes. `compiler_source` and `runtime_source` record
where they were built and are never executed. A pinned compiler sits at the
same depth below the checkout as its Buck output, so its $ORIGIN RPATH still
finds the checkout's pinned LLVM toolchain; the measured programs' environment
is unchanged. Right before each batch, a harness runs
`--check <path> --identity <this tool's JSON>`, which re-hashes the path and
fails unless it matches the recorded identity."""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from std_native import wait_load

ROOT = Path(__file__).resolve().parents[2]
COMPILER_TARGET = '//:hirsel-core-driver-bin-hirsel-core'
RUNTIME_TARGET = '//:hirsel-core-rt-archives'
PLATFORM = 'x86_64-unknown-linux-gnu'
RUNTIME_VIEW = 'buck-out/v2/art/root/*/__hirsel-core-rt-archives__/hirsel-core-rt-archives/' + PLATFORM + '/libhirsel_core_rt.a'
# Six directories below the checkout, as Buck's buck-out/v2/art/root/<cfg>/__<target>__/.
PINS = 'buck-out/measurement/pinned/v1/sha256'
SHIPPING = 'shipping-release-fat-lto'
PINNED_RUNTIME = re.compile(r'libhirsel_core_rt-(?P<profile>[a-z0-9-]+)-(?P<sha>[0-9a-f]{64})\.a')


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def pinned_shipping(runtime, roots):
    """A pin of the shipping archive: in a pin directory, named shipping, content as named."""
    match = PINNED_RUNTIME.fullmatch(runtime.name)
    return (match is not None and match['profile'] == SHIPPING
            and any(runtime.resolve().parent == (root / PINS / 'runtime').resolve() for root in roots)
            and sha256(runtime) == match['sha'])


def identity(compiler, runtime, compiler_profile, runtime_profile=None, diagnostic=False):
    """Reject non-shipping archives, including ordinary O3 staticlibs."""
    roots = {ROOT, *(parent for parent in runtime.absolute().parents
                     if (parent / 'buck-out/v2/art/root').is_dir() or (parent / PINS).is_dir())}
    canonical = runtime.resolve()
    shipping = (any(path.is_file() and path.resolve() == canonical
                    for root in roots for path in root.glob(RUNTIME_VIEW))
                or pinned_shipping(runtime, roots))
    profile = SHIPPING if shipping else runtime_profile or 'unknown'
    if not diagnostic and (compiler_profile != 'release' or not shipping):
        raise ValueError('ratios require a release compiler and the shipping fat-LTO runtime; '
                         'run python3 core/tools/measurement_artifacts.py; '
                         'use --diagnostic explicitly for other artifacts')
    return {'compiler_build_profile': compiler_profile,
            'compiler_sha256': sha256(compiler),
            'runtime_archive_profile': profile,
            'runtime_archive_sha256': sha256(runtime),
            'diagnostic': diagnostic}


def pin(source, kind, stem, suffix, mode):
    """A read-only copy named by the sha256 of exactly the bytes it holds."""
    data = source.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    directory = ROOT / PINS / kind
    directory.mkdir(parents=True, exist_ok=True)
    target = directory / f'{stem}-{digest}{suffix}'
    if target.exists():
        if sha256(target) != digest:
            raise ValueError(f'pinned {target} no longer matches its name; move it aside and rerun')
        return target, digest
    with tempfile.NamedTemporaryFile(dir=directory, prefix='.pin-', delete=False) as staged:
        staged.write(data)
    os.chmod(staged.name, mode)
    os.replace(staged.name, target)
    return target, digest


def pin_all(compiler, runtime, result):
    """Pin both artifacts; the copies become the paths to execute."""
    live = len(compiler.absolute().relative_to(ROOT).parent.parts)
    if live != len(Path(PINS, 'compiler').parts):
        raise ValueError(f'the compiler is {live} directories below the checkout, the pins are not; '
                         'a pinned copy would not find its $ORIGIN libraries')
    pinned_compiler, compiler_digest = pin(compiler, 'compiler', f"hirsel_core-{result['compiler_build_profile']}", '', 0o555)
    pinned_runtime, runtime_digest = pin(runtime, 'runtime', f"libhirsel_core_rt-{result['runtime_archive_profile']}", '.a', 0o444)
    if (compiler_digest, runtime_digest) != (result['compiler_sha256'], result['runtime_archive_sha256']):
        raise ValueError('a live artifact changed while it was pinned; rerun')
    return pinned_compiler, pinned_runtime


def check(path, recorded):
    """Re-hash an artifact path right before a batch; raise unless it is the recorded identity."""
    path = Path(path).absolute()
    wanted = {Path(recorded['compiler']).absolute(): recorded['compiler_sha256'],
              Path(recorded['runtime']).absolute(): recorded['runtime_archive_sha256']}.get(path)
    if wanted is None:
        raise ValueError(f'{path} is not a pinned path of this identity; execute only its compiler or runtime')
    actual = sha256(path)
    if actual != wanted:
        raise ValueError(f'{path} hashes to {actual}, the recorded identity is {wanted}')
    return actual


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
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument('--compiler', type=Path)
    p.add_argument('--runtime', type=Path)
    p.add_argument('--compiler-profile', choices=['release', 'checked', 'unknown'], default='unknown')
    p.add_argument('--runtime-profile', choices=['ordinary-release', 'checked', 'unknown'])
    p.add_argument('--diagnostic', action='store_true')
    p.add_argument('--check', type=Path, metavar='PATH',
                   help='re-hash PATH and fail unless it matches --identity; call right before each batch')
    p.add_argument('--identity', type=Path, metavar='JSON', help='an earlier output of this tool, for --check')
    args = p.parse_args()
    if bool(args.check) != bool(args.identity):
        p.error('--check and --identity go together')
    if args.check:
        try:
            print(f'ok {check(args.check, json.loads(args.identity.read_text()))} {args.check}')
        except (OSError, ValueError, KeyError) as error:
            p.error(str(error))
        return 0
    if bool(args.compiler) != bool(args.runtime):
        p.error('supply both --compiler and --runtime, or neither to build/locate them')
    try:
        if args.compiler is None:
            args.compiler, args.runtime = build()
            args.compiler_profile = 'release'
        result = identity(args.compiler, args.runtime, args.compiler_profile,
                          args.runtime_profile, args.diagnostic)
        compiler, runtime = pin_all(args.compiler, args.runtime, result)
        result.update(compiler=str(compiler), runtime=str(runtime),
                      compiler_source=str(args.compiler.absolute()), runtime_source=str(args.runtime.absolute()),
                      pins=str(ROOT / PINS), compiler_target=COMPILER_TARGET,
                      runtime_archive_target=RUNTIME_TARGET, target=PLATFORM)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        p.error(str(error))
    print(json.dumps(result, indent=2))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
