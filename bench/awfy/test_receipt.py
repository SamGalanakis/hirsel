from argparse import Namespace
from contextlib import redirect_stdout
from copy import deepcopy
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import arena
import build
import bench_cpus
import receipt
from receipt import build_record, fixture_identity, make_receipt, runtime_case


class InstructionGuard(unittest.TestCase):
    def test_mandelbrot_injection_and_incomplete_pairs(self):
        # profilesmash-1288 used a checked LangArena row with a 710,000-iteration
        # loop before exec. The counter values below are the measured replay.
        baseline = make_receipt(["arena/CLBG::Mandelbrot"])
        stdout = "CLBG::Mandelbrot checksum=683987290 expected=683987290 status=OK\n"
        baseline["rows"] = [{
            "key": "arena/CLBG::Mandelbrot",
            "suite": "arena",
            "mode": "certified",
            "certified": True,
            "flags": ["--backend", "llvm", "--release"],
            "argv": ["./clbg-64", "one-iteration.json", "CLBG::Mandelbrot"],
            "affinity": [10],
            "compiler_sha256": "a" * 64,
            "runtime_archive_sha256": "b" * 64,
            "image_sha256": "f" * 64,
            "marker": stdout,
            "stdout": stdout,
            "sample": {"status": "ok", "verified": True, "instructions": 69_882_571,
                       "pmu_times_ns": {"instructions:u": [[1_000_000, 1_000_000]]}},
        }]
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            base, candidate = (directory / name for name in ("base.json", "candidate.json"))
            manifest, fixture = (directory / name for name in ("inputs.json", "one-iteration.json"))
            packages = []
            for package, module in (("core", "list"), ("std", "math"), ("langarena", "clbg")):
                source = directory / f"{module}.hirsel"
                source.write_text(f"// {module}\n")
                packages.append({
                    "package": {"name": package, "version": "1"},
                    "sources": [{"file": str(source), "modules": [module], "signatures": package != "langarena"}],
                    "dependencies": [],
                })
            manifest.write_text(json.dumps({"core": packages[0]["package"], "packages": packages}))
            fixture.write_text('{"iterations": 1}')
            baseline["rows"][0].update(fixture_identity(manifest, fixture))
            base.write_text(json.dumps(baseline))

            def compare(value, *options):
                candidate.write_text(json.dumps(value))
                return subprocess.run([sys.executable, Path(__file__).with_name("receipt.py"),
                                       "compare", base, candidate, *options], capture_output=True, text=True)

            result = compare(baseline)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("nothing differs: A/A", result.stderr)
            for field in ("compiler_sha256", "runtime_archive_sha256", "std_sha256"):
                self.assertIn(f"{field}={baseline['rows'][0][field]}", result.stderr)
            self.assertEqual(compare(baseline, "--aa").returncode, 0)
            injected = deepcopy(baseline)
            injected["rows"][0]["sample"]["instructions"] = 71_302_841
            result = compare(injected, "--aa")
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("CLBG::Mandelbrot 69,882,571 -> 71,302,841 (+2.032%) FAIL", result.stdout)

            for field, value in (("affinity", [11]), ("argv", ["./clbg-64", "one-iteration.json", "other"])):
                with self.subTest(field=field):
                    changed = deepcopy(baseline)
                    changed["rows"][0][field] = value
                    result = compare(changed)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn(f"identity differs: arena/CLBG::Mandelbrot: {field}", result.stderr)

            def record_build(receipt):
                row, = receipt["rows"]
                row["build_record"] = build_record({
                    "accepted": True, "native": {"linked": True, "step_charging_elided": True},
                    "measurement": {key: row[key] for key in ("compiler_sha256", "runtime_archive_sha256",
                                                              *build.MANIFEST_HASHES, "image_sha256", "flags")},
                })

            paired = deepcopy(injected)
            paired["rows"][0]["compiler_sha256"] = "d" * 64
            record_build(paired)
            result = compare(paired)
            self.assertEqual(result.returncode, 2)
            self.assertIn("arena/CLBG::Mandelbrot requires a verified build record", result.stderr)

            for running in (500_000, 999_999):
                with self.subTest(running=running):
                    inactive = deepcopy(baseline)
                    inactive["rows"][0]["sample"]["pmu_times_ns"]["instructions:u"][0][1] = running
                    result = compare(inactive)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn("inactive-counter", result.stderr)

            record_build(baseline)
            base.write_text(json.dumps(baseline))
            result = compare(paired)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn(f"Variable compiler_sha256 {'a' * 64} -> {'d' * 64}", result.stdout)
            self.assertIn("(+2.032%) FAIL", result.stdout)

            paired["rows"][0]["runtime_archive_sha256"] = "e" * 64
            result = compare(paired)
            self.assertEqual(result.returncode, 2)
            self.assertIn("arena/CLBG::Mandelbrot requires a verified build record", result.stderr)

            record_build(paired)
            result = compare(paired)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn(f"Paired runtime_archive_sha256 {'b' * 64} -> {'e' * 64}", result.stdout)

            runtime_only = deepcopy(injected)
            runtime_only["rows"][0]["runtime_archive_sha256"] = "e" * 64
            result = compare(runtime_only)
            self.assertEqual(result.returncode, 2)
            self.assertIn("arena/CLBG::Mandelbrot requires a verified build record", result.stderr)
            record_build(runtime_only)
            result = compare(runtime_only)
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn(f"Paired runtime_archive_sha256 {'b' * 64} -> {'e' * 64}", result.stdout)

            changed = deepcopy(baseline)
            for module in ("list", "math"):
                with self.subTest(std_source=module):
                    (directory / f"{module}.hirsel").write_text(f"// revised {module}\n")
                    changed["rows"][0].update(fixture_identity(manifest, fixture))
                    result = compare(changed)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn("requires a verified build record", result.stderr)
                    record_build(changed)
                    result = compare(changed)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIn("Variable std_sha256 ", result.stdout)
                    self.assertIn("(+0.000%)", result.stdout)
                    result = compare(changed, "--aa")
                    self.assertEqual(result.returncode, 2, result.stderr)
                    self.assertIn("--aa requires identical candidate hashes", result.stderr)
                    self.assertIn("std_sha256", result.stderr)

            for source, content in ((directory / "clbg.hirsel", "// revised benchmark\n"),
                                    (fixture, '{"iterations": 2}')):
                with self.subTest(workload_file=source.name):
                    original = source.read_text()
                    source.write_text(content)
                    changed["rows"][0].update(fixture_identity(manifest, fixture))
                    record_build(changed)
                    result = compare(changed)
                    self.assertEqual(result.returncode, 2)
                    self.assertIn("identity differs: arena/CLBG::Mandelbrot: fixture_sha256", result.stderr)
                    source.write_text(original)

            paired["rows"][0]["flags"] += ["--debug-lines", "on"]
            record_build(paired)
            result = compare(paired)
            self.assertEqual(result.returncode, 2)
            self.assertIn("identity differs: arena/CLBG::Mandelbrot: flags", result.stderr)

            missing = deepcopy(baseline)
            missing["rows"].clear()
            result = compare(missing)
            self.assertEqual(result.returncode, 2)
            self.assertIn("missing: ['arena/CLBG::Mandelbrot']", result.stderr)

            second = deepcopy(baseline["rows"][0])
            second["key"] = "arena/CLBG::Spectralnorm"
            baseline["expected_rows"].append(second["key"])
            baseline["rows"].append(second)
            base.write_text(json.dumps(baseline))
            mixed = deepcopy(baseline)
            mixed["rows"][0]["affinity"] = [11]
            mixed["rows"][1]["sample"]["instructions"] *= 2
            result = compare(mixed)
            self.assertEqual(result.returncode, 2)
            self.assertIn("arena/CLBG::Mandelbrot REFUSED", result.stderr)
            self.assertIn("arena/CLBG::Spectralnorm 69,882,571 -> 139,765,142 (+100.000%) FAIL", result.stdout)


class QuietCyclePairs(unittest.TestCase):
    def test_a_busy_sibling_in_either_run_discards_the_pair(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            topology = root / "thread_siblings_list"
            topology.write_text("7,23\n")
            stat = root / "stat"
            # Each run advances 20 ticks. CPU 7 is always busy. CPU 23 is
            # busy in run two of the first pair and run one of the second.
            windows = iter((0, 1, 2, 0, 0, 0))
            busy, idle = 10, 100

            def snapshot():
                stat.write_text(f"cpu7 {busy + idle} 0 0 0 0 0 0 0\n"
                                f"cpu23 {busy} 0 0 {idle} 0 0 0 0\n")

            def run(command):
                nonlocal busy, idle
                active = next(windows)
                busy += active
                idle += 20 - active
                snapshot()
                return subprocess.CompletedProcess(command, 0)

            def sample(command, directory, *, env, cwd, cycles, pmu_times, smt_siblings):
                directory.mkdir()
                activity = directory / "smt.json"
                bench_cpus.run_with_smt(smt_siblings, activity, command)
                (directory / "stdout").write_text("verified\n")
                return {"status": "ok", "cycles": 100, "instructions": 100,
                        "smt_busy_percent": {int(cpu): value for cpu, value in receipt.read(activity).items()},
                        "pmu_times_ns": {event: [[100, 100]] for event in ("cycles", "instructions")}}

            snapshot()
            row = {"argv": ["./sample"], "cwd": str(root), "marker": "verified\n"}
            args = Namespace(max_attempts=3, pairs=1, sibling_busy_threshold=5)

            def linux_file(path):
                if path == "/proc/stat":
                    return stat
                self.assertEqual(path, "/sys/devices/system/cpu/cpu7/topology/thread_siblings_list")
                return topology

            with (patch.object(bench_cpus, "Path", side_effect=linux_file),
                  patch.object(bench_cpus.subprocess, "run", side_effect=run),
                  patch.object(receipt.measure.core_measure, "sample", side_effect=sample)):
                siblings = bench_cpus.sibling_cpus([7])
                kept, attempts = receipt.cycle_samples(row, row, args, siblings, {})
                args.sibling_busy_threshold = None
                windows = iter((20, 20))
                unfiltered, unfiltered_attempts = receipt.cycle_samples(row, row, args, siblings, {})
            self.assertEqual((len(kept), attempts), (1, 3))
            self.assertEqual([run["smt_busy_percent"] for run in kept[0]], [{23: 0.0}, {23: 0.0}])
            self.assertEqual((len(unfiltered), unfiltered_attempts), (1, 1))
            self.assertEqual([run["smt_busy_percent"] for run in unfiltered[0]], [{23: 100.0}, {23: 100.0}])

    def test_ride_uses_the_median_of_the_paired_cycle_ratios(self):
        for center, verdict, expected in ((1009, "RIDES", 0), (1011, "FAILS", 1)):
            with self.subTest(verdict=verdict), redirect_stdout(io.StringIO()) as output:
                samples = [
                    ({"cycles": 1000 * scale, "instructions": 100, "smt_busy_percent": {}},
                     {"cycles": count * scale, "instructions": 100, "smt_busy_percent": {}})
                    for scale, count in ((100, 1130), (1, center), (1, 910))
                ]
                self.assertEqual(receipt.report_cycle_pairs("awfy/List", samples, 3, 3), expected)
                self.assertIn(f"awfy/List {verdict}\n", output.getvalue())

    def test_peer_pairs_use_the_same_workload_and_peer_checksum(self):
        args = Namespace(peer=["rust", "goose"], upstream=None, pairs=1,
                         max_attempts=1, sibling_busy_threshold=None)
        with tempfile.TemporaryDirectory() as temp:
            cwd = Path(temp)
            name = "Sort::Merge"
            config = cwd / "run.js"
            config.write_text(json.dumps([{"name": name, "iterations": 100, "checksum": 793}]))
            native = {"suite": "arena", "cwd": str(cwd), "argv": ["./sort-64", config.name, name],
                      "fixture_files": {str(config): build.sha256(config)},
                      "marker": r"Sort::Merge checksum=793 expected=793 status=OK\n"}

            def pair(peer, candidate, *_):
                self.assertIn(Path(peer["argv"][0]).name, args.peer)
                self.assertEqual(peer["argv"][1:], [str(config), name])
                self.assertRegex("Sort::Merge: OK in 1.234s\n", peer["marker"])
                self.assertTrue(peer["line_marker"])
                self.assertEqual(candidate, native)
                base = {"cycles": 100, "instructions": 150, "smt_busy_percent": {}}
                return [(base, base | {"cycles": 200})], 1

            with (patch.object(receipt, "cycle_samples", side_effect=pair) as sample,
                  redirect_stdout(io.StringIO()) as output):
                self.assertEqual(receipt.peer_cycle_pairs(f"arena/{name}", native, args, (), {}), 0)
                self.assertEqual(sample.call_count, 2)
                self.assertIn("Hirsel/rust: 1/1 unfiltered pairs, cycles 2.000000x", output.getvalue())
                self.assertIn("Hirsel/goose: 1/1 unfiltered pairs, cycles 2.000000x", output.getvalue())
                with self.assertRaisesRegex(ValueError, "do not match"):
                    receipt.peer_cycle_pairs(f"arena/{name}", native | {"marker": "other\n"}, args, (), {})

    def test_peer_observation_requires_the_checksum_line(self):
        row = {"argv": ["peer"], "cwd": ".", "marker": "Sort::Merge: OK in [0-9.]+s\n",
               "line_marker": True}
        with tempfile.TemporaryDirectory() as temp:
            for status in ("OK", "ERR"):
                with self.subTest(status=status):
                    directory = Path(temp) / status

                    def run(_, target, **_kwargs):
                        target.mkdir()
                        (target / "stdout").write_text(f"start: 123\nSort::Merge: {status} in 0.1s\nSummary: 0.1s\n")
                        return {"status": "ok", "pmu_times_ns": {"instructions:u": [[1, 1]]}}

                    with patch.object(receipt.measure.core_measure, "sample", side_effect=run):
                        observed = receipt.observe(row.copy(), directory, {}, ())
                    self.assertEqual(observed["verified"], status == "OK")
                    self.assertEqual(observed["status"], "ok" if status == "OK" else "wrong-result")


class ProfileSelfCosts(unittest.TestCase):
    def test_excludes_call_edges_and_keeps_inlined_line(self):
        from profile_diff import self_counts

        capture = """positions: instr line
events: Ir
ob=/release/awfy
fl=/src/mandelbrot.hirsel
fn=mandelbrot
0x100 31 8
cob=/release/awfy
cfi=/src/helper.hirsel
cfn=helper
calls=8 0x200 9
0x103 31 32
fi=/src/helper.hirsel
0x108 9 8
fe=/src/mandelbrot.hirsel
0x10b 32 8
fl=/src/helper.hirsel
fn=helper
0x200 9 32
totals: 56
"""
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "callgrind.out"
            path.write_text(capture)
            self.assertEqual(self_counts(path), {
                ("/release/awfy", "mandelbrot", 0x100, "/src/mandelbrot.hirsel", 31): 8,
                ("/release/awfy", "mandelbrot", 0x108, "/src/helper.hirsel", 9): 8,
                ("/release/awfy", "mandelbrot", 0x10b, "/src/mandelbrot.hirsel", 32): 8,
                ("/release/awfy", "helper", 0x200, "/src/helper.hirsel", 9): 32,
            })
            path.write_text(capture.replace("totals: 56", "totals: 88"))
            with self.assertRaises(ValueError):
                self_counts(path)


class NativeProvenance(unittest.TestCase):
    def test_arena_fixture_hashes_the_built_group_and_its_shared_sources(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            source = directory / "core/langarena"
            source.mkdir(parents=True)
            for module in ("sort", "clbg", "harness", "helper"):
                (source / f"{module}.hirsel").write_text(f"// {module}\n")
            (directory / "one-iteration.json").write_text("fixture")
            fixture = receipt.runtime_fixture("arena", directory, None)

            def identity(module):
                manifest = directory / f"{module}-64-inputs.json"
                with patch.object(arena, "ROOT", directory):
                    manifest.write_text(json.dumps(arena.group_inputs(module)))
                return receipt.source_fixture(manifest, fixture["fixture_files"])["fixture_sha256"]

            sort, clbg = identity("sort"), identity("clbg")
            (source / "clbg.hirsel").write_text("// revised CLBG\n")
            self.assertEqual(identity("sort"), sort)
            self.assertNotEqual(identity("clbg"), clbg)
            (source / "helper.hirsel").write_text("// revised shared helper\n")
            self.assertNotEqual(identity("sort"), sort)

    def test_runtime_receipt_refuses_a_replaced_binary(self):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp)
            compiler = directory / "compiler"
            runtime = directory / "runtime"
            binary = directory / "hirsel-harness"
            manifest = directory / "hirsel-inputs.json"
            compiler.write_bytes(b"compiler")
            runtime.write_bytes(b"runtime")
            binary.write_bytes(b"linked image")
            manifest.write_text('{"packages": []}')
            fixture = directory / "rebench.conf"
            fixture.write_text("fixture")
            toolchain = {"compiler_sha256": build.sha256(compiler), "runtime_archive_sha256": build.sha256(runtime)}
            command = [compiler, "build", "--release", "--runtime", runtime, "--inputs", manifest, "-o", binary]
            provenance = build.native_provenance(command, manifest)
            build.write_native_report(binary, json.dumps({
                "accepted": True, "native": {"linked": True, "executable": str(binary)},
            }), provenance)

            def row():
                return runtime_case("awfy", "Mandelbrot", directory, [binary, "Mandelbrot", "1", "500"],
                                    b"verified\n", fixture_identity(manifest, fixture), toolchain)

            self.assertEqual(row()["flags"], ["--release", "--runtime", "runtime", "--inputs", "manifest", "-o", "image"])
            binary.write_bytes(b"replacement image")
            with self.assertRaisesRegex(ValueError, "image_sha256 differs from the build"):
                row()
