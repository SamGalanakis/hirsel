#!/usr/bin/env python3
"""Codec encode against its Rust twin (core/twins/codec): rows and strings, LLVM release.

Builds core/twins/codec/codec.hirsel with --release and the Rust twin with the twins'
release profile, then measures instructions:u per element of one encode pass:
(MODE 1 - MODE 0) / (N * R), the median of K samples on one CPU under the shared
measurement lock. --check fails when Hirsel/Rust exceeds the ceiling on either row.

The ceilings ratchet: main's ratios (2.75 rows, 2.16 strings, the encoders behind call
boundaries) plus noise, so a regression fails while the row waits for an inlining rule that
prices the encoders by their hot bodies (about 1.3); they drop when that rule lands. The twin
is infallible (`extend_from_slice` aborts on OOM) where std.codec returns Capacity.
"""
import argparse, fcntl, json, os, statistics, subprocess, sys, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CEILING = {'rows': 2.80, 'strings': 2.20}
WORKS = {'rows': 0, 'strings': 2}
p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
p.add_argument('--builder', type=Path, required=True, help='the hirsel compiler')
p.add_argument('--runtime', type=Path, required=True, help='the runtime archive without recording')
p.add_argument('--out', type=Path, required=True)
p.add_argument('--cpu', default=os.environ.get('HIRSEL_MEASURE_CPU', '16'))
p.add_argument('--samples', type=int, default=5)
p.add_argument('--check', action='store_true', help='fail when a ratio exceeds its row ceiling')
a = p.parse_args()
out = a.out.resolve()
out.mkdir(parents=True, exist_ok=True)
T = ROOT / 'tools/buck2/toolchains/llvm-files'
env = dict(os.environ, HIRSEL_MEMORY_LIMIT='2G', HIRSEL_JOBS='1', LD_LIBRARY_PATH=str(T / 'lib'), LLVM_SYS_231_PREFIX=str(T),
           XDG_CACHE_HOME=str(out / 'cache'), XDG_DATA_HOME=str(out / 'data'))


def wait_load():
    while os.getloadavg()[0] > 32:
        time.sleep(15)


def sources(folder, signatures):
    return [{'file': str(f), 'modules': [f.stem], 'signatures': signatures} for f in sorted(folder.glob('*.hirsel'))]


core, std, app = ({'name': n, 'version': '1'} for n in ('core', 'std', 'app'))
manifest = out / 'inputs.json'
manifest.write_text(json.dumps({'core': core, 'packages': [
    {'package': core, 'sources': sources(ROOT / 'core/core', True), 'dependencies': []},
    {'package': std, 'sources': sources(ROOT / 'core/std', False), 'dependencies': [core]},
    {'package': app, 'sources': [{'file': str(ROOT / 'core/twins/codec/codec.hirsel'), 'modules': ['main'], 'signatures': False}],
     'dependencies': [core, std]}]}))
hirsel = out / 'codec-hirsel'
wait_load()
r = subprocess.run([str(a.builder.resolve()), 'build', '--inputs', str(manifest), '--entry', 'app@1/main/main', '--backend', 'llvm',
                    '--release', '--runtime', str(a.runtime.resolve()), '--grant', 'Stdout', '--store', str(out / 'store'),
                    '-o', str(hirsel)], cwd=ROOT, env=env, capture_output=True, text=True, timeout=900)
if r.returncode:
    sys.exit('hirsel build failed:\n' + r.stdout[-2000:] + r.stderr[-2000:])
wait_load()
r = subprocess.run(['cargo', 'build', '--release', '--quiet', '-p', 'codec', '--target-dir', str(out / 'cargo')],
                   cwd=ROOT / 'core/twins', capture_output=True, text=True, timeout=900)
if r.returncode:
    sys.exit('twin build failed:\n' + r.stderr[-2000:])
twin = out / 'cargo/release/codec'
N, R = 100_000, 20


def count(binary, work, mode):
    wait_load()
    with open('/tmp/hirsel-langarena-measure.lock', 'a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        r = subprocess.run(['timeout', '-s', 'KILL', '120', 'taskset', '-c', a.cpu, 'perf', 'stat', '-x,', '-e', 'instructions:u',
                            str(binary), str(work), str(mode), str(N), str(R)], capture_output=True, text=True)
    assert r.returncode == 0, (binary, work, mode, r.stderr[-800:])
    return int(r.stderr.strip().splitlines()[-1].split(',')[0]), r.stdout


rows, failures = [], []
for name, work in WORKS.items():
    per = {}
    for side, binary in (('hirsel', hirsel), ('rust', twin)):
        samples = []
        for _ in range(a.samples):
            (base, _), (enc, printed) = count(binary, work, 0), count(binary, work, 1)
            samples.append((enc - base) / (N * R))
            per[side + '_output'] = printed
        per[side] = statistics.median(samples)
    assert per['hirsel_output'] == per['rust_output'], (name, per['hirsel_output'], per['rust_output'])
    row = dict(row=name, hirsel=round(per['hirsel'], 1), rust=round(per['rust'], 1), ratio=round(per['hirsel'] / per['rust'], 3))
    rows.append(row)
    print(json.dumps(row), flush=True)
    if row['ratio'] > CEILING[name]:
        failures.append(row)
(out / 'rows.json').write_text(json.dumps(rows, indent=2) + '\n')
if a.check and failures:
    sys.exit('codec encode over its ceiling against the Rust twin: ' + json.dumps(dict(ceiling=CEILING, rows=failures)))
