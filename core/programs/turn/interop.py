#!/usr/bin/env python3
"""TURN U7 interop laws: interop_coturn and interop_pion (docs/spec/turn.md, Build laws).

Builds interop.hirsel with the given compiler and runtime, then on loopback:

- interop_coturn: the Hirsel client against coturn's `turnserver` with a long-term user,
  over UDP and over TCP. It allocates, permits and binds a Python UDP peer, and sends
  numbered payloads until one has gone as a Send indication and one as ChannelData. The
  oracle is coturn and the peer's own socket: every datagram the peer receives comes from
  the relayed address coturn granted and equals a payload sent, byte for byte; both kinds
  arrive; the peer's echoes come back to the client as data from the peer.
- interop_pion: Pion's examples/turn-client (udp and tcp) with `-ping` against the Hirsel
  relay, with credentials the relay minted. The oracle is Pion: it exits 0 and logs all
  ten ping replies, each one a datagram that went pinger -> relay -> Pion client -> relay
  -> pinger.

coturn (with libevent) and Pion are built from clones in /tmp at the commits pinned below.
A peer that cannot be built here (no clone, no cmake or no go) is reported SKIP, and the
script then exits 2: a skip is not a pass.

Usage: interop.py --compiler HIRSEL --runtime LIBHIRSEL_CORE_RT.a --scratch DIR
                  [--coturn /tmp/ref-coturn] [--libevent /tmp/ref-libevent]
                  [--pion /tmp/ref-pion-turn] [--only coturn|pion]
"""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import shutil
import socket
import subprocess
import threading
import time
import tomllib

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
COTURN_COMMIT = '04fad0c85e0c03acc16dbd56ee0134c6a57b45a3'
LIBEVENT_COMMIT = '5df3037d10556bfcb675bc73e516978b75fc7bc7'  # release-2.1.12-stable
PION_COMMIT = '7bffbda6e8137fbfe15e239f8c3655ec4024d562'
DEADLINE = 40
CPUS = ['taskset', '-c', '8-11']
REALM = 'hirsel.test'
PINGS = 10  # examples/turn-client's doPingTest sends ten


class Skip(Exception):
    pass


def library(name, extra=()):
    base = ROOT / 'core' / name
    files = sorted(base.glob('*.hirsel')) + [f for d in extra for f in sorted((base / d).glob('*.hirsel'))]
    return [{'file': str(f), 'modules': list(f.relative_to(base).with_suffix('').parts),
             'signatures': name == 'core'} for f in files]


def program(directory, dependencies):
    manifest = tomllib.loads((directory / 'hirsel.toml').read_text())
    return {'package': {'name': manifest['package']['name'], 'version': '1'}, 'dependencies': dependencies,
            'sources': [{'file': str(directory / f), 'modules': m.split('.'), 'signatures': False}
                        for f, m in manifest['sources'].items()]}


def build(args):
    core, std = {'name': 'core', 'version': '1'}, {'name': 'std', 'version': '1'}
    stun, turn = {'name': 'stun_ice', 'version': '1'}, {'name': 'turn', 'version': '1'}
    packages = [
        {'package': core, 'dependencies': [], 'sources': library('core')},
        {'package': std, 'dependencies': [core], 'sources': library('std', ['sim'])},
        program(HERE.parent / 'stun_ice', [core, std]),
        program(HERE, [core, std, stun]),
        {'package': {'name': 'turn_interop', 'version': '1'}, 'dependencies': [core, std, stun, turn],
         'sources': [{'file': str(HERE / 'interop.hirsel'), 'modules': ['main'], 'signatures': False}]},
    ]
    inputs = args.scratch / 'inputs.json'
    inputs.write_text(json.dumps({'core': core, 'packages': packages}))
    binary = args.scratch / 'turn-interop'
    command = [*CPUS, str(args.compiler), 'build', '--source-core', '--inputs', str(inputs),
               '--store', str(args.scratch / 'store'), '--entry', 'turn_interop@1/main/main',
               '--backend', 'cranelift', '--runtime', str(args.runtime), '--limit', '268435456',
               '--grant', 'Clock', '--grant', 'Random', '--grant', 'Stdout',
               '--grant', 'Net=UdpBind 127.0.0.1/32 0-65535;UdpSend 127.0.0.1/32 1-65535;'
                          'Connect 127.0.0.1/32 1-65535;Listen 127.0.0.1/32 0-65535',
               '--cc', str(args.cc), '--json', '-o', str(binary)]
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True)
    if result.returncode:
        raise SystemExit(result.stdout[-20000:] + result.stderr[-20000:])
    return binary


def pinned(clone, commit, name):
    if not (clone / '.git').exists():
        raise Skip(f'no {name} clone at {clone}')
    head = subprocess.check_output(['git', '-C', str(clone), 'rev-parse', 'HEAD'], text=True).strip()
    if head != commit:
        raise SystemExit(f'{clone} is at {head}, not {name} {commit}')


def build_coturn(args):
    binary = args.coturn / 'bin/turnserver'
    if binary.exists():
        return binary
    for tool in ('cmake', 'make', 'pkg-config'):
        if not shutil.which(tool):
            raise Skip(f'{tool} is not installed')
    pinned(args.coturn, COTURN_COMMIT, 'coturn')
    pinned(args.libevent, LIBEVENT_COMMIT, 'libevent')
    prefix = args.libevent / 'prefix'
    if not (prefix / 'lib/libevent_core.a').exists():
        build = args.libevent / 'build'
        build.mkdir(exist_ok=True)
        subprocess.run([*CPUS, 'cmake', '..', f'-DCMAKE_INSTALL_PREFIX={prefix}', '-DEVENT__LIBRARY_TYPE=STATIC',
                        '-DEVENT__DISABLE_TESTS=ON', '-DEVENT__DISABLE_BENCHMARK=ON', '-DEVENT__DISABLE_SAMPLES=ON',
                        '-DEVENT__DISABLE_REGRESS=ON', '-DCMAKE_POSITION_INDEPENDENT_CODE=ON'],
                       cwd=build, check=True, capture_output=True)
        subprocess.run([*CPUS, 'make', '-j4', 'install'], cwd=build, check=True, capture_output=True)
    # coturn's autotools configure leaves out what this host lacks (its cmake build requires
    # libmicrohttpd); the run needs none of the databases, Prometheus or systemd.
    without = {f'TURN_NO_{x}': '1' for x in ('PROMETHEUS', 'SYSTEMD', 'MYSQL', 'PQ', 'MONGO', 'HIREDIS', 'SQLITE')}
    env = os.environ | without | {'PKG_CONFIG_PATH': str(prefix / 'lib/pkgconfig')}
    subprocess.run(['./configure'], cwd=args.coturn, env=env, check=True, capture_output=True)
    subprocess.run([*CPUS, 'make', '-j4', 'bin/turnserver'], cwd=args.coturn, env=env, check=True, capture_output=True)
    return binary


def build_pion(args):
    out = args.pion / 'bin'
    binaries = {t: out / f'turn-client-{t}' for t in ('udp', 'tcp')}
    if all(b.exists() for b in binaries.values()):
        return binaries
    go = shutil.which('go') or (Path.home() / '.local/go/bin/go' if (Path.home() / '.local/go/bin/go').exists() else None)
    if not go:
        raise Skip('go is not installed')
    pinned(args.pion, PION_COMMIT, 'pion/turn')
    for t, b in binaries.items():
        subprocess.run([*CPUS, str(go), 'build', '-o', str(b), f'./examples/turn-client/{t}'],
                       cwd=args.pion, check=True, capture_output=True)
    return binaries


def free_port():
    """A port free for both UDP and TCP on 127.0.0.1."""
    while True:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as u:
            u.bind(('127.0.0.1', 0))
            port = u.getsockname()[1]
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as t:
                try:
                    t.bind(('127.0.0.1', port))
                    return port
                except OSError:
                    continue


class Process:
    """A child whose stdout (and stderr) lines are read on a thread."""

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

    def drain(self):
        while True:
            try:
                line = self.lines.get_nowait()
            except queue.Empty:
                return self.seen
            if line is None:
                return self.seen
            self.seen.append(line)

    def stop(self):
        if self.child.poll() is None:
            self.child.terminate()
        try:
            self.child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            self.child.wait()


class EchoPeer:
    """A UDP peer that records every datagram and its source, and echoes it back."""

    def __init__(self):
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(('127.0.0.1', 0))
        self.sock.settimeout(0.2)
        self.port = self.sock.getsockname()[1]
        self.received, self.running = [], True
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _run(self):
        while self.running:
            try:
                data, source = self.sock.recvfrom(65536)
            except socket.timeout:
                continue
            except OSError:
                return
            self.received.append((source, data))
            self.sock.sendto(data, source)

    def stop(self):
        self.running = False
        self.thread.join()
        self.sock.close()


def wait_listening(port, end):
    while time.monotonic() < end:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            if s.connect_ex(('127.0.0.1', port)) == 0:
                return
        time.sleep(0.05)
    raise AssertionError(f'turnserver never listened on {port}')


def coturn_case(hirsel, turnserver, transport, scratch):
    end = time.monotonic() + DEADLINE
    port = free_port()
    user, password = 'alice', 'hirsel-interop-password'
    server = Process([turnserver, '-n', '--no-tls', '--listening-ip', '127.0.0.1',
                      '--relay-ip', '127.0.0.1', '--listening-port', port, '--min-port', '50000', '--max-port', '50199',
                      '--lt-cred-mech', '--user', f'{user}:{password}', '--realm', 'example.org',
                      '--allow-loopback-peers', '--log-file', 'stdout', '--simple-log',
                      '--pidfile', scratch / f'turnserver-{transport}.pid'])
    peer = EchoPeer()
    ours = None
    try:
        wait_listening(port, end)
        ours = Process([hirsel, 'client', transport, port, user, password, peer.port])
        relayed = ('127.0.0.1', int(ours.expect(r'^allocated (\d+)$', end)[1]))
        ours.expect('^done$', end)
        if ours.child.wait(timeout=max(1, end - time.monotonic())) != 0:
            raise AssertionError('Hirsel client exited nonzero')
        lines = ours.drain()
        sent = {}
        for line in lines:
            if m := re.match(r'^sent (send|channel) ([0-9a-f]+)$', line):
                sent[bytes.fromhex(m[2])] = m[1]
        time.sleep(0.2)
        peer.stop()
        kinds = set()
        for source, data in peer.received:
            if source != relayed:
                raise AssertionError(f'a datagram from {source}, not the relayed address {relayed}')
            if data not in sent:
                raise AssertionError(f'the peer received {data.hex()}, which the client never sent')
            kinds.add(sent[data])
        if kinds != {'send', 'channel'}:
            raise AssertionError(f'the peer received only {sorted(kinds)} payloads')
        echoes = [bytes.fromhex(line[5:]) for line in lines if line.startswith('echo ')]
        if not echoes or any(e not in sent for e in echoes):
            raise AssertionError('no echo, or an echo the client never sent')
    finally:
        peer.stop() if peer.running else None
        for child in (ours, server):
            if child:
                child.stop()


def pion_case(hirsel, client, transport):
    end = time.monotonic() + DEADLINE
    ours = Process([hirsel, 'relay', REALM])
    pion = None
    try:
        port = int(ours.expect(r'^listening (\d+)$', end)[1])
        user = ours.expect(r'^user (\S+)$', end)[1]
        password = ours.expect(r'^password (\S+)$', end)[1]
        pion = Process([client, '-host', '127.0.0.1', '-port', port, '-user', f'{user}={password}',
                        '-realm', REALM, '-ping'])
        code = pion.child.wait(timeout=max(1, end - time.monotonic()))
        lines = pion.drain()
        if code != 0:
            raise AssertionError(f'Pion exited {code}:\n' + '\n'.join(lines[-30:]))
        if not any('relayed-address=127.0.0.1:' in line for line in lines):
            raise AssertionError('Pion logged no relayed address:\n' + '\n'.join(lines[-30:]))
        pongs = [line for line in lines if re.search(r'\d+ bytes from from 127\.0\.0\.1:\d+ time=', line)]
        if len(pongs) != PINGS:
            raise AssertionError(f'{len(pongs)} of {PINGS} ping replies came back:\n' + '\n'.join(lines[-30:]))
        ours.expect(r'^allocated 1$', end)
    finally:
        for child in (pion, ours):
            if child:
                child.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--cc', type=Path, default=ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang')
    parser.add_argument('--coturn', type=Path, default=Path('/tmp/ref-coturn'))
    parser.add_argument('--libevent', type=Path, default=Path('/tmp/ref-libevent'))
    parser.add_argument('--pion', type=Path, default=Path('/tmp/ref-pion-turn'))
    parser.add_argument('--only', choices=['coturn', 'pion'])
    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    args.scratch.mkdir(parents=True, exist_ok=True)
    hirsel = build(args)
    failed = skipped = 0
    suites = []
    if args.only != 'pion':
        suites.append(('interop_coturn', build_coturn,
                       lambda server, t: coturn_case(hirsel, server, t, args.scratch)))
    if args.only != 'coturn':
        suites.append(('interop_pion', build_pion, lambda clients, t: pion_case(hirsel, clients[t], t)))
    for law, prepare, run in suites:
        try:
            peer = prepare(args)
        except Skip as e:
            skipped += 1
            print('SKIP', law, e, flush=True)
            continue
        for transport in ('udp', 'tcp'):
            try:
                run(peer, transport)
                print('PASS', law, transport, flush=True)
            except Exception as e:  # one failing cell must not hide the others
                failed += 1
                print('FAIL', law, transport, e, flush=True)
    return 1 if failed else 2 if skipped else 0


if __name__ == '__main__':
    raise SystemExit(main())
