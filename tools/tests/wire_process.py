"""Stop only a direct endpoint child started by these wire qualification scripts."""
import errno
from pathlib import Path
import socket
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from loopback_peer import accept_from as accept_from


def stop_process(process):
    if process.poll() is not None:
        return
    observed = subprocess.run(['ps', '-o', 'comm=', '-p', str(process.pid)], capture_output=True, text=True)
    if process.poll() is not None:
        return
    expected = Path(process.args[0]).name[:15]
    assert observed.returncode == 0 and observed.stdout.strip() == expected, observed.stdout
    process.terminate()
    process.wait()


def stop_listener(process, port):
    stop_process(process)
    # A pending io_uring poll can hold the listener after the child is reaped.
    # Check that the address can be rebound before launching the next fixture.
    deadline = time.monotonic() + 2
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        while True:
            try:
                probe.bind(('127.0.0.1', port))
                return
            except OSError as error:
                if error.errno != errno.EADDRINUSE or time.monotonic() >= deadline:
                    raise
                time.sleep(.02)
