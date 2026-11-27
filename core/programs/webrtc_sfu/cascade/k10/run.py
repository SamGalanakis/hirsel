#!/usr/bin/env python3
"""Law K10 `three_node_room_on_boat` (docs/spec/sfu_cascade.md, slice KS5):
3 SFU cascade nodes in two regions and 30 participants, measured end to end over
real OS UDP sockets and QUIC mutual-pin cascade links.

  build: compiles node.hirsel into a standalone native binary + Boat bundle.
  exec:  runs single-node baseline (30 participants) and 3-node 2-region cascade
         (Home + Edge A in Region 1, Edge B in Region 2 with real tc netem delay
         and ping RTT oracle), checking all 870 audio & 870 video pairs and
         `cascade_p95_ms <= single_node_p95_ms + ping_rtt_ms + 20.0`.
  boat:  dispatches the built bundle to a Boat VM via tools/boat-dispatch and
         fetches k10-result.json.

Usage:
  run.py build --compiler HIRSEL --runtime LIBHIRSEL_CORE_RT.a --scratch DIR
  run.py exec  --binary PATH --scratch DIR [--region-delay-ms 20] [--duration-ms 5000] [--no-netns]
  run.py boat  --scratch DIR [--region-delay-ms 20] [--duration-ms 5000]
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import tomllib

HERE = Path(__file__).resolve().parent
SFU = HERE.parents[1]
ROOT = SFU.parents[2] if len(SFU.parents) >= 3 else HERE
DISPATCH = Path('/workspace/code/hirsel-briefs/tools/boat-dispatch')
GRANTS = [
    '--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
    '--grant', 'Net=UdpBind 0.0.0.0/0 1-65535;UdpSend 0.0.0.0/0 1-65535',
]


def library(name, extra=()):
    base = ROOT / 'core' / name
    files = sorted(base.glob('*.hirsel')) + [
        f for d in extra for f in sorted((base / d).glob('*.hirsel'))
    ]
    return [{
        'file': str(f),
        'modules': list(f.relative_to(base).with_suffix('').parts),
        'signatures': name == 'core',
    } for f in files]


def build_inputs(scratch):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    packages = [
        {'package': core, 'dependencies': [], 'sources': library('core')},
        {'package': std, 'dependencies': [core], 'sources': library('std', ['sim'])},
    ]
    done = {}

    def visit(directory):
        directory = directory.resolve()
        if directory in done:
            return done[directory]
        manifest = tomllib.loads((directory / 'hirsel.toml').read_text())
        deps = [core]
        for name, dep in manifest.get('dependencies', {}).items():
            if 'path' in dep:
                deps.append(visit(directory / dep['path']))
            else:
                deps.append({'name': name, 'version': str(dep['major'])})
        pkg = {'name': manifest['package']['name'], 'version': str(manifest['package']['version'].split('.')[0])}
        packages.append({
            'package': pkg,
            'dependencies': deps,
            'sources': [
                {'file': str(directory / f), 'modules': m.split('.'), 'signatures': False}
                for f, m in manifest['sources'].items()
            ],
        })
        done[directory] = pkg
        return pkg

    sfu_pkg = visit(SFU)
    quic_pkg = done[(SFU.parent / 'quic').resolve()]
    rtp_pkg = done[(SFU.parent / 'rtp').resolve()]
    packages.append({
        'package': {'name': 'cascade_k10', 'version': '1'},
        'dependencies': [core, std, quic_pkg, rtp_pkg, sfu_pkg],
        'sources': [{'file': str(HERE / 'node.hirsel'), 'modules': ['main'], 'signatures': False}],
    })
    inputs_path = scratch / 'inputs.json'
    inputs_path.write_text(json.dumps({'core': core, 'packages': packages}) + '\n')
    return inputs_path


def cmd_build(args):
    bundle = args.scratch / 'bundle'
    shutil.rmtree(bundle, ignore_errors=True)
    bundle.mkdir(parents=True)
    inputs_path = build_inputs(args.scratch)
    binary = bundle / 'k10-node'
    cmd = [
        str(args.compiler), 'build', '--source-core',
        '--inputs', str(inputs_path),
        '--store', str(args.scratch / 'store'),
        '--entry', 'cascade_k10@1/main/main',
        '--backend', args.backend,
        '--runtime', str(args.runtime),
        '--limit', '1073741824',
        '--dir', '.',
        *GRANTS,
        '--cc', str(args.cc),
        '--json', '-o', str(binary),
    ]
    env = os.environ.copy()
    lld_dir = ROOT / 'tools/buck2/toolchains/llvm-files/bin'
    if lld_dir.is_dir():
        env['PATH'] = f'{lld_dir}:{env.get("PATH", "")}'
    res = subprocess.run(cmd, cwd=ROOT, env=env, capture_output=True, text=True)
    if res.returncode != 0:
        raise SystemExit(res.stdout[-20000:] + res.stderr[-20000:])
    shutil.copy(HERE / 'run.py', bundle / 'run.py')
    print(binary)


def gen_identity(cert_dir, cn):
    cert_dir.mkdir(parents=True, exist_ok=True)
    key_pem = cert_dir / 'key.pem'
    cert_pem = cert_dir / 'cert.pem'
    key_der = cert_dir / 'key.der'
    cert_der = cert_dir / 'cert.der'
    subprocess.run(['openssl', 'ecparam', '-name', 'prime256v1', '-genkey', '-noout', '-out', str(key_pem)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['openssl', 'req', '-new', '-x509', '-key', str(key_pem), '-out', str(cert_pem),
                    '-days', '2', '-subj', f'/CN={cn}'],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['openssl', 'x509', '-in', str(cert_pem), '-outform', 'DER', '-out', str(cert_der)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['openssl', 'pkcs8', '-topk8', '-nocrypt', '-in', str(key_pem), '-outform', 'DER',
                    '-out', str(key_der)],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return hashlib.sha256(cert_der.read_bytes()).hexdigest()


def parse_node_json(stdout_text, label):
    for line in stdout_text.splitlines():
        if line.startswith('K10_NODE_JSON '):
            return json.loads(line[len('K10_NODE_JSON '):])
    raise RuntimeError(f'{label} produced no K10_NODE_JSON line:\n{stdout_text}')


def setup_two_regions(one_way_ms):
    r1, r2 = 'k10-r1', 'k10-r2'
    teardown_two_regions()
    cmds = [
        ['sudo', '-n', 'ip', 'netns', 'add', r1],
        ['sudo', '-n', 'ip', 'netns', 'add', r2],
        ['sudo', '-n', 'ip', 'link', 'add', 'veth-k10-1', 'type', 'veth', 'peer', 'name', 'veth-k10-2'],
        ['sudo', '-n', 'ip', 'link', 'set', 'veth-k10-1', 'netns', r1],
        ['sudo', '-n', 'ip', 'link', 'set', 'veth-k10-2', 'netns', r2],
        ['sudo', '-n', 'ip', 'netns', 'exec', r1, 'ip', 'link', 'set', 'lo', 'up'],
        ['sudo', '-n', 'ip', 'netns', 'exec', r2, 'ip', 'link', 'set', 'lo', 'up'],
        ['sudo', '-n', 'ip', 'netns', 'exec', r1, 'ip', 'addr', 'add', '10.200.1.1/24', 'dev', 'veth-k10-1'],
        ['sudo', '-n', 'ip', 'netns', 'exec', r1, 'ip', 'addr', 'add', '10.200.1.2/24', 'dev', 'veth-k10-1'],
        ['sudo', '-n', 'ip', 'netns', 'exec', r2, 'ip', 'addr', 'add', '10.200.1.3/24', 'dev', 'veth-k10-2'],
        ['sudo', '-n', 'ip', 'netns', 'exec', r1, 'ip', 'link', 'set', 'veth-k10-1', 'up'],
        ['sudo', '-n', 'ip', 'netns', 'exec', r2, 'ip', 'link', 'set', 'veth-k10-2', 'up'],
    ]
    if one_way_ms > 0:
        cmds.append(['sudo', '-n', 'ip', 'netns', 'exec', r1, 'tc', 'qdisc', 'add', 'dev', 'veth-k10-1',
                     'root', 'netem', 'delay', f'{one_way_ms}ms'])
        cmds.append(['sudo', '-n', 'ip', 'netns', 'exec', r2, 'tc', 'qdisc', 'add', 'dev', 'veth-k10-2',
                     'root', 'netem', 'delay', f'{one_way_ms}ms'])
    for c in cmds:
        subprocess.run(c, check=True)


def teardown_two_regions():
    for ns in ('k10-r1', 'k10-r2'):
        subprocess.run(['sudo', '-n', 'ip', 'netns', 'delete', ns],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['sudo', '-n', 'ip', 'link', 'delete', 'veth-k10-1'],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def measure_ping_rtt_ms(use_netns, dst_ip):
    cmd = (['sudo', '-n', 'ip', 'netns', 'exec', 'k10-r1'] if use_netns else []) + [
        'ping', '-c', '15', '-i', '0.05', dst_ip
    ]
    res = subprocess.run(cmd, capture_output=True, text=True, check=True)
    m = re.search(r'rtt min/avg/max/mdev = [\d.]+/([\d.]+)/', res.stdout)
    if not m:
        raise RuntimeError(f'failed to parse ping output:\n{res.stdout}')
    return float(m.group(1))


def cmd_exec(args):
    binary = args.binary.resolve()
    scratch = args.scratch.resolve()
    scratch.mkdir(parents=True, exist_ok=True)
    certs = scratch / 'certs'
    shutil.rmtree(certs, ignore_errors=True)
    home_dir = certs / 'home'
    edge_a_dir = certs / 'edge-a'
    edge_b_dir = certs / 'edge-b'
    home_pin = gen_identity(home_dir, 'home')
    edge_a_pin = gen_identity(edge_a_dir, 'edge-a')
    edge_b_pin = gen_identity(edge_b_dir, 'edge-b')

    # Step 1: Single-node baseline (30 participants on 1 node).
    single_res = subprocess.run(
        [str(binary), 'single', '30', str(min(args.duration_ms, 2500))],
        cwd=home_dir, capture_output=True, text=True, check=True,
    )
    single = parse_node_json(single_res.stdout, 'single')
    single_p95_ms = single['p95_us'] / 1000.0

    # Step 2: 3-node, 2-region cascade (10 participants per node = 30 total).
    use_netns = not args.no_netns
    if use_netns:
        setup_two_regions(args.region_delay_ms)
        home_ip, edge_a_ip, edge_b_ip = '10.200.1.1', '10.200.1.2', '10.200.1.3'
        pfx_r1 = ['sudo', '-n', 'ip', 'netns', 'exec', 'k10-r1']
        pfx_r2 = ['sudo', '-n', 'ip', 'netns', 'exec', 'k10-r2']
    else:
        home_ip = edge_a_ip = edge_b_ip = '127.0.0.1'
        pfx_r1 = pfx_r2 = []

    try:
        ping_rtt_ms = measure_ping_rtt_ms(use_netns, edge_b_ip)
        dur = str(args.duration_ms)
        home_cmd = pfx_r1 + [
            str(binary), 'home', f'{home_ip}:15001', '1', '10', '30', dur,
            'edge-a', edge_a_pin, 'edge-b', edge_b_pin,
        ]
        edge_a_cmd = pfx_r1 + [
            str(binary), 'edge', f'{edge_a_ip}:15002', f'{home_ip}:15001',
            'edge-a', '11', '10', '30', dur, home_pin,
        ]
        edge_b_cmd = pfx_r2 + [
            str(binary), 'edge', f'{edge_b_ip}:15003', f'{home_ip}:15001',
            'edge-b', '21', '10', '30', dur, home_pin,
        ]
        p_home = subprocess.Popen(home_cmd, cwd=home_dir, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        # Wait until home prints K10_READY before dialing from edges.
        ready_lines = []
        deadline = time.monotonic() + 15.0
        while time.monotonic() < deadline:
            line = p_home.stdout.readline()
            if not line:
                break
            ready_lines.append(line)
            if line.strip().startswith('K10_READY'):
                break
        p_ea = subprocess.Popen(edge_a_cmd, cwd=edge_a_dir, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        p_eb = subprocess.Popen(edge_b_cmd, cwd=edge_b_dir, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        timeout_s = args.duration_ms / 1000.0 + 30.0
        out_ea, err_ea = p_ea.communicate(timeout=timeout_s)
        out_eb, err_eb = p_eb.communicate(timeout=timeout_s)
        rest_home, err_home = p_home.communicate(timeout=timeout_s)
        out_home = ''.join(ready_lines) + rest_home
        for label, proc, err in (('home', p_home, err_home), ('edge-a', p_ea, err_ea), ('edge-b', p_eb, err_eb)):
            if proc.returncode != 0:
                raise RuntimeError(f'{label} exited {proc.returncode}:\n{err}')
    finally:
        if use_netns:
            teardown_two_regions()

    nodes = [
        parse_node_json(out_home, 'home'),
        parse_node_json(out_ea, 'edge-a'),
        parse_node_json(out_eb, 'edge-b'),
    ]
    total_expected = sum(n['expected_pairs'] for n in nodes)
    total_audio_pairs = sum(n['audio_pairs'] for n in nodes)
    total_video_pairs = sum(n['video_pairs'] for n in nodes)
    cascade_p95_ms = max(n['p95_us'] for n in nodes) / 1000.0
    remote_p95_ms = max(n['remote_p95_us'] for n in nodes) / 1000.0
    budget_ms = single_p95_ms + ping_rtt_ms + 20.0
    passed = (
        single['audio_pairs'] == single['expected_pairs']
        and single['video_pairs'] == single['expected_pairs']
        and total_audio_pairs == total_expected
        and total_video_pairs == total_expected
        and all(n['samples'] > 0 and n['remote_samples'] > 0 for n in nodes)
        and cascade_p95_ms <= budget_ms
        and remote_p95_ms <= budget_ms
    )
    report = {
        'law': 'K10 three_node_room_on_boat',
        'passed': passed,
        'two_region_netns': use_netns,
        'region_one_way_ms': args.region_delay_ms if use_netns else 0,
        'ping_rtt_ms': round(ping_rtt_ms, 3),
        'single_node_p95_ms': round(single_p95_ms, 3),
        'cascade_p95_ms': round(cascade_p95_ms, 3),
        'remote_p95_ms': round(remote_p95_ms, 3),
        'budget_ms': round(budget_ms, 3),
        'audio_pairs': f'{total_audio_pairs}/{total_expected}',
        'video_pairs': f'{total_video_pairs}/{total_expected}',
        'single': single,
        'nodes': nodes,
    }
    out_path = scratch / 'k10-result.json'
    out_path.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    return 0 if passed else 1


def cmd_boat(args):
    bundle = args.scratch / 'bundle'
    if not (bundle / 'k10-node').exists():
        raise SystemExit(f'missing built binary at {bundle / "k10-node"}; run `build` first')
    results = args.scratch / 'results'
    results.mkdir(parents=True, exist_ok=True)
    remote_cmd = (
        f'python3 run.py exec --binary ./k10-node --scratch ./k10-run '
        f'--region-delay-ms {args.region_delay_ms} --duration-ms {args.duration_ms}'
    )
    subprocess.run([
        str(DISPATCH), 'cascade-k10', str(bundle), remote_cmd,
        '--fetch', f'k10-run/k10-result.json:{results}',
    ], cwd=args.scratch, check=True)
    res_path = results / 'k10-result.json'
    data = json.loads(res_path.read_text())
    print(f"K10 Boat verdict: passed={data['passed']} audio={data['audio_pairs']} video={data['video_pairs']} "
          f"single_p95={data['single_node_p95_ms']}ms ping_rtt={data['ping_rtt_ms']}ms "
          f"cascade_p95={data['cascade_p95_ms']}ms remote_p95={data['remote_p95_ms']}ms "
          f"budget={data['budget_ms']}ms")
    return 0 if data['passed'] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    b = sub.add_parser('build')
    b.add_argument('--compiler', type=Path, required=True)
    b.add_argument('--runtime', type=Path, required=True)
    b.add_argument('--backend', default='llvm', choices=['llvm', 'cranelift'])
    b.add_argument('--cc', type=Path, default=Path('cc'))
    b.add_argument('--scratch', type=Path, required=True)

    e = sub.add_parser('exec')
    e.add_argument('--binary', type=Path, required=True)
    e.add_argument('--scratch', type=Path, required=True)
    e.add_argument('--region-delay-ms', type=int, default=20)
    e.add_argument('--duration-ms', type=int, default=5000)
    e.add_argument('--no-netns', action='store_true')

    bt = sub.add_parser('boat')
    bt.add_argument('--scratch', type=Path, required=True)
    bt.add_argument('--region-delay-ms', type=int, default=20)
    bt.add_argument('--duration-ms', type=int, default=5000)

    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(parents=True, exist_ok=True)
    fn = {'build': cmd_build, 'exec': cmd_exec, 'boat': cmd_boat}[args.command]
    return fn(args) or 0


if __name__ == '__main__':
    raise SystemExit(main())
