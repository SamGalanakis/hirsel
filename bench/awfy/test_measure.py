from argparse import Namespace
from contextlib import redirect_stdout
import fcntl
import io
import json
import os
import re
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

from board import Board, measure, measurement_slot, receipt
import bench_cpus


class WorkloadMatching(unittest.TestCase):
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
                    for goal, names in measure.EXPECTED.items():
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
                                mode = name.split()[-1] if suite == "compile" else "certified"
                                board.report["instruction_receipt"]["rows"].append({
                                    "key": f"{suite}/{name}", "suite": suite, "mode": mode,
                                    "certified": True, "flags": ["--release"], "argv": ["./sample"],
                                    "affinity": [10], "compiler_sha256": "a" * 64,
                                    "runtime_archive_sha256": "b" * 64, "fixture_sha256": "c" * 64,
                                    "marker": "verified\n", "stdout": "verified\n",
                                    "sample": samples["hirsel"],
                                })
                            board.row(goal, name, True, samples, fields)
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
    def test_refuses_busy_sibling_from_linux_ticks(self):
        # CPU 18 sees four active ticks out of twenty. Linux already
        # includes guest time in user time. CPU 19 only sees I/O wait.
        before = "cpu18 100 0 50 300 80 0 0 0 20 0\ncpu19 100 0 50 300 80 0 0 0 20 0\n"
        with tempfile.TemporaryDirectory() as temp:
            stat = Path(temp) / "stat"
            stat.write_text(before)

            def advance_ticks(_interval):
                stat.write_text("cpu18 104 0 50 316 80 0 0 0 24 0\ncpu19 100 0 50 300 100 0 0 0 20 0\n")

            with patch.object(bench_cpus, "Path", return_value=stat), patch.object(
                bench_cpus.time, "sleep", side_effect=advance_ticks
            ):
                with self.assertRaisesRegex(ValueError, r"CPU 18 20\.00% busy$"):
                    bench_cpus.require_idle([18, 19])
                stat.write_text(before)
                bench_cpus.require_idle([19])


class PairedSampling(unittest.TestCase):
    def test_cycle_noise_does_not_override_instruction_regression(self):
        key = "awfy/List"
        base = {
            "key": key, "suite": "awfy", "mode": "certified", "certified": True,
            "flags": ["--release"], "argv": ["./sample", "List"], "marker": "verified\n",
            "compiler_sha256": "a" * 64, "runtime_archive_sha256": "b" * 64,
            "fixture_sha256": "c" * 64, "image_sha256": "d" * 64,
        }
        candidate = base | {"image_sha256": "e" * 64}
        cpu = min(os.sched_getaffinity(0))
        events = ("instructions:u", "cycles:u")
        counts = iter((100, 96, 101, 100, 100, 104, 100, 103, 103, 100, 100, 103))
        images = []

        def observe(row, directory, env, *, cycles):
            images.append(row["image_sha256"])
            row["affinity"] = [cpu]
            row["stdout"] = "verified\n"
            row["sample"] = {
                "status": "ok", "verified": True, "cycles": next(counts),
                "instructions": 100 if row["image_sha256"] == base["image_sha256"] else 101,
                "pmu_times_ns": {event: [[100, 100]] for event in events},
            }

        with (tempfile.TemporaryDirectory() as temp, redirect_stdout(io.StringIO()),
              patch.object(receipt, "runtime_rows", return_value=[(key, ())]),
              patch.object(receipt, "prepare_pair_rows", side_effect=[{key: base}, {key: base}, {key: candidate}]),
              patch.object(receipt, "require_idle"), patch.object(receipt, "sibling_cpus", return_value=[]),
              patch.object(receipt.os, "sched_setaffinity"), patch.object(receipt, "observe", side_effect=observe)):
            root = Path(temp)
            args = Namespace(upstream=None, candidate_awfy_build=None, candidate_pool_build=None,
                             candidate_arena_build=None, candidate_artifacts=None, aa=None, artifacts=None,
                             rows="List", pairs=3, cpu=cpu, lock=root / "lock", threshold=None, out=root / "aa")
            self.assertEqual(receipt.pair_samples(args), 0)
            args.aa = args.out / "pairs.json"
            args.out = root / "ab"
            args.candidate_artifacts = root / "candidate-toolchain.json"
            self.assertEqual(receipt.pair_samples(args), 1)
            result = receipt.read(args.out / "pairs.json")["summary"][key]
        self.assertEqual(result["cycles"]["median_percent"], "3")
        self.assertIn("within noise", result["cycles"]["verdict"])
        a, b = base["image_sha256"], candidate["image_sha256"]
        self.assertEqual(images[-6:], [a, b, b, a, a, b])
