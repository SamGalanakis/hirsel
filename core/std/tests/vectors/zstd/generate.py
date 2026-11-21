#!/usr/bin/env python3
"""Print the checked-in Zstandard vectors as Hirsel. --refresh runs zstd."""
import argparse
import json
from pathlib import Path
import random
import subprocess
import tempfile

CORPUS = Path(__file__).with_name("reference.json")


def zstd(*args, data):
    return subprocess.run(["zstd", "-q", *args], input=data, capture_output=True, check=True).stdout


def refresh():
    rng = random.Random(0x8878)
    phrase = b"window block sequence offset standard content checksum "
    binary = rng.randbytes(73)
    text = bytes(rng.choices(b"abcdefghijklmnopqrstuvwxyz ", range(1, 28), k=640))
    inputs = [b"", b"x", binary, text, phrase * 9]
    frames = []
    for level in range(1, 20):
        original = (level - 1) % len(inputs)
        plain = inputs[original]
        packed = zstd("-c", f"-{level}", f"--stream-size={len(plain)}", "--check", data=plain)
        assert zstd("-dc", data=packed) == plain
        frames.append([original, 0, packed.hex()])

    long_frame = zstd("-c", "-3", "--long=23", "--no-content-size", "--check", data=inputs[4])
    descriptor, window = long_frame[4:6]
    assert not descriptor & 32 and window >> 3 == 13
    frames.append([4, 0, long_frame.hex()])
    frames.append([4, 0, (bytes.fromhex("502a4d1803000000") + b"abc" + long_frame).hex()])

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        samples = []
        for i in range(70):
            path = root / f"sample-{i:03}.txt"
            path.write_bytes((phrase + f"record={i} field={i*i} ".encode()) * 6)
            samples.append(str(path))
        formatted = root / "formatted"
        subprocess.run(
            ["zstd", "-q", "--train", "--maxdict=512", "--dictID=717003",
             "-o", str(formatted), *samples],
            capture_output=True, check=True,
        )
        raw = root / "raw"
        raw.write_bytes(phrase * 9)
        dictionaries = [formatted.read_bytes().hex(), raw.read_bytes().hex()]
        for index, dictionary in enumerate([formatted, raw], 1):
            packed = zstd("-c", "-3", "-D", str(dictionary), "--check", data=inputs[4])
            assert zstd("-dc", "-D", str(dictionary), data=packed) == inputs[4]
            frames.append([4, index, packed.hex()])

    corpus = dict(inputs=[x.hex() for x in inputs], frames=frames, dictionaries=dictionaries)
    CORPUS.write_text(json.dumps(corpus, indent=2) + "\n")


def literal(hex_bytes):
    return 'b"' + "".join(
        fr"\x{hex_bytes[at:at + 2]}" for at in range(0, len(hex_bytes), 2)
    ) + '"'


def emit():
    corpus = json.loads(CORPUS.read_text())
    for name in ["inputs", "dictionaries"]:
        print(f"fn {name}() -> List[Bytes] {{")
        print("    let mut items: List[Bytes] = list.new()")
        for value in corpus[name]:
            print(f"    list.push(mut items, {literal(value)})")
        print("    take items\n}")
    print("fn references() -> List[(u64, u64, Bytes)] {")
    print("    let mut items: List[(u64, u64, Bytes)] = list.new()")
    for original, dictionary, packed in corpus["frames"]:
        print(f"    list.push(mut items, ({original}u64, {dictionary}u64, {literal(packed)}))")
    print("    take items\n}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--refresh", action="store_true")
    if parser.parse_args().refresh:
        refresh()
    else:
        emit()
