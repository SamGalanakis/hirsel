#!/usr/bin/env python3
"""Build and measure three Fleet processes in three Linux network namespaces.

run.py build COMPILER RUNTIME WORK
run.py measure WORK [--shield]
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
import tomllib
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]


def build(args):
    sys.path.insert(0, str(ROOT / 'core/tools'))
    from store_tests import inputs
    corpus = inputs(ROOT, package='fleet')
    fleet = corpus['packages'][-1]
    # inputs() uses dependency aliases. CorpusManifest needs package identities.
    bindings = tomllib.loads((HERE.parent / 'hirsel.toml').read_text())['dependencies']
    for edge in fleet['dependencies']:
        alias = edge['name']
        edge['name'] = bindings.get(alias, {}).get('package', alias)
    for source in (HERE.parent / 'cluster_laws.hirsel', HERE / 'node.hirsel'):
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
        '-o', args.work / 'isolated-node',
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


def drive(args):
    binary = args.work / 'isolated-node'
    with ExitStack() as stack:
        def launch(n):
            proc = subprocess.Popen([
                'sudo', '-n', 'ip', 'netns', 'exec', f'{args.prefix}-{n}',
                'setpriv', f'--reuid={os.getuid()}', f'--regid={os.getgid()}', '--clear-groups',
                'timeout', '30', binary, str(n),
            ], cwd=args.run / str(n), stdout=subprocess.PIPE, text=True)
            return stack.enter_context(proc)

        controller = launch(0)
        seed = bytes.fromhex(controller.stdout.readline().strip())
        if not seed:
            raise RuntimeError('controller did not publish its bootstrap row')
        for n in (1, 2):
            (args.run / str(n) / 'seed').write_bytes(seed)
        workers = [launch(n) for n in (1, 2)]
        joins, samples = [], []
        for worker in workers:
            output = worker.communicate()[0]
            if worker.returncode:
                raise RuntimeError(f'worker exited {worker.returncode}: {output}')
            times = {}
            for line in output.splitlines():
                key, value = line.split()
                times.setdefault(key, []).append(int(value))
            if len(times.get('join_roster_ns', [])) != 1 or len(times.get('rpc_ns', [])) != 50:
                raise RuntimeError('incomplete worker measurements')
            joins.extend(times['join_roster_ns'])
            samples.extend(times['rpc_ns'])
        if controller.wait():
            raise RuntimeError('controller missed the revocation acknowledgement')
    samples.sort()
    print(json.dumps(dict(join_roster_ns=joins, rpc_samples=len(samples),
                          rpc_p50_ns=samples[49], rpc_p99_ns=samples[98])))


def measure(args):
    with ExitStack() as stack:
        run = Path(stack.enter_context(tempfile.TemporaryDirectory(dir=args.work)))
        prefix = 'fleet-' + uuid.uuid4().hex[:10]
        namespaces(stack, prefix)
        credentials(run)
        command = [sys.executable, __file__, 'drive', str(args.work), str(run), prefix]
        if args.shield:
            command = [sys.executable, str(ROOT / 'core/tools/timing_shield.py'), 'run', '--',
                       'taskset', '-c', '16', str(ROOT / 'tools/capped'), *command]
        subprocess.run(command, check=True, env={**os.environ, 'HIRSEL_MEMORY_LIMIT': '2G', 'HIRSEL_JOBS': '1'})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest='mode', required=True)
    b = modes.add_parser('build')
    b.add_argument('compiler', type=Path)
    b.add_argument('runtime', type=Path)
    b.add_argument('work', type=Path)
    m = modes.add_parser('measure')
    m.add_argument('work', type=Path)
    m.add_argument('--shield', action='store_true')
    d = modes.add_parser('drive')
    d.add_argument('work', type=Path)
    d.add_argument('run', type=Path)
    d.add_argument('prefix')
    args = parser.parse_args()
    args.work = args.work.resolve()
    {'build': build, 'measure': measure, 'drive': drive}[args.mode](args)


if __name__ == '__main__':
    main()
