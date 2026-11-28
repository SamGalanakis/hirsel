#!/usr/bin/env python3
"""Law Q5 `interop_runner_cases` (docs/spec/quic.md): the QUIC Interop Runner's cases
against quinn, quiche and msquic, in both roles, on Boat VMs.

`build` compiles endpoint.hirsel (LLVM release) with the given compiler and runtime and
stages a Docker build context: the binary, run_endpoint.sh and the Dockerfile.
`run` sends the context and the runner (a clone outside the repo, RUNNER_COMMIT) to one
Boat VM per peer, builds the image there, runs the runner's cases with Hirsel as server
and as client, and fetches each VM's result JSON and the logs of failed cases.

The oracle is the runner's own checks on its pcaps and downloaded files. Cases a peer
cannot decrypt without a key log come back `unsupported` from the runner; Hirsel never
exports keys (TLS-8), so those cells rest on the peer's SSLKEYLOGFILE.

Usage: run.py build --compiler HIRSEL --runtime LIBHIRSEL_CORE_RT.a --scratch DIR
       run.py run --scratch DIR [--runner /tmp/ref-quic-interop-runner] [--peers quinn,quiche,msquic]
"""
import argparse
import json
import os
import sys
from pathlib import Path
import shutil
import subprocess

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / 'tools/toolchain-dist'))
import hcfg

HERE = Path(__file__).resolve().parent
QUIC = HERE.parent
ROOT = QUIC.parents[2]
RUNNER_COMMIT = '5e698b2'
DISPATCH = Path('/workspace/code/hirsel-briefs/tools/boat-dispatch')
# The spec's Q5 cases as runner tests. The spec's `multiconnect` is the endpoint case the
# runner's handshakeloss and handshakecorruption tests start; it has no test of its own.
CASES = ['handshake', 'transfer', 'retry', 'keyupdate', 'chacha20',
         'rebind-port', 'rebind-addr', 'ecn', 'amplificationlimit', 'blackhole', 'handshakeloss',
         'transferloss', 'handshakecorruption', 'transfercorruption']
PEERS = ['quinn', 'quiche', 'msquic']
GRANTS = ['--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
          '--grant', 'Net=UdpBind 0.0.0.0/0 1-65535;UdpSend 0.0.0.0/0 1-65535',
          '--dir', '/www', '--dir', '/downloads', '--dir', '/hq']


def library(name, extra=()):
    base = ROOT / 'core' / name
    files = sorted(base.glob('*.hirsel')) + [f for d in extra for f in sorted((base / d).glob('*.hirsel'))]
    return [{'file': str(f), 'modules': list(f.relative_to(base).with_suffix('').parts),
             'signatures': name == 'core'} for f in files]


def build(args):
    context = args.scratch / 'context'
    shutil.rmtree(context, ignore_errors=True)
    context.mkdir(parents=True)
    core, std, quic = {'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}, {'name': 'quic', 'version': '1'}
    manifest = hcfg.package(QUIC)
    packages = [
        {'package': core, 'dependencies': [], 'sources': library('core')},
        {'package': std, 'dependencies': [core], 'sources': library('std', ['sim'])},
        {'package': quic, 'dependencies': [core, std],
         'sources': [{'file': str(QUIC / f), 'modules': m.split('.'), 'signatures': False}
                     for f, m in manifest['sources'].items()]},
        {'package': {'name': 'quic_qns', 'version': '1'}, 'dependencies': [core, std, quic],
         'sources': [{'file': str(HERE / 'endpoint.hirsel'), 'modules': ['main'], 'signatures': False}]},
    ]
    inputs = args.scratch / 'inputs.json'
    inputs.write_text(json.dumps({'core': core, 'packages': packages}))
    command = [str(args.compiler), 'build', '--source-core', '--inputs', str(inputs),
               '--store', str(args.scratch / 'store'), '--entry', 'quic_qns@1/main/main',
               '--backend', args.backend, '--runtime', str(args.runtime), '--limit', '1073741824',
               *GRANTS, '--cc', str(args.cc), '--json', '-o', str(context / 'endpoint')]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(result.stdout[-20000:] + result.stderr[-20000:])
    for name in ('Dockerfile', 'run_endpoint.sh'):
        shutil.copy(HERE / name, context / name)
    print(context)


def job(peer, cases):
    """The VM's command: build the image, run the runner with Hirsel as server, then as
    client, against one peer. Each run writes its own JSON."""
    tests = ','.join(cases)
    runs = []
    for role, server, client in (('server', 'hirsel', peer), ('client', peer, 'hirsel')):
        runs.append(f'(cd runner && timeout 5400 python3 run.py -d -s {server} -c {client} -t {tests} '
                    f'-l ../logs-{role} -j ../{peer}-{role}.json -r hirsel=hirsel-qns:local '
                    f'> ../{peer}-{role}.out 2>&1); echo "{role} exit $?"')
    return ' && '.join([
        'set -e',
        # The runner needs tshark >= 4.5; Ubuntu 24.04 ships 4.2.
        'sudo -n add-apt-repository -y ppa:wireshark-dev/stable >/dev/null',
        'sudo -n apt-get -qq update >/dev/null && sudo -n DEBIAN_FRONTEND=noninteractive apt-get -qq install -y tshark >/dev/null',
        'tshark --version | head -1',
        'sudo -n docker version --format "docker {{.Server.Version}}"',
        'python3 -m venv venv && . venv/bin/activate && pip -q install -r runner/requirements.txt',
        'sudo -n docker build -q -t hirsel-qns:local context >/dev/null',
        'set +e',
        *runs,
        f'tar czf {peer}-logs.tgz logs-server logs-client 2>/dev/null; true',
    ])


def run(args):
    runner = args.runner.resolve()
    head = subprocess.check_output(['git', '-C', str(runner), 'rev-parse', 'HEAD'], text=True).strip()
    if not head.startswith(RUNNER_COMMIT):
        raise SystemExit(f'{runner} is at {head}, not the interop runner {RUNNER_COMMIT}')
    bundle = args.scratch / 'bundle'
    shutil.rmtree(bundle, ignore_errors=True)
    bundle.mkdir(parents=True)
    shutil.copytree(args.scratch / 'context', bundle / 'context')
    shutil.copytree(runner, bundle / 'runner', ignore=shutil.ignore_patterns('.git'))
    implementations = json.loads((runner / 'implementations_quic.json').read_text())
    implementations['hirsel'] = {'image': 'hirsel-qns:local', 'url': 'https://github.com/SamGalanakis/hirsel', 'role': 'both'}
    (bundle / 'runner/implementations_quic.json').write_text(json.dumps(implementations, indent=2))
    cases = args.cases.split(',')
    shards = [{'lane': f'qs8-{peer}', 'bundle': str(bundle), 'command': job(peer, cases), 'seconds': 3600,
               'fetch': [f'{peer}-{role}.{ext}:{args.scratch / "results"}' for role in ('server', 'client')
                         for ext in ('json', 'out')] + [f'{peer}-logs.tgz:{args.scratch / "results"}']}
              for peer in args.peers.split(',')]
    manifest = args.scratch / 'shards.json'
    manifest.write_text(json.dumps(shards, indent=2))
    (args.scratch / 'results').mkdir(exist_ok=True)
    subprocess.run([str(DISPATCH), '--manifest', str(manifest), '--batch-seconds', '3600', '--ttl', '7200'],
                   cwd=args.scratch, check=True)
    summarize(args.scratch / 'results')


def summarize(results):
    """One line per peer and role: each case's runner verdict."""
    for path in sorted(results.glob('*.json')):
        data = json.loads(path.read_text())
        cells = data['results'][0] if data.get('results') else []
        print(path.stem, ' '.join(f'{c["name"]}={c["result"]}' for c in cells))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    b = sub.add_parser('build')
    b.add_argument('--compiler', type=Path, required=True)
    b.add_argument('--runtime', type=Path, required=True)
    b.add_argument('--backend', default='llvm', choices=['llvm', 'cranelift'])
    b.add_argument('--cc', type=Path, default=ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang')
    r = sub.add_parser('run')
    r.add_argument('--runner', type=Path, default=Path('/tmp/ref-quic-interop-runner'))
    r.add_argument('--peers', default=','.join(PEERS))
    r.add_argument('--cases', default=','.join(CASES))
    s = sub.add_parser('summary')
    for p in (b, r, s):
        p.add_argument('--scratch', type=Path, required=True)
    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(parents=True, exist_ok=True)
    {'build': build, 'run': run, 'summary': lambda a: summarize(a.scratch / 'results')}[args.command](args)


if __name__ == '__main__':
    raise SystemExit(main())
