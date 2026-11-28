"""Run the pinned upstream libwebp directly to make the wrapper's test fixtures."""

import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tools/toolchain-dist"))
import hcfg


TEST_DATA_REVISION = "06ddd96e276c2c638a72d39d3c0f340afd61978c"
STILLS = (
    "test.webp", "test-nostrong.webp",
    "lossless1.webp", "lossless2.webp", "lossless3.webp", "lossless4.webp",
    "lossy_alpha1.webp", "lossy_alpha2.webp", "lossy_alpha3.webp", "lossy_alpha4.webp",
    "alpha_filter_0_method_0.webp", "alpha_filter_1_method_1.webp",
    "vp80-00-comprehensive-001.webp", "vp80-00-comprehensive-006.webp",
    "vp80-00-comprehensive-008.webp", "vp80-00-comprehensive-010.webp",
)


def chunk(name, body):
    return name + struct.pack("<I", len(body)) + body + b"\0" * (len(body) % 2)


def webp(*chunks):
    return chunk(b"RIFF", b"WEBP" + b"".join(chunks))


def u24(value):
    return value.to_bytes(3, "little")


def extended(flags, width, height):
    return chunk(b"VP8X", struct.pack("<I", flags) + u24(width - 1) + u24(height - 1))


def still_vector(data):
    # The legacy VP8 vectors carry IART/ICOP/INAM chunks after the bitstream.
    # Simple WebP contains just the bitstream chunk. Keep its payload intact.
    name, size = struct.unpack_from("<4sI", data, 12)
    if name == b"VP8 ":
        return webp(data[12:20 + size + size % 2])
    return data


def frame(source, width, height, x, y, duration, flags):
    payload = b""
    at = 12
    while at < len(source):
        name, size = struct.unpack_from("<4sI", source, at)
        end = at + 8 + size + size % 2
        if name in (b"VP8 ", b"VP8L", b"ALPH"):
            payload += source[at:end]
        at = end
    header = b"".join(map(u24, (x // 2, y // 2, width - 1, height - 1, duration)))
    return chunk(b"ANMF", header + bytes([flags]) + payload)


def reference(executable, mode, infile, outfile):
    with outfile.open("wb") as target:
        subprocess.run([executable, mode, infile], stdout=target, check=True)


def stage_dependency(programs, work, name):
    original = programs / name
    target = work / name
    target.mkdir(exist_ok=True)
    for filename in hcfg.package_files(original):
        shutil.copyfile(original / filename, target / filename)


def main():
    work, libs, test_data, llvm = (Path(arg).resolve() for arg in sys.argv[1:])
    revision = subprocess.check_output(["git", "-C", test_data, "rev-parse", "HEAD"], text=True).strip()
    if revision != TEST_DATA_REVISION:
        raise SystemExit(f"expected libwebp test data at {TEST_DATA_REVISION}")
    vectors = Path(__file__).resolve().parent
    codec = vectors.parent
    fixtures = work / "fixtures"
    fixtures.mkdir(parents=True, exist_ok=True)
    package = work / "image_webp"
    package.mkdir(exist_ok=True)
    dependencies = ("image_core", "media_container", "media_bits")
    for name in dependencies:
        stage_dependency(codec.parent, work, name)
    hcfg.workspace(work, [*dependencies, "image_webp"])
    for filename in ("webp.hirsel", "native.hirsel", "libhirsel_webp.a"):
        shutil.copyfile(codec / filename, package / filename)
    for filename in ("tests.hirsel", "example.hirsel"):
        shutil.copyfile(vectors / filename, package / filename)
    hcfg.vector_package(codec, vectors, package, fixtures)

    executable = work / "reference"
    subprocess.run([
        llvm / "bin/clang", "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
        "-I", libs / "include", vectors / "reference.c",
        *(libs / f"lib{name}.a" for name in ("webpdemux", "webp", "sharpyuv")),
        "-lm", "-pthread", "-o", executable,
    ], check=True)
    for i, name in enumerate(STILLS):
        source = fixtures / f"{i}.webp"
        original = subprocess.check_output(["git", "-C", test_data, "show", f"{TEST_DATA_REVISION}:{name}"])
        source.write_bytes(still_vector(original))
        reference(executable, "still", source, fixtures / f"{i}.rgba")
        if i in (0, 2, 6):
            reference(executable, "crop", source, fixtures / f"{i}.scaled")

    encode_input = fixtures / "encode.rgba"
    scaled = bytearray((fixtures / "6.scaled").read_bytes())
    scaled[:4] = bytes([13, 7, 245, 0])
    encode_input.write_bytes(scaled)
    for mode in ("lossless", "quality"):
        reference(executable, mode, encode_input, fixtures / f"{mode}.webp")

    tiny = fixtures / "tiny.webp"
    tiny_input = fixtures / "tiny-input.rgba"
    tiny_input.write_bytes(scaled[:4 * 4 * 4])
    reference(executable, "tiny", tiny_input, tiny)
    reference(executable, "still", tiny, fixtures / "tiny.rgba")

    first = frame((fixtures / "5.webp").read_bytes(), 256, 256, 2, 4, 30, 2)
    alpha = (fixtures / "9.webp").read_bytes()
    second = frame(alpha, 100, 100, 48, 36, 70, 1)
    third = frame(alpha, 100, 100, 6, 8, 110, 2)
    animation = extended(0x12, 260, 260), chunk(b"ANIM", b"\0" * 6)
    for name, frames in (("animation", (first, second, third)), ("single-frame", (first,))):
        source = fixtures / (name + ".webp")
        source.write_bytes(webp(*animation, *frames))
        reference(executable, "animation", source, fixtures / (name + ".expected"))

    # Keep the first VP8L header but remove its entropy payload.
    corrupt = bytearray(first[:37])
    struct.pack_into("<I", corrupt, 4, 30)
    struct.pack_into("<I", corrupt, 28, 5)
    corrupt.append(0)
    (fixtures / "corrupt-animation.webp").write_bytes(webp(*animation, corrupt, second, third))
    (fixtures / "129-frames.webp").write_bytes(webp(*animation, *([second] * 129)))
    (fixtures / "bomb.webp").write_bytes(webp(extended(0, 50000, 50000)))


if __name__ == "__main__":
    main()
