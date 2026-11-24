#!/usr/bin/env python3
"""Generate the Runner's hirsel.4 byte oracle directly from the wire widths."""
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
NONCE = 0x2122232425262728
POSITION = 0x3132333435363738
VERSION = 0x6162636465666768
NOW = 0x0000019A6B2C3D4E
RETENTION = 5_184_000_000
HN = '0x01020304u32'
HV = '0x0102030405060708u64'
HS = f'store.StreamId(t: {HV}, n: 0x{NONCE:016x}u64)'
HR = f'runner.Room(requested: {HV}, available: 0x1112131415161718u64)'
HD = f'runner.Done(stream: {HS}, p: {HV}, outcome: Returned(b("0000000001")))'


def u32(n):
    return pack('<I', n)


def blob(data):
    return u32(len(data)) + data


def seq(items):
    return u32(len(items)) + b''.join(items)


def optional(data):
    return u32(0) if data is None else u32(1) + data


def row(slot, key, value):
    option = u32(0) if value is None else u32(1) + blob(value)
    return pack('<H', slot) + blob(key) + option


def board_row(chunk):
    key = pack('>II', N, chunk)
    pixels = bytes(range(64)) if chunk != 1953 else bytes(range(8)) + bytes(56)
    return row(0, key, blob(pixels))


def group(sub, value, rows):
    return u32(sub) + blob(value) + seq(rows)


def batch(complete, groups, outcomes, version=V):
    return pack('<IQ', 6, version) + seq([u32(sub) for sub in complete]) + seq(groups) + seq(outcomes)


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
    stream = pack('<QQ', V, NONCE)
    frame('call_streamid', f'Call({HS}, 0x{POSITION:016x}u64, 0x11121314u32, 0x21222324u32, b("aabb"))',
          u32(1) + stream + pack('<QII', POSITION, 0x11121314, 0x21222324) + blob(identity),
          'Call tag, stream timestamp and nonce, position, slot, deadline and argument blob')
    subscribe = pack('<III', 2, N, 7) + blob(identity)
    frame('subscribe_none', f'Subscribe({HN}, 7u32, b("aabb"), None)',
          subscribe + u32(0), 'Subscribe tag, sub, slot, argument blob and None')
    frame('subscribe_some', f'Subscribe({HN}, 7u32, b("aabb"), Some(b("cafe")))',
          subscribe + u32(1) + blob(bytes.fromhex('cafe')), 'Subscribe fields with Some resume blob')
    frame('unsubscribe', f'Unsubscribe({HN})', pack('<II', 3, N), 'Unsubscribe tag and sub u32')
    subject = bytes(range(16))
    digest = bytes(range(32))
    target = 'x86_64-unknown-linux-gnu'
    caller = f'store.Caller(issuer: 2u8, subject: b("{subject.hex()}"))'
    welcome = f'Welcome({caller}, 4u32, 1048576u64, b("{digest.hex()}"), "{target}", 16777216u64, 0x{NOW:016x}u64, {RETENTION}u64)'
    frame('welcome_times', welcome, pack('<IB', 4, 2) + blob(subject) + pack('<IQ', 4, 1048576)
          + blob(digest) + blob(target.encode()) + pack('<QQQ', 16777216, NOW, RETENTION),
          'Welcome tag, Caller issuer and subject, q, budget, digest, target, n_place, keyed time, retention', True)
    for name, value, payload in entries:
        frame('placed_' + name, f'Placed({value})', u32(5) + payload, 'Placed tag and Entry bytes')

    returned = u32(0) + blob(bytes.fromhex('0000000001'))
    done = stream + pack('<Q', V) + returned
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

    add('done', 'Done', HD, done, 'stream timestamp and nonce, position and Outcome')
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
        add('done_refused_' + name, 'Done', f'runner.Done(stream: {HS}, p: {HV}, outcome: Refused({value}))',
            stream + pack('<QII', V, 2, tag), 'stream timestamp and nonce, position and precise refusal')

    budget = 'Refused(Budget(runner.Room(requested: 0x4142434445464748u64, available: 0x5152535455565758u64)))'
    budget_expr = f'runner.Done(stream: {HS}, p: 0x{POSITION:016x}u64, outcome: {budget})'
    budget_done = stream + pack('<QIIQQ', POSITION, 2, 4, 0x4142434445464748, 0x5152535455565758)
    add('done_refused_budget', 'Done', budget_expr, budget_done, '48 bytes: stream, position, two tags and Room')
    frame('batch_one_refusal', f'Batch(0x{VERSION:016x}u64, list.new(), list.new(), one({budget_expr}))',
          batch([], [], [budget_done], VERSION), '24-byte Batch prefix and one 48-byte Done', True)
    frame('reset_one_refusal', f'Reset(0x{VERSION:016x}u64, one({budget_expr}))',
          pack('<IQ', 8, VERSION) + seq([budget_done]), '16-byte Reset prefix and one 48-byte Done', True)
    future = f'Refused(Malformed(runner.Bad(kind: 7u32, at: 0x{NOW:016x}u64)))'
    add('done_stream_future', 'Done', f'runner.Done(stream: {HS}, p: 0u64, outcome: {future})',
        stream + pack('<QIIIQ', 0, 2, 1, 7, NOW), 'stream, position, refusal tags, STREAM_FUTURE and its keyed time')

    frame('set_state', 'Set(7u32, Some(b("aabb")))', pack('<II', 9, 7) + optional(blob(identity)),
          'Set tag, sub, Some tag and state blob')
    frame('send_direct', 'Send(7u32, Some(8u32), b("aabb"))', pack('<II', 10, 7) + optional(u32(8)) + blob(identity),
          'Send tag, sub, Some tag, peer and message blob')
    part = (pack('<IB', 7, 1) + optional(u32(8)) + seq([pack('<IB', 9, 2) + blob(subject)])
            + seq([u32(9) + optional(blob(identity))]) + seq([pack('<IB', 9, 1) + blob(b'\xca\xfe')])
            + seq([u32(10)]) + pack('<IIB', 11, 12, 1))
    part_expr = (f'runner.Part(sub: 7u32, complete: true, you: Some(8u32), joined: one((9u32, {caller})), '
                 'states: one((9u32, Some(b("aabb")))), msgs: one((9u32, true, b("cafe"))), '
                 'left: one(10u32), gap: 11u32, lost: 12u32, full: true)')
    frame('live_all_fields', f'Live(one({part_expr}))', u32(11) + seq([part]),
          'Live tag and one Part, with all options and lists populated', True)
    return cases


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    args = parser.parse_args()
    document = dict(contract='RUNNER-SPEC section 3, hirsel.4',
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
