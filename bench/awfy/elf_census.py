"""File bytes and symbol ownership for paired release ELF images."""
from collections import Counter
import json
from pathlib import Path
import subprocess
import tempfile

from build import sha256

RELOCATIONS = {"SHT_REL", "SHT_RELA", "SHT_RELR"}
UNWIND = (".eh_frame", ".gcc_except_table", ".debug_frame", ".ARM.exidx", ".ARM.extab")


def covered_bytes(ranges):
    end = total = 0
    for first, last in sorted(ranges):
        if last > end:
            total += last - max(first, end)
            end = last
    return total


def inspect(path, symbols=False):
    command = ["llvm-readelf", "--elf-output-style=JSON", "--sections", "--relocations"]
    if symbols:
        command.append("--symbols")
    elf, = json.loads(subprocess.check_output([*command, path], text=True))
    indexed = {}
    ranges = []
    for item in elf["Sections"]:
        section = item["Section"]
        if section["Index"] == 0:
            continue
        kind, name = section["Type"]["Name"], section["Name"]["Name"]
        size, offset = section["Size"], section["Offset"]
        size_on_disk = 0 if kind == "SHT_NOBITS" else size
        indexed[section["Index"]] = {
            "name": name, "type": kind, "address": section["Address"], "offset": offset,
            "size": size, "file_bytes": size_on_disk,
        }
        if size_on_disk:
            ranges.append((offset, offset + size_on_disk))

    file_size = path.stat().st_size
    if any(end > file_size for _, end in ranges):
        raise ValueError(f"{path}: ELF section extends past the file")
    section_file_bytes = covered_bytes(ranges)
    relocation_bytes = sum(section["file_bytes"] for section in indexed.values() if section["type"] in RELOCATIONS)
    relocations = Counter(
        record["Relocation"]["Type"]["Name"]
        for group in elf["Relocations"] for record in group["Relocs"]
    )
    unwind = {
        section["name"]: section["file_bytes"]
        for section in indexed.values() if section["name"].startswith(UNWIND)
    }
    owners = []
    for item in elf.get("Symbols", []):
        symbol = item["Symbol"]
        section = indexed.get(symbol["Section"]["Value"])
        if section is None or not symbol["Size"]:
            continue
        owners.append({
            "name": symbol["Name"]["Name"], "type": symbol["Type"]["Name"], "section": section["name"],
            "value": symbol["Value"], "size": symbol["Size"],
        })
    return {
        "file_bytes": file_size, "section_file_bytes": section_file_bytes,
        "headers_and_padding_bytes": file_size - section_file_bytes,
        "sha256": sha256(path), "sections": list(indexed.values()), "relocation_bytes": relocation_bytes,
        "relocations": relocations, "unwind_sections": unwind, "unwind_bytes": sum(unwind.values()),
        "symbol_owners": owners,
    }


def strip_image(source, target):
    subprocess.run(["strip", "--strip-all", "-R", ".comment", "-o", target, source], check=True)


def image(path):
    full = inspect(path, symbols=True)
    with tempfile.TemporaryDirectory(prefix="hirsel-elf-") as scratch:
        stripped = Path(scratch) / "stripped"
        strip_image(path, stripped)
        shipping = inspect(stripped)
    return {"path": str(path.resolve()), "unstripped": full, "stripped": shipping}


def census(before, after):
    left, right = image(before), image(after)
    deltas = {}
    for view in ("unstripped", "stripped"):
        baseline, candidate = left[view], right[view]
        before_sections = Counter()
        after_sections = Counter()
        for sections, data in ((before_sections, baseline), (after_sections, candidate)):
            for section in data["sections"]:
                sections[section["name"]] += section["file_bytes"]
        deltas[view] = {
            key: candidate[key] - baseline[key] for key in
            ("file_bytes", "section_file_bytes", "headers_and_padding_bytes", "relocation_bytes", "unwind_bytes")
        } | {"sections": {name: after_sections[name] - before_sections[name]
                           for name in sorted(before_sections.keys() | after_sections.keys())}}
    return {"baseline": left, "candidate": right, "delta": deltas}
