"""Check media_record's files with FFmpeg, outside Hirsel.

Usage: validate.py WORK

For each codec dumped by vectors/dump.hirsel (VP8, VP9 and AV1 in WebM,
H.264 in fragmented MP4, each with its Opus track in Ogg):

1. ffprobe opens the file and finds one stream of the right codec.
2. ffmpeg decodes it with no error, one decoded frame per packet.
3. The packets' presentation times rise, match the sent frames' true times
   (capture less the recording's zero, from <codec>.times) within one frame,
   and the file's span is the sent frames' span within one frame.
4. The video's first frame against the audio packet at the same true time:
   the A/V offset is under 40 ms.
"""

import json
from pathlib import Path
import subprocess
import sys

CODECS = {"vp8": ("webm", "vp8"), "vp9": ("webm", "vp9"), "av1": ("webm", "av1"), "h264": ("mp4", "h264")}
FRAME = {"audio": 0.020, "video": 1 / 30}


def run(*args):
    done = subprocess.run(args, capture_output=True)
    if done.returncode != 0:
        raise AssertionError(f"{args[0]} failed: {done.stderr.decode()[-2000:]}")
    return done


def probe(path, entries):
    return json.loads(run("ffprobe", "-v", "error", "-of", "json", *entries, str(path)).stdout)


def check(path, kind, codec, truth):
    streams = probe(path, ["-show_entries", "stream=codec_name,codec_type"])["streams"]
    assert len(streams) == 1 and streams[0]["codec_name"] == codec, f"{path}: streams {streams}"
    packets = probe(path, ["-show_entries", "packet=pts_time"])["packets"]
    pts = [float(p["pts_time"]) for p in packets]
    decode = run("ffmpeg", "-v", "error", "-xerror", "-i", str(path), "-f", "framemd5", "-")
    assert decode.stderr == b"", f"{path}: decoder errors {decode.stderr.decode()[-2000:]}"
    decoded = [line for line in decode.stdout.decode().splitlines() if line and not line.startswith("#")]
    assert len(decoded) == len(pts), f"{path}: {len(decoded)} decoded frames for {len(pts)} packets"
    assert all(b >= a for a, b in zip(pts, pts[1:])), f"{path}: timestamps go back"
    assert len(pts) == len(truth), f"{path}: {len(pts)} packets for {len(truth)} sent frames"
    frame = FRAME[kind]
    for got, want in zip(pts, truth):
        assert abs(got - want) < frame, f"{path}: packet at {got:.6f} s, sent at {want:.6f} s"
    span = pts[-1] - pts[0]
    assert abs(span - (truth[-1] - truth[0])) < frame, f"{path}: span {span:.6f} s"
    return pts


def main():
    out = Path(sys.argv[1]).resolve() / "out"
    for name, (extension, codec) in CODECS.items():
        truth = {"audio": [], "video": []}
        for line in (out / f"{name}.times").read_text().splitlines():
            kind, ns = line.split()
            truth[kind].append(int(ns) / 1e9)
        zero = truth["audio"][0]
        truth = {k: [t - zero for t in v] for k, v in truth.items()}
        audio = check(out / f"{name}-audio.ogg", "audio", "opus", truth["audio"])
        video = check(out / f"{name}-video.{extension}", "video", codec, truth["video"])
        # Audio packet k and video frame 0 were captured this far apart.
        k = min(range(len(audio)), key=lambda i: abs(truth["audio"][i] - truth["video"][0]))
        offset = (video[0] - audio[k]) - (truth["video"][0] - truth["audio"][k])
        assert abs(offset) < 0.040, f"{name}: A/V offset {offset * 1000:.1f} ms"
        print(f"{name}: {len(video)} video and {len(audio)} audio packets, A/V offset {offset * 1000:.2f} ms")


if __name__ == "__main__":
    main()
