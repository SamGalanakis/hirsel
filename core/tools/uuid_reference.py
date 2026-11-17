#!/usr/bin/env python3
"""Print the U2 mask oracles using Python 3.14's UUID constructor."""
from uuid import UUID

for raw, version in ((b'\xff' * 16, 4), (bytes(16), 4), (bytes(6) + b'\xff' * 10, 7)):
    u = UUID(bytes=raw, version=version)
    print(f'{u} hi=0x{u.int >> 64:016x} lo=0x{u.int & ((1 << 64) - 1):016x}')
