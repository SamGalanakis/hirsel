"""R4. The fixture's primary rows are intact. Its covering index has all three logical faults."""
from base64 import b64decode
import json
from pathlib import Path
import shutil
import subprocess


def check_index_repair(hirsel, release, root, fixture):
    damaged = root / "damaged"
    damaged.mkdir()
    saved = {k: b64decode(v, validate=True) for k, v in json.loads(fixture.read_bytes()).items()}
    for name, data in saved.items():
        (damaged / name).write_bytes(data)
    (damaged / "LOCK").touch()

    def run(*args, code=0):
        p = subprocess.run([hirsel, "store", *args], text=True, capture_output=True)
        assert p.returncode == code, (args, p.returncode, p.stdout, p.stderr)
        return p.stdout + p.stderr

    run("verify", damaged)
    report = run("verify", damaged, "--release", release, code=1)
    faults = sorted(line.split()[2] for line in report.splitlines() if line.startswith("finding cover "))
    assert faults == ["Extra", "Missing", "Payload", "Payload"], report
    repaired = root / "repaired"
    shutil.copytree(damaged, repaired)
    run("repair", repaired, "--release", release)
    report = run("verify", repaired, "--release", release)
    assert "walked 4 rows and entries, 0 problems" in report, report
    subprocess.run([release, "check-cut"], cwd=repaired, check=True)
    physical = root / "combined"
    shutil.copytree(damaged, physical)
    pack = physical / "p-3"
    body = bytearray(pack.read_bytes())
    body[len(body) // 2] ^= 1
    pack.write_bytes(body)
    run("verify", physical, code=1)
    run("verify", physical, "--release", release, code=1)
    assert all((damaged / key).read_bytes() == data for key, data in saved.items())
    print("R4: physical check passed; release found Missing, Extra and two Payload findings; repaired rows matched; combined damage refused", flush=True)
