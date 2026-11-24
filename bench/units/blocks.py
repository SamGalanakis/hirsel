"""Timing-block discipline for mc-units-proto (host rules).

A block holds the regimes lock (and the board lock when it touches CPUs
10-11), refuses the 02:45-04:00 window for board CPUs, and is discarded when
`pgrep -x cc1plus` prints anything before or after it.
"""
from contextlib import ExitStack
import datetime
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "core/tools"))
from timing_shield import BOARD, REGIMES, hold
FORBIDDEN = set(range(12, 16)) | set(range(28, 32)) | set(range(20, 24))


def cpuset(spec):
    out = set()
    for part in str(spec).split(","):
        if "-" in part:
            a, b = part.split("-")
            out |= set(range(int(a), int(b) + 1))
        elif part:
            out.add(int(part))
    return out


def cc1plus():
    r = subprocess.run(["pgrep", "-x", "cc1plus"], capture_output=True, text=True)
    return r.stdout.strip()


def board_window_ok():
    now = datetime.datetime.now().time()
    return not (datetime.time(2, 45) <= now < datetime.time(4, 0))


class Block:
    """with Block(cpus) as b: ... ; b.clean says whether to keep the data."""

    def __init__(self, cpus, log=print, board=None):
        self.cpus = set(cpus)
        bad = self.cpus & FORBIDDEN
        if bad:
            raise SystemExit(f"forbidden CPUs {sorted(bad)}")
        # The board lock too when the block touches CPUs 10-11, or when asked.
        self.board = bool(self.cpus & {10, 11}) if board is None else board
        self.log = log
        self.locks = ExitStack()
        self.clean = False

    def __enter__(self):
        while True:
            while self.board and not board_window_ok():
                self.log("board window closed (02:45-04:00); waiting")
                time.sleep(60)
            while cc1plus():
                self.log("cc1plus running; waiting before block")
                time.sleep(30)
            with ExitStack() as locks:
                locks.enter_context(hold(REGIMES, *([BOARD] if self.board else [])))
                if not cc1plus() and (not self.board or board_window_ok()):
                    self.locks = locks.pop_all()
                    return self

    def __exit__(self, *exc):
        with self.locks:
            after = cc1plus()
            self.clean = not after
            if after:
                self.log(f"cc1plus appeared during block ({after}); discarding")
        return False
