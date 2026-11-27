#!/usr/bin/env python3
"""P0 baseline for in-process Place packing, plus the standing isolate-call row.

Run from the repository with the release compiler and runtime archive:
  python3 crates/hirsel-core-rt/measure-isolate-call.py --hirsel <hirsel> --runtime <libhirsel_core_rt.a>

Select the remaining items with --items metering parked layout codebytes loader.
The script builds selected Hirsel fixtures on Cranelift and LLVM (LLVM with --release),
then reports JSON and human-readable rows for:
1. codec and typed no-codec contained calls (instructions and ns per call);
2. metered/unmetered tight loops copied from core/langarena and bench/awfy;
3. parked-unit RSS and VMA cost at 0, 1k, 10k and, when the 2 GiB cap allows, 50k;
4. runtime record sizes and Context field offsets;
5. incremental executable text/data bytes of minimal Places;
6. the code loader's cached load (G3) and `activate` under 16 dispatching workers (G1c).

Time and instruction rows are slopes: the difference between 2n and n, divided
by n. Wall medians use 60 interleaved pairs. Measurements wait for one-minute
load below 32 and are repeated if the load
rose during a run. Exits 1 when the standing codec isolate call is over its
MEASUREMENTS.md line.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import re
import signal
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[2]
CRATE = ROOT / 'crates/hirsel-core-rt'
LLVM = ROOT / 'tools/buck2/toolchains/llvm-files'
N = 20_000
# 1,532 (LLVM) and 3,018.5 (Cranelift) per call on 2026-11-16, plus 0.5%.
LINES = {'llvm': 1_540, 'cranelift': 3_034}
ENGINES = ('cranelift', 'llvm')
# Corpus sources and repetition counts for each shorter arm of the slope.
LOOP_KERNELS = [
    ('crc32', 'core/langarena/hash.hirsel crc32', 1_500),
    ('hash_mix', 'core/langarena/hash.hirsel sha256 (mixing kernel)', 5_000),
    ('sieve', 'core/langarena/sieve.hirsel sieve', 10_000),
    ('nbody', 'bench/awfy/hirsel/numeric.hirsel advance', 1_000_000),
]
PARKED_50K = 50_000
SECTION_TEXT = ('.text', '.plt')
SECTION_DATA = ('.rodata', '.data', '.got', '.init_array', '.fini_array')
CAPPED = str(ROOT / 'tools/capped')
ITEMS = ('calls', 'metering', 'parked', 'layout', 'codebytes', 'loader')
CAP_BYTES = 2 << 30
TIMING_PAIRS = 60

sys.path.insert(0, str(ROOT / 'core/tools'))
import timing_shield
from store_tests import inputs


def load1():
    return os.getloadavg()[0]


def wait_for_load():
    while (load := load1()) >= 32:
        print(f'measurement deferred: load {load:.2f} >= 32', flush=True)
        time.sleep(10)


def stable_sample(collect):
    while True:
        wait_for_load()
        before = load1()
        if before >= 32:
            continue
        result = collect()
        after = load1()
        if after <= before:
            return dict(result, load_before=before, load_after=after)
        print(f'measurement repeated: load rose {before:.2f} -> {after:.2f}', flush=True)


def measure_env(tmp):
    return dict(os.environ, LD_LIBRARY_PATH=str(LLVM / 'lib'), HIRSEL_MEMORY_LIMIT='2G',
                HIRSEL_JOBS='1', XDG_CACHE_HOME=str(Path(tmp) / 'cache'),
                XDG_DATA_HOME=str(Path(tmp) / 'data'))


def perf_once(exe, args, env, event, prefix=()):
    q = subprocess.run([*prefix, 'perf', 'stat', '-x,', '-e', event, '--',
                        exe, *map(str, args)], capture_output=True, text=True, env=env)
    for line in q.stderr.splitlines():
        fields = line.split(',')
        if q.returncode == 0 and len(fields) > 2 and fields[2] == event and fields[0].strip().isdigit():
            return int(fields[0])
    raise RuntimeError(f'perf {event} failed for {exe} {args}: {q.stderr}')


def measure_pair(exe, arms, n, env):
    counts = (n, 2 * n)
    commands = [(name, [*args, count]) for count in counts for name, args in arms.items()]

    def collect():
        instructions = [perf_once(exe, args, env, 'instructions:u', [CAPPED])
                        for _, args in commands]
        times = [[] for _ in commands]
        with timing_shield.hold(timing_shield.REGIMES, timing_shield.BOARD, memory_bytes=CAP_BYTES):
            for pair in range(TIMING_PAIRS):
                order = range(len(commands))
                for i in (order if pair % 2 == 0 else reversed(order)):
                    times[i].append(perf_once(exe, commands[i][1], env, 'duration_time',
                                              ['taskset', '-c', '16']))
        rows = {}
        for i, name in enumerate(arms):
            j = i + len(arms)
            rows[name] = {
                'instructions': (instructions[j] - instructions[i]) / n,
                'ns': (statistics.median(times[j]) - statistics.median(times[i])) / n,
            }
        return rows

    return stable_sample(collect)


def build_program(hirsel, runtime, source, engine, output, env, limit, grants=(), runner=False):
    private_runtime = output.parent / 'runtime'
    private_runtime.mkdir(mode=0o700, exist_ok=True)
    command = [CAPPED, 'timeout', '300', 'env', f'XDG_RUNTIME_DIR={private_runtime}',
               str(hirsel), 'build']
    if runner:
        manifest = inputs(ROOT, package='runner')
        manifest['packages'].append({
            'package': {'name': 'packing_probe', 'version': '1'},
            'dependencies': [{'name': name, 'version': '1'} for name in ('core', 'std', 'runner')],
            'sources': [{'file': str(source / 'main.hirsel'), 'modules': ['main'], 'signatures': False}],
        })
        inputs_path = output.with_suffix('.json')
        inputs_path.write_text(json.dumps(manifest))
        command += ['--source-core', '--source-std', '--inputs', str(inputs_path),
                    '--entry', 'packing_probe@1/main/main']
    else:
        command += [str(source), '--libraries', str(ROOT / 'core')]
    command += ['--runtime', str(runtime), '--cc', str(LLVM / 'bin/clang'), '--limit', str(limit),
                '--store', str(output.with_suffix('.store')), '--backend', engine, '-o', str(output)]
    for grant in grants:
        command += ['--grant', grant]
    if engine == 'llvm':
        command.append('--release')
    subprocess.run(command, env=env, check=True)


def measure_calls(engine, exe, env):
    row = measure_pair(exe, {'codec': [2], 'typed_no_codec': [4]}, N, env)
    codec = row['codec']
    typed = row['typed_no_codec']
    row['codec_line'] = LINES[engine]
    row['codec_over_line'] = codec['instructions'] > LINES[engine]
    print(f'{engine} codec {codec["instructions"]:,.1f} instr {codec["ns"]:.1f} ns/call, '
          f'typed {typed["instructions"]:,.1f} instr {typed["ns"]:.1f} ns/call', flush=True)
    return row


def measure_metering(engine, exe, env):
    rows = {}
    for index, (name, source, repetitions) in enumerate(LOOP_KERNELS):
        row = measure_pair(exe, {'unmetered': [index, 0], 'metered': [index, 1]}, repetitions, env)
        row['source'] = source
        for metric in ('ns', 'instructions'):
            row[f'{metric}_overhead_percent'] = (row['metered'][metric] / row['unmetered'][metric] - 1) * 100
        rows[name] = row
        print(f'{engine} {name} metering {row["ns_overhead_percent"]:.2f}% ns, '
              f'{row["instructions_overhead_percent"]:.2f}% instr', flush=True)
    return rows


def read_mappings(pid):
    bins = {}
    header = re.compile(r'^([0-9a-f]+)-([0-9a-f]+) (\S+) \S+ \S+ \S+\s*(.*)$')
    with open(f'/proc/{pid}/smaps') as smaps:
        for line in smaps:
            match = header.match(line)
            if match:
                start, end, perms, path = match.groups()
                size = (int(end, 16) - int(start, 16)) // 1024
                key = (size, perms, path.strip() or '[anon]')
                current = bins.setdefault(key, {'size_kib': size, 'perms': perms,
                                                 'path': key[2], 'count': 0, 'rss_kib': 0})
                current['count'] += 1
            elif line.startswith('Rss:'):
                current['rss_kib'] += int(line.split()[1])
    return {
        'rss_kib': sum(row['rss_kib'] for row in bins.values()),
        'vmas': sum(row['count'] for row in bins.values()),
        'stack_rss_kib': sum(bins.get((size, 'rw-p', '[anon]'), {}).get('rss_kib', 0)
                             for size in (64, 1024)),
        'smaps_top_bins': sorted(bins.values(), key=lambda row: row['rss_kib'], reverse=True)[:20],
    }


def wait_parked(pid):
    # The root has published that all helpers were spawned. Let scheduler
    # workers finish dispatching the helpers and enter their idle waits.
    quiet = 0
    last = None
    while quiet < 3:
        time.sleep(0.25)
        fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        ticks = int(fields[11]) + int(fields[12])
        quiet = quiet + 1 if ticks == last else 0
        last = ticks


def parked_sample(exe, shape, count, env):
    command = [CAPPED, 'sh', '-c', 'echo "$$"; exec "$@"', 'sh', exe, str(shape), str(count)]
    with subprocess.Popen(command, env=dict(env, HIRSEL_UNTIED='4', HIRSEL_JOBS='4'),
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True) as proc:
        pid = proc.stdout.readline().strip()
        if not pid.isdecimal():
            return {'failed': proc.stderr.read().strip(), 'exit': proc.wait()}
        try:
            pidfd = os.pidfd_open(int(pid))
        except ProcessLookupError:
            return {'failed': proc.stderr.read().strip(), 'exit': proc.wait()}
        try:
            if proc.stdout.readline().strip() != 'ready':
                return {'failed': proc.stderr.read().strip(), 'exit': proc.wait()}
            wait_parked(pid)
            return read_mappings(pid)
        finally:
            try:
                proc.stdin.close()
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                comm = subprocess.run(['ps', '-o', 'comm=', '-p', pid],
                                      capture_output=True, text=True).stdout.strip()
                if comm == Path(exe).name[:15]:
                    try:
                        signal.pidfd_send_signal(pidfd, signal.SIGTERM)
                    except ProcessLookupError:
                        pass
                proc.wait()
            finally:
                os.close(pidfd)


def measure_parked(engine, exe, env):
    shapes = ('minimal_parked_waits_units', 'parked_units_1kib_live', 'dormant_compute_places')
    rows = {}
    for shape, name in enumerate(shapes):
        samples = {}
        for count in (0, 1_000, 10_000, PARKED_50K):
            sample = stable_sample(lambda: parked_sample(exe, shape, count, env))
            samples[count] = sample
            if 'failed' in sample:
                print(f'{engine} {name} {count}: {sample["failed"]}', flush=True)
                match = re.search(r'max_units=(\d+)', sample['failed'])
                if count != PARKED_50K or not match:
                    raise RuntimeError(f'parked fixture stopped at {count}: {sample["failed"]}')
                # The admission count includes the root Unit.
                count = int(match[1]) - 1
                sample = stable_sample(lambda: parked_sample(exe, shape, count, env))
                samples[count] = sample
            if 'failed' in sample:
                raise RuntimeError(f'parked fixture stopped at {count}: {sample["failed"]}')
            if count:
                base = samples[0]
                sample['incremental_rss_bytes_each'] = (sample['rss_kib'] - base['rss_kib']) * 1024 / count
                sample['incremental_vmas_each'] = (sample['vmas'] - base['vmas']) / count
                sample['stack_rss_bytes_each'] = (sample['stack_rss_kib'] - base['stack_rss_kib']) * 1024 / count if shape < 2 else 0
            print(f'{engine} {name} {count}: {sample["rss_kib"]:,} KiB, {sample["vmas"]:,} VMAs '
                  f'(load {sample["load_before"]:.2f} -> {sample["load_after"]:.2f})', flush=True)
        rows[name] = samples
    return rows


def copy_workspace_for_layout(tmp):
    copy = Path(tmp) / 'layout-workspace'
    copy.mkdir()
    shutil.copy2(ROOT / 'Cargo.toml', copy / 'Cargo.toml')
    shutil.copy2(ROOT / 'Cargo.lock', copy / 'Cargo.lock')
    shutil.copy2(ROOT / 'rust-toolchain.toml', copy / 'rust-toolchain.toml')
    shutil.copytree(ROOT / 'crates', copy / 'crates')
    lib = copy / 'crates/hirsel-core-rt/src/lib.rs'
    context = lib.read_text().split('pub struct Context {', 1)[1].split('\n}', 1)[0]
    # A field under #[cfg] is absent from this build and has no offset to report.
    context = re.sub(r'^    #\[cfg\([^\n]*\n[^\n]*\n', '', context, flags=re.MULTILINE)
    fields = re.findall(r'^    (?:pub(?:\(crate\))? )?(\w+):', context, re.MULTILINE)
    tasks = lib.with_name('tasks.rs')
    with tasks.open('a') as target:
        target.write('\npub(crate) const PACKING_SCHEDULER_SIZE: usize = size_of::<Scheduler>();\n')
    lines = ['', '#[doc(hidden)]', 'pub fn packing_p0_layout() {',
             '    println!("size Unit {}", std::mem::size_of::<units::Unit>());',
             '    println!("size LangUnit {}", std::mem::size_of::<units_lang::LangUnit>());',
             '    println!("size Task {}", std::mem::size_of::<tasks::Task>());',
             '    println!("size Context {}", std::mem::size_of::<Context>());',
             '    println!("size Scheduler {}", tasks::PACKING_SCHEDULER_SIZE);',
             '    println!("size Readies {}", std::mem::size_of::<tasks::Readies>());',
             '    println!("size UnitCtx {}", std::mem::size_of::<units::ledger::UnitCtx>());',
             '    println!("size InboxH {}", std::mem::size_of::<units_lang::InboxH>());',
             '    println!("size HelperH {}", std::mem::size_of::<units_lang::HelperH>());']
    lines += [f'    println!("offset Context.{name} {{}}", std::mem::offset_of!(Context, {name}));' for name in fields]
    lines.append('}')
    with lib.open('a') as target:
        target.write('\n'.join(lines) + '\n')
    example = copy / 'crates/hirsel-core-rt/examples/packing_layout.rs'
    example.write_text('fn main() { hirsel_core_rt::packing_p0_layout() }\n')
    subprocess.run(['cargo', 'build', '--quiet', '--locked', '--features', 'standalone',
                    '-p', 'hirsel-core-rt', '--example', 'packing_layout', '--target-dir', 'target'],
                   cwd=copy, check=True)

    def collect():
        return {'output': subprocess.check_output([copy / 'target/debug/examples/packing_layout'],
                                                  text=True)}

    sample = stable_sample(collect)
    sizes = {}
    offsets = {}
    for line in sample.pop('output').splitlines():
        parts = line.split()
        if len(parts) == 3 and parts[0] == 'size':
            sizes[parts[1]] = int(parts[2])
        elif len(parts) == 3 and parts[0] == 'offset':
            offsets[parts[1].removeprefix('Context.')] = int(parts[2])
    if len(sizes) != 9 or set(offsets) != set(fields):
        raise RuntimeError('incomplete runtime layout probe')
    return dict(sample, sizes=sizes, context_offsets=offsets)


def size_sections(exe):
    q = subprocess.check_output(['readelf', '-S', '-W', str(exe)], text=True)
    sizes = {'text': 0, 'data': 0}
    for line in q.splitlines():
        match = re.match(r'\s*\[\s*\d+\]\s+(\S+)\s+\S+\s+[0-9a-f]+\s+[0-9a-f]+\s+([0-9a-f]+)', line)
        if not match:
            continue
        name, size = match.group(1), int(match.group(2), 16)
        if any(name.startswith(prefix) for prefix in SECTION_TEXT):
            sizes['text'] += size
        if any(name.startswith(prefix) for prefix in SECTION_DATA):
            sizes['data'] += size
    return sizes


def minimal_source(count):
    # Keep the dispatch machinery common to every image. Distinct bodies and
    # runtime index selection keep each entry reachable through a StaticFn.
    entries = [
        f'fn entry_{i}(take x: u64) -> Result[u64, Capacity] {{\n'
        f'    Ok(int.wrapping_add(x, {i + 1}u64))\n}}'
        for i in range(count)
    ]
    descriptions = ', '.join(f'code.describe(entry_{i})' for i in range(count))
    return '\n\n'.join(entries) + f'''

pub fn main(argv: Args) -> Result[(), Exit] {{
    let Some(ns) = args.get(argv, 0) else {{ return Error(Exit(status: 2)) }}
    let pick = result.unwrap(std.int.parse_uint(ns, 10u32))
    if pick >= {count}u64 {{ return Error(Exit(status: 2)) }}
    let entries: Inline[StaticFn[fn(take u64) -> Result[u64, Capacity]], {count}] = [{descriptions}]
    let mut r: runner.Registry[(), ()] = runner.registry()
    for i in 0u64 .. {count}u64 {{
        result.unwrap(runner.compute(mut r, entries[i], 4096u64, runner.DEFAULT_STEPS))
    }}
    let fp = result.unwrap(std.codec.schema_fingerprint(std.codec.of_u64()))
    let key = runner.entry_key(code.identity(entries[pick]), fp, fp)
    let mut arg: Bytes = list.new()
    std.codec.put_u64(10u64, mut arg)
    match runner.call(r, key, arg) {{
        Returned(b) => {{
            let mut at = 0u64
            if result.unwrap(std.codec.take_u64(b, mut at)) == 11u64 + pick {{
                Ok(())
            }} else {{
                Error(Exit(status: 1))
            }}
        }},
        _ => Error(Exit(status: 1)),
    }}
}}
'''


def measure_code_bytes(hirsel, runtime, engine, tmp, env):
    sections = {}
    # Keep one entry in the control so all arms include the shared adapter.
    counts = (1, 17, 33)
    for count in counts:
        source = tmp / f'codebytes-{engine}-{count}'
        source.mkdir()
        (source / 'main.hirsel').write_text(minimal_source(count))
        exe = tmp / f'{source.name}.exe'
        build_program(hirsel, runtime, source, engine, exe, env, 1_048_576, runner=True)
        subprocess.run([CAPPED, exe, str(count - 1)], env=env, check=True)
        sections[count] = stable_sample(lambda: size_sections(exe))
    first, last = counts[0], counts[-1]
    text = (sections[last]['text'] - sections[first]['text']) / (last - first)
    data = (sections[last]['data'] - sections[first]['data']) / (last - first)
    print(f'{engine} small Place code bytes: text {text:.1f}, data {data:.1f}', flush=True)
    return {'sections': sections, 'text_bytes_per_place': text,
            'data_bytes_per_place': data, 'total_bytes_per_place': text + data}


def measure_loader():
    """The rows `crates/hirsel-code/examples/gates.rs` prints, in microseconds."""
    wait_for_load()
    said = subprocess.check_output(
        ['cargo', 'run', '--offline', '--release', '-q', '-p', 'hirsel-code', '--example', 'gates'],
        cwd=ROOT, text=True)
    units = {'ns': 1e-3, 'µs': 1.0, 'ms': 1e3, 's': 1e6}
    rows = {}
    for line in said.splitlines():
        print(line, flush=True)
        name, _, rest = line.partition(': ')
        found = re.findall(r'(p50|p99|p99\.9|max) ([0-9.]+)(ns|µs|ms|s)\b', rest)
        if found:
            rows[name] = {key: float(value) * units[unit] for key, value, unit in found}
    return rows


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--hirsel', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--json-out', type=Path)
    parser.add_argument('--items', nargs='+', default=['calls'], choices=ITEMS)
    args = parser.parse_args()
    hirsel = args.hirsel.resolve(strict=True)
    runtime = args.runtime.resolve(strict=True)
    result = {
        'host': platform.node(),
        'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
    }

    def save():
        if args.json_out:
            args.json_out.parent.mkdir(parents=True, exist_ok=True)
            args.json_out.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')

    fixtures = {
        'calls': ('isolate-call', 1 << 20, ('Tasks',), measure_calls),
        'metering': ('loop-metering', 16 << 20, (), measure_metering),
        'parked': ('parked-units', 512 << 20, ('Tasks', 'Stdin', 'Stdout'), measure_parked),
    }
    with tempfile.TemporaryDirectory(prefix='hirsel-packing-p0-') as directory:
        tmp = Path(directory)
        env = measure_env(tmp)
        for item in dict.fromkeys(args.items):
            if item == 'layout':
                result[item] = copy_workspace_for_layout(tmp)
                save()
                continue
            if item == 'loader':
                result[item] = measure_loader()
                save()
                continue
            result[item] = {}
            for engine in ENGINES:
                if item == 'codebytes':
                    row = measure_code_bytes(hirsel, runtime, engine, tmp, env)
                else:
                    fixture, limit, grants, measure = fixtures[item]
                    exe = tmp / f'{fixture}-{engine}'
                    build_program(hirsel, runtime, CRATE / 'measure' / fixture, engine,
                                  exe, env, limit, grants, runner=item == 'parked')
                    row = measure(engine, exe, env)
                result[item][engine] = row
                save()

    print(json.dumps(result, indent=2, sort_keys=True))
    over = [engine for engine, row in result.get('calls', {}).items() if row['codec_over_line']]
    if over:
        print(f'an isolate call is over its line on {", ".join(over)}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
