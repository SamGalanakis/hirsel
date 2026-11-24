"""Sample release executables and resolve their sampled addresses offline."""
from collections import Counter
from decimal import Decimal
import hashlib
import os
from pathlib import Path
import re
from statistics import median
import struct
import subprocess

import build
from bench_cpus import positive_int, sibling_cpus
from profile_diff import source_locations
from timing_shield import hold, measurement_affinity

EVENTS = ("cycles:u", "instructions:u")
FRAME = re.compile(r"\s*[\da-fA-F]+\s+(.*?) \((.*?)(?:\+0x([\da-fA-F]+))?\)\s*")


def notes(path):
    return subprocess.check_output(["readelf", "-n", path], text=True)


def build_id(note):
    match = re.search(r"Build ID: ([0-9a-f]+)", note)
    return match[1] if match else None


def detached_debug(image, out):
    image_id = build_id(notes(image))
    if image_id:
        relative = Path(".build-id") / image_id[:2] / f"{image_id[2:]}.debug"
        debug = image.parent / relative
        if not debug.is_file():
            debug = out / "image.debug"
            subprocess.run(["objcopy", "--only-keep-debug", image, debug], check=True)
        matches = build_id(notes(debug)) == image_id
    else:
        domain = b"hirsel.text.v1"
        contents = image.read_bytes()
        digest = hashlib.sha256(struct.pack("<Q", len(domain)) + domain
                                + struct.pack("<Q", len(contents)) + contents).hexdigest()
        debug = image.parent / ".hirsel-debug" / f"{digest}.debug"
        note = notes(debug).partition(".note.hirsel.image-digest")[2]
        match = re.search(r"description data: ([\da-f ]+)", note)
        matches = match is not None and match[1].replace(" ", "") == digest
    if not matches:
        raise ValueError(f"{debug}: detached debug image identity differs from {image}")
    sections = subprocess.check_output(["readelf", "-SW", debug], text=True)
    for section in (".debug_info", ".debug_line"):
        match = re.search(rf"\s{re.escape(section)}\s+\S+\s+\S+\s+\S+\s+([0-9a-f]+)", sections)
        if not match or not int(match[1], 16):
            raise ValueError(f"{debug}: {section} is missing or empty for {image}")
    return debug


def rust_marker(suite, marker):
    if suite == "arena":
        return r"start: \d+\n" + marker + r"Summary: \d+\.\d+s, 1, 1, 0\n"
    name = marker.partition(":")[0]
    return (rf"Starting {name} benchmark \.\.\.\n" + marker
            + rf"{name}: iterations=1 average: \d+us total: \d+us\n\nTotal Runtime: \d+us\n")


def capture(row, out, env):
    with (out / "stdout").open("wb") as stdout, (out / "stderr").open("wb") as stderr:
        subprocess.run(
            [build.ROOT / "tools/capped", "perf", "record", "-N", "-e", ",".join(EVENTS), "-F", "997",
             "--call-graph", "dwarf,4096", "-o", out / "perf.data", "--", *row["argv"]],
            cwd=row["cwd"], env=env, stdout=stdout, stderr=stderr, check=True,
        )


def check_output(marker, stdout, side):
    if not re.fullmatch(marker.encode(), stdout):
        raise ValueError(f"{side}: stdout differs from the checked workload")


def parse_samples(lines):
    samples = []
    for line in lines:
        if not line.strip():
            continue
        if match := re.fullmatch(r"\s*(\d+)\s+(\S+):\s*", line):
            period, event = int(match[1]), match[2]
            if period <= 0 or event not in EVENTS:
                raise ValueError(f"unexpected sample event or weight: {line}")
            samples.append({"event": event, "period": period, "stack": []})
        elif samples and (match := FRAME.fullmatch(line)):
            symbol, obj, offset = match.groups()
            samples[-1]["stack"].append({
                "symbol": symbol, "object": obj, "offset": int(offset, 16) if offset else None,
            })
        else:
            raise ValueError(f"unexpected perf record (or lost samples): {line}")
    if {sample["event"] for sample in samples} != set(EVENTS) or any(not s["stack"] for s in samples):
        raise ValueError("expected weighted stacks for both user PMU events")
    return samples


def executable_segments(image):
    headers = subprocess.check_output(["readelf", "-lW", image], text=True)
    segments = []
    for line in headers.splitlines():
        fields = line.split()
        if fields and fields[0] == "LOAD" and "E" in fields[6:-1]:
            offset, address, size = (int(fields[index], 16) for index in (1, 2, 4))
            segments.append((offset, offset + size, address - offset))
    return segments


def mapped_samples(out, image, debug):
    result = subprocess.check_output(
        ["perf", "script", "--no-inline", "--show-lost-events", "-i", out / "perf.data",
         "-F", "event,period,ip,sym,dso,dsoff"],
        text=True,
    )
    samples = parse_samples(result.splitlines())
    segments = executable_segments(image)
    app_frames = []
    for sample in samples:
        for frame in sample["stack"]:
            offset = frame["offset"]
            if offset is None or frame["object"] != str(image):
                continue
            for start, end, bias in segments:
                if start <= offset < end:
                    app_frames.append((frame, offset + bias))
                    break
    if not app_frames:
        raise ValueError(f"{image}: no sampled addresses map to the expected image")
    locations = source_locations(debug, {address for _, address in app_frames}, short_names=True)
    for frame, address in app_frames:
        frame["source"] = locations[address]
    return samples


def summarize(side, samples, top):
    for event in EVENTS:
        leaves = Counter()
        source_weight = 0
        count = 0
        for sample in samples:
            if sample["event"] != event:
                continue
            count += 1
            leaf = sample["stack"][0]
            weight = sample["period"]
            frames = leaf.get("source", [])
            if frames:
                source_weight += weight
            source = " <- ".join(f"{f['symbol']} {f['file']}:{f['line']}".lstrip() for f in frames)
            symbol = frames[-1]["symbol"] if frames and frames[-1]["symbol"] not in ("", "??") else leaf["symbol"]
            leaves[symbol, Path(leaf["object"]).name, source] += weight
        total = leaves.total()
        print(f"{side} {event} sampled self cost ({count:,} samples, "
              f"{source_weight / total:.1%} weight with source lines)")
        print("  Weight %       Period  Function [image] and inline source")
        for (symbol, image, source), weight in leaves.most_common(top):
            print(f"  {weight / total:8.2%} {weight:12,}  {symbol} [{image}] {source}")


def run(args, key, left, rust_command, cycle_samples, write):
    left_image = (Path(left["cwd"]) / left["argv"][0]).resolve()
    left = left | {"marker": re.escape(left["stdout"])}
    right_name = "control" if args.aa else "rust"
    right_label = "Hirsel control" if args.aa else "Rust"
    if args.aa:
        if args.rust_image:
            raise ValueError("--rust-image requires --peer rust")
        right_image = left_image
        right = left
    else:
        command, marker = rust_command
        right_image = (args.rust_image or command[0]).resolve()
        right_digest = build.sha256(right_image)
        if right_digest == left["image_sha256"]:
            raise ValueError("identical images require --aa")
        right = {"argv": [str(right_image), *map(str, command[1:])], "cwd": left["cwd"],
                 "marker": rust_marker(left["suite"], marker.decode()),
                 "image_sha256": right_digest}

    out = args.out.resolve()
    if out.is_relative_to(build.ROOT):
        raise ValueError("use a new evidence directory outside the checkout")
    if args.cpu not in measurement_affinity(args.lock, force=True):
        raise ValueError(f"CPU {args.cpu} is outside the assigned affinity")
    out.mkdir(parents=True)
    comparison = "identical-image control" if args.aa else "peer gap (Hirsel/Rust)"
    arms = []
    for name, label, row, image in (("hirsel", "Hirsel", left, left_image),
                                    (right_name, right_label, right, right_image)):
        directory = out / name
        directory.mkdir()
        arms.append((label, row, image, directory, detached_debug(image, directory)))
    siblings = sibling_cpus((args.cpu,))
    affinity = os.sched_getaffinity(0)
    args.max_attempts = args.pairs
    args.sibling_busy_threshold = None
    env = build.capped_env(out)
    pairs = []
    with hold(args.lock, force=True):
        try:
            os.sched_setaffinity(0, {args.cpu})
            kept, _ = cycle_samples(left, right, args, siblings, env, pairs, user_counters=True)
            for _, row, _, directory, _ in arms:
                capture(row, directory, env)
        finally:
            os.sched_setaffinity(0, affinity)
    for label, row, image, directory, _ in arms:
        if build.sha256(image) != row["image_sha256"]:
            raise ValueError(f"{label}: image changed during measurement")
        check_output(row["marker"], (directory / "stdout").read_bytes(), label)
    for path, digest in left["fixture_files"].items():
        if build.sha256(Path(path)) != digest:
            raise ValueError(f"fixture changed during measurement: {path}")
    if args.aa and any(abs(a["instructions"] - b["instructions"]) * 10_000 > a["instructions"]
                       for a, b in kept):
        raise ValueError("identical-image instruction counts differ by more than 0.01%")

    summary = {}
    for event in ("cycles", "seconds", "instructions"):
        a, b = ([Decimal(sample[event]) for sample in group] for group in zip(*kept))
        if min(*a, *b) <= 0:
            raise ValueError(f"expected positive paired {event}")
        summary[event] = (median(a), median(b), median(x / y for x, y in zip(a, b)))
    print(f"{key} {comparison}: {len(kept)} interleaved pairs on CPU {args.cpu}")
    print(f"  {'Metric':<15} {'Hirsel':>18} {right_label:>18} {'Median pair ratio':>18}")
    for event, (a, b, ratio) in summary.items():
        print(f"  {event:<15} {a:>18,} {b:>18,} {ratio:>17.6f}x")
    for label, _, image, directory, debug in arms:
        samples = mapped_samples(directory, image, debug)
        summarize(label, samples, args.top)
        write(directory / "samples.json", samples)
    write(out / "pairs.json", {"row": key, "comparison": comparison, "cpu": args.cpu,
                               "ratio": f"Hirsel/{right_label}", "summary": summary, "pairs": pairs})
    print(f"Profiles and uninstrumented pairs: {out}")
    return 0


def add_parser(commands):
    peers = commands.add_parser("profile-peer", help="measure and profile one checked runtime row against Rust")
    peers.add_argument("row", metavar="SUITE/ROW")
    peers.add_argument("baseline", type=Path, help="directory containing the checked receipt or board")
    peers.add_argument("out", type=Path)
    peer = peers.add_mutually_exclusive_group(required=True)
    peer.add_argument("--peer", choices=("rust",))
    peer.add_argument("--aa", action="store_true", help="measure and profile the same Hirsel image twice")
    peers.add_argument("--rust-image", type=Path,
                       help="release Rust image with matching detached or embedded DWARF (default: build sibling)")
    peers.add_argument("--upstream", type=lambda path: Path(path).resolve(), help="pinned AWFY checkout")
    peers.add_argument("--cpu", type=int, choices=range(16, 20), default=16)
    peers.add_argument("--lock", type=Path, required=True)
    peers.add_argument("--pairs", type=positive_int, default=60, help="interleaved repetitions (default: 60)")
    peers.add_argument("--top", type=positive_int, default=12, help="source rows per event and image")
