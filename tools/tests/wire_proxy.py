#!/usr/bin/env python3
"""Exercise the shipped direct-ingress configs against spoofed forwarded headers."""
import http.client
import http.server
import os
from pathlib import Path
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time

from wire_process import in_docker_loopback

ROOT = Path(__file__).resolve().parents[2]
NGINX = 'nginx:1.28.0-alpine'


def main():
    network = os.environ.get('HIRSEL_WIRE_DOCKER_NETWORK')
    if network is None:
        return in_docker_loopback(NGINX)
    observed = []
    probe = secrets.token_hex(16)

    class Listener(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.headers.get('X-Hirsel-Test-Peer') != probe:
                print(f'skipped unexpected loopback connection from {self.client_address}',
                      file=sys.stderr, flush=True)
                self.close_connection = True
                return
            observed.append(self.headers.get_all('X-Forwarded-For'))
            self.send_response(200)
            self.end_headers()

        def log_message(self, *_):
            pass

    listener = http.server.HTTPServer(('127.0.0.1', 0), Listener)
    backend = listener.server_port
    thread = threading.Thread(target=listener.serve_forever, daemon=True)
    thread.start()
    try:
        with tempfile.TemporaryDirectory(prefix='wire-proxy-') as tmp:
            for name, image, file, destination in [
                ('caddy', 'caddy:2.10.2-alpine', 'Caddyfile', '/etc/caddy/Caddyfile'),
                ('nginx', NGINX, 'nginx.conf', '/etc/nginx/nginx.conf'),
            ]:
                # Neither image inherits a listener. The private namespace has
                # no other process that can claim the port before the proxy binds.
                with socket.socket() as reservation:
                    reservation.bind(('127.0.0.1', 0))
                    frontend = reservation.getsockname()[1]
                    config = Path(tmp) / file
                    text = (ROOT / 'deploy/wire' / file).read_text()
                    assert text.count('8080') == text.count('127.0.0.1:8081') == 1
                    config.write_text(text.replace('8080', str(frontend))
                                      .replace('127.0.0.1:8081', f'127.0.0.1:{backend}'))
                container = subprocess.check_output([
                    'docker', 'run', '-d', '--rm', '--network', network,
                    '-v', f'{config}:{destination}:ro', image,
                ], text=True).strip()
                try:
                    before = len(observed)
                    for _ in range(100):
                        connection = http.client.HTTPConnection('127.0.0.1', frontend, timeout=1)
                        try:
                            connection.request('GET', '/hirsel', headers={
                                'X-Forwarded-For': '9.9.9.9', 'X-Hirsel-Test-Peer': probe})
                            response = connection.getresponse()
                            response.read()
                            assert response.status == 200
                            break
                        except (ConnectionError, OSError):
                            time.sleep(.05)
                        finally:
                            connection.close()
                    else:
                        raise AssertionError(f'{name} did not start')
                    assert observed[before:] == [['127.0.0.1']], (name, observed[before:])
                    print(f'{name}: PASS spoofed 9.9.9.9 replaced by 127.0.0.1')
                finally:
                    subprocess.run(['docker', 'stop', container], check=True, stdout=subprocess.DEVNULL)
    finally:
        listener.shutdown()
        listener.server_close()
        thread.join()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
