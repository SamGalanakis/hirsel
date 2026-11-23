#!/usr/bin/env python3
"""Exercise the shipped direct-ingress configs against spoofed forwarded headers."""
import http.client
import http.server
from pathlib import Path
import secrets
import subprocess
import sys
import threading
import time

ROOT = Path(__file__).resolve().parents[2]


def main():
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

    # The examples listen on 8080 and forward to this protocol listener on 8081.
    listener = http.server.HTTPServer(('127.0.0.1', 8081), Listener)
    thread = threading.Thread(target=listener.serve_forever, daemon=True)
    thread.start()
    try:
        for name, image, file, destination in [
            ('caddy', 'caddy:2.10.2-alpine', 'Caddyfile', '/etc/caddy/Caddyfile'),
            ('nginx', 'nginx:1.28.0-alpine', 'nginx.conf', '/etc/nginx/nginx.conf'),
        ]:
            container = subprocess.check_output([
                'docker', 'run', '-d', '--rm', '--network', 'host', '--cpuset-cpus', '0-1',
                '-v', f'{ROOT / "deploy/wire" / file}:{destination}:ro', image,
            ], text=True).strip()
            try:
                for _ in range(100):
                    try:
                        connection = http.client.HTTPConnection('127.0.0.1', 8080, timeout=1)
                        connection.request('GET', '/hirsel', headers={
                            'X-Forwarded-For': '9.9.9.9', 'X-Hirsel-Test-Peer': probe})
                        response = connection.getresponse()
                        response.read()
                        assert response.status == 200
                        connection.close()
                        break
                    except (ConnectionError, OSError):
                        time.sleep(.05)
                else:
                    raise AssertionError(f'{name} did not start')
                assert observed[-1] == ['127.0.0.1'], (name, observed[-1])
                print(f'{name}: PASS spoofed 9.9.9.9 replaced by 127.0.0.1')
            finally:
                subprocess.run(['docker', 'stop', container], check=True, stdout=subprocess.DEVNULL)
    finally:
        listener.shutdown()
        listener.server_close()
        thread.join()


if __name__ == '__main__':
    main()
