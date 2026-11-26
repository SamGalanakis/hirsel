"""Check the lifetime of historical build files and Boat receipts."""
import argparse
from decimal import Decimal
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path[:0] = [str(ROOT / "bench/awfy"), str(ROOT / "core/tools")]

import board_bisect


class BoardBisectTest(unittest.TestCase):
    def test_removes_each_build_and_retains_only_valid_boat_receipts(self):
        base, midpoint, tip = ("a" * 40, "b" * 40, "c" * 40)
        rows = [("arena", "Json::ParseMapping")]
        for malformed in (False, True):
            with self.subTest(malformed=malformed), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                scratch = root / "scratch"
                scratch.mkdir()
                args = argparse.Namespace(out=root / "evidence", awfy=root, langarena=root,
                                          limit=Decimal(2))
                images = []
                checkouts = []

                def git(command, action, option, path, commit=None):
                    self.assertEqual(command, "worktree")
                    self.assertTrue(path.is_relative_to(scratch))
                    if action == "add":
                        self.assertTrue(all(not checkout.exists() for checkout in checkouts))
                        if commit == midpoint:
                            self.assertFalse(images[-1].exists())
                        path.mkdir()
                        checkouts.append(path)
                    else:
                        self.assertEqual(action, "remove")
                        shutil.rmtree(path)

                def stage(arms, bundle, selected, *, instructions_only):
                    self.assertEqual(selected, rows)
                    self.assertTrue(instructions_only)
                    self.assertTrue(all(not checkout.exists() for checkout in checkouts))
                    for arm in (arms.base, arms.candidate):
                        self.assertTrue(arm.is_relative_to(scratch))
                        self.assertTrue((arm / "cache" / "image").is_file())
                    (bundle / "candidate").write_text(arms.candidate.parent.name.split("-")[0])
                    return dict(lane="board-screen", bundle=bundle, command="sample")

                def invoke(command, **kwargs):
                    if kwargs.get("cwd"):
                        out = command[5]
                        cache = out / "cache"
                        cache.mkdir(parents=True)
                        (cache / "image").write_text("prepared")
                        cache.chmod(0o500)
                        images.append(out)
                        return
                    fetch = Path(command[command.index("--fetch") + 1].split(":", 1)[1])
                    cache = command[command.index("--bundle-cache") + 1]
                    self.assertTrue(fetch.is_relative_to(scratch))
                    self.assertTrue(cache.is_relative_to(fetch))
                    cache.mkdir()
                    (cache / "bundle.tar.gz").write_text("archive")
                    short = (command[-2] / "candidate").read_text()
                    commit = tip if short == tip[:12] else midpoint
                    count = 1030 if commit == tip else 1000
                    contents = ["workload\tarm\tcommit\tinstructions",
                                f"arena/Json::ParseMapping\tbase\t{base}\t1000"]
                    if commit == tip or not malformed:
                        contents.append(f"arena/Json::ParseMapping\tcandidate\t{commit}\t{count}")
                    (fetch / "instructions.tsv").write_text("\n".join(contents) + "\n")

                with (patch.object(tempfile, "tempdir", str(scratch)),
                      patch.object(board_bisect, "revisions", return_value=[base, midpoint, tip]),
                      patch.object(board_bisect, "git", side_effect=git),
                      patch.object(board_bisect.screen, "screen_job", side_effect=stage),
                      patch.object(board_bisect.subprocess, "run", side_effect=invoke)):
                    run = board_bisect.run(args, base, tip, rows)
                    if malformed:
                        with self.assertRaisesRegex(ValueError, "incomplete Boat screen"):
                            list(run)
                    else:
                        self.assertEqual(list(run), [
                            (True, f"Culprit {tip} arena/Json::ParseMapping "
                                   "(first-parent +2% instruction crossing)"),
                        ])

                retained = sorted(str(path.relative_to(args.out))
                                  for path in args.out.rglob("*") if path.is_file())
                expected = [f"probe-001-{tip[:12]}/instructions.tsv"]
                if not malformed:
                    expected.append(f"probe-002-{midpoint[:12]}/instructions.tsv")
                self.assertEqual(retained, expected)
                self.assertEqual(list(scratch.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
