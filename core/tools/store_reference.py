#!/usr/bin/env python3
"""Independent STORE-SPEC 1.7/2.1/2.4 vectors. No store/compiler code is imported."""
import ctypes
import ctypes.util
import json
from pathlib import Path
import struct
import sys

MASK = (1 << 64) - 1


def splitmix(x):
    z = (x + 0x9E3779B97F4A7C15) & MASK
    z = ((z ^ (z >> 30)) * 0xBF58476D1CE4E5B9) & MASK
    z = ((z ^ (z >> 27)) * 0x94D049BB133111EB) & MASK
    return z ^ (z >> 31)


def seed(hi, lo, branch, version):
    return splitmix(splitmix(splitmix(splitmix(hi) ^ lo) ^ branch) ^ version)


def reference():
    lib = ctypes.CDLL(ctypes.util.find_library('xxhash'))
    lib.XXH3_64bits.argtypes = [ctypes.c_void_p, ctypes.c_size_t]
    lib.XXH3_64bits.restype = ctypes.c_uint64
    assert lib.XXH3_64bits(b'', 0) == 0x2D06800538D394C2
    # Identity hi, lo; branch; version; time; segment; byte offset.
    contexts = [(1, 2, 0, 257, 999, 9, offset) for offset in (72, 73, 1048576)]
    contexts += [(3, 4, 7, 258, 1001, 10, 73), (1, 9, 1, 65536, 0, 256, 1048576)]
    key = struct.pack('>IQ', 1, 7)
    value = b'\0\0' + bytes(range(32))
    frames = []
    for hi, lo, branch, version, at, seq, offset in contexts:
        payload = struct.pack('<IQQBBIBI', branch, version, at, 1, 0, 1, 1, len(key))
        payload += key + struct.pack('<II', 0, len(value)) + value
        assert len(payload) == 85
        head = struct.pack('<HHI', 0x11, 1, len(payload))
        binding = struct.pack('<QQBQQ', hi, lo, 2, seq, offset)
        covered = binding + head + payload
        check = lib.XXH3_64bits(covered, len(covered))
        frame = head + struct.pack('<Q', check) + payload
        assert len(frame) == 101
        frames.append(list(frame))
    versions = [1, 256, 257, 258]
    return dict(contexts=contexts, frames=frames, versions=versions,
                seeds=[seed(1, 2, 0, v) for v in versions],
                other_seeds=[seed(9, 2, 0, 1), seed(1, 9, 0, 1), seed(1, 2, 1, 1)])


if __name__ == '__main__':
    result = json.dumps(reference(), indent=2) + '\n'
    if len(sys.argv) == 2:
        Path(sys.argv[1]).write_text(result)
    else:
        print(result, end='')
