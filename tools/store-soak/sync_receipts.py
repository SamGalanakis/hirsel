#!/usr/bin/env python3
"""Pull complete receipts from the worker until its terminal receipt."""
import argparse
from pathlib import Path
import subprocess
import time

from observe import receipts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", help="SSH host and absolute remote receipts directory")
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    args.destination.mkdir(parents=True, exist_ok=True)
    while True:
        transfer = subprocess.run([
            "rsync", "--recursive", "--times", "--include=*.json", "--exclude=*",
            "-e", "ssh -o BatchMode=yes -o ConnectTimeout=10",
            args.source.rstrip("/") + "/", args.destination,
        ], timeout=45)
        if transfer.returncode != 0:
            raise SystemExit(f"receipt sync failed with rsync exit {transfer.returncode}")
        if any(args.destination.glob("*.json")):
            rows = receipts(args.destination)
            if rows[-1]["kind"] in {"failed", "complete"}:
                return
        time.sleep(30)


if __name__ == "__main__":
    main()
