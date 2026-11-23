"""Attribute the cycle profiles written by run.py profile."""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import subprocess

SAMPLE = re.compile(r"^\s*(\S+)\s+([0-9]+) cycles:\s*$")
FRAME = re.compile(r"^\s+[0-9a-f]+\s+(.*?)\s+\((.*?)(?:\+0x([0-9a-f]+))?\)$")
PROTOCOL = re.compile(r"^std@\d+/(?:http|http1|http_client|object|sigv4)/")
CRC = re.compile(r"crc32c|crc32_[a-z]|crc32::", re.I)
RUNTIME = re.compile(r"hirsel_core_(?:rt::(?:tasks|os_net|net|process|channels)::|net::reactor::)")
TCP = re.compile(r"^(?:_*(?:tcp|skb|inet|netif|ip|napi|gro)_|"
                 r"_*(?:dev_queue_xmit|sock_(?:send|recv)msg|sys_(?:send|recv))|"
                 r"(?:__)?copy_(?:to|from)_iter|copy_user_)", re.I)
REACTOR = re.compile(r"epoll|_epoll_|io_uring|io_(?:poll|submit|cq|sq)|eventfd", re.I)
BUCKETS = ("kernel_tcp_copy", "protocol", "crc32c", "scheduler_reactor", "other")


def perf_samples(path):
    process = subprocess.run([
        "perf", "script", "-i", path, "--show-lost-events",
        "-F", "misc,period,event,ip,sym,dso,dsoff",
    ], text=True, capture_output=True, check=True)
    samples = []
    for block in process.stdout.strip().split("\n\n"):
        head, *stack = block.splitlines()
        if "PERF_RECORD_LOST" in block:
            raise ValueError(f"{path}: perf lost samples")
        if not (sample := SAMPLE.fullmatch(head)) or not stack:
            raise ValueError(f"{path}: unexpected perf sample {head!r}")
        frames = []
        for line in stack:
            if not (frame := FRAME.fullmatch(line)):
                raise ValueError(f"{path}: unexpected perf frame {line!r}")
            name, dso, offset = frame.groups()
            frames.append((name, dso, int(offset, 16) if offset is not None else None))
        mode, period = sample.groups()
        if mode not in ("U", "K"):
            raise ValueError(f"{path}: unexpected perf CPU mode {mode!r}")
        samples.append((mode, int(period), frames))
    return samples


def debug_info(binary):
    elf = subprocess.check_output(["readelf", "-lWn", binary], text=True)
    segments = []
    for line in elf.splitlines():
        words = line.split()
        if words[:1] == ["LOAD"] and "E" in words[6:-1]:
            offset, vaddr, _, size = (int(word, 16) for word in words[1:5])
            segments.append((offset, offset + size, vaddr - offset))
    if match := re.search(r"Build ID: ([0-9a-f]+)", elf):
        build_id = match[1]
        debug = binary.parent / ".build-id" / build_id[:2] / (build_id[2:] + ".debug")
    else:
        image = binary.read_bytes()
        fmt = b"hirsel.text.v1"
        digest = hashlib.sha256()
        for part in (fmt, image):
            digest.update(len(part).to_bytes(8, "little"))
            digest.update(part)
        debug = binary.parent / ".hirsel-debug" / (digest.hexdigest() + ".debug")
    if not segments or not debug.is_file():
        raise FileNotFoundError(f"missing executable segment or detached DWARF at {debug}")
    return segments, debug


def symbol_names(binary, samples, symbolizer, segments, debug):
    addresses = {}
    for mode, _, frames in samples:
        if mode != "U":
            continue
        for _, dso, offset in frames:
            if dso == str(binary) and offset is not None and offset not in addresses:
                for first, end, delta in segments:
                    if first <= offset < end:
                        addresses[offset] = offset + delta
                        break
    output = subprocess.run([
        symbolizer, f"--obj={debug}", "--functions=short", "--inlines", "--output-style=JSON",
    ], input="".join(f"0x{addr:x}\n" for addr in addresses.values()),
        text=True, capture_output=True, check=True).stdout
    rows = [json.loads(line) for line in output.splitlines()]
    if len(rows) != len(addresses):
        raise ValueError("llvm-symbolizer returned an incomplete address map")
    return {
        offset: [frame["FunctionName"] for frame in row["Symbol"]
                 if frame["FunctionName"] not in ("", "??")]
        for offset, row in zip(addresses, rows)
    }


def attribute(binary, samples, names):
    buckets = Counter()
    user = Counter()
    protocol = Counter()
    unknown = Counter()
    for mode, period, frames in samples:
        if mode == "K":
            stack = [name for name, dso, _ in frames
                     if dso.startswith("[") and dso != "[vdso]"]
            bucket = "other"
            if any(TCP.search(name) for name in stack):
                bucket = "kernel_tcp_copy"
            elif any(REACTOR.search(name) for name in stack):
                bucket = "scheduler_reactor"
            if frames[0][0] == "[unknown]":
                unknown["kernel"] += period
        else:
            stack = [symbol for name, dso, offset in frames
                     for symbol in ((names.get(offset) or [name]) if dso == str(binary) else [name])]
            leaf = stack[0]
            user[leaf] += period
            bucket = "other"
            for name in stack:
                if CRC.search(name):
                    bucket = "crc32c"
                    break
                if RUNTIME.search(name):
                    bucket = "scheduler_reactor"
                    break
                if PROTOCOL.search(name):
                    bucket = "protocol"
                    protocol[name] += period
                    break
            if leaf == "[unknown]" or re.match(r"^(?:h[fs]?[0-9a-f]{64}|__hirsel_debug_)", leaf):
                unknown["user"] += period
        buckets[bucket] += period
    return buckets, user, protocol, unknown


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("runs", type=Path, help="the JSONL output of run.py profile")
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--symbolizer", default="llvm-symbolizer")
    args = parser.parse_args()
    binary = args.binary.resolve()
    segments, debug = debug_info(binary)
    totals = {}
    for line in args.runs.read_text().splitlines():
        row = json.loads(line)
        samples = perf_samples(row["profile"])
        if {mode for mode, _, _ in samples} != {"U", "K"}:
            raise ValueError(f"{row['profile']}: expected both user and kernel samples")
        names = symbol_names(binary, samples, args.symbolizer, segments, debug)
        counters = attribute(binary, samples, names)
        key = (row["reactor"], row["method"])
        cell = totals.setdefault(key, dict(runs=[], counts=tuple(Counter() for _ in counters)))
        cell["runs"].append(dict(run=row["run"], samples=len(samples), buckets=dict(counters[0])))
        for total, run in zip(cell["counts"], counters):
            total.update(run)
    if not totals:
        raise ValueError("no profiles to attribute")
    for (reactor, method), cell in sorted(totals.items()):
        buckets, user, protocol, unknown = cell["counts"]
        cycles = sum(buckets.values())
        print(json.dumps(dict(reactor=reactor, method=method, cycles=cycles, runs=cell["runs"],
                              percent={key: 100 * buckets[key] / cycles for key in BUCKETS},
                              unknown_percent={key: 100 * unknown[key] / cycles for key in ("user", "kernel")},
                              top_user=user.most_common(10), top_protocol=protocol.most_common(10))))


if __name__ == "__main__":
    main()
