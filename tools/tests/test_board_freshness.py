"""Exercise the nightly wrapper and the missed-run check against a fixture board."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

TOOLS = Path(__file__).resolve().parents[1]


class BoardFreshnessTest(unittest.TestCase):
    def test_failed_fetch_preserves_last_complete_and_alerts_when_stale(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            main = base / "main"
            main.mkdir()

            def git(*args):
                return subprocess.check_output(["git", "-C", main, *args], text=True).strip()

            git("init", "-q")
            for message in ("Measured commit", "New main commit"):
                git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.com",
                    "commit", "--allow-empty", "-qm", message)
            commit = git("rev-parse", "HEAD^")
            head = git("rev-parse", "HEAD")
            git("update-ref", "refs/remotes/origin/main", head)
            current = base / "CURRENT-BOARD.md"

            def alert():
                return subprocess.check_output(
                    [sys.executable, TOOLS / "board-freshness.py", "--base", base,
                     "--current", current, "--alert"], text=True,
                )

            self.assertIn("BOARD-STALE age unavailable", alert())
            complete = base / "complete"
            complete.mkdir()
            board = complete / "board.json"
            board.write_text(json.dumps({"commit": commit, "complete": True, "exit_status": 0}))
            (complete / "board.md").write_text("Complete results\n")
            (base / "last-complete.json").symlink_to(board)
            incomplete = base / "runs/20000101T000000Z-old/board"
            incomplete.mkdir(parents=True)
            (incomplete / "board.json").write_text(
                json.dumps({"commit": head, "complete": True, "exit_status": 2}),
            )
            (incomplete / "board.md").write_text("Refused results\n")

            recent = time.time() - 25 * 3600
            os.utime(board, (recent, recent))
            self.assertEqual(alert(), "")
            old = time.time() - 27 * 3600
            os.utime(board, (old, old))
            self.assertIn("BOARD-STALE 27.0 h old", alert())

            peer = base / "peers/langarena/rust"
            peer.mkdir(parents=True)
            (peer / "Cargo.toml").touch()
            taskset = base / "taskset"
            taskset.write_text(
                '#!/bin/bash\nshift 2\n'
                'case "$1" in\n'
                '  git) exit 0 ;;\n'
                '  cargo) echo "fixture: cargo fetch failed" >&2; exit 2 ;;\n'
                '  *) exit 90 ;;\n'
                'esac\n',
            )
            taskset.chmod(0o755)
            result = subprocess.run(
                [TOOLS / "nightly-board.sh"], capture_output=True, text=True,
                env=os.environ | {"HIRSEL_BOARD_BASE": str(base), "HIRSEL_BOARD_CURRENT": str(current),
                                  "PATH": f"{base}:{os.environ['PATH']}"},
            )
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn("fixture: cargo fetch failed", result.stdout)
            self.assertIn("cargo +1.99.0 fetch langarena/rust/Cargo.toml", result.stderr)
            shown = current.read_text()
            self.assertIn(f"**Latest attempt** · `{head}`", shown)
            self.assertIn("Failed · cargo +1.99.0 fetch langarena/rust/Cargo.toml (exit 2)", shown)
            self.assertIn(f"**Last complete board** · `{commit}`", shown)
            self.assertIn("1 commits behind origin/main", shown)
            self.assertIn("Complete results", shown)
            self.assertNotIn("Refused results", shown)
            self.assertEqual((base / "last-complete.json").resolve(), board)


if __name__ == "__main__":
    unittest.main()
