"""Per-address Callgrind self costs and DWARF source locations for a runtime receipt."""
from collections import Counter, defaultdict
import json
import os
from pathlib import Path
import re
import subprocess

import build


def capture(row, out):
    with (out / "stdout").open("wb") as stdout, (out / "stderr").open("wb") as stderr:
        subprocess.run(
            [build.ROOT / "tools/capped", "valgrind", "--tool=callgrind", "--dump-instr=yes",
             "--compress-strings=no", "--compress-pos=no", f"--callgrind-out-file={out / 'callgrind.out'}",
             *row["argv"]],
            cwd=row["cwd"], env=build.capped_env(out), stdout=stdout, stderr=stderr, check=True,
        )
    if (out / "stdout").read_bytes() != row["stdout"].encode():
        raise ValueError(f"Callgrind run did not reproduce the checked stdout: {out}")


def self_counts(path):
    """Read the uncompressed instr/line Ir format requested by capture."""
    costs = Counter()
    obj = symbol = file = actual = ""
    association = False
    positions = events = totals = None
    with path.open() as stream:
        for record in stream:
            line = record.strip()
            if line.startswith("positions:"):
                positions = line.split()[1:]
            elif line.startswith("events:"):
                events = line.split()[1:]
            elif line.startswith("totals:"):
                totals = int(line.split()[1])
            elif line.startswith("ob="):
                obj = line[3:]
            elif line.startswith("fl="):
                file = actual = line[3:]
            elif line.startswith(("fi=", "fe=")):
                actual = line[3:]
            elif line.startswith("fn="):
                symbol, actual = line[3:], file
            elif line.startswith(("calls=", "jump=", "jcnd=")):
                association = True
            elif line and line[0].isdigit():
                pc, source_line, *counts = line.split()
                if not association and counts:
                    count, = map(int, counts)
                    if count:
                        costs[obj, symbol, int(pc, 0), actual, int(source_line)] += count
                association = False
    if positions != ["instr", "line"] or events != ["Ir"] or totals != costs.total() or not costs:
        raise ValueError(f"{path}: expected a complete uncompressed Callgrind instruction profile")
    return costs


def disassembly(binary, addresses):
    result = subprocess.run(["objdump", "-d", "--no-show-raw-insn", binary],
                            capture_output=True, text=True, check=True)
    base = None
    output = {}
    for line in result.stdout.splitlines():
        if match := re.fullmatch(r"([a-f0-9]+) <(.+)>:", line):
            base, symbol = int(match[1], 16), match[2]
        elif base is not None and (match := re.fullmatch(r"\s*([a-f0-9]+):\s+(.*)", line)):
            pc = int(match[1], 16)
            if pc in addresses:
                output[pc] = (symbol, pc - base, match[2])
    return output


def source_locations(binary, addresses, *, short_names=False):
    llvm = build.llvm_path()
    symbolizer = llvm / "bin/llvm-symbolizer"
    env = os.environ.copy()
    if symbolizer.is_file():
        lib = str(llvm / "lib")
        if prior := env.get("LD_LIBRARY_PATH"):
            lib += os.pathsep + prior
        env["LD_LIBRARY_PATH"] = lib
    else:
        symbolizer = "llvm-symbolizer"
    addresses = sorted(addresses)
    names = "short" if short_names else "linkage"
    result = subprocess.run(
        [symbolizer, "--obj", binary, "--inlines", "--no-debuginfod", "--output-style=JSON", f"--functions={names}"],
        text=True, capture_output=True,
        input="".join(f"{pc:#x}\n" for pc in addresses), env=env, check=True,
    )
    return {pc: [
        {"symbol": frame["FunctionName"], "file": frame["FileName"], "line": frame["Line"]}
        for frame in json.loads(line)["Symbol"] if frame["Line"] > 0 and frame["FileName"] not in ("", "??")
    ] for pc, line in zip(addresses, result.stdout.splitlines(), strict=True)}


def mapped_counts(out, executable):
    counts = self_counts(out / "callgrind.out")
    objects = defaultdict(set)
    for obj, _, pc, _, _ in counts:
        objects[obj].add(pc)
    images = {}
    for obj, addresses in objects.items():
        path = Path(obj)
        if path.is_file():
            images[obj] = (disassembly(path, addresses), source_locations(path, addresses))
    result = []
    for (obj, symbol, pc, file, line), count in counts.items():
        instructions, locations = images.get(obj, ({}, {}))
        emitted_symbol, offset, instruction = instructions.get(pc, (symbol, None, None))
        frames = locations.get(pc, [])
        if not frames and file not in ("", "???") and line > 0:
            frames = [{"symbol": symbol, "file": file, "line": line}]
        result.append({
            "object": "executable" if Path(obj).resolve() == executable else obj,
            "symbol": symbol if symbol not in ("", "???") else emitted_symbol, "pc": f"{pc:#x}",
            "emitted_symbol": emitted_symbol, "offset": offset, "instruction": instruction,
            "source": frames, "self_instructions": count,
        })
    return result


def totals(rows, key):
    result = Counter()
    for row in rows:
        result[key(row)] += row["self_instructions"]
    return result


def show_deltas(left, right, limit):
    for key in sorted(left.keys() | right.keys(), key=lambda key: (-abs(right[key] - left[key]), key))[:limit]:
        delta = right[key] - left[key]
        if delta:
            print(f"{delta:+,}  {left[key]:,} -> {right[key]:,}  {key}")


def summarize(before, after):
    base = sum(row["self_instructions"] for row in before)
    candidate = sum(row["self_instructions"] for row in after)
    print(f"Callgrind Ir {base:,} -> {candidate:,} ({candidate - base:+,})")
    print("Largest self-count changes by symbol")
    def symbol(row):
        return f"{row['object']} {row['symbol']}"

    show_deltas(totals(before, symbol), totals(after, symbol), 12)

    def address(row):
        frames = row["source"]
        offset = f"+{row['offset']:#x}" if row["offset"] is not None else f"@{row['pc']}"
        source = f" {frames[0]['file']}:{frames[0]['line']}" if frames else ""
        return f"{row['object']} {row['emitted_symbol']}{offset}{source} {row['instruction'] or ''}".rstrip()

    print("Largest self-count changes by address")
    show_deltas(totals(before, address), totals(after, address), 20)
