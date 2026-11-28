#!/usr/bin/env python3
"""Build RTC-10 N3 with FFmpeg's 720p VP8 fixture and run it on Boat."""
import argparse
import json
from pathlib import Path
import struct
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[4] / 'tools/toolchain-dist'))
import hcfg

HERE = Path(__file__).resolve().parent
SFU = HERE.parent
sys.path.insert(0, str(SFU))
from service.run import ROOT, inputs

DISPATCH = Path("/workspace/code/hirsel-briefs/tools/boat-dispatch")


def video_source(path):
    """Compile one 30-frame GOP into the test program. The VM needs only the executable."""
    video = subprocess.check_output([
        "ffmpeg", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30",
        "-frames:v", "30", "-c:v", "libvpx", "-threads", "2", "-b:v", "300k",
        "-g", "30", "-auto-alt-ref", "0", "-f", "ivf", "pipe:1",
    ])
    if video[:4] != b"DKIF" or struct.unpack_from("<4sHH", video, 8) != (b"VP80", 1280, 720):
        raise ValueError("FFmpeg did not write 720p VP8")
    offset = 32
    arms = []
    while offset < len(video):
        size = struct.unpack_from("<I", video, offset)[0]
        frame = video[offset + 12:offset + 12 + size]
        if not frame or len(frame) != size:
            raise ValueError("truncated IVF frame")
        arms.append(f'        {len(arms)} => b"' + "".join(f"\\x{byte:02x}" for byte in frame) + '",')
        offset += 12 + size
    if len(arms) != 30:
        raise ValueError(f"expected 30 VP8 frames, got {len(arms)}")
    path.write_text('fn n3_video(i: u64) -> Bytes {\n    match i {\n' +
                    "\n".join(arms) + '\n        _ => fail("unknown video frame"),\n    }\n}\n')


def build(args):
    bundle = args.scratch / "bundle"
    bundle.mkdir(parents=True, exist_ok=True)
    fixture = args.scratch / "frames.hirsel"
    video_source(fixture)
    corpus = inputs(args.scratch, SFU)
    data = json.loads(corpus.read_text())
    manifest = hcfg.package(SFU)
    sfu = next(p for p in data["packages"] if p["package"]["name"] == "webrtc_sfu")
    for file, module in manifest["tests"]["sources"].items():
        sfu["sources"].append({"file": str(SFU / file), "modules": module.split("."), "signatures": False})
    for file in (HERE / "pool.hirsel", fixture):
        sfu["sources"].append({"file": str(file), "modules": ["laws"], "signatures": False})
    corpus.write_text(json.dumps(data) + "\n")
    subprocess.run([
        args.compiler, "build", "--source-core", "--inputs", corpus,
        "--store", args.scratch / "store", "--entry", "webrtc_sfu@1/laws/main",
        "--backend", "llvm", "--release", "--runtime", args.runtime, "--limit", str(1 << 30),
        "--grant", "Clock", "--grant", "Tasks", "--grant", "Stdout", "-o", bundle / "n3",
    ], check=True, cwd=ROOT)
    (bundle / "n3.dwarf").unlink(missing_ok=True)
    (bundle / "n3.dwp").unlink(missing_ok=True)
    print(bundle)


def run(args):
    results = args.scratch / "results"
    results.mkdir(parents=True, exist_ok=True)
    subprocess.run([
        args.dispatch, "rtc10-n3", args.scratch / "bundle",
        "systemd-run --user --scope --quiet -p MemoryMax=2G -p MemorySwapMax=0 "
        "/usr/bin/time -v -o n3.time ./n3 >n3.out 2>n3.err",
        "--ttl", "3600", "--fetch", f"n3.out:{results}", "--fetch", f"n3.err:{results}",
        "--fetch", f"n3.time:{results}",
    ], check=True, cwd=args.scratch)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(required=True)
    for name, command in (("build", build), ("run", run)):
        sub = commands.add_parser(name)
        sub.set_defaults(command=command)
        sub.add_argument("--scratch", type=Path, required=True)
        if name == "build":
            sub.add_argument("--compiler", type=Path, required=True)
            sub.add_argument("--runtime", type=Path, required=True)
        else:
            sub.add_argument("--dispatch", type=Path, default=DISPATCH)
    args = parser.parse_args()
    args.scratch = args.scratch.resolve()
    if args.command is build:
        args.compiler = args.compiler.resolve()
        args.runtime = args.runtime.resolve()
    args.command(args)


if __name__ == "__main__":
    main()
