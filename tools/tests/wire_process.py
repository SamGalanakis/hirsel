"""Stop only a direct endpoint child started by these wire qualification scripts."""
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from loopback_peer import accept_from as accept_from, owned_by as owned_by


def in_docker_loopback(image):
    """Run this script and its fixture containers in a private loopback namespace."""
    anchor = subprocess.check_output([
        'docker', 'run', '-d', '--rm', '--network', 'none', '--entrypoint', 'sleep', image, 'infinity',
    ], text=True).strip()
    try:
        pid = subprocess.check_output(['docker', 'inspect', '-f', '{{.State.Pid}}', anchor], text=True).strip()
        return subprocess.run([
            'sudo', '-n', 'nsenter', f'--net=/proc/{pid}/ns/net', '--',
            'setpriv', '--reuid', str(os.getuid()), '--regid', str(os.getgid()), '--init-groups', '--reset-env',
            'env', f'HIRSEL_WIRE_DOCKER_NETWORK=container:{anchor}',
            sys.executable, *sys.argv,
        ]).returncode
    finally:
        subprocess.run(['docker', 'stop', anchor], check=True, stdout=subprocess.DEVNULL)


def read_port(process, timeout=8):
    """The `listening <port>` line a wire fixture writes after host.listen(0)."""
    lines = queue.Queue()
    threading.Thread(target=lambda: lines.put(process.stdout.readline()), daemon=True).start()
    line = lines.get(timeout=timeout)
    assert line.startswith(b'listening '), (line, process.poll())
    port = int(line[len(b'listening '):])
    assert 0 < port < 65536, port
    return port


def start_listening(path, *args, env=None, stderr=subprocess.PIPE):
    process = subprocess.Popen([str(Path(path).resolve()), *map(str, args)], stdout=subprocess.PIPE,
                               stderr=stderr, env=env)
    try:
        return process, read_port(process)
    except BaseException:
        stop_process(process)
        raise


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
