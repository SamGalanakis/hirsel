#!/usr/bin/env python3
"""Fixed LAYOUT schedules and independent arithmetic oracles, seed-free by design."""
import json
from pathlib import Path


def document(n):
    items = [bool(i % 2 == 0) if i % 3 == 0 else
             'abcdefghijklmnopqrstuvw'[:i % 24] if i % 3 == 1 else
             [i, i + 1, i + 2] for i in range(n)]
    checksum = sum(int(x) if isinstance(x, bool) else len(x.encode()) if isinstance(x, str) else sum(x)
                   for x in items)
    used = 8 + sum(4 + (2 if isinstance(x, bool) else 5 + len(x.encode()) if isinstance(x, str) else 29)
                   for x in items)
    return items, checksum, used


def json_size(x):
    if isinstance(x, bool): return 1
    if isinstance(x, int): return 5 + len(str(x))
    if isinstance(x, str): return 5 + len(x.encode())
    return 9 + sum(json_size(y) + 4 for y in x)


def main():
    root = Path(__file__).resolve().parent
    items, checksum, used = document(8192)
    fixtures = root / 'inputs'
    fixtures.mkdir(exist_ok=True)
    (fixtures / 'documents.json').write_text(json.dumps(items, separators=(',', ':')) + '\n')
    (fixtures / 'malformed.json').write_text('[1,]\n')
    phases = {
        'ordinary': checksum * 64,
        'sized': checksum * 64,
        'streaming': checksum * 64,
        'exclusive': sum(range(8192)) + 8192 * 32,
        'churn': sum(range(4096)) + 2 * 4096 * 16,
        'rc': 256 * sum(range(64)) + 33 * 256 * 257 // 2,
        'arc': 256 * sum(range(64)) + 33 * 256 * 257 // 2,
        'charges': 63,
        'lifetime': 45533440,
        'json_parse': 8192 * 32,
        'json_reject': 3,
        'json_capacity': (json_size(items) + 7) // 8 * 8,
        'json_print': sum(json.dumps(items, separators=(',', ':')).encode()) * 32,
    }
    (fixtures / 'expected.json').write_text(json.dumps({
        'phases': phases, 'document_count': 8192, 'batches': 64,
        'ordinary_logical_peak': 8192 * 32 + sum((len(x) + 7) // 8 * 8 if isinstance(x, str) else 24 if isinstance(x, list) else 0 for x in items),
        'churn_logical_peak': 4096 * (28 + 24),
        'snapshot_logical_peak': (31 + 256 * 33) * 32 + 256 * 8,
        'snapshot_lifetime_admitted': (64 + 256 * 33) * 32 + 256 * 8,
        'compact_used': used, 'stream_reserved': 64 * 8192 + 8,
        'exclusive': {'nodes': 8192, 'rounds': 32},
        'churn': {'nodes': 4096, 'rounds': 16},
        'snapshots': {'depth': 64, 'versions': 256, 'copied_nodes_per_version': 33},
        'early_capacity_bytes': [0, 16, 32],
    }, indent=2) + '\n')


if __name__ == '__main__':
    main()
