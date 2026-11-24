"""Render independent signature fixtures for the core contract walker."""

import json
import struct


def read_groups(path):
    source = json.loads(path.read_bytes())
    groups = source['testGroups']
    if sum(len(group['tests']) for group in groups) != source['numberOfTests']:
        raise ValueError(f'{path}: test count mismatch')
    return groups


def literal(value):
    if isinstance(value, bytes):
        return 'b"' + ''.join(f'\\x{byte:02x}' for byte in value) + '"'
    if isinstance(value, bool):
        return str(value).lower()
    return str(value)


def tuple_literal(row):
    return '(' + ', '.join(map(literal, row)) + ')'


def table(name, rows):
    # Each row starts with its ID, verdict and lengths as u64 LE words,
    # followed by the key components, message and signature bytes.
    records = bytearray()
    for case_id, *parts, verdict in rows:
        header = (case_id, verdict, *(len(part) for part in parts))
        records.extend(struct.pack('<' + 'Q' * len(header), *header))
        records.extend(b''.join(parts))
    print(f'fn {name}() -> Bytes {{ {literal(bytes(records))} }}')


def fixture(name, *parts):
    types = ', '.join(['Bytes'] * len(parts))
    print(f'fn {name}() -> ({types}) {{ {tuple_literal(parts)} }}')


def validity(test):
    return {'valid': True, 'invalid': False}[test['result']]
