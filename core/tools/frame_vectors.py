#!/usr/bin/env python3
"""Generate the Runner's hirsel.3 byte oracle directly from the wire widths."""
import argparse
import json
from pathlib import Path
from struct import pack
import subprocess

from runner_tests import frame_module


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / 'core/programs/runner'
N = 0x01020304
V = 0x0102030405060708
OTHER = 0x1112131415161718
HN = '0x01020304u32'
HV = '0x0102030405060708u64'
HR = f'runner.Room(requested: {HV}, available: 0x1112131415161718u64)'
HD = f'runner.Done(stream: {HN}, p: {HV}, outcome: Returned(b("0000000001")))'


def u32(n):
    return pack('<I', n)


def blob(data):
    return u32(len(data)) + data


def seq(items):
    return u32(len(items)) + b''.join(items)


def row(slot, key, value):
    option = u32(0) if value is None else u32(1) + blob(value)
    return pack('<H', slot) + blob(key) + option


def board_row(chunk):
    key = pack('>II', N, chunk)
    pixels = bytes(range(64)) if chunk != 1953 else bytes(range(8)) + bytes(56)
    return row(0, key, blob(pixels))


def group(sub, value, rows):
    return u32(sub) + blob(value) + seq(rows)


def batch(complete, groups, outcomes):
    return pack('<IQ', 6, V) + seq([u32(sub) for sub in complete]) + seq(groups) + seq(outcomes)


def vectors():
    cases = []

    def add(name, type_name, value, payload, derivation, wire=False):
        vector = dict(name=name, type=type_name, value=value, hex=payload.hex(), derivation=derivation)
        if wire:
            count = len(payload)
            vector['wire_bytes'] = count + (2 if count <= 125 else 4 if count <= 65535 else 10)
        cases.append(vector)

    def frame(name, value, payload, derivation, wire=False):
        add(name, 'Frame', value, payload, derivation, wire)

    identity = bytes.fromhex('aabb')
    want = blob(identity) + pack('<QQ', V, OTHER)
    want_expr = f'runner.Want(identity: b("aabb"), args_fp: {HV}, result_fp: 0x1112131415161718u64)'
    add('want', 'Want', want_expr, want, 'identity blob, then two u64 fingerprints')
    entries = [('ready', f'Ready({HN})', pack('<II', 0, N)),
               ('resolve', 'Resolve(7u32, b("cafe"))', pack('<II', 1, 7) + blob(bytes.fromhex('cafe'))),
               ('gone', 'Gone', u32(2))]
    for name, value, payload in entries:
        add('entry_' + name, 'Entry', value, payload, 'Entry tag u32, then declared payload')
    frame('hello_empty', 'Hello(None, list.new())', bytes(12), 'Hello tag, None tag and zero Wants')
    frame('hello_credential', f'Hello(Some(b("aabb")), one({want_expr}))',
          pack('<II', 0, 1) + blob(identity) + seq([want]), 'Hello tag, Some credential blob and one Want')
    frame('call', f'Call({HN}, {HV}, 0x11121314u32, 0x21222324u32, b("aabb"))',
          pack('<IIQII', 1, N, V, 0x11121314, 0x21222324) + blob(identity),
          'Call tag, stream u32, position u64, slot u32, deadline u32 and argument blob')
    subscribe = pack('<III', 2, N, 7) + blob(identity)
    frame('subscribe_none', f'Subscribe({HN}, 7u32, b("aabb"), None)',
          subscribe + u32(0), 'Subscribe tag, sub, slot, argument blob and None')
    frame('subscribe_some', f'Subscribe({HN}, 7u32, b("aabb"), Some(b("cafe")))',
          subscribe + u32(1) + blob(bytes.fromhex('cafe')), 'Subscribe fields with Some resume blob')
    frame('unsubscribe', f'Unsubscribe({HN})', pack('<II', 3, N), 'Unsubscribe tag and sub u32')
    subject = bytes(range(16))
    digest = bytes(range(32))
    target = 'x86_64-unknown-linux-gnu'
    welcome = f'Welcome(store.Caller(issuer: 2u8, subject: b("{subject.hex()}")), 4u32, 1048576u64, b("{digest.hex()}"), "{target}", 16777216u64)'
    frame('welcome', welcome, pack('<IB', 4, 2) + blob(subject) + pack('<IQ', 4, 1048576)
          + blob(digest) + blob(target.encode()) + pack('<Q', 16777216),
          'Welcome tag, Caller issuer and subject, q, budget, digest, target, n_place', True)
    for name, value, payload in entries:
        frame('placed_' + name, f'Placed({value})', u32(5) + payload, 'Placed tag and Entry bytes')

    returned = u32(0) + blob(bytes.fromhex('0000000001'))
    done = pack('<IQ', N, V) + returned
    room = pack('<QQ', V, OTHER)
    outcomes = [('returned', 'Returned(b("0000000001"))', returned)]
    faults = [('raised', 'Raised(9u32)', u32(9)), ('out_of_bytes', f'OutOfBytes({HR})', room),
              ('out_of_time', f'OutOfTime({HV})', pack('<Q', V)), ('gone', 'Gone', b''),
              ('conflict', 'Conflict("unique_value")', blob(b'unique_value'))]
    for tag, (name, value, payload) in enumerate(faults):
        outcomes.append(('failed_' + name, f'Failed({value})', pack('<II', 1, tag) + payload))
    refusals = [('no_entry', 'NoEntry', b''), ('malformed', f'Malformed(runner.Bad(kind: 6u32, at: {HV}))', pack('<IQ', 6, V)),
                ('overloaded', 'Overloaded', b''), ('deadline', 'Deadline', b''), ('budget', f'Budget({HR})', room),
                ('not_allowed', 'NotAllowed', b''), ('request_mismatch', 'RequestMismatch', b''),
                ('history_unavailable', 'HistoryUnavailable', b'')]
    for tag, (name, value, payload) in enumerate(refusals):
        outcomes.append(('refused_' + name, f'Refused({value})', pack('<II', 2, tag) + payload))
    outcomes.extend([('abandoned', 'Abandoned', u32(3)), ('in_doubt', 'InDoubt', u32(4))])
    for name, value, payload in outcomes:
        frame('ended_' + name, f'Ended({HN}, {value})', pack('<II', 7, N) + payload, 'Ended tag, sub and Outcome')

    add('done', 'Done', HD, done, 'stream u32, position u64 and Outcome')
    key = pack('>II', N, 0x05060708)
    add('row_removed', 'Row', f'runner.Row(slot: 3u16, key: b("{key.hex()}"), value: None)',
        row(3, key, None), 'slot u16, key blob and None tag')
    sample = board_row(0x05060708)
    add('row', 'Row', 'row(0x05060708u32)', sample, 'slot u16, key blob, Some tag and value blob')
    frame('batch_empty', 'batch_empty()', batch([], [], []), 'Batch tag, version and three empty lists')
    frame('empty_initial', 'batch_initial()', batch([7], [group(7, b'', [])], []),
          'one complete id and its group with an empty value and no rows', True)
    update = group(7, b'', [sample])
    frame('viewer_update', 'batch_viewer()', batch([], [update], []), 'one group with one row and no outcomes', True)
    frame('caller_update', 'batch_caller()', batch([], [update], [done]), 'one group with one row and one Done', True)
    board = group(7, b'', [board_row(chunk) for chunk in range(1954)])
    frame('full_board', 'batch_board()', batch([7], [board], []), 'one complete id and one group with 1954 rows', True)
    moved = group(7, pack('<I', 19), [row(0, key, None), row(2, key, b'\x07')])
    frame('slot_move', 'batch_move()', batch([], [moved], []), 'one group holding the new value and both slot changes', True)
    reset = pack('<IQ', 8, V)
    frame('reset_empty', f'Reset({HV}, list.new())', reset + seq([]), 'Reset tag, version and no outcomes')
    frame('reset_outcome', f'Reset({HV}, one({HD}))', reset + seq([done]), 'Reset tag, version and one Done')
    for tag, (name, value, _) in enumerate(refusals[6:], 6):
        add('done_refused_' + name, 'Done', f'runner.Done(stream: {HN}, p: {HV}, outcome: Refused({value}))',
            pack('<IQII', N, V, 2, tag), 'stream u32, position u64 and precise refusal')
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    args = parser.parse_args()
    document = dict(contract='RUNNER-SPEC section 3, hirsel.3',
                    oracle='Derived from codec.md widths. hex is the payload; wire_bytes includes the unmasked server header.',
                    vectors=vectors())
    (RUNNER / 'frame-vectors.json').write_text(json.dumps(document, indent=2) + '\n')
    source, count = frame_module(ROOT)
    formatted = subprocess.run([args.compiler.resolve(), 'fmt', '--stdin'], input=source,
                               capture_output=True, text=True, check=True).stdout
    (RUNNER / 'runner_wire.hirsel').write_text(formatted)
    print(f'Generated {count} frame vectors.')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
