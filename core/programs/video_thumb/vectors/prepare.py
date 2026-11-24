#!/usr/bin/env python3
"""Stage the conformance package and generate the independent ffmpeg fixtures."""
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tomllib


def stage(work):
    vectors = Path(__file__).resolve().parent
    programs = vectors.parent.parent
    packages = (
        "av1", "h264", "image_core", "image_jpeg", "image_pipeline", "image_webp", "media_bits",
        "media_container", "video_yuv", "video_thumb",
    )
    for name in packages:
        source = programs / name
        target = work / name
        target.mkdir(parents=True, exist_ok=True)
        manifest = (source / "hirsel.toml").read_text()
        package = tomllib.loads(manifest)
        archives = [
            entry["file"]
            for lib in package.get("foreign", {}).values()
            for entry in lib["archives"]
        ]
        for filename in ("hirsel.toml", *package["sources"], *package.get("tests", {}).get("sources", ()), *archives):
            shutil.copyfile(source / filename, target / filename)
    (work / "hirsel.toml").write_text("[workspace]\nmembers = " + json.dumps(packages) + "\n")
    fixtures = work / "fixtures"
    fixtures.mkdir(exist_ok=True)
    test = (vectors / "test.toml").read_text().replace('"fixtures"', json.dumps(str(fixtures)))
    with (work / "video_thumb" / "hirsel.toml").open("a") as manifest:
        manifest.write(test)
    shutil.copyfile(vectors / "tests.hirsel", work / "video_thumb" / "tests.hirsel")
    return fixtures


def generate(fixtures):
    commands = fixtures.parent / "ffmpeg-commands.txt"

    def ffmpeg(*args):
        command = [
            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-filter_threads", "2",
            *map(str, args),
        ]
        with commands.open("a") as log:
            log.write(shlex.join(command) + "\n")
        subprocess.run(command, check=True)

    commands.write_text("")
    for name, encoder in (
        ("h264", ["-c:v", "libx264", "-preset", "ultrafast", "-crf", "20", "-sc_threshold", "0"]),
        ("av1", ["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "32", "-lag-in-frames", "0"]),
    ):
        short = fixtures / f"{name}-short.mp4"
        ffmpeg(
            "-f", "lavfi", "-i", "testsrc2=size=3840x2160:rate=2:duration=3",
            "-pix_fmt", "yuv420p", "-colorspace", "bt709", "-color_trc", "bt709",
            "-color_primaries", "bt709", "-color_range", "tv", "-g", "2",
            "-threads", "2", *encoder, short,
        )
        large = fixtures / f"{name}.mp4"
        ffmpeg("-stream_loop", "-1", "-i", short, "-c", "copy", "-fs", "1000000000", large)
        if large.stat().st_size < 1_000_000_000:
            raise RuntimeError(f"{large} did not reach 1 GB")
        ffmpeg(
            "-ss", "1", "-threads", "2", "-i", large, "-frames:v", "1",
            "-vf", "scale=in_color_matrix=bt709:in_range=limited", "-pix_fmt", "rgba",
            "-f", "rawvideo", fixtures / f"{name}.rgba",
        )
    webm = fixtures / "av1.webm"
    ffmpeg("-i", fixtures / "av1-short.mp4", "-c", "copy", webm)


if __name__ == "__main__":
    work = Path(sys.argv[1]).resolve()
    fixtures = stage(work)
    if "--stage-only" not in sys.argv[2:]:
        generate(fixtures)
