from contextlib import redirect_stdout
import fcntl
import io
import json
import re
import tempfile
import unittest
from pathlib import Path

from board import Board, measure, measurement_slot, receipt


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
