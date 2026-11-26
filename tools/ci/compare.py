#!/usr/bin/env python3
"""Compare the full Mach-O contents of each Linux and Mac corpus cache."""

import argparse
from collections import Counter
import hashlib
from pathlib import Path
import struct

MAGIC = 0xFEEDFACF
LC = {0x19: 'LC_SEGMENT_64', 0x1b: 'LC_UUID', 0x1d: 'LC_CODE_SIGNATURE', 0x2: 'LC_SYMTAB',
      0xb: 'LC_DYSYMTAB', 0xe: 'LC_LOAD_DYLINKER', 0xc: 'LC_LOAD_DYLIB', 0x80000028: 'LC_MAIN',
      0x32: 'LC_BUILD_VERSION', 0x2a: 'LC_SOURCE_VERSION', 0x26: 'LC_FUNCTION_STARTS',
      0x29: 'LC_DATA_IN_CODE', 0x80000033: 'LC_DYLD_EXPORTS_TRIE', 0x80000034: 'LC_DYLD_CHAINED_FIXUPS'}


def macho(path):
    with path.open("rb") as stream:
        if stream.read(4) != struct.pack("<I", MAGIC):
            return None
        data = stream.read()
    data = struct.pack("<I", MAGIC) + data
    if len(data) < 32 or struct.unpack_from('<I', data)[0] != MAGIC:
        return None
    return data


def regions(data):
    """(start, end, owner) for header, load commands, sections, and linkedit blobs."""
    out = [(0, 32, 'header')]
    ncmds, = struct.unpack_from('<I', data, 16)
    at = 32
    for _ in range(ncmds):
        cmd, size = struct.unpack_from('<II', data, at)
        name = LC.get(cmd, hex(cmd))
        out.append((at, at + size, 'load command ' + name))
        if cmd == 0x19:
            seg = data[at + 8:at + 24].rstrip(b'\0').decode()
            nsects, = struct.unpack_from('<I', data, at + 64)
            for i in range(nsects):
                s = at + 72 + i * 80
                sect = data[s:s + 16].rstrip(b'\0').decode()
                ssize, off = struct.unpack_from('<QI', data, s + 40)
                if off:
                    out.append((off, off + ssize, f'section {seg},{sect}'))
        elif cmd in (0x1d, 0x26, 0x29, 0x80000033, 0x80000034):
            off, sz = struct.unpack_from('<II', data, at + 8)
            out.append((off, off + sz, name + ' data'))
        elif cmd == 0x2:
            symoff, nsyms, stroff, strsize = struct.unpack_from('<IIII', data, at + 8)
            out.append((symoff, symoff + nsyms * 16, 'symbol table'))
            out.append((stroff, stroff + strsize, 'string table'))
        at += size
    return out


def attribute(a, b):
    if len(a) != len(b):
        why = f'size {len(a)} vs {len(b)}'
    else:
        why = 'same size'
    owners = []
    for start, end, owner in sorted(regions(a)):
        if a[start:end] != b[start:end] and owner not in owners:
            owners.append(owner)
    return why, owners



def side(root):
    objects, executables = Counter(), {}
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        data = macho(path)
        if data is None:
            continue
        _, cpu, _, kind = struct.unpack_from("<IiiI", data)
        if cpu != 0x0100000C:
            raise ValueError(f"{path}: expected arm64 Mach-O")
        digest = hashlib.sha256(data).hexdigest()
        if kind == 2:
            executables.setdefault(digest, []).append(path)
        else:
            objects[digest] += 1
    if not objects or not executables:
        raise ValueError(f"{root}: need both Mach-O objects and executables")
    return objects, executables


def compare(linux, mac):
    lo, le = side(linux)
    mo, me = side(mac)
    lex = Counter({digest: len(paths) for digest, paths in le.items()})
    mex = Counter({digest: len(paths) for digest, paths in me.items()})
    print(f"{linux.name}: objects {sum((lo & mo).values())}/"
          f"{sum(lo.values())} Linux and {sum(mo.values())} Mac identical; "
          f"executables {sum((lex & mex).values())}/{sum(lex.values())} Linux "
          f"and {sum(mex.values())} Mac identical")
    linux_only = [le[digest][0] for digest in lex - mex]
    mac_only = [me[digest][0] for digest in mex - lex]
    if len(linux_only) == len(mac_only) == 1:
        why, owners = attribute(linux_only[0].read_bytes(), mac_only[0].read_bytes())
        print(f"Executable difference: {why}, {owners}")
    return lo == mo and lex == mex


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("linux", type=Path)
    parser.add_argument("mac", type=Path)
    args = parser.parse_args()
    left = {path.name for path in args.linux.iterdir() if path.is_dir()}
    right = {path.name for path in args.mac.iterdir() if path.is_dir()}
    if not left or left != right:
        raise SystemExit(f"different or empty corpus: Linux {sorted(left)}, Mac {sorted(right)}")
    results = [compare(args.linux / name, args.mac / name) for name in sorted(left)]
    return not all(results)


if __name__ == "__main__":
    raise SystemExit(main())
