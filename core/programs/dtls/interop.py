#!/usr/bin/env python3
"""DTLS S6 interop laws: interop_openssl, interop_dimpl and the live part of
fragments_reassemble (docs/spec/dtls.md, Build laws).

Builds interop.hirsel with the given compiler and runtime, then runs the Hirsel endpoint on
loopback UDP against `openssl s_server`/`s_client` (DTLS 1.2, use_srtp) and against dimpl
at e341a90 through interop_dimpl.rs, in both roles. The oracle is the peer: its printed
cipher, SRTP profile, keying material and certificate. Python `cryptography` (through
../srtp/oracle.py) seals and opens one SRTP packet under the peer's exported keys, cut as
RFC 5764 section 4.2 cuts them.

Usage: interop.py --compiler HIRSEL --runtime LIBHIRSEL_CORE_RT.a --scratch DIR
                  [--dimpl /tmp/ref-dimpl] [--only openssl|dimpl]
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import queue
import re
import socket
import subprocess
import threading
import time
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
DIMPL_COMMIT = 'e341a90'
DEADLINE = 30

spec = importlib.util.spec_from_file_location('srtp_oracle', HERE.parent / 'srtp' / 'oracle.py')
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)

# RFC 5764 section 4.1.2 and RFC 7714 section 14.2: name, code, oracle profile, key, salt.
PROFILES = {
    'SRTP_AES128_CM_SHA1_80': (1, 'cm', 16, 14),
    'SRTP_AEAD_AES_128_GCM': (7, 'gcm', 16, 12),
    'SRTP_AEAD_AES_256_GCM': (8, 'gcm256', 32, 12),
}

SENT = (0x11223344, 7, b'hirsel-rtp')
ANSWER = (0x55667788, 9, b'peer-rtp')


def library(name, extra=()):
    base = ROOT / 'core' / name
    files = sorted(base.glob('*.hirsel')) + [f for d in extra for f in sorted((base / d).glob('*.hirsel'))]
    return [{'file': str(f), 'modules': list(f.relative_to(base).with_suffix('').parts),
             'signatures': name == 'core'} for f in files]


def program(directory, core, std):
    manifest = tomllib.loads((directory / 'hirsel.toml').read_text())
    return {'package': {'name': manifest['package']['name'], 'version': '1'}, 'dependencies': [core, std],
            'sources': [{'file': str(directory / f), 'modules': m.split('.'), 'signatures': False}
                        for f, m in manifest['sources'].items()]}


def build(args):
    core, std = {'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}
    dtls, srtp = {'name': 'dtls', 'version': '1'}, {'name': 'srtp', 'version': '1'}
    packages = [
        {'package': core, 'dependencies': [], 'sources': library('core')},
        {'package': std, 'dependencies': [core], 'sources': library('std', ['sim'])},
        program(HERE, core, std), program(HERE.parent / 'srtp', core, std),
        {'package': {'name': 'dtls_interop', 'version': '1'}, 'dependencies': [core, std, dtls, srtp],
         'sources': [{'file': str(HERE / 'interop.hirsel'), 'modules': ['main'], 'signatures': False}]},
    ]
    inputs = args.scratch / 'inputs.json'
    inputs.write_text(json.dumps({'core': core, 'packages': packages}))
    binary = args.scratch / 'interop-cranelift'
    command = [str(args.compiler), 'build', '--source-core', '--inputs', str(inputs),
               '--store', str(args.scratch / 'store'), '--entry', 'dtls_interop@1/main/main',
               '--backend', 'cranelift', '--runtime', str(args.runtime), '--limit', '268435456',
               '--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
               '--grant', 'Net=UdpBind 127.0.0.1/32 0-65535;UdpSend 127.0.0.1/32 1-65535',
               '--cc', str(args.cc), '--json', '-o', str(binary)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(result.stdout[-20000:] + result.stderr[-20000:])
    return binary


def build_dimpl(args):
    head = subprocess.check_output(['git', '-C', str(args.dimpl), 'rev-parse', 'HEAD'], text=True).strip()
    if not head.startswith(DIMPL_COMMIT):
        raise SystemExit(f'{args.dimpl} is at {head}, not dimpl {DIMPL_COMMIT}')
    crate = args.scratch / 'dimpl-peer'
    (crate / 'src').mkdir(parents=True, exist_ok=True)
    (crate / 'src/main.rs').write_text((HERE / 'interop_dimpl.rs').read_text())
    (crate / 'Cargo.toml').write_text(
        '[package]\nname = "interop_dimpl"\nversion = "0.0.0"\nedition = "2024"\npublish = false\n\n'
        f'[dependencies]\ndimpl = {{ path = "{args.dimpl.resolve()}" }}\n\n[workspace]\n')
    subprocess.run(['cargo', 'build', '--release', '-j4', '--quiet'], cwd=crate, check=True,
                   env=os.environ | {'CARGO_TARGET_DIR': str(crate / 'target')})
    return crate / 'target/release/interop_dimpl'


def peer_certificate(scratch):
    """A P-256 certificate whose DER exceeds the 300-byte MTU, so it must fragment."""
    names = ','.join(f'DNS:peer-{i}.interop.test' for i in range(40))
    pem, key = scratch / 'peer.pem', scratch / 'peer.key'
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
                    '-nodes', '-keyout', key, '-out', pem, '-days', '30', '-subj', '/CN=peer.interop.test',
                    '-addext', 'subjectAltName=' + names], check=True, capture_output=True)
    der, key_der = scratch / 'peer.der', scratch / 'peer.key.der'
    subprocess.run(['openssl', 'x509', '-in', pem, '-outform', 'DER', '-out', der], check=True)
    subprocess.run(['openssl', 'pkey', '-in', key, '-outform', 'DER', '-out', key_der], check=True)
    printed = subprocess.check_output(['openssl', 'x509', '-in', pem, '-noout', '-fingerprint', '-sha256'], text=True)
    pin = printed.strip().split('=', 1)[1].replace(':', '').lower()
    if der.stat().st_size <= 600:
        raise SystemExit('peer certificate too small to fragment at MTU 300')
    return {'pem': pem, 'key': key, 'der': der, 'key_der': key_der, 'pin': pin}


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Process:
    """A child whose stdout lines are read on a thread; stdin stays open until stop()."""

    def __init__(self, argv):
        self.argv = [str(a) for a in argv]
        self.child = subprocess.Popen(self.argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
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

    def send(self, text):
        self.child.stdin.write(text)
        self.child.stdin.flush()

    def stop(self):
        if self.child.poll() is None:
            self.child.terminate()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait()


def openssl_peer(role, port, certificate, case):
    name, (_, _, key, salt) = case['profile'], PROFILES[case['profile']]
    common = ['-dtls1_2', '-cert', certificate['pem'], '-key', certificate['key'], '-cipher', case['cipher'],
              '-use_srtp', name, '-keymatexport', 'EXTRACTOR-dtls_srtp', '-keymatexportlen', str(2 * (key + salt))]
    if role == 'server':
        argv = ['openssl', 's_server', '-accept', port, '-verify', '1', *common, *case.get('flags', [])]
    else:
        argv = ['openssl', 's_client', '-connect', f'127.0.0.1:{port}', *common]
    return Process(argv)


def rtp(ssrc, seq, body):
    return bytes([0x80, 0x60]) + seq.to_bytes(2, 'big') + (1).to_bytes(4, 'big') + ssrc.to_bytes(4, 'big') + body


def run_case(hirsel, certificate, case):
    end = time.monotonic() + DEADLINE
    hirsel_role = 'client' if case['peer_role'] == 'server' else 'server'
    openssl = 'peer' not in case
    children = []
    try:
        def start_hirsel(port):
            child = Process([hirsel, hirsel_role, port, certificate['pin'], case.get('mtu', 1200), case.get('admission', 'verified')])
            children.append(child)
            return child

        def start_peer(port):
            if openssl:
                child = openssl_peer(case['peer_role'], port, certificate, case)
            else:
                child = Process([case['peer'], case['peer_role'], port, certificate['der'], certificate['key_der']])
            children.append(child)
            return child

        if case['peer_role'] == 'server':
            port = free_port()
            peer = start_peer(port)
            peer.expect('^ACCEPT$' if openssl else '^bound ', end)
            ours = start_hirsel(port)
        else:
            ours = start_hirsel(0)
            port = int(ours.expect('^bound ([0-9a-f]{4})$', end)[1], 16)
            peer = start_peer(port)

        # The peer's own report: cipher, profile and exported keying material.
        if openssl:
            cipher = peer.expect(r'(?:CIPHER is|Cipher is) (\S+)', end)[1]
            profile = peer.expect(r'SRTP Extension negotiated, profile=(\S+)', end)[1]
            material = bytes.fromhex(peer.expect(r'Keying material: ([0-9A-Fa-f]+)', end)[1])
        else:
            cipher = case['cipher']
            profile = peer.expect(r'^profile (\S+)$', end)[1]
            material = bytes.fromhex(peer.expect(r'^keymat ([0-9a-f]+)$', end)[1])
        if cipher != case['cipher'] or profile != case['profile']:
            raise AssertionError(f'negotiated {cipher} {profile}, expected {case["cipher"]} {case["profile"]}')
        code, label, key, salt = PROFILES[profile]
        if len(material) != 2 * (key + salt):
            raise AssertionError(f'{len(material)} bytes of keying material')
        # RFC 5764 section 4.2: client key, server key, client salt, server salt.
        masters = {'client': material[:key] + material[2 * key:2 * key + salt],
                   'server': material[key:2 * key] + material[2 * key + salt:]}

        ours.expect('^connected$', end)
        reported = ours.expect('^peer ([0-9a-f]{64})$', end)[1]
        if reported != certificate['pin']:
            raise AssertionError(f'peer certificate SHA-256 {reported}, OpenSSL printed {certificate["pin"]}')
        if int(ours.expect('^profile ([0-9a-f]{4})$', end)[1], 16) != code:
            raise AssertionError('Hirsel reports a different SRTP profile')
        sent = bytes.fromhex(ours.expect('^rtp ([0-9a-f]+)$', end)[1])
        ssrc, seq, body = SENT
        expected = oracle.protect(label, masters[hirsel_role], rtp(ssrc, seq, b''), body, ssrc, seq)
        if sent != expected:
            raise AssertionError(f'protect_rtp gave {sent.hex()}, the peer keys give {expected.hex()}')

        # Hirsel's application record reached the peer.
        peer.expect('hirsel-app-data' if openssl else '^data ' + b'hirsel-app-data\n'.hex() + '$', end)
        # One packet sealed under the peer's send keys travels as an application line.
        ssrc, seq, body = ANSWER
        packet = oracle.protect(label, masters[case['peer_role']], rtp(ssrc, seq, b''), body, ssrc, seq)
        peer.send(packet.hex() + '\n')
        opened = bytes.fromhex(ours.expect('^opened ([0-9a-f]+)$', end)[1])
        if opened != rtp(ssrc, seq, body):
            raise AssertionError(f'unprotect_rtp gave {opened.hex()}')
        ours.expect('^done$', end)
        if ours.child.wait(timeout=max(1, end - time.monotonic())) != 0:
            raise AssertionError('Hirsel endpoint exited nonzero')
        if case.get('mtu_check'):
            sizes = [int(m) for m in re.findall(r'write to .*?\((\d+) bytes =>', '\n'.join(peer.seen))]
            if not sizes or max(sizes) > 300:
                raise AssertionError(f'OpenSSL datagrams {sizes} not cut at 300')
    finally:
        for child in children:
            child.stop()


# Each suite and each SRTP profile at least once; the Hirsel server once through admit;
# the Hirsel client once against -listen; the Certificate fragmented at MTU 300.
OPENSSL = [
    {'name': 'openssl s_server / hirsel client, AES128-GCM, AEAD_AES_128_GCM', 'peer_role': 'server',
     'cipher': 'ECDHE-ECDSA-AES128-GCM-SHA256', 'profile': 'SRTP_AEAD_AES_128_GCM'},
    {'name': 'openssl s_server -listen / hirsel client, AES256-GCM, AEAD_AES_256_GCM', 'peer_role': 'server',
     'cipher': 'ECDHE-ECDSA-AES256-GCM-SHA384', 'profile': 'SRTP_AEAD_AES_256_GCM', 'flags': ['-listen']},
    {'name': 'openssl s_server -mtu 300 / hirsel client, CHACHA20, AES128_CM_SHA1_80 (fragments_reassemble)',
     'peer_role': 'server', 'cipher': 'ECDHE-ECDSA-CHACHA20-POLY1305', 'profile': 'SRTP_AES128_CM_SHA1_80',
     'flags': ['-mtu', '300', '-debug'], 'mtu_check': True},
    {'name': 'openssl s_client / hirsel server (Cookie), AES128-GCM, AES128_CM_SHA1_80', 'peer_role': 'client',
     'cipher': 'ECDHE-ECDSA-AES128-GCM-SHA256', 'profile': 'SRTP_AES128_CM_SHA1_80', 'admission': 'cookie'},
    {'name': 'openssl s_client / hirsel server, CHACHA20, AEAD_AES_256_GCM', 'peer_role': 'client',
     'cipher': 'ECDHE-ECDSA-CHACHA20-POLY1305', 'profile': 'SRTP_AEAD_AES_256_GCM'},
]

DIMPL = [
    {'name': 'dimpl server / hirsel client, AES128-GCM, AEAD_AES_128_GCM', 'peer_role': 'server',
     'cipher': 'ECDHE-ECDSA-AES128-GCM-SHA256', 'profile': 'SRTP_AEAD_AES_128_GCM'},
    {'name': 'dimpl client / hirsel server (Cookie), AES128-GCM, AEAD_AES_128_GCM', 'peer_role': 'client',
     'cipher': 'ECDHE-ECDSA-AES128-GCM-SHA256', 'profile': 'SRTP_AEAD_AES_128_GCM', 'admission': 'cookie'},
]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--cc', type=Path, default=ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang')
    parser.add_argument('--dimpl', type=Path, default=Path('/tmp/ref-dimpl'))
    parser.add_argument('--only', choices=['openssl', 'dimpl'])
    parser.add_argument('--build-only', action='store_true')
    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(parents=True, exist_ok=True)
    hirsel = build(args)
    if args.build_only:
        print('built', hirsel, flush=True)
        return 0
    certificate = peer_certificate(args.scratch)
    cases = []
    if args.only != 'dimpl':
        cases += OPENSSL
    if args.only != 'openssl':
        dimpl = build_dimpl(args)
        cases += [dict(case, peer=dimpl) for case in DIMPL]
    failed = 0
    for case in cases:
        try:
            run_case(hirsel, certificate, case)
            print('PASS', case['name'], flush=True)
        except Exception as e:  # one failing cell must not hide the others
            failed += 1
            print('FAIL', case['name'], e, flush=True)
    return 1 if failed else 0


if __name__ == '__main__':
    raise SystemExit(main())
