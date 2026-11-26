#!/usr/bin/env python3
"""QUIC QS3 interop: law Q4 `handshake_against_quinn_and_quiche`, handshake only, and the
live part of Q16 `pin_mismatch_refuses` (docs/spec/quic.md, Laws).

Builds interop.hirsel with the given compiler and runtime, builds interop_quinn.rs against
quinn at QUINN_COMMIT (a clone outside the repo, /tmp/ref-quinn by default), and runs the
Hirsel endpoint on loopback UDP against quinn in both roles. Each quinn run offers exactly
one cipher suite and one key-exchange group.

The oracle is quinn's own view: its handshake completes, the ALPN it reports is ours, and
the transport parameters it decoded from us (rustls's `quic_transport_parameters`, through
quinn-proto's `TransportParameters`) are the ones the Hirsel endpoint was configured with.
The reverse is checked against quinn's own parameters as quinn encoded them. Certificate
pins are SHA-256 digests OpenSSL prints. Client-pin refusals carry the RFC 8446 alerts
`bad_certificate` (0x012a) and `certificate_required` (0x0174) as quinn reads them
(RFC 9001 §4.8).

Usage: interop.py --compiler HIRSEL --runtime LIBHIRSEL_CORE_RT.a --scratch DIR
                  [--quinn /tmp/ref-quinn]
"""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import subprocess
import threading
import time
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
QUINN_COMMIT = '75d08d9'
DEADLINE = 20
ALPN = 'hq-interop'

# interop.hirsel's TransportConfig and EndpointConfig, as quinn must read them (RFC 9000 §18.2).
HIRSEL_PARAMS = {
    'max_idle_timeout': 7000,
    'max_udp_payload_size': 1200,
    'initial_max_data': 0,
    'initial_max_stream_data_bidi_local': 0,
    'initial_max_stream_data_bidi_remote': 0,
    'initial_max_stream_data_uni': 0,
    'initial_max_streams_bidi': 0,
    'initial_max_streams_uni': 0,
    'active_connection_id_limit': 8,
}


def library(name, extra=()):
    base = ROOT / 'core' / name
    files = sorted(base.glob('*.hirsel')) + [f for d in extra for f in sorted((base / d).glob('*.hirsel'))]
    return [{'file': str(f), 'modules': list(f.relative_to(base).with_suffix('').parts),
             'signatures': name == 'core'} for f in files]


def build(args):
    core, std, quic = {'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}, {'name': 'quic', 'version': '1'}
    manifest = tomllib.loads((HERE / 'hirsel.toml').read_text())
    packages = [
        {'package': core, 'dependencies': [], 'sources': library('core')},
        {'package': std, 'dependencies': [core], 'sources': library('std', ['sim'])},
        {'package': quic, 'dependencies': [core, std],
         'sources': [{'file': str(HERE / f), 'modules': m.split('.'), 'signatures': False}
                     for f, m in manifest['sources'].items()]},
        {'package': {'name': 'quic_interop', 'version': '1'}, 'dependencies': [core, std, quic],
         'sources': [{'file': str(HERE / 'interop.hirsel'), 'modules': ['main'], 'signatures': False}]},
    ]
    inputs = args.scratch / 'inputs.json'
    inputs.write_text(json.dumps({'core': core, 'packages': packages}))
    binary = args.scratch / 'interop-cranelift'
    command = [str(args.compiler), 'build', '--source-core', '--inputs', str(inputs),
               '--store', str(args.scratch / 'store'), '--entry', 'quic_interop@1/main/main',
               '--backend', 'cranelift', '--runtime', str(args.runtime), '--limit', '268435456',
               '--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
               '--grant', 'Net=UdpBind 127.0.0.1/32 0-65535;UdpSend 127.0.0.1/32 1-65535',
               '--cc', str(args.cc), '--json', '-o', str(binary)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(result.stdout[-20000:] + result.stderr[-20000:])
    return binary


def build_quinn(args):
    head = subprocess.check_output(['git', '-C', str(args.quinn), 'rev-parse', 'HEAD'], text=True).strip()
    if not head.startswith(QUINN_COMMIT):
        raise SystemExit(f'{args.quinn} is at {head}, not quinn {QUINN_COMMIT}')
    quinn = args.quinn.resolve()
    crate = args.scratch / 'quinn-peer'
    (crate / 'src').mkdir(parents=True, exist_ok=True)
    (crate / 'src/main.rs').write_text((HERE / 'interop_quinn.rs').read_text())
    (crate / 'Cargo.lock').write_text((quinn / 'Cargo.lock').read_text())
    (crate / 'Cargo.toml').write_text(
        '[package]\nname = "hirsel_interop"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n\n'
        f'[dependencies]\nquinn = {{ path = "{quinn}/quinn" }}\n'
        f'quinn-proto = {{ path = "{quinn}/quinn-proto", default-features = false, features = ["rustls-ring"] }}\n'
        'rustls = { version = "0.23.5", default-features = false, features = ["std", "ring"] }\n'
        'ring = "0.17"\ntokio = { version = "1.28.1", features = ["rt", "macros", "time"] }\n\n[workspace]\n')
    subprocess.run(['cargo', 'build', '-j8', '--quiet'], cwd=crate, check=True,
                   env=os.environ | {'CARGO_TARGET_DIR': str(crate / 'target')})
    return crate / 'target/debug/hirsel_interop'


def certificate(scratch, name):
    """A P-256 certificate and PKCS#8 key from OpenSSL, with OpenSSL's SHA-256 fingerprint."""
    pem, key = scratch / f'{name}.pem', scratch / f'{name}.key'
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
                    '-nodes', '-keyout', key, '-out', pem, '-days', '30', '-subj', f'/CN={name}',
                    '-addext', 'subjectAltName=DNS:localhost'], check=True, capture_output=True)
    der, key_der = scratch / f'{name}.der', scratch / f'{name}.key.der'
    subprocess.run(['openssl', 'x509', '-in', pem, '-outform', 'DER', '-out', der], check=True)
    subprocess.run(['openssl', 'pkcs8', '-topk8', '-nocrypt', '-in', key, '-outform', 'DER', '-out', key_der], check=True)
    printed = subprocess.check_output(['openssl', 'x509', '-in', pem, '-noout', '-fingerprint', '-sha256'], text=True)
    pin = printed.strip().split('=', 1)[1].replace(':', '').lower()
    return {'der': der, 'key_der': key_der, 'pin': pin}


class Process:
    """A child whose stdout lines are read on a thread."""

    def __init__(self, argv):
        self.argv = [str(a) for a in argv]
        self.child = subprocess.Popen(self.argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                      stderr=subprocess.STDOUT, text=True, bufsize=1)
        self.lines, self.seen = queue.Queue(), []
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.child.stdout:
            self.lines.put(line.rstrip('\n'))
        self.lines.put(None)

    def expect(self, pattern, end):
        for line in self.seen:
            if m := re.search(pattern, line):
                return m
        while True:
            try:
                line = self.lines.get(timeout=max(0.01, end - time.monotonic()))
            except queue.Empty:
                raise AssertionError(f'{Path(self.argv[0]).name}: no {pattern!r}; output:\n' + '\n'.join(self.seen[-40:]))
            if line is None:
                raise AssertionError(f'{Path(self.argv[0]).name} exited before {pattern!r}:\n' + '\n'.join(self.seen[-40:]))
            self.seen.append(line)
            if m := re.search(pattern, line):
                return m

    def stop(self):
        if self.child.poll() is None:
            self.child.terminate()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait()


def quinn_params(text):
    """quinn-proto's `TransportParameters` Debug form: `name: n` and `name: Some(n)`."""
    return {name: int(value) for name, value in re.findall(r'(\w+): (?:Some\()?(\d+)\b', text)}


def hirsel_params(match):
    idle, udp, ack_delay, datagram, cids = (int(x, 16) for x in match.groups())
    return {'max_idle_timeout': idle, 'max_udp_payload_size': udp, 'max_ack_delay': ack_delay,
            'max_datagram_frame_size': datagram, 'active_connection_id_limit': cids}


def check_hirsel_read_quinn(ours, peer, end):
    """Our peer_params must be the parameters quinn itself encoded (RFC 9000 §18.2 defaults
    where quinn omits one)."""
    local = quinn_params(peer.expect(r'^local (.*)$', end)[1])
    read = hirsel_params(ours.expect(r'^params ([0-9a-f]{16}) ([0-9a-f]{16}) ([0-9a-f]{16}) ([0-9a-f]{16}) ([0-9a-f]{16})$', end))
    expected = {'max_idle_timeout': local['max_idle_timeout'], 'max_udp_payload_size': local['max_udp_payload_size'],
                'max_ack_delay': local['max_ack_delay'], 'max_datagram_frame_size': local.get('max_datagram_frame_size', 0),
                'active_connection_id_limit': local['active_connection_id_limit']}
    if read != expected:
        raise AssertionError(f'Hirsel read {read}, quinn sent {expected}')


def check_quinn_read_hirsel(peer, end):
    read = peer.expect(r'^params (.*)$', end)[1]
    decoded = quinn_params(read)
    wrong = {k: (decoded.get(k), v) for k, v in HIRSEL_PARAMS.items() if decoded.get(k) != v}
    if wrong or 'max_datagram_frame_size: None' not in read:
        raise AssertionError(f'quinn read {read}; differing (quinn, ours): {wrong}')


def run_case(hirsel, quinn, certs, case):
    end = time.monotonic() + DEADLINE
    children = []
    try:
        if case['quinn'] == 'server':
            peer = Process([quinn, 'server', certs['server']['der'], certs['server']['key_der'], case['suite'], case['group']])
            children.append(peer)
            port = peer.expect(r'^bound (\d+)$', end)[1]
            ours = Process([hirsel, 'client', port, case.get('pin', certs['server']['pin'])])
            children.append(ours)
        else:
            required = case.get('client_pins', 'none')
            if required == 'client':
                required = certs['client']['pin']
            ours = Process([hirsel, 'server', required])
            children.append(ours)
            port = int(ours.expect(r'^bound ([0-9a-f]{4})$', end)[1], 16)
            pin = ours.expect(r'^certificate ([0-9a-f]{64})$', end)[1]
            argv = [quinn, 'client', port, pin, case['suite'], case['group']]
            if case.get('client_cert'):
                argv += [certs['client']['der'], certs['client']['key_der']]
            peer = Process(argv)
            children.append(peer)

        refusal = case.get('refused')
        if refusal:
            reason, code = refusal
            ours.expect(rf'^closed refused {reason}$', end)
            got = peer.expect(r'^error (.*)$', end)[1]
            if got != f'{code:x}':
                raise AssertionError(f'quinn saw close {got}, RFC 8446 alert gives {code:x}')
            if any(line.startswith('connected') for line in ours.seen):
                raise AssertionError('refused client reached Connected')
        else:
            # quinn's own view: its handshake completed with our ALPN.
            peer.expect(r'^connected$', end)
            if (alpn := peer.expect(r'^alpn (.*)$', end)[1]) != ALPN:
                raise AssertionError(f'quinn negotiated ALPN {alpn!r}')
            if (alpn := ours.expect(r'^connected (.*)$', end)[1]) != ALPN:
                raise AssertionError(f'Hirsel reports ALPN {alpn!r}')
            check_quinn_read_hirsel(peer, end)
            check_hirsel_read_quinn(ours, peer, end)
            if case.get('client_pins') == 'client':
                if (got := ours.expect(r'^peer ([0-9a-f]{64})$', end)[1]) != certs['client']['pin']:
                    raise AssertionError(f'Hirsel pinned client {got}, OpenSSL printed {certs["client"]["pin"]}')
            peer.expect(r'^closed$', end)
            if (got := ours.expect(r'^closed (.*)$', end)[1]) != 'application 0000000000000000':
                raise AssertionError(f'Hirsel saw the close as {got}')
        for child in children:
            if child.child.wait(timeout=max(1, end - time.monotonic())) != 0:
                raise AssertionError(f'{Path(child.argv[0]).name} exited nonzero:\n' + '\n'.join(child.seen[-20:]))
    finally:
        for child in children:
            child.stop()


def off_by_one_bit(pin):
    return f'{int(pin[:2], 16) ^ 1:02x}' + pin[2:]


CASES = [
    {'name': 'quinn client / hirsel server, AES-128-GCM, X25519', 'quinn': 'client',
     'suite': 'TLS13_AES_128_GCM_SHA256', 'group': 'X25519'},
    {'name': 'quinn client / hirsel server, AES-256-GCM, P-256', 'quinn': 'client',
     'suite': 'TLS13_AES_256_GCM_SHA384', 'group': 'secp256r1'},
    {'name': 'quinn client / hirsel server, ChaCha20-Poly1305, X25519', 'quinn': 'client',
     'suite': 'TLS13_CHACHA20_POLY1305_SHA256', 'group': 'X25519'},
    {'name': 'quinn server / hirsel client, AES-128-GCM, X25519', 'quinn': 'server',
     'suite': 'TLS13_AES_128_GCM_SHA256', 'group': 'X25519'},
    {'name': 'quinn server / hirsel client, AES-256-GCM, X25519', 'quinn': 'server',
     'suite': 'TLS13_AES_256_GCM_SHA384', 'group': 'X25519'},
    {'name': 'quinn server / hirsel client, ChaCha20-Poly1305, P-256 (HelloRetryRequest)', 'quinn': 'server',
     'suite': 'TLS13_CHACHA20_POLY1305_SHA256', 'group': 'secp256r1'},
    # Q16 client pins against quinn's client certificate.
    {'name': 'quinn client with a pinned certificate / hirsel server with client_pins', 'quinn': 'client',
     'suite': 'TLS13_AES_128_GCM_SHA256', 'group': 'X25519', 'client_cert': True, 'client_pins': 'client'},
    {'name': 'quinn client off the pins / hirsel server refuses bad_certificate', 'quinn': 'client',
     'suite': 'TLS13_AES_128_GCM_SHA256', 'group': 'X25519', 'client_cert': True, 'client_pins': 'other',
     'refused': ('pin', 0x012a)},
    {'name': 'quinn client without a certificate / hirsel server refuses certificate_required', 'quinn': 'client',
     'suite': 'TLS13_AES_128_GCM_SHA256', 'group': 'X25519', 'client_pins': 'client',
     'refused': ('certificate', 0x0174)},
]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--cc', type=Path, default=ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang')
    parser.add_argument('--quinn', type=Path, default=Path('/tmp/ref-quinn'))
    parser.add_argument('--only', help='run the cases whose name contains this text')
    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(parents=True, exist_ok=True)
    hirsel = build(args)
    quinn = build_quinn(args)
    certs = {'server': certificate(args.scratch, 'server'), 'client': certificate(args.scratch, 'client')}
    failed = 0
    for case in CASES:
        if args.only and args.only not in case['name']:
            continue
        case = dict(case)
        if case.get('client_pins') == 'other':
            case['client_pins'] = off_by_one_bit(certs['client']['pin'])
        try:
            run_case(hirsel, quinn, certs, case)
            print('PASS', case['name'], flush=True)
        except Exception as e:  # one failing cell must not hide the others
            failed += 1
            print('FAIL', case['name'], e, flush=True)
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
