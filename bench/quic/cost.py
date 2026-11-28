#!/usr/bin/env python3
"""The QUIC cost target (docs/spec/quic.md): one-stream bulk transfer on loopback at >= 50 %
of quinn's throughput per core, same cipher, GSO on both, same CPU; 0 allocations per
packet in steady state.

Two senders, Hirsel's (sender.hirsel, LLVM release, through quic.drive) and quinn's
(cost_quinn.rs, --release, current-thread tokio), each send BYTES on one unidirectional
stream to the same quinn receiver. Both offer only TLS13_AES_128_GCM_SHA256 and X25519,
keep datagrams at 1200 bytes and send GSO trains. The sender runs on CPU SENDER and the
receiver on CPU RECEIVER. Throughput per core is the stream's bytes over the sender's CPU
time (user + system, /proc/PID/stat) from READY to DONE; DONE waits for the receiver's
acknowledgement of the last byte and FIN. The pairs are interleaved and the medians reported.

Allocations come from the counted runtime between the same READY and DONE lines
(measurement_artifacts.allocation_linker), divided by the datagrams the receiver counted
on the connection. Payload copies are the bytes of stream data the Hirsel sender copied
into packets (quic.payload_bytes_copied, printed after DONE): a copy into scratch and one
more into the slot count twice. They are divided by the datagrams too.

The receive side is timed in process by batch.hirsel: one full 64-packet udp_recv batch of
1,100-byte datagrams through the endpoint, BATCH_ROUNDS times on CPU SENDER; the median,
10th and 90th percentile are reported.

--tree builds the Hirsel programs from another checkout's core/programs/quic and
bench/quic (a before arm) instead of this one.

`build` builds both senders and the receiver. `measure` runs the pairs and the count; run it
inside a shielded hold:
    python3 core/tools/timing_shield.py run -- python3 bench/quic/cost.py measure OUT
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import statistics
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'tools/toolchain-dist'))
import hcfg

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
QUIC = ROOT / 'core/programs/quic'
sys.path.insert(0, str(ROOT / 'core/tools'))
from measurement_artifacts import (  # noqa: E402
    ALLOCATION_RUNTIME_TARGET, COMPILER_TARGET, PLATFORM, RUNTIME_TARGET, allocation_linker,
    phase_allocations,
)

QUINN_COMMIT = '75d08d9'
BYTES = 1 << 30
PAIRS = 7
BATCH_ROUNDS = 2000
SENDER, RECEIVER = '16', '17'
GRANTS = ['--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
          '--grant', 'Net=UdpBind 127.0.0.1/32 1-65535;UdpSend 127.0.0.1/32 1-65535']
TICK = os.sysconf('SC_CLK_TCK')


def interop():
    spec = importlib.util.spec_from_file_location('quic_interop', QUIC / 'interop.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def library(name, extra=()):
    base = ROOT / 'core' / name
    files = sorted(base.glob('*.hirsel')) + [f for d in extra for f in sorted((base / d).glob('*.hirsel'))]
    return [{'file': str(f), 'modules': list(f.relative_to(base).with_suffix('').parts),
             'signatures': name == 'core'} for f in files]


def build_hirsel(out, tree):
    quic_dir, bench = tree / 'core/programs/quic', tree / 'bench/quic'
    paths = json.loads(subprocess.check_output([
        ROOT / '.buck2/bin/buck2', 'build', '-j4', '--target-platforms', '//tools/buck2:release',
        '--show-json-output', COMPILER_TARGET, RUNTIME_TARGET, ALLOCATION_RUNTIME_TARGET], cwd=ROOT))
    compiler = ROOT / paths['root' + COMPILER_TARGET]
    cc = ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang'
    core, std, quic = ({'name': n, 'version': '1'} for n in ('core', 'std', 'quic'))
    manifest = hcfg.package(quic_dir)
    sources = dict(manifest['sources'])

    def write_inputs(name, program, extra):
        inputs = out / f'inputs-{name}.json'
        inputs.write_text(json.dumps({'core': core, 'packages': [
            {'package': core, 'dependencies': [], 'sources': library('core')},
            {'package': std, 'dependencies': [core], 'sources': library('std', ['sim'])},
            {'package': quic, 'dependencies': [core, std],
             'sources': [{'file': str(quic_dir / f), 'modules': m.split('.'), 'signatures': False}
                         for f, m in (sources | extra).items()]},
            {'package': {'name': 'quic_cost', 'version': '1'}, 'dependencies': [core, std, quic],
             'sources': [{'file': str(bench / program), 'modules': ['main'], 'signatures': False}]},
        ]}))
        return inputs

    shipping = ROOT / paths['root' + RUNTIME_TARGET] / PLATFORM / 'libhirsel_core_rt.a'
    counted = ROOT / paths['root' + ALLOCATION_RUNTIME_TARGET] / PLATFORM / 'libhirsel_core_rt.a'
    # The batch bench hands its batch over through the test seam drive uses.
    seam = {'test_seams.hirsel': manifest['tests']['sources']['test_seams.hirsel']}
    for name, inputs, runtime, linker, grants in (
            ('shipping', write_inputs('sender', 'sender.hirsel', {}), shipping, cc, GRANTS),
            ('counted', write_inputs('sender', 'sender.hirsel', {}), counted, allocation_linker(cc, out), GRANTS),
            ('batch', write_inputs('batch', 'batch.hirsel', seam), shipping, cc, GRANTS[:6])):
        subprocess.run([compiler, 'build', '--release', '--backend', 'llvm', '--source-core',
                        '--inputs', inputs, '--entry', 'quic_cost@1/main/main', '--limit', str(1 << 30),
                        '--runtime', runtime, '--cc', linker, *grants, '--store', out / 'store',
                        '-o', out / f'hirsel-{name}'], cwd=ROOT, check=True,
                       env=os.environ | {'HIRSEL_MEMORY_LIMIT': '2G'})


def build_quinn(out, quinn):
    head = subprocess.check_output(['git', '-C', str(quinn), 'rev-parse', 'HEAD'], text=True).strip()
    if not head.startswith(QUINN_COMMIT):
        raise SystemExit(f'{quinn} is at {head}, not quinn {QUINN_COMMIT}')
    crate = out / 'quinn-cost'
    (crate / 'src').mkdir(parents=True, exist_ok=True)
    (crate / 'src/main.rs').write_text((HERE / 'cost_quinn.rs').read_text())
    (crate / 'Cargo.lock').write_text((quinn / 'Cargo.lock').read_text())
    (crate / 'Cargo.toml').write_text(
        '[package]\nname = "cost_quinn"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n\n'
        f'[dependencies]\nquinn = {{ path = "{quinn}/quinn" }}\n'
        'rustls = { version = "0.23.5", default-features = false, features = ["std", "ring"] }\n'
        'ring = "0.17"\nbytes = "1"\ntokio = { version = "1.28.1", features = ["rt", "macros", "time"] }\n\n'
        '[profile.release]\ndebug = "line-tables-only"\n\n[workspace]\n')
    subprocess.run(['cargo', 'build', '--release', '-j8', '--quiet'], cwd=crate, check=True,
                   env=os.environ | {'CARGO_TARGET_DIR': str(crate / 'target')})
    (out / 'cost_quinn').unlink(missing_ok=True)
    (out / 'cost_quinn').symlink_to(crate / 'target/release/cost_quinn')


def cpu_seconds(pid):
    fields = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
    return (int(fields[11]) + int(fields[12])) / TICK


def transfer(out, sender, cert, nbytes, counted=False):
    """One transfer; returns sender CPU seconds, wall seconds, the receiver's datagram
    count, stderr and the lines the sender printed after DONE."""
    receiver = subprocess.Popen(['taskset', '-c', RECEIVER, out / 'cost_quinn', 'receive', cert['der'], cert['key_der']],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        port = receiver.stdout.readline().split()[1]
        argv = [out / 'cost_quinn', 'send'] if sender == 'quinn' else [out / f'hirsel-{"counted" if counted else "shipping"}']
        child = subprocess.Popen(['taskset', '-c', SENDER, *argv, port, cert['pin'], str(nbytes)],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                 env=os.environ | {'HIRSEL_MEMORY_LIMIT': '2G'})
        if child.stdout.readline() != 'READY\n':
            raise RuntimeError(f'{sender}: no READY: {child.communicate()}')
        cpu, wall = cpu_seconds(child.pid), time.monotonic()
        done = child.stdout.readline()
        cpu, wall = cpu_seconds(child.pid) - cpu, time.monotonic() - wall
        # communicate() with a timeout reads the pipe beneath readline()'s buffer, which may
        # already hold the lines after DONE.
        rest = child.stdout.read()
        errors = child.stderr.read()
        child.wait(timeout=30)
        if done != f'DONE {nbytes}\n' or child.returncode:
            raise RuntimeError(f'{sender}: {done!r} {rest!r} exit {child.returncode}\n{errors[-4000:]}')
        line = receiver.stdout.readline().split()
        receiver.wait(timeout=30)
        if line[:2] != ['received', str(nbytes)]:
            raise RuntimeError(f'receiver: {line}')
        return cpu, wall, int(line[2]), errors, rest
    finally:
        if receiver.poll() is None:
            receiver.terminate()
            receiver.wait()


def batch(out, rounds):
    """Time `rounds` full 64-packet receive batches; ns at the median, p10 and p90."""
    lines = subprocess.run(['taskset', '-c', SENDER, out / 'hirsel-batch', str(rounds)], check=True,
                           capture_output=True, text=True, env=os.environ | {'HIRSEL_MEMORY_LIMIT': '2G'}).stdout
    ns = sorted(int(w[1]) for w in (l.split() for l in lines.splitlines()) if w[:1] == ['BATCH'])
    if len(ns) != rounds:
        raise RuntimeError(f'batch: {len(ns)} of {rounds} rounds timed')
    return {'rounds': rounds, 'median_ns': statistics.median(ns),
            'p10_ns': ns[rounds // 10], 'p90_ns': ns[rounds * 9 // 10]}


def measure(out, nbytes, pairs):
    cert = interop().certificate(out, 'receiver')
    rows = {'hirsel': [], 'quinn': []}
    copies = []
    for _ in range(pairs):
        for sender in ('hirsel', 'quinn'):
            cpu, wall, datagrams, _, rest = transfer(out, sender, cert, nbytes)
            rows[sender].append((cpu, wall, datagrams))
            # The counted run's phase gate holds it at DONE, so copies come from these runs.
            copies += [(int(w[1]), datagrams) for w in (l.split() for l in rest.splitlines()) if w[:1] == ['COPIED']]
            print(f'{sender} cpu {cpu:.3f} s wall {wall:.3f} s datagrams {datagrams}', flush=True)
    result = {}
    for sender, samples in rows.items():
        cpu = statistics.median(s[0] for s in samples)
        wall = statistics.median(s[1] for s in samples)
        result[sender] = {'cpu_s': cpu, 'wall_s': wall, 'gbit_per_cpu_s': nbytes * 8 / cpu / 1e9,
                          'gbit_per_s': nbytes * 8 / wall / 1e9, 'datagrams': statistics.median(s[2] for s in samples)}
    result['ratio_per_core'] = result['hirsel']['gbit_per_cpu_s'] / result['quinn']['gbit_per_cpu_s']
    _, _, datagrams, errors, _ = transfer(out, 'hirsel', cert, nbytes, counted=True)
    allocations = phase_allocations(errors)
    result['allocations'] = allocations
    result['allocations_per_datagram'] = allocations / datagrams
    if copies:
        copied, sent = copies[-1]
        result['payload_bytes_copied'] = copied
        result['payload_bytes_copied_per_datagram'] = copied / sent
        result['payload_copies_per_stream_byte'] = copied / nbytes
    result['batch'] = batch(out, BATCH_ROUNDS)
    result['bytes'] = nbytes
    (out / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result, indent=2))
    return 0 if result['ratio_per_core'] >= 0.5 and allocations == 0 else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('command', choices=['build', 'measure', 'batch'])
    parser.add_argument('output', type=Path)
    parser.add_argument('--quinn', type=Path, default=Path('/tmp/ref-quinn'))
    parser.add_argument('--bytes', type=int, default=BYTES)
    parser.add_argument('--pairs', type=int, default=PAIRS)
    parser.add_argument('--tree', type=Path, default=ROOT)
    parser.add_argument('--rounds', type=int, default=BATCH_ROUNDS)
    args = parser.parse_args()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=True)
    if args.command == 'build':
        build_hirsel(out, args.tree.resolve())
        build_quinn(out, args.quinn.resolve())
        return 0
    if args.command == 'batch':
        print(json.dumps(batch(out, args.rounds), indent=2))
        return 0
    return measure(out, args.bytes, args.pairs)


if __name__ == '__main__':
    sys.exit(main())
