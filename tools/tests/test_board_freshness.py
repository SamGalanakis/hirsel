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
    def test_vopr_runs_after_board_and_preserves_its_verdict(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            main = base / "main"
            main.mkdir()
            subprocess.run(["git", "init", "-q", main], check=True)
            subprocess.run(["git", "-C", main, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.com",
                            "commit", "--allow-empty", "-qm", "Measured commit"], check=True)
            subprocess.run(["git", "-C", main, "update-ref", "refs/remotes/origin/main", "HEAD"], check=True)
            (base / "baseline-refused").touch()
            taskset = base / "taskset"
            taskset.write_text(r'''#!/bin/bash
shift 2
case "$1 $2 $3" in
  git*) exit 0 ;;
  'python3 bench/awfy/screen.py prepare') mkdir -p "$4" ;;
  'python3 bench/awfy/board.py '*) touch "${HIRSEL_BOARD_BASE}/board-completed" ;;
  'python3 tools/buck2/prepare.py ') exit 0 ;;
  '.buck2/bin/buck2 build -j4') echo '{"root//:hirsel-toolchain":"prefix"}' ;;
  'python3 core/tools/sim_campaign.py --compiler') exit 0 ;;
  */tools/boat-dispatch\ *) exit 0 ;;
  */tools/sim-campaign\ batch\ *)
    [[ -f "${HIRSEL_BOARD_BASE}/board-completed" && "$4" == prepared/manifest.json ]] || exit 90
    exit "$FIXTURE_VOPR_STATUS" ;;
  *) exit 91 ;;
esac
''')
            taskset.chmod(0o755)
            for campaign_status, expected in ((0, 0), (1, 1), (75, 0)):
                with self.subTest(campaign_status=campaign_status):
                    (base / "board-completed").unlink(missing_ok=True)
                    # Keep the existing two setup phases on the stub's first-run path.
                    (base / "last-screen-inputs").unlink(missing_ok=True)
                    result = subprocess.run(
                        [TOOLS / "nightly-board.sh"], capture_output=True, text=True,
                        env=os.environ | {"HIRSEL_BOARD_BASE": str(base),
                                          "HIRSEL_BOARD_CURRENT": str(base / "CURRENT-BOARD.md"),
                                          "HIRSEL_VOPR_LEDGER": str(base / "seeds.sqlite"),
                                          "FIXTURE_VOPR_STATUS": str(campaign_status),
                                          "PATH": f"{base}:{os.environ['PATH']}"},
                    )
                    self.assertEqual(result.returncode, expected, result.stderr)
                    log = (Path(result.stdout.strip()) / "run.log").read_text()
                    if campaign_status == 1:
                        self.assertIn("VOPR campaign exited 1", log)
                    if campaign_status == 75:
                        self.assertIn("VOPR deferred", log)

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
                '  python3) [[ "$2 $3" == "bench/awfy/screen.py prepare" ]] && mkdir -p "$4" ;;\n'
                '  */tools/boat-dispatch) exit 0 ;;\n'
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
