#!/usr/bin/env python3
"""tls_front_stream (launch.md H1-b): route the shipped TLS fronts by SNI in containers.

HAProxy runs the configuration platform's generator writes, and nginx runs the
hand-written deploy/ingress/nginx.conf. Behind each, a Hirsel listener accepts
with std.proxy.accept and logs every accept. The oracle is OpenSSL's s_client
and the address the kernel gave its socket, never Hirsel's decoder.
"""
import argparse
import http.server
import os
from pathlib import Path
import queue
import socket
import subprocess
import tempfile
import threading
import time

from wire_process import in_docker_loopback, start_listening, stop_process

ROOT = Path(__file__).resolve().parents[2]

INGRESS = ROOT / 'deploy/ingress'
HAPROXY = 'haproxy:3.2.27-alpine'
NGINX = 'nginx:1.31.4-alpine'
CLIENT = '127.0.0.5'  # not the front's address, so a direct dial is untrusted
FRONT = ('127.0.0.1', 443)
CC = ROOT / 'tools/buck2/toolchains/llvm-files/bin/hirsel-clang'


def build(compiler, runtime, work):
    built = {}
    for entry, grants in [('haproxy', ['Stdout']),
                          ('listener', ['Stdout', 'Clock', 'Net=Listen 127.0.0.1/32 0-65535'])]:
        command = [str(compiler), 'build', str(ROOT / 'core/programs/wire'), '--libraries', str(ROOT / 'core'),
                   '--source-core', '--store', str(work / 'store'),
                   '--entry', 'wire@1/front/' + entry, '--backend', 'cranelift', '--runtime', str(runtime),
                   '--limit', '268435456', '--cc', str(CC), '-o', str(work / entry)]
        for grant in grants:
            command += ['--grant', grant]
        subprocess.run(command, cwd=ROOT, check=True)
        built[entry] = work / entry
    return built


def replace_exact(text, old, new, count):
    assert text.count(old) == count, (old, text.count(old))
    return text.replace(old, new)


def certificates(work):
    certs = work / 'certs'
    (certs / 'haproxy').mkdir(parents=True)
    for name in ('web', 'stream'):
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes',
                        '-days', '1', '-subj', f'/CN={name}.test', '-addext', f'subjectAltName=DNS:{name}.test',
                        '-keyout', certs / f'{name}.key', '-out', certs / f'{name}.crt'],
                       check=True, capture_output=True)
        # HAProxy loads every file in its directory, so it gets only the combined PEMs.
        (certs / 'haproxy' / f'{name}.pem').write_bytes((certs / f'{name}.crt').read_bytes() + (certs / f'{name}.key').read_bytes())
    for path in [certs, *certs.rglob('*')]:
        path.chmod(0o755 if path.is_dir() else 0o644)
    return certs


class Lines:
    """The Hirsel listener's log, one line per accept, header, refusal or end."""

    def __init__(self, process):
        self.queue = queue.Queue()
        threading.Thread(target=self.pump, args=(process.stdout,), daemon=True).start()

    def pump(self, stream):
        for line in stream:
            self.queue.put(line.decode().strip())

    def next(self, timeout=5):
        return self.queue.get(timeout=timeout)

    def none(self, wait=.5):
        time.sleep(wait)
        assert self.queue.empty(), f'the stream listener saw {self.queue.get()}'


def s_client(port, *flags):
    return subprocess.Popen(['openssl', 's_client', '-connect', '%s:%d' % FRONT, '-bind', f'{CLIENT}:{port}', *flags],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)


def law(front, lines, forwarded, port):
    # A stream name reaches the stream listener with the client's own socket address.
    client = s_client(port, '-servername', 'stream.test')
    accepted = lines.next()
    assert accepted.startswith('accept 127.0.0.1:'), (front, accepted)
    header = lines.next()
    assert header == f'header {CLIENT}:{port} 127.0.0.1:443', (front, header)
    token = f'ping {front} {port}\n'.encode()
    client.stdin.write(token)
    client.stdin.flush()
    echoed = b''
    deadline = time.monotonic() + 5
    while token not in echoed:
        assert time.monotonic() < deadline, (front, echoed[-200:])
        echoed += client.stdout.read1(4096)
    client.stdin.close()  # s_client sends close_notify and exits
    assert client.wait(5) == 0, front
    assert lines.next() == f'end {len(token)}', front
    print(f'{front}: PASS stream.test reached the stream listener as {CLIENT}:{port}, echoed, ended', flush=True)

    # An HTTPS name reaches the HTTP listener with one X-Forwarded-For, never the stream listener.
    before = len(forwarded)
    client = s_client(port + 1, '-servername', 'web.test', '-quiet')
    out, _ = client.communicate(b'GET /front HTTP/1.1\r\nHost: web.test\r\nX-Forwarded-For: 9.9.9.9\r\nConnection: close\r\n\r\n', 10)
    assert b'200 OK' in out, (front, out[-300:])
    assert forwarded[before:] == [[CLIENT]], (front, forwarded[before:])
    lines.none()
    print(f'{front}: PASS web.test reached the HTTP listener with X-Forwarded-For {CLIENT}', flush=True)

    # No SNI, or an unknown name, reaches neither listener.
    for offset, flags in [(2, ['-noservername']), (3, ['-servername', 'other.test'])]:
        client = s_client(port + offset, *flags)
        client.communicate(b'ping\n', 10)
        lines.none()
        assert len(forwarded) == before + 1, (front, flags, forwarded[before:])
    print(f'{front}: PASS no SNI and other.test reached no listener', flush=True)


def direct(lines, stream_port):
    # A connection from outside the trusted front is closed before any byte is read.
    with socket.socket() as s:
        s.bind((CLIENT, 0))
        s.connect(('127.0.0.1', stream_port))
        port = s.getsockname()[1]
        s.sendall(b'\r\n\r\n\x00\r\nQUIT\n\x21\x11\x00\x0c\x7f\x00\x00\x05\x7f\x00\x00\x01\x9c\x41\x01\xbb')
        assert lines.next() == f'accept {CLIENT}:{port}'
        assert lines.next() == 'refused Untrusted'
        s.settimeout(5)
        try:
            assert s.recv(1) == b''
        except ConnectionResetError:
            pass
    print('direct: PASS a non-front peer was refused Untrusted and closed', flush=True)


def run_front(network, name, image, mounts, lines, forwarded, port, user=()):
    container = f'ingress-front-{os.getpid()}-{name}'
    subprocess.run(['docker', 'run', '-d', '--rm', '--name', container, '--network', network,
                    *user, *[arg for mount in mounts for arg in ('-v', mount)], image], check=True, stdout=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                with socket.create_connection(FRONT, .2):
                    break
            except OSError:
                time.sleep(.05)
        else:
            raise AssertionError(f'{name} did not listen on 443')
        version = subprocess.run(['docker', 'exec', container, *(['haproxy', '-v'] if name == 'haproxy' else ['nginx', '-v'])],
                                 capture_output=True, text=True)
        print((version.stdout + version.stderr).splitlines()[0], flush=True)
        law(name, lines, forwarded, port)
    finally:
        subprocess.run(['docker', 'stop', container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, required=True)
    parser.add_argument('--haproxy-config', type=Path, help='run this HAProxy configuration instead of the committed one')
    args = parser.parse_args()
    network = os.environ.get('HIRSEL_WIRE_DOCKER_NETWORK')
    if network is None:
        return in_docker_loopback(NGINX)
    with tempfile.TemporaryDirectory(prefix='ingress-front-') as scratch:
        work = Path(scratch)
        work.chmod(0o755)
        built = build(args.compiler.resolve(), args.runtime.resolve(), work)

        # One source: the committed HAProxy front is exactly the generator's output.
        generated = subprocess.run([str(built['haproxy'])], check=True, capture_output=True, text=True).stdout
        committed = (INGRESS / 'haproxy.cfg').read_text()
        assert generated == committed, 'deploy/ingress/haproxy.cfg differs from the generator; regenerate it'
        haproxy = (args.haproxy_config or INGRESS / 'haproxy.cfg').read_text()
        # Keep the PROXY header's destination on port 443 in the private namespace.
        haproxy = replace_exact(haproxy, '    bind :443 ', '    bind 127.0.0.1:443 ', 1)
        nginx = replace_exact((INGRESS / 'nginx.conf').read_text(),
                              '        listen 443 ssl', '        listen 127.0.0.1:443 ssl', 3)
        certs = certificates(work)

        forwarded = []

        class Web(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                forwarded.append(self.headers.get_all('X-Forwarded-For'))
                self.send_response(200)
                self.send_header('Content-Length', '0')
                self.end_headers()

            def log_message(self, *_):
                pass

        web = http.server.HTTPServer(('127.0.0.1', 0), Web)
        thread = threading.Thread(target=web.serve_forever, daemon=True)
        thread.start()
        try:
            listener, stream_port = start_listening(built['listener'], stderr=None)
            try:
                for name, config in [('haproxy.cfg', haproxy), ('nginx.conf', nginx)]:
                    config = replace_exact(config, '127.0.0.1:18201', f'127.0.0.1:{web.server_port}', 1)
                    config = replace_exact(config, '127.0.0.1:18202', f'127.0.0.1:{stream_port}', 1)
                    (work / name).write_text(config)
                    (work / name).chmod(0o644)
                lines = Lines(listener)
                direct(lines, stream_port)
                run_front(network, 'haproxy', HAPROXY, [f'{work / "haproxy.cfg"}:/usr/local/etc/haproxy/haproxy.cfg:ro',
                                               f'{certs / "haproxy"}:/usr/local/etc/haproxy/certs:ro'],
                          lines, forwarded, 40100, user=('--user', '0'))
                run_front(network, 'nginx', NGINX, [f'{work / "nginx.conf"}:/etc/nginx/nginx.conf:ro', f'{certs}:/etc/nginx/certs:ro'],
                          lines, forwarded, 40200)
            finally:
                stop_process(listener)
        finally:
            web.shutdown()
            web.server_close()
            thread.join()
    print('tls_front_stream: PASS', flush=True)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
