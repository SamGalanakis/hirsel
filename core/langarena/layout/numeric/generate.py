#!/usr/bin/env python3
"""Fixed numeric kernel inputs and an independent integer checksum oracle."""
import json
from pathlib import Path


def checksum(n, seed):
    values = [(i * 37 + seed) % 1000 for i in range(n)]
    stencil = [values[(i + n - 1) % n] + values[i] + values[(i + 1) % n] for i in range(n)]
    return sum(stencil[(i * 17 + 3) % n] for i in range(n))


def main():
    root = Path(__file__).resolve().parent
    rows = [dict(name='Layout::Numeric::' + layout, n=64, seed=42, iterations=1,
                 checksum=checksum(64, 42) * 2) for layout in ['Fixed', 'AoS', 'SoA']]
    (root / 'test.json').write_text(json.dumps(rows, indent=2) + '\n')
    rows = [dict(row, n=64 if row['name'].endswith('Fixed') else 65536, iterations=100)
            for row in rows]
    for row in rows:
        row['checksum'] = (checksum(row['n'], row['seed']) * 120) % (1 << 32)
    (root / 'run.json').write_text(json.dumps(rows, indent=2) + '\n')
    (root / 'malformed.json').write_text(json.dumps([
        dict(name='Layout::Numeric::SoA', n=1, seed=42, iterations=1, checksum=0)], indent=2) + '\n')


if __name__ == '__main__':
    main()
