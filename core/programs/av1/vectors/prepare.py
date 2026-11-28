"""Fetch dav1d conformance inputs and generate expectations with direct C calls."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tools/toolchain-dist"))
import hcfg
import tempfile

CODEC = Path(__file__).resolve().parents[1]
TEST_DATA = "61afa1ceb6029be1a8ea3f6b9c9a9672700f13bb"
DECODE = (
    ("8-bit/issues/309_odd_width.ivf", "30d31f7c74575e58366898534a87841d"),
    ("10-bit/argon/test5606.obu", "0888c66e9ad2f6ebc7f6d6fd8b464dd8"),
    ("12-bit/data/00000686.ivf", "7333ff714f1f00df3f0d3ab5fd979599"),
    # 16×16 frames decode with stack calls.
    ("8-bit/size/av1-1-b8-01-size-16x16.ivf", "0d627c5089fa1595c6e7c0626b882bb4"),
)


def run(*command, **options):
    return subprocess.run(command, check=True, **options)


def ivf_payloads(source):
    assert source[:4] == b"DKIF" and source[8:12] == b"AV01"
    offset = struct.unpack_from("<H", source, 6)[0]
    while offset < len(source):
        size, = struct.unpack_from("<I", source, offset)
        offset += 12
        assert offset + size <= len(source)
        yield source[offset : offset + size]
        offset += size


def leb128(source, offset):
    value = shift = 0
    while True:
        byte = source[offset]
        offset += 1
        value |= (byte & 127) << shift
        shift += 7
        if byte < 128:
            return value, offset


def write_leb128(value):
    out = bytearray()
    while True:
        out.append((value & 127) | (128 if value > 127 else 0))
        value >>= 7
        if not value:
            return bytes(out)


def annexb_payloads(source):
    """Annex B temporal units as low-overhead OBUs (AV1 §5.3, Annex B)."""
    offset = 0
    while offset < len(source):
        size, offset = leb128(source, offset)
        unit_end = offset + size
        packet = bytearray()
        while offset < unit_end:
            frame_size, offset = leb128(source, offset)
            frame_end = offset + frame_size
            while offset < frame_end:
                length, offset = leb128(source, offset)
                obu = source[offset : offset + length]
                offset += length
                header = 2 if obu[0] & 4 else 1
                payload = obu[header:]
                if obu[0] & 2:
                    # obu_size may be shorter than obu_length; the rest is padding.
                    size, at = leb128(obu, header)
                    payload = obu[at : at + size]
                packet += bytes([obu[0] | 2]) + obu[1:header] + write_leb128(len(payload)) + payload
            assert offset == frame_end
        assert offset == unit_end
        yield bytes(packet)


def pixel_frames(pictures):
    offset = 0
    while offset < len(pictures):
        width, height, depth, layout = struct.unpack_from("<IIII", pictures, offset)
        offset += 16
        cw = (width + 1) // 2 if layout in (1, 2) else width
        ch = (height + 1) // 2 if layout == 1 else height
        size = (width * height + (2 * cw * ch if layout else 0)) * (2 if depth > 8 else 1)
        assert offset + size <= len(pictures)
        yield pictures[offset : offset + size]
        offset += size


def svt_frame():
    # SvtAv1EncFastDecodeApiTest.cc's fill_frame at the pinned SVT release.
    pixels = bytearray()
    seed = 4321
    for i in range(128 * 128 * 3 // 2):
        seed = (seed * 1664525 + 1013904223) & 0xFFFFFFFF
        if i < 128 * 128:
            dx, dy = i % 128 - 64, i // 128 - 48
            pixels.append(48 + (((dx * dx + dy * dy) >> 4) & 127) + (seed >> 28))
        else:
            pixels.append(100 + ((i // 5) & 63) + (seed >> 29))
    return pixels


def main(work, reference):
    fixtures = work / "fixtures"
    fixtures.mkdir(parents=True, exist_ok=True)
    shutil.copy2(CODEC / "vectors/tests.hirsel", work)
    # The test runs in its own empty working directory, so its grant is absolute.
    hcfg.vector_package(CODEC, CODEC / "vectors", work, fixtures)
    executable = work / "reference"
    llvm = Path(os.environ["LLVM_SYS_231_PREFIX"])
    run(llvm / "bin/clang", "-O2", "-I", reference,
        CODEC / "vectors/reference.c", reference / "libdav1d.a", reference / "libSvtAv1Enc.a",
        "-pthread", "-lm", "-o", executable)
    with tempfile.TemporaryDirectory(prefix="ref-dav1d-test-data.", dir="/tmp") as fresh:
        # DAV1D_TEST_DATA may name an existing clone that holds TEST_DATA.
        checkout = os.environ.get("DAV1D_TEST_DATA")
        if not checkout:
            checkout = fresh
            run("git", "init", "--quiet", checkout)
            run("git", "-C", checkout, "fetch", "--quiet", "--depth=1", "--filter=blob:none",
                "https://code.videolan.org/videolan/dav1d-test-data.git", TEST_DATA)
        for i, (name, digest) in enumerate(DECODE):
            source = run("git", "-c", "gc.auto=0", "-C", checkout, "show", f"{TEST_DATA}:{name}", stdout=subprocess.PIPE).stdout
            packets = ivf_payloads(source) if name.endswith(".ivf") else annexb_payloads(source)
            framed = b"".join(struct.pack("<I", len(packet)) + packet for packet in packets)
            packet_file = fixtures / f"decode-{i}.av1"
            packet_file.write_bytes(framed)
            expected = fixtures / f"decode-{i}.yuv"
            run(executable, "decode", packet_file, expected)
            frames = list(pixel_frames(expected.read_bytes()))
            actual = hashlib.md5(b"".join(frames)).hexdigest()
            assert frames and actual == digest, (name, actual, digest)
            print(f"{name}: {len(frames)} frames, upstream MD5 {actual}")

    frame = svt_frame()
    for i, (avif, speed, fast, depth, count) in enumerate(((1, 6, 3, 8, 1), (0, 13, 2, 10, 20))):
        raw = frame if depth == 8 else b"".join(struct.pack("<H", x * 4) for x in frame)
        infile = fixtures / f"encode-{i}.yuv"
        infile.write_bytes(raw * count)
        run(executable, "encode", infile, fixtures / f"encode-{i}.av1",
            str(avif), str(speed), str(fast), str(depth))


if __name__ == "__main__":
    main(*(Path(value).resolve() for value in sys.argv[1:]))
