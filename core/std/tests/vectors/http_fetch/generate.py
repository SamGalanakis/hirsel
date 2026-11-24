#!/usr/bin/env python3
"""Emit pinned HTTP coding fixtures. --refresh checks the three external decoders."""
import argparse
import json
from pathlib import Path
import subprocess
import zlib

CORPUS = Path(__file__).with_name("reference.json")


def cli(tool, *flags, data):
    return subprocess.run(
        [tool, *flags], input=data, capture_output=True, check=True
    ).stdout


def refresh():
    at = bytes(32768)
    over = bytes(32769)
    first = cli("gzip", "-nc", data=at[:16384])
    gzip_at = first + cli("gzip", "-nc", data=at[16384:])
    gzip_over = first + cli("gzip", "-nc", data=over[16384:])
    assert cli("gzip", "-dc", data=gzip_at) == at
    assert cli("gzip", "-dc", data=gzip_over) == over

    zlib_at = zlib.compress(at)
    zlib_over = zlib.compress(over)
    raw_at = zlib.compress(at, wbits=-15)
    assert zlib.decompress(zlib_at) == at
    assert zlib.decompress(zlib_over) == over
    assert zlib.decompress(raw_at, wbits=-15) == at

    first = cli("zstd", "-q", "-c", "--stream-size=16384", data=at[:16384])
    second = cli("zstd", "-q", "-c", "--stream-size=16385", data=over[16384:])
    skip = bytes.fromhex("502a4d1803000000") + b"abc"
    zstd_at = first + skip + first
    zstd_over = first + skip + second
    assert cli("zstd", "-q", "-dc", data=zstd_at) == at
    assert cli("zstd", "-q", "-dc", data=zstd_over) == over

    # RFC 8878: no single-segment bit, window descriptor 0 gives a 1024-byte
    # window, with one final empty raw block. Ask 1 byte more than the test cap.
    window = bytes.fromhex("28b52ffd0000010000")
    assert cli("zstd", "-q", "-dc", data=window) == b""
    rows = [
        ("x-gzip", gzip_at, 0),
        ("gzip", gzip_over, 1),
        ("gzip", gzip_at + b"!", 2),
        ("deflate", zlib_at, 0),
        ("deflate", zlib_over, 1),
        ("deflate", raw_at, 3),
        ("zstd", zstd_at, 0),
        ("zstd", zstd_over, 1),
        ("zstd", window, 4),
    ]
    CORPUS.write_text(json.dumps(
        [[coding, data.hex(), expected] for coding, data, expected in rows],
        indent=2,
    ) + "\n")


def emit():
    print("fn coding_vectors() -> List[(String, Bytes, u8)] {")
    print("    let mut cases: List[(String, Bytes, u8)] = list.new()")
    for coding, data, expected in json.loads(CORPUS.read_text()):
        literal = "".join(r"\x" + data[i:i + 2] for i in range(0, len(data), 2))
        print(f'    list.push(mut cases, ("{coding}", b"{literal}", {expected}u8))')
    print("    take cases\n}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh", action="store_true")
    if parser.parse_args().refresh:
        refresh()
    else:
        emit()
