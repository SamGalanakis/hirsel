"""Check the objects the ladder law stored, outside Hirsel.

Usage: validate.py WORK

For each case prepared by prepare.py and run by the `ladder` test:

1. ffprobe opens master.m3u8 and manifest.mpd and finds every rendition,
   and opens each media and I-frame playlist.
2. Each video rendition (init + segments, the exact stored bytes) decodes
   with ffmpeg to exactly the source's frames: one decoded frame per
   sample, and the same presentation times as the source sample table, so
   no gap or duplicate at a GOP seam. The HLS playlist decodes to the same
   frame count and its segment durations add up to the source's.
3. SDR: every frame's luma PSNR against the source, rotated by ffmpeg's
   display-matrix autorotation and area-scaled to the rung, is at least
   PSNR_FLOOR dB.
4. HDR: the top rung's luma at each grey bar's centre equals the
   BT.2390 EETF reference value (computed here from the source luma that
   ffmpeg decodes) within TONE_TOLERANCE code values, and chroma is
   neutral.
5. Audio: the audio rendition's packets equal the source's, byte for byte.
"""

import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys

import numpy as np

PSNR_FLOOR = 30.0
TONE_TOLERANCE = 2
# SMPTE ST 2084.
M1, M2 = 2610 / 16384, 2523 / 4096 * 128
C1, C2, C3 = 3424 / 4096, 2413 / 4096 * 32, 2392 / 4096 * 32


def pq_eotf(e):
    p = e ** (1 / M2)
    return 10000 * (max(p - C1, 0) / (C2 - C3 * p)) ** (1 / M1)


def pq_inverse(nits):
    y = (nits / 10000) ** M1
    return ((C1 + C2 * y) / (1 + C3 * y)) ** M2


def eetf(e, source_peak, target_peak):
    """ITU-R BT.2390 §5.4.1 with Lb = Lmin = 0, on a normalized PQ value."""
    lw = pq_inverse(source_peak)
    max_lum = pq_inverse(target_peak) / lw
    e1 = min(e / lw, 1.0)
    ks = 1.5 * max_lum - 0.5
    if e1 < ks:
        e2 = e1
    else:
        t = (e1 - ks) / (1 - ks)
        e2 = (2 * t**3 - 3 * t**2 + 1) * ks + (t**3 - 2 * t**2 + t) * (1 - ks) + (-2 * t**3 + 3 * t**2) * max_lum
    return e2 * lw


def bt709_oetf(l):
    return 4.5 * l if l < 0.018 else 1.099 * l**0.45 - 0.099


def tone_reference(y10, peak):
    """Grey 10-bit limited PQ luma to 8-bit limited BT.709 SDR luma."""
    e = (y10 - 64) / 876
    linear = min(pq_eotf(eetf(e, peak, 100)) / 100, 1.0)
    return 16 + 219 * bt709_oetf(linear)


def capture(*args):
    done = subprocess.run(args, capture_output=True)
    if done.returncode != 0:
        raise AssertionError(f"{args[0]} failed: {done.stderr.decode()[-2000:]}")
    return done.stdout


def probe_json(*args):
    return json.loads(capture("ffprobe", "-v", "error", "-of", "json", *args))


def frames(path):
    """Sample presentation times in stream ticks, after checking that ffmpeg
    decodes exactly one frame per sample."""
    packets = probe_json("-select_streams", "v:0", "-show_entries", "packet=pts", str(path))["packets"]
    samples = sorted(p["pts"] for p in packets)
    decoded = [f["pts"] for f in probe_json("-select_streams", "v:0", "-show_entries", "frame=pts", str(path))["frames"]]
    assert decoded == samples, f"{path}: decoded frames do not match the samples"
    return samples


def raw(path, width, height, extra=()):
    data = capture("ffmpeg", "-v", "error", "-i", str(path), *extra, "-f", "rawvideo", "-pix_fmt", "yuv420p", "-")
    size = width * height * 3 // 2
    assert len(data) % size == 0, f"{path}: {len(data)} bytes is not whole {width}x{height} frames"
    return np.frombuffer(data, dtype=np.uint8).reshape(-1, size)


def rebuild(fixtures, index, out):
    """Stored objects (flattened as a__b) back into their key layout."""
    for item in (fixtures / f"out-{index}").iterdir():
        target = out / item.name.replace("__", "/")
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(item, target)


def concat(directory):
    segments = sorted((p for p in directory.glob("*.m4s") if not p.name.startswith("iframe-")), key=lambda p: int(p.stem))
    target = directory / "all.mp4"
    with target.open("wb") as out:
        out.write((directory / "init.mp4").read_bytes())
        for segment in segments:
            out.write(segment.read_bytes())
    return target, len(segments)


def source_video(path):
    info = probe_json("-select_streams", "v:0", "-show_entries", "packet=pts,duration:stream=width,height,time_base:stream_side_data=rotation", str(path))
    stream = info["streams"][0]
    rotation = 0
    for side in stream.get("side_data_list", []):
        rotation = side.get("rotation", rotation)
    pts = sorted(p["pts"] for p in info["packets"])
    width, height = stream["width"], stream["height"]
    if abs(rotation) in (90, 270):
        width, height = height, width
    last = max(info["packets"], key=lambda p: p["pts"])
    num, den = map(int, stream["time_base"].split("/"))
    seconds = (last["pts"] + last["duration"] - pts[0]) * num / den
    return [t - pts[0] for t in pts], width, height, seconds


def check_case(work, index, case, report):
    fixtures = work / "fixtures"
    source = fixtures / f"case-{index}.mp4"
    out = work / "objects" / f"case-{index}"
    shutil.rmtree(out, ignore_errors=True)
    rebuild(fixtures, index, out)
    result = {"kind": case["kind"], "renditions": []}
    report.append(result)

    source_pts, display_w, display_h, source_seconds = source_video(source)
    has_audio = bool(probe_json("-select_streams", "a", "-show_entries", "stream=index", str(source))["streams"])
    rungs = case["rungs"]

    # 1. Manifests parse and list every rendition.
    master = probe_json("-show_entries", "stream=codec_name,width,height", str(out / "master.m3u8"))
    videos = [s for s in master["streams"] if s.get("codec_name") == "av1"]
    assert len(videos) == len(rungs), f"master.m3u8 lists {len(videos)} video streams, want {len(rungs)}"
    dash = probe_json("-show_entries", "stream=codec_name,width,height", str(out / "manifest.mpd"))
    dash_videos = [s for s in dash["streams"] if s.get("codec_name") == "av1"]
    assert len(dash_videos) == len(rungs), f"manifest.mpd lists {len(dash_videos)} video streams, want {len(rungs)}"
    if has_audio:
        assert any(s.get("codec_name") == "opus" for s in master["streams"]), "master.m3u8 has no opus stream"
        assert any(s.get("codec_name") == "opus" for s in dash["streams"]), "manifest.mpd has no opus stream"
    result["manifests"] = "ok"

    for r, height in enumerate(rungs, start=1):
        directory = out / f"r{r}"
        width = (height * display_w + display_h) // (2 * display_h) * 2
        whole, segments = concat(directory)
        # 2. Frame-exact: count and presentation times equal the source's.
        got = frames(whole)
        pts = [p - got[0] for p in got]
        assert len(got) == len(source_pts), f"r{r}: {len(got)} frames, source has {len(source_pts)}"
        assert pts == source_pts, f"r{r}: presentation times differ from the source"
        listed = sum(float(line[8:].rstrip(",")) for line in (directory / "media.m3u8").read_text().splitlines() if line.startswith("#EXTINF:"))
        assert abs(listed - source_seconds) < 1e-6, f"r{r}: playlist lasts {listed} s, source {source_seconds} s"
        hls_count = len(probe_json("-select_streams", "v:0", "-show_entries", "frame=pts", str(directory / "media.m3u8"))["frames"])
        assert hls_count == len(source_pts), f"r{r}: media.m3u8 decodes {hls_count} frames"
        iframes = len(probe_json("-select_streams", "v:0", "-show_entries", "frame=pts", str(directory / "iframe.m3u8"))["frames"])
        assert iframes == segments, f"r{r}: iframe.m3u8 decodes {iframes} frames for {segments} segments"
        entry = {"rung": r, "size": f"{width}x{height}", "frames": len(got), "segments": segments}
        decoded = raw(whole, width, height)
        if case["kind"] in ("sdr", "bench"):
            # 3. PSNR against the autorotated, area-scaled source.
            reference = raw(source, width, height, ("-vf", f"scale={width}:{height}:flags=area"))
            luma = width * height
            a = decoded[:, :luma].astype(np.float64)
            b = reference[:, :luma].astype(np.float64)
            mse = ((a - b) ** 2).mean(axis=1)
            psnr = 10 * np.log10(255.0**2 / np.maximum(mse, 1e-10))
            entry["psnr_min"] = round(float(psnr.min()), 2)
            entry["psnr_mean"] = round(float(psnr.mean()), 2)
            assert psnr.min() >= PSNR_FLOOR, f"r{r}: frame PSNR {psnr.min():.2f} dB is under the {PSNR_FLOOR} dB floor"
        if case["kind"] == "hdr" and r == 1:
            # 4. Tone curve at each bar centre.
            src = capture("ffmpeg", "-v", "error", "-i", str(source), "-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-")
            source_luma = np.frombuffer(src[: 2 * width * height], dtype="<u2").reshape(height, width)
            frame = decoded[0]
            luma = frame[: width * height].reshape(height, width)
            u = frame[width * height: width * height * 5 // 4].reshape(height // 2, width // 2)
            v = frame[width * height * 5 // 4:].reshape(height // 2, width // 2)
            bar = width // len(case["bars"])
            curve = []
            for k in range(len(case["bars"])):
                x0, x1 = k * bar + bar // 4, (k + 1) * bar - bar // 4
                y10 = float(source_luma[height // 4: 3 * height // 4, x0:x1].mean())
                want = tone_reference(y10, case["peak"])
                have = float(luma[height // 4: 3 * height // 4, x0:x1].mean())
                curve.append({"nits": case["bars"][k], "source": round(y10, 1), "want": round(want, 2), "have": round(have, 2)})
                assert abs(have - want) <= TONE_TOLERANCE, f"bar {case['bars'][k]} cd/m²: luma {have:.2f}, reference {want:.2f}"
                for plane in (u, v):
                    c = float(plane[height // 8: 3 * height // 8, x0 // 2: x1 // 2].mean())
                    assert abs(c - 128) <= TONE_TOLERANCE, f"bar {case['bars'][k]} cd/m²: chroma {c:.2f} is not neutral"
            result["tone"] = curve
        result["renditions"].append(entry)

    # 5. Audio passthrough is byte-exact.
    if has_audio:
        directory = out / f"r{len(rungs) + 1}"
        whole, _ = concat(directory)
        def packets(path):
            data = capture("ffmpeg", "-v", "error", "-i", str(path), "-map", "0:a", "-c", "copy", "-f", "framemd5", "-")
            # Column 6 hashes the packet payload; timestamps and the edit
            # list's skip side data are container framing, not payload.
            return [line.split(",")[5].strip() for line in data.decode().splitlines() if not line.startswith("#")]
        want, have = packets(source), packets(whole)
        assert want == have, f"audio: {len(have)} packets differ from the source's {len(want)}"
        result["audio_packets"] = len(have)


def main():
    work = Path(sys.argv[1]).resolve()
    cases = json.loads((work / "cases.json").read_text())
    report = []
    failed = []
    for index, case in enumerate(cases):
        try:
            check_case(work, index, case, report)
        except AssertionError as problem:
            failed.append(f"case {index} ({case['kind']}): {problem}")
        summary = struct.unpack("<4Q", (work / "fixtures" / f"case-{index}.summary").read_bytes())
        report[-1]["summary"] = dict(zip(("jobs", "frames", "output_bytes", "elapsed_ns"), summary))
    (work / "results.json").write_text(json.dumps(report, indent=1))
    print(json.dumps(report, indent=1))
    if failed:
        print("\n".join(failed), file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
