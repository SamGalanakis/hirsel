#!/usr/bin/env python3
"""NET-7 loopback half-close/drain and continuously ready deadline controls."""
import argparse
from pathlib import Path
import socket
import subprocess
import threading
import time

from wire_process import accept_from, stop_process


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--probe', type=Path, required=True)
    args = parser.parse_args()
    for continuous in [False, True]:
        with socket.socket() as listener:
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(('127.0.0.1', 0))
            listener.listen(1)
            listener.settimeout(5)
            process = subprocess.Popen([str(args.probe.resolve()), str(listener.getsockname()[1])], stderr=subprocess.PIPE)
            try:
                peer = accept_from(listener, process.pid)
                with peer:
                    peer.settimeout(5)
                    # EOF is output shutdown; the client still owns the read half.
                    assert peer.recv(1) == b''
                    before = time.monotonic()
                    if not continuous:
                        peer.sendall(b'x' * 131072)
                        peer.shutdown(socket.SHUT_WR)
                    else:
                        def send_ready():
                            try:
                                while process.poll() is None:
                                    peer.sendall(b'x' * 65536)
                            except (OSError, TimeoutError):
                                pass
                        sending = threading.Thread(target=send_ready, daemon=True)
                        sending.start()
                    _, errors = process.communicate(timeout=5)
                    elapsed = time.monotonic() - before
                    if continuous:
                        sending.join(timeout=5)
                        assert .8 <= elapsed < 2, elapsed
                        # TimedOut is an error value from the main Result.
                        assert process.returncode == 1, (process.returncode, errors)
                        print(f'close_deadline TCP continuous input: PASS {elapsed:.3f}s', flush=True)
                    else:
                        assert process.returncode == 0 and errors == b'', (process.returncode, errors)
                        print('TCP finish: PASS write shutdown, 128 KiB drained, EOF', flush=True)
            finally:
                if process.poll() is None:
                    stop_process(process)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
