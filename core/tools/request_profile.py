#!/usr/bin/env python3
"""Where a request's instructions go: one `perf record --call-graph dwarf`
sample of a request-cost row, classified by stage.

    python3 core/tools/request_profile.py ROW --compiler HIRSEL --runtime RT --out DIR [--period N]

ROW is `edit` (the suite with `let _ = 0u8` in dispatch, over a store holding
the unedited suite's build), `no-change` (the unedited suite again over that
store) or `hello` (a fresh program over a store holding another program's
build). The rows are request_cost.py's. Each sample is classified by the first
frame, leaf to root, that matches a rule in STAGES; the compiler's samples are
reported as shares, with the linker child's separately.
"""
import argparse
import collections
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'core/tools'))
import request_cost  # noqa: E402

# Stage, then a pattern over a demangled frame. The first rule a frame
# matches names it; the first named frame, leaf to root, names the sample.
STAGES = [
    ('toolchain observe (re-hash)', r'tool_inputs::observe'),
    ('link: open_objects', r'native::open_objects'),
    ('link: source map', r'native_sources|source_map|sources_digest'),
    ('link: sites', r'check_linked_sites'),
    ('link: entry plan emit', r'Plan>::emit|native_entry'),
    ('link: debug bind', r'native_debug'),
    ('link: exe publish/get', r'Store>::put_keyed|Store>::get\b|write_framed'),
    ('link: other', r'native::link_objects|direct_link'),
    ('reach: record decode', r'decode_record|reach::read'),
    ('reach: record write', r'reach::publish'),
    ('emit edited instance', r'emit_unit|cranelift'),
    ('reach: walk/compare/expand', r'reach::|reachable_for|build_reachable|WalkRows|instances::'),
    ('check: body keys', r'Keys>::(for_body|new|closure)'),
    ('check: prior rows', r'prior_definition|ProgramIndex'),
    ('check: check/lower', r'check_lower|Emitter'),
    ('front: declarations', r'declare_with_library|lower_signature|hirsel_core_layout'),
    ('front: resolve', r'hirsel_core_resolve'),
    ('front: image index', r'image_index|published::'),
    ('front: parse/lex', r'hirsel_core_syntax'),
    ('front: index', r'NameIndex|index_item'),
    ('load: source read+hash', r'ContentHash>::of_bytes'),
    ('check: other', r'Driver>::check'),
]

HELLO = '''pub fn main(out: Stdout) -> Result[(), Exit] waits {
    match stdout.write_bytes(out, string.as_bytes("%s")) {
        Ok(()) => Ok(()),
        Error(_) => Error(Exit(status: 1)),
    }
}
'''


def hello_inputs(out, name):
    spec = json.loads((out / 'suite-inputs.json').read_text())
    source = out / f'{name}.hirsel'
    source.write_text(HELLO % (name + '\\n'))
    spec['packages'][-1]['sources'] = [{'file': str(source), 'modules': [name], 'signatures': False}]
    path = out / f'{name}-inputs.json'
    path.write_text(json.dumps(spec))
    return path


def run(argv, out, log, tools):
    """One capped request with request_cost's environment: one job, 2 GiB, the
    pinned LLVM and link kit, and private caches."""
    env = dict(os.environ, HIRSEL_JOBS='1', HIRSEL_MEMORY_LIMIT='2G', LLVM_SYS_231_PREFIX=tools['llvm'],
               HIRSEL_LINK_KIT=tools['kit'], LD_LIBRARY_PATH=tools['llvm'] + '/lib',
               XDG_CACHE_HOME=str(out / 'xdg-cache'), XDG_DATA_HOME=str(out / 'xdg-data'))
    with open(log, 'w') as f:
        code = subprocess.run([str(ROOT / 'tools/capped'), *argv], stdout=f, stderr=subprocess.STDOUT,
                              env=env, cwd=ROOT).returncode
    if code:
        raise SystemExit(f'failed ({code}): {log}')


def frames(data, *, env=None):
    """Each sample as (command, [frame leaf to root])."""
    raw = subprocess.run(['perf', 'script', '-i', str(data), '-F', 'comm,ip,sym'], capture_output=True,
                         text=True, env=env, check=True).stdout
    raw = subprocess.run(['rustfilt'], input=raw, capture_output=True, text=True, check=True).stdout
    samples, current = [], None
    for line in raw.splitlines():
        if not line.strip():
            if current:
                samples.append(current)
            current = None
        elif not line.startswith((' ', '\t')) or current is None:
            current = (line.split()[0], [])
        else:
            parts = line.strip().split(None, 1)
            if len(parts) == 2:
                current[1].append(re.sub(r'::h[0-9a-f]{16}$', '', parts[1]))
    if current:
        samples.append(current)
    return samples


def classify(samples):
    rules = [(name, re.compile(pattern)) for name, pattern in STAGES]
    shares = collections.Counter()
    compiler = [stack for command, stack in samples if command.startswith('hirsel')]
    for stack in compiler:
        shares[next((name for frame in stack for name, rule in rules if rule.search(frame)), 'other')] += 1
    return len(samples), len(compiler), shares


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('row', choices=['edit', 'no-change', 'hello'])
    parser.add_argument('--compiler', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--period', type=int, default=300007)
    args = parser.parse_args()
    out = args.out.resolve()
    request_cost.manifests(ROOT, out)
    tools = request_cost.toolchain(ROOT)
    cc = Path(tools['clang'])

    def command(manifest, entry, store, tag):
        return request_cost.command(args.compiler, args.runtime, out, manifest, entry, store, tag, cc)

    suite = 'langarena@1/suite/main'
    base = out / 'base.store'
    shutil.rmtree(base, ignore_errors=True)
    if args.row == 'hello':
        run(command(hello_inputs(out, 'warm'), 'langarena@1/warm/main', base, 'base'), out, out / 'base.log', tools)
        manifest, entry = hello_inputs(out, 'hello'), 'langarena@1/hello/main'
    else:
        run(command(out / 'suite-inputs.json', suite, base, 'base'), out, out / 'base.log', tools)
        manifest = out / ('suite-edited-inputs.json' if args.row == 'edit' else 'suite-inputs.json')
        entry = suite
    store = out / f'{args.row}.store'
    shutil.rmtree(store, ignore_errors=True)
    shutil.copytree(base, store, symlinks=True)
    data = out / f'{args.row}.perf.data'
    run(['perf', 'record', '-e', 'instructions:u', '-c', str(args.period), '--call-graph', 'dwarf,32768',
         '-o', str(data), *command(manifest, entry, store, args.row)], out, out / f'{args.row}.log', tools)
    total, compiler, shares = classify(frames(data))
    print(f'{args.row}: {total} samples, {compiler} in the compiler, {total - compiler} in children')
    for name, n in shares.most_common():
        print(f'{100 * n / compiler:5.1f}%  {name}')


if __name__ == '__main__':
    main()
