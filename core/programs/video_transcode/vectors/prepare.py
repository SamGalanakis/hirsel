"""Prepare the video_transcode law (or bench) workspace and fixtures.

Usage: prepare.py WORK law
       prepare.py WORK bench WORKERS

Builds WORK/ as its own Hirsel workspace (a path dependency must be a
workspace member), with the packages copied from core/programs and the
test sources from vectors/, and writes fixtures with ffmpeg:

law   case 0: H.264 High 640x360 30 fps, 6 s, 3 B-frames, IDRs forced at
              0, 1.3, 2.0, 3.7, 4.1 and 5.5 s (uneven closed GOPs), a 90°
              display rotation and Opus audio.
      case 1: AV1 10-bit 512x128 24 fps, 2 s, BT.2020 PQ grey bars at known
              luminances, a key frame every second.
bench case 0: H.264 1920x1080 30 fps, 20 s, 2 s GOPs, temporal noise and
              Opus audio, transcoded to a four-rung ladder.
"""

import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / "tools/toolchain-dist"))
import hcfg

import numpy as np

PACKAGES = ["av1", "h264", "media_bits", "media_container", "video_packager", "video_yuv"]
MIB = 1 << 20

# SMPTE ST 2084 constants.
M1, M2 = 2610 / 16384, 2523 / 4096 * 128
C1, C2, C3 = 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32
# Grey bar luminances in cd/m² for the HDR case (source peak 1000).
BARS = [1, 10, 50, 100, 200, 400, 700, 1000]


def pq_inverse(nits):
    y = (nits / 10000) ** M1
    return ((C1 + C2 * y) / (1 + C3 * y)) ** M2


def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)


def ffmpeg(*args):
    run("ffmpeg", "-hide_banner", "-nostdin", "-y", *args)


def sdr(path):
    ffmpeg(
        "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=30:duration=6",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=6",
        "-c:v", "libx264", "-preset", "medium", "-profile:v", "high", "-pix_fmt", "yuv420p",
        "-bf", "3", "-x264-params", "scenecut=0:keyint=1000:min-keyint=1:open-gop=0",
        "-force_key_frames", "0,1.3,2.0,3.7,4.1,5.5",
        "-c:a", "libopus", "-b:a", "64k",
        str(path.with_suffix(".plain.mp4")),
    )
    # Tag a 90° display rotation without re-encoding.
    ffmpeg("-display_rotation", "90", "-i", str(path.with_suffix(".plain.mp4")), "-c", "copy", str(path))
    path.with_suffix(".plain.mp4").unlink()


def hdr(path):
    width, height, frames = 512, 128, 48
    y = np.zeros((height, width), dtype="<u2")
    bar = width // len(BARS)
    for i, nits in enumerate(BARS):
        y[:, i * bar:(i + 1) * bar] = round(64 + 876 * pq_inverse(nits))
    chroma = np.full((height // 2, width // 2), 512, dtype="<u2")
    picture = y.tobytes() + chroma.tobytes() + chroma.tobytes()
    raw = path.with_suffix(".yuv")
    raw.write_bytes(picture * frames)
    ffmpeg(
        "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-s", f"{width}x{height}", "-r", "24", "-i", str(raw),
        "-c:v", "libsvtav1", "-preset", "8", "-crf", "4", "-g", "24",
        "-svtav1-params", "irefresh-type=2",
        "-pix_fmt", "yuv420p10le",
        "-color_primaries", "bt2020", "-color_trc", "smpte2084", "-colorspace", "bt2020nc", "-color_range", "tv",
        "-movflags", "+write_colr",
        str(path),
    )
    raw.unlink()


def bench(path):
    ffmpeg(
        "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=30:duration=20",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=20",
        "-vf", "noise=alls=6:allf=t",
        "-c:v", "libx264", "-preset", "fast", "-profile:v", "high", "-pix_fmt", "yuv420p",
        "-bf", "3", "-g", "60", "-x264-params", "scenecut=0:open-gop=0",
        "-c:a", "libopus", "-b:a", "96k",
        str(path),
    )


def config(workers, segment_ms, enc_mode, unit_bytes, rungs):
    words = [workers, segment_ms, enc_mode, unit_bytes, len(rungs)]
    for height, qp in rungs:
        words += [height, qp]
    return struct.pack(f"<{len(words)}Q", *words)


def main():
    work = Path(sys.argv[1]).resolve()
    mode = sys.argv[2]
    vectors = Path(__file__).resolve().parent
    package = vectors.parent
    programs = package.parent
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    members = PACKAGES + ["video_transcode"]
    hcfg.workspace(work, members)
    # Members must resolve inside the workspace, so copy rather than link.
    for name in PACKAGES:
        shutil.copytree(programs / name, work / name, ignore=shutil.ignore_patterns("vectors", "hirsel.lock"))
    target = work / "video_transcode"
    target.mkdir()
    for source in package.glob("*.hirsel"):
        shutil.copyfile(source, target / source.name)
    shutil.copyfile(vectors / "tests.hirsel", target / "tests.hirsel")
    fixtures = work / "fixtures"
    fixtures.mkdir()
    hcfg.vector_package(package, vectors, target, fixtures)

    cases = []
    if mode == "law":
        sdr(fixtures / "case-0.mp4")
        (fixtures / "case-0.cfg").write_bytes(config(3, 1000, 12, 320 * MIB, [(640, 30), (480, 32), (240, 34)]))
        cases.append({"kind": "sdr", "rungs": [640, 480, 240]})
        hdr(fixtures / "case-1.mp4")
        (fixtures / "case-1.cfg").write_bytes(config(2, 1000, 12, 96 * MIB, [(128, 8), (64, 20)]))
        cases.append({"kind": "hdr", "rungs": [128, 64], "bars": BARS, "peak": 1000})
    elif mode == "bench":
        workers = int(sys.argv[3])
        bench(fixtures / "case-0.mp4")
        (fixtures / "case-0.cfg").write_bytes(config(workers, 2000, 12, 384 * MIB, [(1080, 35), (720, 36), (480, 37), (360, 38)]))
        cases.append({"kind": "bench", "rungs": [1080, 720, 480, 360]})
    else:
        raise SystemExit(f"unknown mode {mode}")
    for index in range(len(cases)):
        (fixtures / f"out-{index}").mkdir()
    (fixtures / "cases").write_bytes(struct.pack("<Q", len(cases)))
    (work / "cases.json").write_text(json.dumps(cases))


if __name__ == "__main__":
    main()
