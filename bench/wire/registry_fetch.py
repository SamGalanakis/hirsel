#!/usr/bin/env python3
"""Measure production registry publish/add/update with a real service and isolated S3.

Linux, iproute2, util-linux, openssl, python3-cryptography, and a linker on PATH.
Build hirsel, core-driver-runtime-pic and hirsel-core-driver-bench-package_layer.
Prepare with "prepare --work EMPTY_DIR --driver HIRSEL --runtime PIC --package-layer BENCH".
Then "run --work DIR --driver HIRSEL". "smoke" measures the isolated link alone.
The frozen socket check requires strace.
sudo is used only to set up private mount/network namespaces. The workload runs
as the invoking user. No outside network or deployment credentials are required.
"""
import argparse
import base64
from collections import Counter
from contextlib import ExitStack, contextmanager, nullcontext
import datetime
import hashlib
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import math
import os
from pathlib import Path
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

from cryptography.hazmat.primitives import serialization as ser
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

REPO = Path(__file__).resolve().parents[2]
ORIGIN = 'https://registry.test:4443'
PUBLISHER = 'fixture.test'
# An isolated peer owns this address. Enrollment excludes the registry host itself.
DOMAIN_IP = '93.184.215.14'
CLASSES = ('blobs', 'tiles', 'bundles', 'release_checkpoint', 'status_checkpoint',
           'status_delta', 'hints', 'evidence')
BARS = {'G3': (1_000_000_000, 2_200_000), 'G4': (300_000_000, 100_000)}


def compact(value):
    return json.dumps(value, separators=(',', ':')).encode()


def b64(data):
    return base64.b64encode(data).decode()


def check(condition, message):
    if not condition:
        raise ValueError(message)


def key(work, name):
    return ser.load_der_private_key((work / (name + '.der')).read_bytes(), password=None)


def public(key):
    return key.public_key().public_bytes(ser.Encoding.Raw, ser.PublicFormat.Raw)


def verifier(name, key):
    data = b'\x01' + public(key)
    ident = hashlib.sha256(name.encode() + b'\n' + data).hexdigest()[:8]
    return f'{name}+{ident}+{b64(data)}'


def prepare(args):
    w = args.work
    w.mkdir(parents=True, exist_ok=True)
    subprocess.run([args.package_layer, 'registry-fixture', w, REPO / 'core/std'], check=True)
    for name in ['release', 'status', 'publisher', 'legal']:
        k = Ed25519PrivateKey.generate()
        (w / (name + '.der')).write_bytes(k.private_bytes(
            ser.Encoding.DER, ser.PrivateFormat.PKCS8, ser.NoEncryption()))
    release, status = [verifier(f'registry.test:4443/{log}', key(w, name))
                       for log, name in [('log', 'release'), ('status', 'status')]]
    legal = b64(public(key(w, 'legal')))
    subjects = [['ed25519', b64(public(key(w, 'publisher')))]]
    (w / 'enroll').write_bytes(compact(subjects))
    (w / 'document').write_bytes(compact(['hirsel-enroll-v1', ORIGIN, PUBLISHER, subjects]))
    (w / 'registry.json').write_bytes(compact([ORIGIN, legal, release, status, 'preview', [],
        ['https://s3.test:8112', 'us-east-1', 'bucket', ''], [], '8111', [PUBLISHER + '/*']]))
    (w / 'config/hirsel').mkdir(parents=True)
    (w / 'config/hirsel/config.hcfg').write_text(
        f'user_v1.Config(contexts: [], routes: [Route(prefix: HostPrefix("{PUBLISHER}"), '
        f'url: "{ORIGIN}", release_key: "{release}", status_key: "{status}", '
        f'legal_key: "{legal}", quorum: Preview)])\n')
    (w / 'hosts').write_text(f'127.0.0.1 localhost registry.test bucket.s3.test {socket.gethostname()}\n{DOMAIN_IP} {PUBLISHER}\n')
    (w / 'san').write_text(f'subjectAltName=DNS:registry.test,DNS:bucket.s3.test,DNS:{PUBLISHER}\n'
                           'basicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n')
    for cmd in [
        ['req', '-new', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256',
         '-nodes', '-subj', '/CN=Wire fixture CA', '-keyout', 'ca.key', '-out', 'ca.pem',
         '-days', '2', '-addext', 'basicConstraints=critical,CA:TRUE'],
        ['req', '-new', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes',
         '-subj', '/CN=registry.test', '-keyout', 'tls.key', '-out', 'tls.csr'],
        ['x509', '-req', '-in', 'tls.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key',
         '-CAcreateserial', '-out', 'tls.pem', '-days', '2', '-extfile', 'san'],
    ]:
        subprocess.run(['openssl', *cmd], cwd=w, check=True)
    grants = ['Compiler', 'Clock', 'Tasks', 'Random',
              'Net=Listen 127.0.0.1/32 8111-8111;Connect 127.0.0.1/32 8112-8112;'
              'Connect 0.0.0.0/0 1-65535 public;Connect ::/0 1-65535 public;Resolve *;Tls * 1-65535 System',
              'Cred:aws=imds:127.0.0.1:8113 kdf']
    for name in ['release', 'status']:
        grants.append(f'Secret:{name}_signer=file:{w}/{name}.der sign(ed25519)')
    subprocess.run([args.driver, 'build', REPO / 'core/programs/registry', '--libraries', REPO / 'core',
                    '--runtime', args.runtime, '--cc', 'cc', '--dir', w, '--limit', '4294967296',
                    '--output', w / 'registry', *(v for grant in grants for v in ['--grant', grant])],
                   env=dict(os.environ, XDG_CACHE_HOME=str(w / 'build-cache')), check=True)


class Handler(BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    disable_nagle_algorithm = True

    def log_message(self, *args):
        pass

    def reply(self, code, data=b'', headers=()):
        self.send_response_only(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        if self.command != 'HEAD':
            self.wfile.write(data)


class Metadata(Handler):
    def do_PUT(self):
        self.reply(200, b'fixture-token')

    def do_GET(self):
        if self.path.endswith('/'):
            self.reply(200, b'fixture')
        else:
            expires = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=1)
            self.reply(200, compact(dict(Code='Success', AccessKeyId='fixture',
                SecretAccessKey='fixture-s3-key', Token='fixture-session',
                Expiration=expires.strftime('%Y-%m-%dT%H:%M:%SZ'))))


class Enrollment(Handler):
    def do_GET(self):
        if self.path == '/.well-known/hirsel-registry.json':
            self.reply(200, self.server.document)
        else:
            self.reply(404)


class Bucket(Handler):
    """Strong conditional PUT and exact object GET/HEAD for std.object."""
    def do_PUT(self):
        body = self.rfile.read(int(self.headers['Content-Length']))
        with self.server.lock:
            old = self.server.objects.get(self.path)
            if ((self.headers.get('If-None-Match') == '*' and old is not None)
                or ('If-Match' in self.headers and (not old or old[1]['ETag'] != self.headers['If-Match']))):
                self.reply(412)
                return
            headers = {k: v for k, v in self.headers.items() if k.lower().startswith('x-amz-meta-')}
            headers['ETag'] = '"' + hashlib.sha256(body).hexdigest()[:32] + '"'
            self.server.objects[self.path] = body, headers
        self.reply(200, headers=headers.items())

    def do_GET(self):
        with self.server.lock:
            obj = self.server.objects.get(self.path)
        if obj is None:
            self.reply(404)
        else:
            try:
                self.reply(200, obj[0], obj[1].items())
            except (ssl.SSLEOFError, BrokenPipeError, ConnectionResetError):
                # std.object may close a GET after reading the size and retry with room.
                self.close_connection = True

    do_HEAD = do_GET


def path_class(path):
    if path.startswith('/c/'):
        return 'blobs'
    if path.startswith('/versions/'):
        return 'hints'
    if path.startswith('/e/'):
        return 'evidence'
    if path == '/status/checkpoint' or path.startswith('/status/heads/'):
        return 'status_checkpoint'
    if path.startswith('/status/tile/'):
        return 'status_delta'
    if path == '/log/checkpoint' or path.startswith('/log/heads/'):
        return 'release_checkpoint'
    if path.startswith('/log/tile/entries/'):
        return 'bundles'
    if path.startswith('/log/tile/'):
        return 'tiles'
    raise ValueError(f'unaccounted registry path {path}')


class Proxy(Handler):
    """Terminate TLS and forward each request to the compiled Hirsel listener."""
    def setup(self):
        super().setup()
        self.server.connections += 1
        self.backend = http.client.HTTPConnection('127.0.0.1', 8111, timeout=60)

    def finish(self):
        self.backend.close()
        super().finish()

    def forward(self):
        arrived = time.monotonic_ns()
        body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
        self.backend.request(self.command, self.path, body=body, headers={
            k: v for k, v in self.headers.items() if k.lower() not in ('connection', 'host')})
        res = self.backend.getresponse()
        data = res.read()
        self.server.rows.append((self.path, res.status, len(data), arrived))
        self.reply(res.status, data, [(k, v) for k, v in res.getheaders() if k.lower()
                   not in ('connection', 'content-length', 'transfer-encoding')])

    do_GET = do_POST = forward


class Listener(ThreadingHTTPServer):
    request_queue_size = 32


@contextmanager
def server(address, handler, tls=None, **state):
    with Listener(address, handler) as http:
        vars(http).update(state)
        if tls:
            http.socket = tls.wrap_socket(http.socket, server_side=True, do_handshake_on_connect=False)
        t = threading.Thread(target=http.serve_forever, daemon=True)
        t.start()
        try:
            yield http
        finally:
            http.shutdown()
            t.join()


# The private namespace has no external network interface. Netem shapes TCP setup,
# TLS handshakes, requests and responses, with one shared queue per direction.
SHAPE = '''
tc qdisc add dev lo root handle 1: prio bands 3 priomap 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2 2
for rule in "1 dst_port" "2 src_port"; do
    band=${rule% *}
    field=${rule#* }
    tc qdisc add dev lo parent 1:$band handle ${band}0: netem delay 25ms rate 50mbit
    tc filter add dev lo parent 1: protocol ip pref "$band" flower ip_proto tcp "$field" 4443 flowid 1:$band
done
'''

NAMESPACE = '''
set -eu
ip link set lo up mtu 1500
mount -t tmpfs tmpfs /run/netns
ip netns add enrollment
ip link add registry-net type veth peer name domain-net netns enrollment
ip addr add 93.184.215.13/30 dev registry-net
ip link set registry-net up
ip -n enrollment addr add 93.184.215.14/30 dev domain-net
ip -n enrollment link set domain-net up
ip -n enrollment link set lo up
ip netns exec enrollment sysctl -q -w net.ipv4.ip_unprivileged_port_start=0
sysctl -q -w net.ipv4.ip_unprivileged_port_start=0
mount --bind "$1" /etc/hosts
''' + SHAPE + '''
shift
# EOF on the pipe closes the domain peer when the registry client exits.
drop=(setpriv --reuid "$SUDO_UID" --regid "$SUDO_GID" --init-groups --)
"${drop[@]}" "$@" 3>&1 1>&2 | ip netns exec enrollment "${drop[@]}" "$1" "$2" domain --work "$5"
'''


def wire_bytes():
    rows = json.loads(subprocess.check_output(['tc', '-s', '-j', 'qdisc', 'show', 'dev', 'lo']))
    meters = [r for r in rows if r['handle'] in ('10:', '20:')]
    check(len(meters) == 2 and all(r['kind'] == 'netem' and not r['drops'] for r in meters), meters)
    return sum(r['bytes'] for r in meters)


def smoke():
    # Use the same classifiers and queues as the registry. TCP setup takes one
    # round trip. A sustained transfer must pay for serialization as well.
    payload = bytes(4_000_000)
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 4443))
        listener.listen()
        def respond():
            with listener.accept()[0] as peer:
                peer.sendall(payload)
        worker = threading.Thread(target=respond)
        worker.start()
        before = wire_bytes()
        start = time.monotonic_ns()
        with socket.create_connection(listener.getsockname()) as client:
            connected = time.monotonic_ns()
            with client.makefile('rb') as stream:
                check(stream.read() == payload, 'incomplete calibration body')
        elapsed = time.monotonic_ns() - connected
        worker.join()
    total = wire_bytes() - before
    print(json.dumps(dict(connect_ms=(connected-start)/1e6, payload_bytes=len(payload),
                          transfer_ms=elapsed/1e6, wire_bytes=total)))
    check(connected-start >= 48_000_000 and elapsed >= len(payload)*8*1e9/50e6
          and total > len(payload), 'link calibration failed')


def lock_rows(app):
    lock = tomllib.loads((app / 'hirsel.lock').read_text())
    return {row['id']: row for row in lock['package'] if row['id'].startswith(PUBLISHER + '/')}


def cli(args, cwd, store, *command, expected=0, trace=None, pin=False):
    env = dict(os.environ, XDG_CONFIG_HOME=str(args.work / 'config'),
               XDG_CACHE_HOME=str(store.parent / 'cache'), SSL_CERT_FILE=str(args.work / 'ca.pem'))
    for k in list(env):
        if k.lower().endswith('_proxy'):
            del env[k]
    prefix = ['strace', '-f', '-qq', '-e', 'trace=socket,connect', '-o', trace] if trace else []
    if pin:
        prefix += ['taskset', '-c', '16']
    libraries = ['--libraries', REPO / 'core'] if command[0] in ('registry', 'check') else []
    run = subprocess.run([*prefix, args.driver, *command, '--package-store', store, *libraries],
                         env=env, cwd=cwd, capture_output=True, text=True)
    check(run.returncode == expected, f'{command}: exit {run.returncode}\n{run.stdout}{run.stderr}')
    return run.stdout + run.stderr


def sample(args, app, store, proxy, gate, command, pin):
    time.sleep(.15)  # Drain previous connection teardown outside the wall timer.
    before = wire_bytes()
    connections, start = proxy.connections, len(proxy.rows)
    begin = time.monotonic_ns()
    output = cli(args, app, store, *command, pin=pin)
    ns = time.monotonic_ns() - begin
    time.sleep(.15)
    transferred = wire_bytes() - before
    sizes = Counter(dict.fromkeys(CLASSES, 0))
    requests = Counter()
    hints = []
    check('preview (quorum 0)' in output, output)
    for path, code, length, arrived in proxy.rows[start:]:
        check(code == 200, (path, code))
        if path.startswith('/versions/'):
            hints.append(arrived)
        category = path_class(path)
        sizes[category] += length
        requests[category] += 1
    check(transferred >= sum(sizes.values()) and transferred > 0, 'missing shaped traffic')
    result = dict(gate=gate, ns=ns, bytes=transferred, bodies=sizes, requests=requests,
                  connections=proxy.connections - connections, hint_span_ns=max(hints) - min(hints) if hints else 0)
    print(json.dumps(result), flush=True)
    return result


def status(work, action, root):
    name = 'legal' if action == 'takedown' else 'publisher'
    k = key(work, name)
    intent = ['hirsel-intent-v1', ORIGIN, action, PUBLISHER, '0' if name == 'legal' else '1',
              root, [None, ['legal' if name == 'legal' else 'security', 'qualification']]]
    meta = compact([intent, ['ed25519', b64(public(k))], b64(k.sign(compact(intent))), [], []])
    connection = http.client.HTTPSConnection('registry.test', 4443,
        context=ssl.create_default_context(cafile=work / 'ca.pem'))
    try:
        connection.request('POST', '/v1/submit', len(meta).to_bytes(4, 'big') + meta,
                           headers={'Content-Type': 'application/octet-stream'})
        res = connection.getresponse()
        body = res.read()
        check(res.status == 200 and json.loads(body)[3] == root, (action, res.status, body))
    finally:
        connection.close()


def qualify(args, proxy, scratch):
    w = args.work
    producer, store = scratch / 'producer', scratch / 'publish-store'
    shutil.copytree(w / 'producer', producer, ignore=shutil.ignore_patterns('.hirsel-core', 'hirsel.lock'))
    publish = ['registry', 'publish', '--signing-key', w / 'publisher.der']
    output = cli(args, producer, store, *publish, '--workspace', '--enroll', w / 'enroll')
    receipts = dict(re.findall(r'^fixture\.test/(\w+)@1 1\.0\.0 sha256:([0-9a-f]{64}) ', output, re.M))
    check(len(receipts) == 30, output)
    print('30 verified workspace receipts', flush=True)
    apps = []
    results = []
    for i in range(args.samples):
        app = scratch / f'consumer-{i}'
        app.mkdir()
        (app / 'hirsel.hcfg').write_text('''pkg_v1.Package(
    name: "local/app",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("main.hirsel", "main")],
    dependencies: [],
)
''')
        (app / 'main.hirsel').write_text('pub fn main() -> Result[(), Exit] { Ok(()) }\n')
        apps.append(app)
        packages = app / 'store'
        result = measured(args, app, packages, proxy, 'G3', ['add', PUBLISHER + '/root@1.0.0'])
        locked = lock_rows(app)
        for name, digest in receipts.items():
            check(locked[f'{PUBLISHER}/{name}@1']['root'] == 'sha256:' + digest, name)
        check(len(locked) == 30 and result['requests']['blobs'] == 30, result)
        results.append(result)

    app = apps[0]
    before = proxy.connections, len(proxy.rows)
    frozen = cli(args, app, app / 'store', 'check', '--frozen', trace=w / 'frozen.sockets')
    sockets = (w / 'frozen.sockets').read_text()
    check('socket(AF_INET' not in sockets and 'connect(' not in sockets, sockets)
    check(before == (proxy.connections, len(proxy.rows)), frozen)
    base = producer / 'base/hirsel.hcfg'
    original = base.read_text()
    base.write_text(original.replace('version = "1.0.0"', 'version = "1.0.1"'))
    new = cli(args, base.parent, store, *publish)
    updated = re.search(r'fixture\.test/base@1 1\.0\.1 sha256:([0-9a-f]{64}) ', new)[1]
    status(w, 'retire', receipts['base'])
    # Use a separate copy so this refusal cannot warm the measured update.
    trial = scratch / 'cooldown'
    shutil.copytree(app, trial)
    refusal = cli(args, trial, trial / 'store', 'update', PUBLISHER + '/base', expected=2)
    check('72 h' in refusal and 'base@1.0.1' in refusal, refusal)
    for app in apps:
        result = measured(args, app, app / 'store', proxy, 'G4', ['update', PUBLISHER + '/base@1.0.1'])
        locked = lock_rows(app)
        check(len(locked) == 30 and all(
            locked[f'{PUBLISHER}/{name}@1']['root'] == 'sha256:' + (updated if name == 'base' else root)
            for name, root in receipts.items()), locked)
        check(result['requests']['blobs'] == 1, result)
        results.append(result)
    audit = json.loads(cli(args, app, app / 'store', 'audit', '--json'))
    row = next(p for p in audit['packages'] if p['id'] == PUBLISHER + '/base@1')
    check(row['root'] == 'sha256:' + updated and row['registry']['status'] == 'active'
          and 'preview (quorum 0)' in row['registry']['trust']
          and row['card']['verdict'] == 'pure' and 'carried' in row['card'] and 'reached' in row['card'], row)
    status(w, 'takedown', updated)
    fresh = scratch / 'taken-down'
    fresh.mkdir()
    for name in ('hirsel.hcfg', 'hirsel.lock', 'main.hirsel'):
        shutil.copy(app / name, fresh / name)
    start = len(proxy.rows)
    refused = cli(args, fresh, fresh / 'store', 'check', expected=2)
    check('taken down' in refused.lower() or 'takedown' in refused.lower(), refused)
    check(all(path != f'/c/{updated}.zst' for path, _, _, _ in proxy.rows[start:]), refused)
    print('Frozen check, cooldown, exact update, audit and fresh takedown refusal passed.', flush=True)
    passed = True
    for gate, (max_ns, max_bytes) in BARS.items():
        rows = [row for row in results if row['gate'] == gate]
        times = sorted(row['ns'] for row in rows)
        percentile = lambda p: times[math.ceil(p * len(rows)) - 1]
        good = all(row['ns'] <= max_ns and row['bytes'] <= max_bytes for row in rows)
        print(json.dumps(dict(gate=gate, p50_ns=percentile(.5), p95_ns=percentile(.95),
                              max_bytes=max(row['bytes'] for row in rows), passed=good)), flush=True)
        passed &= good
    return 0 if passed else 1


def measured(args, *params):
    sys.path.insert(0, str(REPO / 'core/tools'))
    import timing_shield
    local = timing_shield.local_host()
    hold = timing_shield.hold(timing_shield.REGIMES, timing_shield.BOARD, memory_bytes=2 << 30)
    with hold if local else nullcontext():
        return sample(args, *params, pin=local)


def run(args):
    w = args.work
    check(socket.gethostbyname(PUBLISHER) == DOMAIN_IP
          and socket.gethostbyname('registry.test') == '127.0.0.1', 'fixture namespace required')
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(w / 'tls.pem', w / 'tls.key')
    with ExitStack() as stack:
        stack.enter_context(server(('127.0.0.1', 8113), Metadata))
        stack.enter_context(server(('127.0.0.1', 8112), Bucket, tls, objects={}, lock=threading.Lock()))
        proxy = stack.enter_context(server(('127.0.0.1', 4443), Proxy, tls, rows=[], connections=0))
        with subprocess.Popen([w / 'registry', f'--hirsel-compiler-worker={args.driver}'], cwd=w, env=dict(os.environ, SSL_CERT_FILE=str(w / 'ca.pem'))) as service:
            try:
                deadline = time.monotonic() + 45
                while True:
                    check(service.poll() is None, f'registry exited with {service.returncode} at startup')
                    try:
                        with socket.create_connection(('127.0.0.1', 8111), .25):
                            break
                    except OSError:
                        check(time.monotonic() < deadline, 'registry failed to listen')
                        time.sleep(.05)
                with tempfile.TemporaryDirectory(dir=w) as scratch:
                    return qualify(args, proxy, Path(scratch))
            finally:
                if service.poll() is None:
                    comm = subprocess.check_output(['ps', '-o', 'comm=', '-p', str(service.pid)], text=True).strip()
                    check(comm == 'registry', f'unexpected service {comm}')
                    service.terminate()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['prepare', 'run', 'inside', 'domain', 'smoke', 'smoke-inside'])
    for arg in ('work', 'driver', 'runtime', 'package-layer'):
        parser.add_argument('--' + arg, type=lambda s: Path(s).resolve())
    parser.add_argument('--samples', type=int, default=5)
    args = parser.parse_args()
    if args.action == 'smoke':
        setup = 'set -eu; ip link set lo up mtu 1500\n' + SHAPE + """
exec setpriv --reuid "$SUDO_UID" --regid "$SUDO_GID" --init-groups -- "$@"
"""
        return subprocess.call(['sudo', '-n', 'unshare', '--net', 'sh', '-ec', setup, 'fixture',
                                sys.executable, Path(__file__).resolve(), 'smoke-inside'])
    if args.action == 'smoke-inside':
        smoke()
        for path, expected in [('/log/checkpoint', 'release_checkpoint'), ('/log/tile/entries/000', 'bundles'),
                               ('/log/tile/0/000', 'tiles'), ('/status/tile/entries/000.p/1', 'status_delta')]:
            check(path_class(path) == expected, path)
        return 0
    if args.action == 'domain':
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.load_cert_chain(args.work / 'tls.pem', args.work / 'tls.key')
        with server((DOMAIN_IP, 443), Enrollment, tls, document=(args.work / 'document').read_bytes()):
            sys.stdin.buffer.read()
        return 0
    if not args.work or not args.driver or args.samples <= 0:
        parser.error('pass --work, --driver and a positive sample count')
    if args.action == 'prepare':
        if not args.runtime or not args.package_layer:
            parser.error('prepare requires --runtime and --package-layer')
        prepare(args)
    elif args.action == 'inside':
        return run(args)
    else:
        return subprocess.call(['sudo', '-n', 'unshare', '--mount', '--net', '--propagation', 'private',
            'bash', '-eo', 'pipefail', '-c', NAMESPACE, 'fixture', args.work / 'hosts', sys.executable, Path(__file__).resolve(),
            'inside', '--work', args.work, '--driver', args.driver, '--samples', str(args.samples)])
    return 0


if __name__ == '__main__':
    sys.exit(main())
