#!/usr/bin/env python3
"""Build and measure three Fleet processes in three Linux network namespaces.

run.py build COMPILER RUNTIME WORK [--release]
run.py measure WORK [--shield] [--scenario rpc|move|bench] [--concurrency N] [--creates N] [--cpus A/B/C]

Scenario rpc joins the roster and reads one Directory row. Scenario move moves
a partition between the workers, replays a stale claim and drains worker 1.
Scenario bench runs compute@1 creates: node 0 is the credit desk, node 1 the
dispatcher, node 2 the worker. Under --shield all three nodes share CPU 16
unless --cpus names a reserved CPU list for each node, as in 1/2/3.
"""
import argparse
from contextlib import ExitStack
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]

sys.path[:0] = [str(ROOT / 'tools/toolchain-dist'), str(ROOT / 'core/tools')]
import hcfg
from bench_cpus import cpu_list


def build(args):
    from store_tests import inputs
    corpus = inputs(ROOT, package='fleet')
    fleet = corpus['packages'][-1]
    # inputs() uses dependency aliases. CorpusManifest needs package identities.
    bindings = hcfg.package(HERE.parent)['dependencies']
    for edge in fleet['dependencies']:
        alias = edge['name']
        edge['name'] = bindings.get(alias, {}).get('package', alias)
    for source in (HERE.parent / 'cluster_laws.hirsel', HERE / 'node.hirsel', HERE / 'move.hirsel', HERE / 'bench.hirsel'):
        fleet['sources'].append(dict(file=str(source), modules=['laws'], signatures=False))
    args.work.mkdir(parents=True, exist_ok=True)
    manifest = args.work / 'inputs.json'
    manifest.write_text(json.dumps(corpus))
    subprocess.run([
        str(args.compiler), 'build', '--source-core', '--inputs', manifest,
        '--entry', 'hirsel.ai/fleet@1/laws/isolated_node', '--dir', '.',
        '--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
        '--grant', 'Net=UdpBind 10.77.0.0/24 7339-7339;UdpSend 10.77.0.0/24 7339-7339',
        '--limit', '134217728', '--runtime', args.runtime, '--store', args.work / 'probe-store',
        '-o', args.work / 'isolated-node', *(['--release'] if args.release else []),
    ], cwd=ROOT, check=True)


def ip(*args):
    subprocess.run(['sudo', '-n', 'ip', *args], check=True)


def namespaces(stack, prefix):
    switch = prefix + '-sw'
    nodes = [f'{prefix}-{n}' for n in range(3)]
    for ns in [switch, *nodes]:
        ip('netns', 'add', ns)
        stack.callback(ip, 'netns', 'delete', ns)
    ip('-n', switch, 'link', 'add', 'br0', 'type', 'bridge')
    ip('-n', switch, 'link', 'set', 'br0', 'up')
    for n, node in enumerate(nodes):
        port = f'p{n}'
        ip('-n', switch, 'link', 'add', port, 'type', 'veth', 'peer', 'name', 'eth0', 'netns', node)
        ip('-n', switch, 'link', 'set', port, 'master', 'br0')
        ip('-n', switch, 'link', 'set', port, 'up')
        ip('-n', node, 'addr', 'add', f'10.77.0.{n + 1}/24', 'dev', 'eth0')
        ip('-n', node, 'link', 'set', 'eth0', 'up')
        ip('-n', node, 'link', 'set', 'lo', 'up')


def openssl(*args, data=None):
    return subprocess.run(['openssl', *args], input=data, capture_output=True, check=True).stdout


def credentials(run):
    for n in range(3):
        host = run / str(n)
        host.mkdir(mode=0o700)
        key, cert = host / 'key.pem', host / 'cert.der'
        openssl('req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
                '-nodes', '-keyout', key, '-outform', 'DER', '-out', cert, '-days', '1',
                '-subj', f'/CN=fleet-{n}')
        openssl('pkcs8', '-topk8', '-nocrypt', '-in', key, '-outform', 'DER', '-out', host / 'key.der')
        pub = openssl('x509', '-inform', 'DER', '-in', cert, '-noout', '-pubkey')
        spki = openssl('pkey', '-pubin', '-outform', 'DER', data=pub)
        (run / '0' / f'pin{n}').write_bytes(hashlib.sha256(spki).digest())


SCENARIOS = {'rpc': 0, 'move': 1, 'bench': 2}


def cpu_ns(proc):
    """On-CPU nanoseconds of every thread of the node binary under `proc`."""
    pid = proc.pid
    while Path(f'/proc/{pid}/comm').read_text().strip() != 'isolated-node':
        pid = int(Path(f'/proc/{pid}/task/{pid}/children').read_text().split()[0])
    return sum(int(stat.read_text().split()[0]) for stat in Path(f'/proc/{pid}/task').glob('*/schedstat'))


def values(output):
    found = {}
    for line in output.splitlines():
        key, *rest = line.split()
        found.setdefault(key, []).append([int(v) for v in rest])
    return found


def rank(samples, percent):
    return samples[(len(samples) * percent + 99) // 100 - 1]


def drive(args):
    binary = args.work / 'isolated-node'
    extra = [str(SCENARIOS[args.scenario]), str(args.concurrency), str(args.creates)]
    with ExitStack() as stack:
        def launch(n):
            pin = ['taskset', '-c', args.cpus.split('/')[n]] if args.cpus else []
            proc = subprocess.Popen([
                'sudo', '-n', 'ip', 'netns', 'exec', f'{args.prefix}-{n}',
                'setpriv', f'--reuid={os.getuid()}', f'--regid={os.getgid()}', '--clear-groups',
                *pin, 'timeout', str(args.timeout), binary, str(n), *extra,
            ], cwd=args.run / str(n), stdout=subprocess.PIPE, text=True)
            return stack.enter_context(proc)

        controller = launch(0)
        seed = bytes.fromhex(controller.stdout.readline().strip())
        if not seed:
            raise RuntimeError('controller did not publish its bootstrap row')
        for n in (1, 2):
            (args.run / str(n) / 'seed').write_bytes(seed)
        # The desk prints a line at the first and last measured grant. Read
        # its CPU time as each line arrives.
        lines, marks = [], []

        def follow():
            for line in controller.stdout:
                if line.startswith('desk_mark'):
                    marks.append(cpu_ns(controller))
                lines.append(line)

        reader = threading.Thread(target=follow)
        reader.start()
        workers = [launch(n) for n in (1, 2)]
        outputs = []
        for worker in workers:
            output = worker.communicate()[0]
            if worker.returncode:
                raise RuntimeError(f'worker exited {worker.returncode}: {output}')
            outputs.append(values(output))
        if controller.wait():
            raise RuntimeError(f'controller exited {controller.returncode}')
        reader.join()
        root = values(''.join(lines))
    if args.scenario == 'rpc':
        joins, samples = [], []
        for times in outputs:
            if len(times.get('join_roster_ns', [])) != 1 or len(times.get('rpc_ns', [])) != 50:
                raise RuntimeError('incomplete worker measurements')
            joins.extend(v[0] for v in times['join_roster_ns'])
            samples.extend(v[0] for v in times['rpc_ns'])
        samples.sort()
        report = dict(join_roster_ns=joins, rpc_samples=len(samples),
                      rpc_p50_ns=samples[49], rpc_p99_ns=samples[98])
    elif args.scenario == 'move':
        acked = root['acked_rows'][0][0]
        held = [times['held_rows'][0][0] for times in outputs]
        if held != [0, acked] or root['stale_refused'] != [[3]]:
            raise RuntimeError(f'workers hold {held} rows, the client was acknowledged {acked}')
        report = dict(move_ns=root['move_ns'][0][0], drain_ns=root['drain_ns'][0][0],
                      acked_rows=acked, worker_rows=held, stale_requests_refused=3)
    else:
        caller = outputs[0]
        count, elapsed, _ = caller['creates'][0]
        failed, repeats, _ = caller['failed'][0]
        demands, grants_received, _ = caller['demands'][0]
        sent, answers, _ = caller['create_datagrams'][0]
        arrived, answered, _ = outputs[1]['worker_creates'][0]
        samples = caller['samples'][0]
        (first, step_first, rounds_first), (last, step_last, rounds_last) = root['desk_mark']
        if count != args.creates or len(samples) != count or failed or len(marks) != 2:
            raise RuntimeError(f'incomplete benchmark: {count} creates, {failed} failed')
        ordered = sorted(samples)
        grants = last - first
        report = dict(
            concurrency=args.concurrency, creates=count, elapsed_ns=elapsed,
            creates_per_s=count * 1e9 / elapsed,
            latency_p50_ns=rank(ordered, 50), latency_p99_ns=rank(ordered, 99),
            latency_min_ns=ordered[0], latency_max_ns=ordered[-1],
            desk_grants=grants, desk_cpu_ns=marks[1] - marks[0],
            desk_cpu_ns_per_100k_grants=(marks[1] - marks[0]) * 100000 // grants,
            desk_kernel_ns_per_100k_grants=(step_last - step_first) * 100000 // grants,
            desk_io_rounds=rounds_last - rounds_first,
            repeated_creates=repeats, create_datagrams_sent=sent, create_datagrams_arrived=arrived,
            reply_datagrams_sent=answered, reply_datagrams_arrived=answers, demand_datagrams=demands, grants_received=grants_received,
            latency_samples_ns=samples)
    print(json.dumps(dict(scenario=args.scenario, **report)))


def measure(args):
    with ExitStack() as stack:
        run = Path(stack.enter_context(tempfile.TemporaryDirectory(dir=args.work)))
        prefix = 'fleet-' + uuid.uuid4().hex[:10]
        namespaces(stack, prefix)
        credentials(run)
        command = [sys.executable, __file__, 'drive', str(args.work), str(run), prefix, *workload(args)]
        env = {**os.environ, 'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'}
        if args.shield:
            cpus = {16}
            if args.cpus:
                cpus.update(cpu_list(args.cpus.replace('/', ',')))
            # The scope accounts for all nodes and the driver together.
            env['HIRSEL_JOBS'] = str(len(cpus))
            command = [sys.executable, str(ROOT / 'core/tools/timing_shield.py'), 'run', '--',
                       'taskset', '-c', ','.join(map(str, sorted(cpus))), str(ROOT / 'tools/capped'),
                       'taskset', '-c', '16', *command]
        subprocess.run(command, check=True, env=env)


def workload(args):
    return ['--scenario', args.scenario, '--concurrency', str(args.concurrency),
            '--creates', str(args.creates), '--timeout', str(args.timeout), '--cpus', args.cpus]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest='mode', required=True)
    b = modes.add_parser('build')
    b.add_argument('compiler', type=Path)
    b.add_argument('runtime', type=Path)
    b.add_argument('work', type=Path)
    b.add_argument('--release', action='store_true')
    m = modes.add_parser('measure')
    m.add_argument('work', type=Path)
    m.add_argument('--shield', action='store_true')
    d = modes.add_parser('drive')
    d.add_argument('work', type=Path)
    d.add_argument('run', type=Path)
    d.add_argument('prefix')
    for mode in (m, d):
        mode.add_argument('--scenario', choices=sorted(SCENARIOS), default='rpc')
        mode.add_argument('--concurrency', type=int, default=16)
        mode.add_argument('--creates', type=int, default=100000)
        mode.add_argument('--timeout', type=int, default=30)
        mode.add_argument('--cpus', default='')
    args = parser.parse_args()
    args.work = args.work.resolve()
    {'build': build, 'measure': measure, 'drive': drive}[args.mode](args)


if __name__ == '__main__':
    main()
