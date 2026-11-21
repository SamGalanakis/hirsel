from argparse import Namespace
from contextlib import redirect_stdout
import fcntl
import gzip
import io
import json
import os
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from board import EXPECTED, Board, measure, measurement_slot, receipt
import bench_cpus
import build
import cold_phases
from cold_phases import pass_costs


class PassCostAccounting(unittest.TestCase):
    def test_rejects_double_counted_nested_passes(self):
        with tempfile.TemporaryDirectory() as temp:
            trace = Path(temp) / "passes.jsonl.gz"
            rows = [
                {"phase": "release", "pass": "child", "ir_unit": "f", "instructions": 70},
                {"phase": "release", "pass": "parent", "ir_unit": "m", "instructions": 20},
                {"phase": "release", "ir_unit": "m", "pipeline_instructions": 100},
                {"phase": "object-emission", "ir_unit": "part", "pipeline_instructions": 40},
            ]

            def write_trace():
                with gzip.open(trace, "wt") as stream:
                    for row in rows:
                        stream.write(json.dumps(row) + "\n")

            write_trace()
            passes, phases = pass_costs(trace)
            self.assertEqual(sum(row["instructions"] for row in passes.values()), 90)
            self.assertEqual(phases["release"]["unattributed_instructions"], 10)
            self.assertEqual(phases["object-emission"]["unattributed_instructions"], 40)
            rows[1]["instructions"] = 90
            write_trace()
            with self.assertRaisesRegex(ValueError, "exceed their pipeline counters"):
                pass_costs(trace)


class WorkloadMatching(unittest.TestCase):
    def test_build_cli_selects_pool_index_images(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            out = root / "out"
            clang = root / "bin/clang"
            clang.parent.mkdir()
            clang.write_bytes(b"toolchain fixture")
            toolchain = root / "toolchain.json"
            toolchain.write_text(json.dumps({"compiler": str(clang), "runtime": str(clang)}))
            rap = root / "benchmarks/JavaScript/json.js"
            rap.parent.mkdir(parents=True)
            rap.write_text('const RAP_BENCHMARK_MINIFIED = "{}";\n')

            def compile_image(command, **kwargs):
                Path(command[command.index("-o") + 1]).write_bytes(b"image")
                return subprocess.CompletedProcess(command, 0, stdout=b"{}")

            argv = ["build.py", str(out), "--upstream", str(root),
                    "--artifacts", str(toolchain), "--hirsel-entries", "pool_index",
                    "--goose", "goose", "--goose-entries", "pool_index"]
            with (patch.object(sys, "argv", argv), patch.object(build, "llvm_path", return_value=root),
                  patch.object(build.subprocess, "run", side_effect=compile_image)):
                build.main()
            build_report = json.loads((out / "hirsel-pool_index-build.json").read_text())
            self.assertIn("awfy@1/pool_index/main", build_report["measurement"]["flags"])
            sources = json.loads((out / "hirsel-inputs.json").read_text())["packages"][-1]["sources"]
            self.assertIn(["pool_index"], [source["modules"] for source in sources])
            self.assertIn("havlak_pool.benchmark(inner)", (out / "pool_index.hirsel").read_text())
            self.assertIn("import havlak_pool;", (out / "goose-src/pool_index.goose").read_text())
            self.assertEqual((out / "goose-pool_index").read_bytes(), b"image")

    def test_document_pools_do_not_use_original_rust_rows(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "rebench.conf").write_text(
                "            - Json:\n                extra_args: 100\n"
                "            - Storage:\n                extra_args: 25\n")
            originals = dict(measure.rows(root, False, root))
            pools = dict(measure.matched_pool_rows(root, root))
        for name, inner, checksum in (("Json", "100", 156), ("Storage", "25", 5461)):
            pool = name + "Pool"
            self.assertEqual(originals[name]["rust"][0][1:], [name, "1", inner])
            command, marker = pools[pool]["rust"]
            self.assertNotEqual(command[0], originals[name]["rust"][0][0])
            self.assertEqual(command[1:], [pool, "1", inner])
            self.assertIsNone(re.fullmatch(marker, f"{name}: iterations=1 runtime: 1us\n".encode()))
            for side in ("hirsel", "goose"):
                command, marker = pools[pool][side]
                self.assertEqual(command[1:], [pool, "1", inner])
                self.assertIsNotNone(re.fullmatch(marker, f"verified checksum={checksum}\n".encode()))

class BoardCompletion(unittest.TestCase):
    def test_receipt_requires_every_row_and_side(self):
        with tempfile.TemporaryDirectory() as temp, redirect_stdout(io.StringIO()):
            out = Path(temp)
            for omit_edit, sample_status, exit_code in (
                (True, "ok", 1),
                (False, "missing-counter", 1),
                (False, "ok", 0),
            ):
                with self.subTest(omit_edit=omit_edit, sample_status=sample_status):
                    board = Board(out, "abcd1234")
                    board.report["size_measurement"] = {
                        "strip_version": "GNU strip 2.42", "strip_flags": ["--strip-all"],
                        "verified_workloads": sorted(measure.EXPECTED["AWFY original"]),
                        "pins": board.report["pins"],
                    }
                    suites = {goal: suite for suite, goal in measure.SUITES.items()}
                    for goal, names in EXPECTED.items():
                        for name in names:
                            if name == "LangArena edit" and omit_edit:
                                continue
                            sides = ("hirsel", "rust") if goal in ("Compile", "Checkbox") else measure.SIDES
                            fields, values = self.sample(goal, name)
                            samples = {side: {"status": "ok", **values} for side in sides}
                            if name == "LangArena edit":
                                samples["rust"]["status"] = sample_status
                            if goal in suites:
                                suite = suites[goal]
                                mode = name.split()[-1] if suite == "compile" else "release"
                                board.report["instruction_receipt"]["rows"].append({
                                    "key": f"{suite}/{name}", "suite": suite, "mode": mode,
                                    "flags": ["--release"], "argv": ["./sample"],
                                    "affinity": [10], "compiler_sha256": "a" * 64,
                                    "runtime_archive_sha256": "b" * 64, "fixture_sha256": "c" * 64,
                                    "std_sha256": "d" * 64, "workload_sources_sha256": "e" * 64,
                                    "fixture_files": {"fixture.json": "c" * 64},
                                    "marker": "verified\n", "stdout": "verified\n",
                                    "sample": samples["hirsel"],
                                })
                            board.row(goal, name, samples, fields)
                    self.assertEqual(board.finish(), exit_code)
                    report = json.loads((out / "board.json").read_text())
                    self.assertEqual(report["complete"], exit_code == 0)
                    edit = next(row for row in report["expected_rows"]
                                if (row["goal"], row["row"]) == ("Compile", "LangArena edit"))
                    status = "missing" if omit_edit else "failed" if exit_code else "ok"
                    self.assertEqual(edit["status"], status)

    @staticmethod
    def sample(goal, name):
        if goal == "Size":
            return ("bytes",), {"bytes": 2048, "image_sha256": "d" * 64, "binary_sha256": "e" * 64}
        if goal == "Checkbox":
            if name.startswith("converge"):
                return ("toggles_per_second",), {"toggles_per_second": 450}
            return ("server_rss_bytes",), {"server_rss_bytes": 4096, "connections": 64}
        return ("instructions", "cycles"), {
            "instructions": 123, "cycles": 99, "verified": True,
            "pmu_times_ns": {event: [[100, 100]] for event in ("instructions:u", "cycles:u")},
        }

    def test_second_descriptor_cannot_take_measurement_lock(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "board.lock"
            with path.open("a") as other:
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(other, fcntl.LOCK_UN)
                with measurement_slot(path):
                    with self.assertRaises(BlockingIOError):
                        fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)


class SiblingActivity(unittest.TestCase):
    def test_cold_phases_reuses_the_lane_cpu_set(self):
        original = os.sched_getaffinity(0)
        cpus = sorted(original)[:2]
        self.addCleanup(os.sched_setaffinity, 0, original)

        def collect(args, scratch):
            self.assertEqual(os.sched_getaffinity(0), set(cpus))
            self.assertEqual(args.measure_cpus, (cpus[0],))
            return 0

        with tempfile.TemporaryDirectory() as temp:
            argv = ["cold_phases.py", str(Path(temp) / "out"), "--artifacts", "toolchain.json",
                    "--fixture", "fixture.json", "--cpus", ",".join(map(str, cpus))]
            with patch.object(sys, "argv", argv), patch.object(cold_phases, "collect", side_effect=collect):
                self.assertEqual(cold_phases.main(), 0)

    def test_records_busy_sibling_from_linux_ticks(self):
        # CPU 18 sees four active ticks out of twenty. Linux already
        # includes guest time in user time. CPU 19 only sees I/O wait.
        with tempfile.TemporaryDirectory() as temp:
            stat = Path(temp) / "stat"
            stat.write_text("cpu18 100 0 50 300 80 0 0 0 20 0\ncpu19 100 0 50 300 80 0 0 0 20 0\n")
            with patch.object(bench_cpus, "Path", return_value=stat):
                with bench_cpus.smt_activity([18, 19]) as busy:
                    stat.write_text("cpu18 104 0 50 316 80 0 0 0 24 0\ncpu19 100 0 50 300 100 0 0 0 20 0\n")
        self.assertEqual(busy, {18: 20.0, 19: 0.0})

    def test_refuses_threads_sharing_a_core_in_one_run(self):
        with tempfile.TemporaryDirectory() as temp:
            topology = Path(temp) / "thread_siblings_list"
            topology.write_text("2,18\n")
            with patch.object(bench_cpus, "Path", return_value=topology):
                self.assertEqual(bench_cpus.sibling_cpus([2], [1]), [18])
                with self.assertRaisesRegex(ValueError, "build set"):
                    bench_cpus.sibling_cpus([2], [18])
                with self.assertRaisesRegex(ValueError, "measurement set"):
                    bench_cpus.sibling_cpus([2, 18], [1])


class PairedSampling(unittest.TestCase):
    def test_cycle_noise_does_not_override_instruction_regression(self):
        key = "awfy/List"
        base = {
            "key": key, "suite": "awfy", "mode": "release",
            "flags": ["--release"], "argv": ["./sample", "List"], "marker": "verified\n",
            "compiler_sha256": "a" * 64, "runtime_archive_sha256": "b" * 64,
            "fixture_sha256": "c" * 64, "image_sha256": "d" * 64, "std_sha256": "e" * 64,
            "fixture_files": {"rebench.conf": "c" * 64}, "workload_sources_sha256": "f" * 64,
        }
        candidate = base | {"image_sha256": "e" * 64}
        cpu = min(os.sched_getaffinity(0))
        events = ("instructions:u", "cycles:u")
        counts = iter((100, 96, 101, 100, 100, 104) + (100, 103, 103, 100, 100, 103) * 2)
        activity = iter((5, 5, 10, 10, 25, 25, 10, 30, 49, 29, 25, 25, 10, 30, 50, 29, 25, 25))
        images = []

        def observe(row, directory, env, siblings, *, cycles):
            images.append(row["image_sha256"])
            row["affinity"] = [cpu]
            row["stdout"] = "verified\n"
            row["sample"] = {
                "status": "ok", "verified": True, "cycles": next(counts),
                "smt_busy_percent": {18: next(activity)},
                "instructions": 100 if row["image_sha256"] == base["image_sha256"] else 101,
                "pmu_times_ns": {event: [[100, 100]] for event in events},
            }

        with (tempfile.TemporaryDirectory() as temp, redirect_stdout(io.StringIO()),
              patch.object(receipt, "runtime_rows", return_value=[(key, ())]),
              patch.object(receipt, "prepare_pair_rows", side_effect=[{key: base}] + [{key: base}, {key: candidate}] * 2),
              patch.object(receipt, "sibling_cpus", return_value=[18]),
              patch.object(receipt.os, "sched_setaffinity"), patch.object(receipt, "observe", side_effect=observe)):
            root = Path(temp)
            args = Namespace(upstream=None, candidate_awfy_build=None, candidate_pool_build=None,
                             candidate_arena_build=None, candidate_artifacts=None, aa=None, artifacts=None,
                             rows="List", pairs=3, cpu=cpu, lock=root / "lock", threshold=None, out=root / "aa")
            self.assertEqual(receipt.pair_samples(args), 0)
            args.aa = args.out / "pairs.json"
            args.candidate_artifacts = root / "candidate-toolchain.json"
            for verdict in ("within noise", "sibling busy"):
                args.out = root / verdict.replace(" ", "-")
                self.assertEqual(receipt.pair_samples(args), 1)
                result = receipt.read(args.out / "pairs.json")["summary"][key]
                self.assertEqual(result["cycles"]["median_percent"], "3")
                self.assertEqual(result["cycles"]["verdict"], verdict)
        a, b = base["image_sha256"], candidate["image_sha256"]
        self.assertEqual(images[6:], [a, b, b, a, a, b] * 2)
