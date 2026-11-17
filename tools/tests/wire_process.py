"""Stop only a direct endpoint child started by these wire qualification scripts."""
from pathlib import Path
import subprocess


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
