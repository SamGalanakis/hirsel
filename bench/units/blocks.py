"""Timing-block discipline for mc-units-proto (host rules).

A block holds the regimes lock (and the board lock when it touches CPUs
10-11), refuses the 02:45-04:00 window for board CPUs, and is discarded when
`pgrep -x cc1plus` prints anything before or after it.
"""
import datetime, fcntl, os, subprocess, time

REGIMES = "/run/user/1000/hirsel-mc-regimes.lock"
BOARD = "/run/user/1000/hirsel-board.lock"
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
        self.files = []
        self.clean = False

    def __enter__(self):
        while True:
            while self.board and not board_window_ok():
                self.log("board window closed (02:45-04:00); waiting")
                time.sleep(60)
            while cc1plus():
                self.log("cc1plus running; waiting before block")
                time.sleep(30)
            paths = [REGIMES] + ([BOARD] if self.board else [])
            self.files = []
            for p in paths:
                f = open(p, "a")
                fcntl.flock(f, fcntl.LOCK_EX)  # wait our turn; never break a lock
                self.files.append(f)
            if cc1plus():
                self._release()
                continue
            if self.board and not board_window_ok():
                self._release()
                continue
            return self

    def _release(self):
        for f in reversed(self.files):
            fcntl.flock(f, fcntl.LOCK_UN)
            f.close()
        self.files = []

    def __exit__(self, *exc):
        after = cc1plus()
        self.clean = not after
        if after:
            self.log(f"cc1plus appeared during block ({after}); discarding")
        self._release()
        return False
