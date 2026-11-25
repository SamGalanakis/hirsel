#!/usr/bin/env python3
"""Instrument, train and optimize the release compiler or its libLLVM with BOLT.

A BOLTed compiler must emit what its input emits on programs its profile never saw:
every M1 program, outside WORKLOADS. Both compilers build each as a release executable
and every .text section must be byte-identical; the builds' total user CPU time on each
side is printed, not gated.
"""

import argparse
from collections import Counter
import json
import os
from pathlib import Path
import resource
import shutil
import subprocess
import tempfile

from engine_llvm import CC, ROOT, arena, m1_programs, shadow, shadow_engine, shadow_environment

ENGINE = 'libLLVM.so.23.1'
TARGET = 'x86_64-unknown-linux-gnu'
LLVM = ROOT / 'tools/buck2/toolchains/llvm-files/engine' / ENGINE
OBJCOPY = CC.with_name('llvm-objcopy')
# BOLT keeps the disassembled functions and their instrumentation tables in
# memory. Profile 16 MiB of the engine's text at a time under the 8 GiB cap.
ENGINE_SHARD_BYTES = 16 << 20
WORKLOADS = {'release std', 'cold LangArena'}
HELD_OUT = 'M1'
# rustc src/tools/opt-dist/src/bolt.rs, on x86-64.
OPTIMIZE = [
    '-reorder-blocks=ext-tsp', '-reorder-functions=cdsort',
    '-split-functions', '-split-strategy=cdsplit', '-split-all-cold',
    '-jump-tables=move', '-icf=all', '-update-debug-sections', '-dyno-stats',
]


def run(command, work, env=None, **options):
    subprocess.run(command, cwd=work, env=env, check=True, **options)


def publish_std(compiler, work, env):
    package = work / 'packages'
    run([compiler, 'std-product', '--store', package, '--target', TARGET], work, env,
        stdout=subprocess.DEVNULL)
    return package


def release(compiler, runtime, package, arguments, work, store, output):
    return [compiler, 'build', *arguments, '--backend', 'llvm', '--release',
            '--core-product', package, '--runtime', runtime, '--cc', CC,
            '--store', work / store, '-o', work / output]


def train(compiler, runtime, work, profile):
    work.mkdir()
    env = shadow_environment(compiler) | dict(XDG_CACHE_HOME=str(work / 'cache'),
                                               XDG_DATA_HOME=str(work / 'data'))
    recorded = set()

    def workload(name, build):
        before = set(profile.parent.glob('prof.fdata.*'))
        print(f'bolt: training on {name}', flush=True)
        result = build()
        profiles = set(profile.parent.glob('prof.fdata.*')) - before
        if any(p.stat().st_size for p in profiles):
            recorded.add(name)
        return result

    def build_program(name, arguments):
        run(release(compiler, runtime, package, arguments, work, name + '-store', name), work, env)
        if not (work / name).is_file():
            raise RuntimeError(f'{name} did not produce a release executable')

    package = workload('release std', lambda: publish_std(compiler, work, env))
    workload('cold LangArena', lambda: build_program('arena', arena(work)))
    return recorded


def held_out(compilers, runtime, work):
    """Build the HELD_OUT programs with each compiler; fail unless their .text sections are identical."""
    text, seconds = {}, {}
    for side, compiler in compilers.items():
        folder = work / f'held-out-{side}'
        folder.mkdir()
        exe = shadow(LLVM.parent, compiler, folder / 'shadow')
        env = shadow_environment(exe) | dict(XDG_CACHE_HOME=str(folder / 'cache'),
                                             XDG_DATA_HOME=str(folder / 'data'))
        package = publish_std(exe, folder, env)
        before = resource.getrusage(resource.RUSAGE_CHILDREN).ru_utime
        text[side] = {}
        for tag, arguments in m1_programs(folder):
            run(release(exe, runtime, package, arguments, folder, tag + '-store', tag), folder, env)
            section = folder / f'{tag}.text'
            run([OBJCOPY, f'--dump-section=.text={section}', folder / tag, folder / f'{tag}.copy'], folder)
            text[side][tag] = section.read_bytes()
        seconds[side] = resource.getrusage(resource.RUSAGE_CHILDREN).ru_utime - before
        shutil.rmtree(folder)
    print(f"bolt: held-out {HELD_OUT} build user CPU {seconds['input']:.2f} s input, "
          f"{seconds['bolt']:.2f} s BOLTed", flush=True)
    if differing := sorted(tag for tag in text['input'] if text['input'][tag] != text['bolt'][tag]):
        raise RuntimeError(f'the BOLTed compiler emits different .text on held-out {", ".join(differing)}')
    return seconds


def engine_shards(source, work):
    """Partition defined function symbols by address so aliases stay together."""
    symbols = subprocess.check_output(
        [CC.with_name('llvm-nm'), '--format=posix', '--numeric-sort', '--defined-only', source],
        text=True)
    local_ids = Counter()
    shards = {}
    start = None
    for line in symbols.splitlines():
        name, kind, address, *_ = line.split()
        if kind not in 'TtWwi':
            continue
        address = int(address, 16)
        if start is None:
            start = address
        if kind == 't':
            # BOLT suffixes local symbols with a one-based occurrence number.
            local_ids[name] += 1
            name = f'{name}/{local_ids[name]}'
        shards.setdefault((address - start) // ENGINE_SHARD_BYTES, []).append(name)
    if not shards:
        raise RuntimeError(f'no defined function symbols in {source}')
    for index, names in sorted(shards.items()):
        batch = work / str(index)
        batch.mkdir()
        selection = batch / 'functions'
        selection.write_text('\n'.join(names) + '\n')
        yield batch, [f'-funcs-file-no-regex={selection}']


def optimize(kind, compiler, library, runtime, output, keep=None):
    # LangArena opens more files than Docker's default soft descriptor limit.
    _, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    resource.setrlimit(resource.RLIMIT_NOFILE, (hard, hard))
    bolt = os.environ.get('LLVM_BOLT', 'llvm-bolt')
    merge = os.environ.get('MERGE_FDATA', 'merge-fdata')
    jobs = os.environ.get('HIRSEL_JOBS', '4')
    tool = [bolt, f'-thread-count={jobs}', '-relocs']
    with tempfile.TemporaryDirectory(prefix='hirsel-bolt-') as scratch:
        work = Path(scratch)
        source = library
        if kind == 'compiler':
            # BOLT updates DWARF addresses in uncompressed sections.
            source = work / 'hirsel'
            run([OBJCOPY, '--decompress-debug-sections', compiler, source], work)
        instrumented = shadow(library.parent, compiler, work / 'shadow')
        target = instrumented if kind == 'compiler' else shadow_engine(instrumented)
        staged = target.with_name('instrumented')
        batches = engine_shards(source, work) if kind == 'engine' else [(work, [])]
        recorded = set()
        for batch, selection in batches:
            if selection:
                print(f'bolt: instrumenting engine function group {batch.name}', flush=True)
            profile = batch / 'prof.fdata'
            run([*tool, source, '-instrument', *selection, f'-instrumentation-binpath={target}',
                 f'-instrumentation-file={profile}', '-instrumentation-file-append-pid', '-o', staged], work)
            staged.replace(target)
            recorded.update(train(instrumented, runtime, batch / 'training', profile))
            target.unlink()
            shutil.rmtree(batch / 'training')
        if missing := WORKLOADS - recorded:
            raise RuntimeError(f'no nonempty BOLT profile from {", ".join(sorted(missing))}')
        profile = work / 'merged.fdata'
        raw = sorted(path for path in work.rglob('prof.fdata.*') if path.stat().st_size)
        with profile.open('wb') as merged:
            run([merge, *raw], work, stdout=merged)
        if profile.stat().st_size == 0:
            raise RuntimeError('merge-fdata produced an empty profile')
        for path in raw:
            path.unlink()
        if keep is not None:
            shutil.copyfile(profile, keep / 'merged.fdata')
        with tempfile.TemporaryDirectory(prefix='.bolt-', dir=output.parent) as staging:
            candidate = Path(staging) / output.name
            # BOLT's note records its command line, whose paths are temporary
            # directories; the shipped compiler must not depend on them.
            note = ['--bolt-info=0'] if kind == 'compiler' else []
            run([*tool, source, '-data', profile, *OPTIMIZE, *note, '-o', candidate], work)
            if kind == 'compiler':
                run([OBJCOPY, '--compress-debug-sections=zlib', candidate], work)
                seconds = held_out({'input': compiler, 'bolt': candidate}, runtime, work)
                if keep is not None:
                    (keep / 'held-out.json').write_text(json.dumps(dict(workload=HELD_OUT, user_seconds=seconds)))
            candidate.replace(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='command', required=True)
    engine = commands.add_parser('engine')
    engine.add_argument('library', type=Path)
    engine.add_argument('output', type=Path)
    engine.add_argument('--artifacts', type=Path, required=True)
    compiler = commands.add_parser('compiler')
    compiler.add_argument('compiler', type=Path)
    compiler.add_argument('runtime', type=Path)
    compiler.add_argument('output', type=Path)
    compiler.add_argument('--keep', type=Path, help='copy the merged BOLT profile into this directory')
    args = parser.parse_args()
    if args.command == 'engine':
        artifacts = json.loads(args.artifacts.read_text())
        optimize('engine', Path(artifacts['compiler']).resolve(), args.library.resolve(),
                 Path(artifacts['runtime']).resolve(), args.output.absolute())
    else:
        optimize('compiler', args.compiler.resolve(), LLVM, args.runtime.resolve(), args.output.absolute(),
                 args.keep.resolve() if args.keep else None)


if __name__ == '__main__':
    main()
