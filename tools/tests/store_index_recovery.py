"""R4. The fixture's primary rows are intact. Its covering index has all three logical faults."""
from pathlib import Path
import shutil
import subprocess


def check_index_repair(hirsel, release, root):
    damaged = root / "damaged"
    damaged.mkdir()
    subprocess.run([release, "seed-index"], cwd=damaged, check=True)

    # Build the fault injector in an isolated Store package so only the drill
    # can make these edits. The indexed release supplies its own catalog.
    package = root / "store-fixture"
    shutil.copytree("core/programs/store", package)
    source = Path("tools/tests/fixtures/store-recovery-release/damage-index.hirsel")
    with (package / "store.hirsel").open("ab") as module:
        module.write(b"\n" + source.read_bytes())
    injector = root / "damage-recovery-index"
    subprocess.run([hirsel, "build", package, "--entry", "store@1/store/damage_recovery_index",
                    "--dir", "/proc/self/cwd", "--grant", "Clock", "--grant", "Random", "--grant", "Tasks",
                    "--limit", "2147483648", "-o", injector], cwd=root, check=True)
    subprocess.run([injector], cwd=damaged, check=True)
    saved = {path.name: path.read_bytes() for path in damaged.iterdir()}

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
    pack = max(physical.glob("p-*"), key=lambda path: int(path.name[2:]))
    body = bytearray(pack.read_bytes())
    body[len(body) // 2] ^= 1
    pack.write_bytes(body)
    run("verify", physical, code=1)
    run("verify", physical, "--release", release, code=1)
    assert all((damaged / key).read_bytes() == data for key, data in saved.items())
    print("R4: physical check passed; release found Missing, Extra and two Payload findings; repaired rows matched; combined damage refused", flush=True)
