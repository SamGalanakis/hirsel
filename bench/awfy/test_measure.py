import re
import tempfile
import unittest
from pathlib import Path

from board import measure


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
