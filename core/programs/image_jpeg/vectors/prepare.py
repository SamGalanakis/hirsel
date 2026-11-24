"""Generate matching direct-libjpeg expectations from the pinned test images."""

from pathlib import Path
import json
import os
import random
import shutil
import subprocess
import sys


def main(work, reference):
    vectors = Path(__file__).resolve().parent
    codec = vectors.parent
    fixtures = work / "fixtures"
    fixtures.mkdir(parents=True, exist_ok=True)
    for name in ("image_jpeg.hirsel", "native.hirsel", "libhirsel_jpeg.a"):
        shutil.copyfile(codec / name, work / name)
    shutil.copyfile(vectors / "tests.hirsel", work / "tests.hirsel")
    manifest = (codec / "hirsel.toml").read_text()
    tests = (vectors / "test.toml").read_text()
    (work / "hirsel.toml").write_text(manifest + tests.replace('"fixtures"', json.dumps(str(fixtures))))

    direct = work / "direct"
    subprocess.run([
        os.environ.get("CC", "cc"), "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
        "-I", reference, vectors / "reference.c", reference / "libjpeg.a", "-o", direct,
    ], check=True)

    def run(mode, source, output, *args):
        subprocess.run([direct, mode, source, output, *map(str, args)], check=True)

    baseline = (reference / "testimages" / "testorig.jpg").read_bytes()
    (fixtures / "baseline.jpg").write_bytes(baseline)
    # A legal APP1 spans multiple source windows.
    (fixtures / "long-app.jpg").write_bytes(
        baseline[:2] + b"\xff\xe1\xff\xff" + bytes(65533) + baseline[2:]
    )
    ppm = (reference / "testimages" / "testorig.ppm").read_bytes()
    magic, dimensions, depth, pixels = ppm.split(b"\n", 3)
    assert magic == b"P6" and depth == b"255"
    width, height = map(int, dimensions.split())
    assert (width, height) == (227, 149) and len(pixels) == 3 * width * height
    rgb = fixtures / "source.rgb"
    rgb.write_bytes(pixels)
    for options in range(4):
        run("encode", rgb, fixtures / f"encode-{options}.jpg", options, width, height)

    # High-entropy scanlines exercise output suspension in sequential encoding.
    wide_rgb = fixtures / "wide.rgb"
    wide_rgb.write_bytes(random.Random(2233).randbytes(2048 * 37 * 3))
    run("encode", wide_rgb, fixtures / "wide.jpg", 0, 2048, 37)
    # A flat progressive scan can complete rows without consuming input.
    flat_rgb = fixtures / "flat.rgb"
    flat_jpeg = fixtures / "flat.jpg"
    flat_rgb.write_bytes(bytes(512 * 512 * 3))
    run("encode", flat_rgb, flat_jpeg, 2, 512, 512)
    with flat_jpeg.open("ab") as stream:
        stream.write(bytes(65536))
    for name in ("baseline", "encode-2", "long-app", "wide", "flat"):
        for scale in range(1, 9) if name in ("baseline", "encode-2") else (8,):
            run("decode", fixtures / f"{name}.jpg", fixtures / f"{name}-{scale}.rgb", scale)


if __name__ == "__main__":
    main(*(Path(argument).resolve() for argument in sys.argv[1:]))
