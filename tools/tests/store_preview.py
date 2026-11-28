"""By-reference Preview lifecycle through the installed CLI and a signed archive."""
import json
import os
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile

from store_archive_server import ArchiveServer, KEYS


def check_preview(hirsel, probe, server, work, descriptor):
    env = dict(os.environ, XDG_STATE_HOME=str(work / "state"))
    for role, key in [("READ", "reader"), ("HOLD", "expiry")]:
        env[f"HIRSEL_ARCHIVE_{role}_ACCESS_KEY_ID"] = key
        env[f"HIRSEL_ARCHIVE_{role}_SECRET_ACCESS_KEY"] = KEYS[key].decode()
    source = work / "place.json"
    source.write_text(json.dumps(descriptor))

    def run(*args, code=0):
        p = subprocess.run([hirsel, *args, "--json"], cwd=work, env=env,
                           capture_output=True, text=True, check=False)
        assert p.returncode == code, (args, p.stdout, p.stderr)
        response = json.loads(p.stdout)
        assert response["exit"] == code, response
        return response["result"]

    before = server.objects.copy()
    created = run("preview", "create", "@" + str(source), "--at", "version:main,3", "--ttl", "5m")
    child = created["child"]
    mount = Path(created["mount"])
    assert (mount / "MANIFEST").is_file()
    assert created["version"] == "3" and child != descriptor["parent"]
    def check_rows(directory, mode):
        subprocess.run([probe, descriptor["endpoint"], mode], cwd=directory,
                       env={"PREVIEW_READER_SECRET": KEYS["reader"].decode()}, check=True)

    check_rows(mount, "write")
    check_rows(mount, "after")
    sibling = run("preview", "create", str(source), "--at", "version:main,3")
    check_rows(Path(sibling["mount"]), "original")
    run("preview", "release", sibling["child"])
    status = run("preview", "status", child)
    for key in ("stamp", "incarnation", "until_ms", "mount"):
        assert status[key] == created[key], (key, status, created)
    extended = run("preview", "extend", child, "--ttl", "20m")
    assert int(extended["until_ms"]) > int(created["until_ms"])
    assert run("preview", "status", child)["until_ms"] == extended["until_ms"]
    backup = subprocess.run([hirsel, "store", "backup", mount, work / "backup"],
                            cwd=work, env=env, text=True, capture_output=True)
    assert backup.returncode != 0 and "PreviewSource" in backup.stderr, backup
    run("preview", "release", child)
    assert run("preview", "status", child)["state"] == "released"
    run("preview", "release", child)
    env["HIRSEL_RESTORE_BUCKET_ACCESS_KEY_ID"] = "child"
    env["HIRSEL_RESTORE_BUCKET_SECRET_ACCESS_KEY"] = KEYS["child"].decode()
    profile = work / "bucket.json"
    location = {k: v for k, v in descriptor.items() if k != "parent"}
    location["prefix"] = "children/preview/"
    profile.write_text(json.dumps(dict(location=location, level="preview",
                                       source_place=None, host_stream=None)))
    profile.chmod(0o600)
    bucket = run("preview", "create", str(source), "--to", "@" + str(profile),
                 "--at", "version:main,3")
    assert bucket["mount"] == location and bucket["version"] == "3"
    assert "children/preview/head" in server.objects
    assert run("preview", "status", bucket["child"])["mount"] == location
    run("preview", "release", bucket["child"])
    assert {k for k in before.keys() | server.objects.keys()
            if not k.startswith("children/") and before.get(k) != server.objects.get(k)} == {
                "fixture/" + descriptor["parent"] + "/archive-head"}


if __name__ == "__main__":
    fixture = Path("tools/tests/fixtures/store-archive-format15.json")
    parent = "0bf3a9c6c8ae7ecec7ae1379345336d2"
    driver = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(os.environ["HIRSEL_TOOLCHAIN_BUNDLE"]) / "hirsel"
    with tempfile.TemporaryDirectory(dir=os.environ.get("HIRSEL_OPERATOR_TEST_ROOT")) as temp, ArchiveServer(fixture) as archive:
        driver = driver.resolve()
        probe = Path(temp) / "child-check"
        if len(sys.argv) > 2:
            probe = Path(sys.argv[2]).resolve()
        else:
            package = Path(temp) / "store-preview-child"
            shutil.copytree(fixture.parent / "store-preview-child", package, ignore=shutil.ignore_patterns("hirsel.lock"))
            subprocess.run([driver, "build", package, "--backend", "llvm",
                            "--dir", "/proc/self/cwd", "--grant", "Clock", "--grant", "Tasks",
                            "--grant", "Random", "--grant", "Net=Resolve 127.0.0.1;Connect 127.0.0.1/32 1-65535",
                            "--grant", "Secret:secret_key=env:PREVIEW_READER_SECRET kdf",
                            "--limit", "2147483648", "-o", probe], check=True)
        check_preview(driver, probe, archive, Path(temp),
                      dict(endpoint=f"http://127.0.0.1:{archive.server_port}", region="us-east-1",
                           bucket="drill", prefix="fixture/", parent=parent))
