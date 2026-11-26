"""Nightly receipts retain both campaign evidence and failures before dispatch."""

import gzip
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from sim_campaign import CANARY


class NightlyReceipt(unittest.TestCase):
    def test_sometimes_counts_hits_per_seed_across_batches(self):
        from vopr_nightly_receipt import summarize

        with tempfile.TemporaryDirectory() as directory:
            run = Path(directory)
            manifest = run / "manifest.json"
            manifest.write_text(json.dumps([
                {"build": "commit", "law": "a", "required_witnesses": ["never", "rare", "repeat"]}
            ]))
            for batch, rows in enumerate((
                [{"repeat": 10, "rare": 0}, None],
                [{"repeat": 2, "extra": 4}, {"rare": 1}],
            )):
                with gzip.open(run / f"{batch}.jsonl.gz", "wt") as stream:
                    for i, witnesses in enumerate(rows):
                        print(json.dumps({"build": "commit", "law": "a", "seed": 2 * batch + i + 1,
                                          "status": 0, "witnesses": witnesses}), file=stream)
            probes = summarize(run, manifest, "commit")["probes"]
            self.assertEqual([(probe["label"], probe["hits"], probe["true_calls"]) for probe in probes],
                             [("extra", 1, 4), ("never", 0, 0), ("rare", 1, 1), ("repeat", 2, 12)])
            self.assertTrue(all(probe["runs"] == 4 and probe["observed_runs"] == 3 for probe in probes))

    def test_receipt_preserves_red_nights_and_executable_revisions(self):
        tool = Path(__file__).with_name("vopr_nightly_receipt.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / "receipts.ndjson"
            run = root / "night"
            run.mkdir()
            (run / "inventory").write_text(f"a\nb\n{CANARY}\n")
            source = root / "revision.c"
            source.write_text(
                'const unsigned char hirsel_code_revision[32] = {1, 2, 3};\n'
                'int main(void) { return 0; }\n'
            )
            binary = root / "law"
            subprocess.run(["cc", "-o", binary, source], check=True)
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps([
                {"build": "commit", "law": law, "executable": str(binary), "recording": str(binary)}
                for law in ("a", CANARY)
            ]))
            (run / "prepare.jsonl").write_text(json.dumps({
                "build": "commit", "law": "b", "kind": "prepare_red", "reason": "refused"
            }) + "\n")
            with gzip.open(run / "batch.jsonl.gz", "wt") as stream:
                for law, seed, status in (("a", 1, 1), (CANARY, 2, 0), (CANARY, 3, 1)):
                    row = {"build": "commit", "law": law, "seed": seed, "status": status,
                           "stdout": "", "stderr": "error: ()\n" if status else "",
                           "tape_replay": {"status": 0}}
                    if law == "a":
                        row.update(tape_divergence="Outcome", witnesses={"fault:lost-write": 1})
                    print(json.dumps(row), file=stream)
            command = [sys.executable, tool, run, manifest, "commit", "sweep", "0"]
            self.assertEqual(subprocess.run(command).returncode, 1)
            self.assertEqual(subprocess.run(command).returncode, 1)
            early = root / "build-failed"
            early.mkdir()
            (early / "inventory").write_text("a\n")
            self.assertEqual(subprocess.run([
                sys.executable, tool, early, early / "absent", "parent", "build", "2"
            ]).returncode, 2)
            receipt, failure = map(json.loads, log.read_text().splitlines())
            revision = (b"\x01\x02\x03" + bytes(29)).hex()
            self.assertEqual(receipt["code_revision"]["a"], {"normal": revision, "recording": revision})
            self.assertEqual(receipt["laws_unprepared"], {"b": "refused"})
            self.assertEqual(receipt["seeds_executed"], 3)
            self.assertEqual(receipt["seeds_failed"], 1)
            self.assertEqual(receipt["replays_diverged"], 1)
            self.assertEqual(receipt["fault_classes_sampled"], ["lost-write"])
            self.assertTrue(receipt["canary_red"])
            self.assertTrue(receipt["not_covered"])
            self.assertEqual((failure["commit"], failure["phase"], failure["status"]),
                             ("parent", "build", 2))
            self.assertEqual(failure["laws_unprepared"], {"a": "no prepared executable"})
            self.assertEqual(failure["code_revision"], {})
            self.assertIsNone(failure["canary_red"])


if __name__ == "__main__":
    unittest.main()
