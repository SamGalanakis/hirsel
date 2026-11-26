"""Run the nightly checker controls against intact builds with bad inputs."""

import argparse
import ctypes
import json
from pathlib import Path
import shutil
import struct
import subprocess

from sim_campaign import CANARY, ROOT, SEED
from store_crash_tests import bridge_env, compile_damage


STORE_CONTROLS = {
    "index-vs-table": "store@1/store/test_index_walker_corruptions",
    "scrub": "store@1/store/disk_test_scrub_misdirected",
}
CONTROL_NAMES = (*STORE_CONTROLS, "W2 prefix", "SEM-8 replay")


def run(command, **options):
    return subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True,
                          text=True, timeout=300, **options)


def run_controls(log, checks):
    failed = False
    with log.open("x") as output:
        for name, check in checks.items():
            try:
                result, rejected = check()
                row = {"exit_status": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
            except (OSError, TypeError, ValueError, KeyError, RuntimeError, struct.error,
                    subprocess.SubprocessError) as error:
                rejected = False
                row = {"exit_status": None, "error": str(error)}
            print(json.dumps({"oracle": name, "rejected": rejected, **row}), file=output, flush=True)
            print(f"{name}: {'rejected the injected fault' if rejected else 'negative control failed'}", flush=True)
            failed |= not rejected
    return int(failed)


def store_control(args, name):
    programs = args.manifest.parent / "programs"
    if not programs.exists():
        shutil.copytree(ROOT / "core/programs", programs,
                        ignore=shutil.ignore_patterns("hirsel.lock", "node_modules"))
    law = STORE_CONTROLS[name]
    result = run([args.compiler, "test", "--backend", "cranelift", "--runtime", args.runtime,
                  "--libraries", ROOT / "core", "--store", args.work / "store-objects",
                  "--json", "--exact", law, programs / "store"], cwd=args.work)
    if result.returncode:
        return result, False
    tests = json.loads(result.stdout)["tests"]
    return result, (len(tests) == 1 and tests[0]["definition"] == law
                    and tests[0]["source"] == "executed" and tests[0]["passed"] is True)


def w2_control(args):
    executable = compile_damage(
        argparse.Namespace(compiler=args.compiler, runtime=args.crash_runtime, bridge=args.bridge),
        args.work / "w2", "disk", "cranelift",
    )
    env = bridge_env(args.bridge)
    clean = run([executable, "genesis", "0"], env=env, cwd=args.work)
    if clean.returncode or not clean.stdout.startswith("cuts "):
        return clean, False
    result = run([executable, "genesis", "false-ack"], env=env, cwd=args.work)
    return result, (result.returncode == 33 and result.stderr.endswith(
        "W2 false ack: no durable creation, requiring new\nstore phase read: status 33\n"))


def wrong_outcome(tape):
    """Reseal the canary's root END with a different return code (R9)."""
    head = struct.Struct("<B3sIQI")
    _, magic, stream, seq, length = head.unpack_from(tape)
    if magic != b"HTB" or stream != 0 or seq != 0 or length < 19:
        raise ValueError("canary has no root header")
    tape_id = tape[head.size + 3:head.size + 19]
    at = head.size + length + 8
    flags, magic, stream, seq, length = head.unpack_from(tape, at)
    end = at + head.size + length
    # Run END encodes returned status as a literal four-byte LE value.
    if (flags, magic, stream, seq) != (2, b"HTB", 0, 1) or end + 8 != len(tape) or (
        tape[end - 7:end] != b"\x00\x00\x04\x00\x00\x00\x00"
    ):
        raise ValueError("expected the canary's sealed, successful root END")
    block = tape[at:end - 4] + b"\x01\x00\x00\x00"
    digest = ctypes.CDLL("libxxhash.so.0").XXH3_64bits
    digest.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
    digest.restype = ctypes.c_uint64
    sealed = tape_id + block
    return tape[:at] + block + struct.pack("<Q", digest(sealed, len(sealed)))


def replay_control(args):
    canary, = (row for row in json.loads(args.manifest.read_text()) if row["law"] == CANARY)
    executable = canary["recording"]
    tape = args.work / "clean.tape"
    recorded = run([executable], cwd=args.work, env={SEED: "2", "HIRSEL_RECORD": str(tape)})
    if recorded.returncode or recorded.stdout or recorded.stderr:
        return recorded, False
    command = [executable, "--hirsel-replay-recording", "same"]
    clean = run([*command, tape], cwd=args.work, env={})
    if clean.returncode or clean.stdout or clean.stderr:
        return clean, False
    altered = args.work / "wrong-outcome.tape"
    altered.write_bytes(wrong_outcome(tape.read_bytes()))
    result = run([*command, altered], cwd=args.work, env={})
    return result, (result.returncode == 1 and not result.stdout
                    and result.stderr.startswith("replay: Diverged(Outcome) at "))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("compiler", "runtime", "manifest", "work", "output"):
        parser.add_argument(name, type=Path)
    parser.add_argument("--bridge", type=Path)
    parser.add_argument("--crash-runtime", type=Path)
    args = parser.parse_args()
    args.work.mkdir()

    def prefix():
        if args.bridge is None or args.crash_runtime is None:
            raise ValueError("the W2 crash bridge or scripted runtime did not build")
        return w2_control(args)

    checks = {name: lambda name=name: store_control(args, name) for name in STORE_CONTROLS}
    checks["W2 prefix"] = prefix
    checks["SEM-8 replay"] = lambda: replay_control(args)
    return run_controls(args.output, checks)


if __name__ == "__main__":
    raise SystemExit(main())
