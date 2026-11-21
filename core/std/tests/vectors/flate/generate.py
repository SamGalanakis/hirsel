#!/usr/bin/env python3
"""Print the checked-in codec vectors as Hirsel. --refresh uses gzip and zlib."""
import argparse
import json
from pathlib import Path
import random
import subprocess
import zlib

CORPUS = Path(__file__).with_name("reference.json")


def refresh():
    rng = random.Random(0x1951)
    dynamic = bytes(rng.choices(b"abcdefghijklmnopqrstuvwxyz ", range(1, 28), k=640))
    binary = rng.randbytes(73)
    originals = [b"", b"x", binary, dynamic, b"window wrap above 32k\n" * 1700]
    rows = []

    def add(fmt, original, packed):
        rows.append([fmt, original, packed.hex()])

    for level in range(1, 10):
        original = [0, 1, 2, 3][(level - 1) % 4]
        data = originals[original]
        add(2, original, subprocess.run(
            ["gzip", "-n", "-c", f"-{level}"], input=data, capture_output=True, check=True
        ).stdout)
        for fmt, window in [(0, -15), (1, 15)]:
            compressor = zlib.compressobj(level, wbits=window)
            add(fmt, original, compressor.compress(data) + compressor.flush())

    for fmt, window in [(0, -15), (1, 15)]:
        compressor = zlib.compressobj(0, wbits=window)
        add(fmt, 2, compressor.compress(binary) + compressor.flush())
    add(2, 4, subprocess.run(
        ["gzip", "-n", "-9", "-c"], input=originals[4], capture_output=True, check=True
    ).stdout)
    prefix = bytes.fromhex("1f8b081e0000000000ff") + b"\x03\x00abcname\x00comment\x00"
    packed = prefix + (zlib.crc32(prefix) & 0xffff).to_bytes(2, "little")
    compressor = zlib.compressobj(6, wbits=-15)
    packed += compressor.compress(binary) + compressor.flush()
    packed += zlib.crc32(binary).to_bytes(4, "little") + len(binary).to_bytes(4, "little")
    assert zlib.decompress(packed, wbits=31) == binary
    add(2, 2, packed)
    corpus = dict(inputs=[x.hex() for x in originals[:4]], rows=rows)
    CORPUS.write_text(json.dumps(corpus, indent=2) + "\n")


def literal(hex_bytes):
    return 'b"' + "".join(
        fr"\x{hex_bytes[at:at + 2]}" for at in range(0, len(hex_bytes), 2)
    ) + '"'


def emit():
    corpus = json.loads(CORPUS.read_text())
    print("fn originals() -> List[Bytes] {")
    print("    let mut items: List[Bytes] = list.new()")
    for plain in corpus["inputs"]:
        print(f"    list.push(mut items, {literal(plain)})")
    print("    let mut repeated: Bytes = list.new()")
    print("    for i in 0u64 .. 1700 {")
    print('        list.append(mut repeated, string.as_bytes("window wrap above 32k\\n"))')
    print("    }")
    print("    list.push(mut items, take repeated)")
    print("    take items\n}")
    print("fn references() -> List[(u8, u64, Bytes)] {")
    print("    let mut items: List[(u8, u64, Bytes)] = list.new()")
    for fmt, original, packed in corpus["rows"]:
        print(f"    list.push(mut items, ({fmt}u8, {original}u64, {literal(packed)}))")
    print("    take items\n}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh", action="store_true")
    if parser.parse_args().refresh:
        refresh()
    else:
        emit()
