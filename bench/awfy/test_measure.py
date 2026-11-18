from contextlib import redirect_stdout
import fcntl
import io
import json
import re
import tempfile
import unittest
from pathlib import Path

from unittest.mock import patch

from board import Board, measure, measurement_slot


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
            expected = {"Compile": ("LangArena cold", "LangArena edit")}
            with patch("board.EXPECTED", expected):
                for omit_edit, sample_status, exit_code in (
                    (True, "ok", 1),
                    (False, "missing-counter", 1),
                    (False, "ok", 0),
                ):
                    with self.subTest(omit_edit=omit_edit, sample_status=sample_status):
                        board = Board(out, "abcd1234")
                        samples = dict.fromkeys(("hirsel", "rust"), {"status": "ok", "instructions": 123})
                        board.row("Compile", "LangArena cold", True, samples, ("instructions",))
                        if not omit_edit:
                            samples = samples | {"rust": {"status": sample_status, "instructions": 123}}
                            board.row("Compile", "LangArena edit", True, samples, ("instructions",))
                        self.assertEqual(board.finish(), exit_code)
                        receipt = json.loads((out / "board.json").read_text())
                        self.assertEqual(receipt["complete"], exit_code == 0)
                        status = "missing" if omit_edit else "failed" if exit_code else "ok"
                        self.assertEqual(receipt["expected_rows"][1], {
                            "goal": "Compile", "row": "LangArena edit", "status": status,
                        })

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
