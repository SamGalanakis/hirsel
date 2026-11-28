"""L-A9 Directory restore through the installed operator CLI."""
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


CORPUS = Path("core/programs/store/corpus/format-15/directory").resolve()


def contents(directory):
    return {p.relative_to(directory): p.read_bytes()
            for p in directory.rglob("*") if p.is_file()}


def drill(hirsel, work):
    env = dict(os.environ, XDG_STATE_HOME=str(work / "state"), TMPDIR=str(work))

    def run(*args, code=0, stdin=None, fault=None):
        p = subprocess.run([hirsel, *args], cwd=work, env=env, text=True,
                           input=stdin, capture_output=True, check=False)
        assert p.returncode == code, (args, p.returncode, p.stdout, p.stderr)
        if fault:
            assert fault in p.stderr or fault in p.stdout, (args, p.stdout, p.stderr)
        if "--json" in args:
            reply = json.loads(p.stdout)
            assert (reply["v"], reply["exit"]) == (1, code), reply
            return reply
        return p.stdout

    source = work / "source"
    export = work / "backup"
    dest = work / "child"
    # The executor mounts corpus inputs read-only. Faults belong to this private copy.
    shutil.copytree(CORPUS, source, copy_function=shutil.copyfile)
    source.chmod(0o700)
    (source / "LOCK").touch()
    original = contents(source)
    # The frozen writer acknowledged main version 4. Its j-5 END covers bytes
    # [0, 654), with one transaction following the 72-byte segment header.
    for case, code, fault in [
        ("covered-truncation", 1, "Truncated"), ("covered-flip", 1, "Checksum"),
        ("torn-tail", 0, None), ("missing-end", 0, None), ("invalid-end", 0, None),
        ("lost-acknowledged-commit", 0, None),
    ]:
        damaged = work / case
        shutil.copytree(source, damaged)
        journal = damaged / "j-5"
        end = damaged / "j-5.end"
        if case == "covered-truncation":
            journal.write_bytes(journal.read_bytes()[:100])
        elif case == "covered-flip":
            body = bytearray(journal.read_bytes())
            body[200] ^= 1
            journal.write_bytes(body)
        elif case == "torn-tail":
            journal.write_bytes(journal.read_bytes()[:654] + b"torn")
        elif case == "invalid-end":
            body = bytearray(end.read_bytes())
            body[-1] ^= 1
            end.write_bytes(body)
        else:
            end.unlink()
            if case == "lost-acknowledged-commit":
                journal.write_bytes(journal.read_bytes()[:72])
        evidence = contents(damaged)
        run("store", "verify", damaged, code=code, fault=fault)
        if case == "lost-acknowledged-commit":
            assert run("store", "inspect", damaged).splitlines()[0].endswith(" head 3")
        assert contents(damaged) == evidence
    with (source / "LOCK").open("rb") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        run("store", "backup", source, "held", code=75)
    stamp = run("store", "backup", source, export).split()
    assert stamp[0] == "backup" and stamp[2:] == ["head", "4"], stamp
    assert contents(source) == original
    source.rename(work / "offline")
    evidence = contents(export)

    def plan(target, image=export, to=dest, code=0):
        return run("store", "restore", "--from", image, "--target", target,
                   "--to", to, "--plan", "--json", code=code)

    def vote(review, verdict="Approve", code=0, digest=None):
        doc = 'host_v1.Decision(plan_digest: "{}", verdict: {}, entries: [])'.format(
            digest or review["result"]["plan_digest"], verdict)
        return run("op", "decide", review["operation"]["id"], "--decision", "-",
                   "--json", code=code, stdin=doc)

    run("store", "restore", export, dest, code=2)
    assert plan("latest")["result"]["point"]["version"] == "4"
    assert plan("at:1,1795860956983")["result"]["point"]["version"] == "3"
    review = plan("version:1,3")
    cut = review["result"]["point"]
    assert (cut["parent"], cut["branch"], cut["version"]) == (stamp[1], "1", "3")
    assert review["outcome"]["effect"] == "none"
    assert plan("boundary:" + cut["boundary"])["result"]["point"]["boundary"] == cut["boundary"]
    plan("version:main,5", code=1)
    plan("latest", to=export / "nested", code=2)
    vote(review, digest="00" * 32, code=2)
    assert not dest.exists()
    for name, remove in (("MANIFEST", False), ("p-1", False), ("p-1", True)):
        damaged = work / ("damaged-" + name + str(remove))
        shutil.copytree(export, damaged)
        path = damaged / name
        if remove:
            path.unlink()
        else:
            data = bytearray(path.read_bytes())
            data[len(data) // 2] ^= 1
            path.write_bytes(data)
        before = contents(damaged)
        plan("latest", image=damaged, code=1)
        assert contents(damaged) == before and not dest.exists()
    reply = vote(review)
    result = reply["result"]
    child = result["child"]
    assert child != stamp[1] and result["attempt"] == review["result"]["attempt"]
    assert all(result[k] == v for k, v in cut.items())
    assert result["publication"] == "Published" and result["verification"] == "store_graph"
    assert run("store", "inspect", dest).splitlines()[0] == f"store {child} head 3"
    run("store", "verify", dest)
    assert run("store", "backup", dest, "second").split() == ["backup", child, "head", "3"]
    assert contents(export) == evidence
    export.rename(work / "unmounted")
    # Lose the client's publication reply. Get must recover from the Store origin.
    published, = (work / "state").rglob("*.published.json")
    published.unlink()
    op = review["operation"]["id"]
    assert run("op", "get", op, "--json")["result"] == result
    assert vote(review)["result"] == result
    dest.rename(work / "published")
    shutil.copytree(work / "offline", dest)
    run("op", "get", op, "--json", code=1)
    assert contents(dest) == original
    print("L-A9 Directory: held writer, corruption, four selectors, pinned approval, fresh child, retry and second backup")


if __name__ == "__main__":
    executable = (Path(sys.argv[1]) if len(sys.argv) > 1 else
                  Path(os.environ["HIRSEL_TOOLCHAIN_BUNDLE"]) / "hirsel")
    with tempfile.TemporaryDirectory(prefix="store-drill-") as tmp:
        drill(executable.resolve(), Path(tmp))
