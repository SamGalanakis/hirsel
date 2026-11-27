#!/usr/bin/env python3
"""Off-guest Linux controller: give it an SSH command that starts linux_guest.py."""
import json
import os
from pathlib import Path
import subprocess
import sys

from history import History, require
from recover import verify_copy

FAULTS = ('log-writes', 'flakey-drop', 'flakey-eio', 'space-journal', 'space-pack',
          'space-manifest', 'space-rename', 'ring', 'dust-pack', 'dust-journal', 'damage-D6')


def qualify(call, fs, fault, directory, hirsel):
    name = fs + '-' + fault
    results = directory / 'linux.ndjson'
    def emit(status, **fields):
        with results.open('a') as out:
            print(json.dumps(dict(law='D6' if fault == 'damage-D6' else 'N1',
                                  row=name, scheduled=fault, status=status, **fields)), file=out, flush=True)
    try:
        init = call('setup', fs, fault)
        if 'reason' in init:
            emit('incomplete', native=[], reason=init['reason'])
            return
        history = History(directory / (name + '.history'), init['base'])
        def tx(op, ambiguous=False, crash=False):
            history.invoke(op)
            response = call('tx', op, ambiguous, crash)
            history.returned(op, response)
            return response
        for op in (1, 2):
            require('ACK' in tx(op), 'baseline transaction did not acknowledge')
        witness = call('arm')
        if fault == 'log-writes':
            tx(3, ambiguous=True)
            call('tx', 0)  # Include cut/rename/dir-sync in the captured device log.
        elif fault in ('space-journal', 'ring'):
            for op in range(3, 255):
                if 'ACK' not in tx(op):
                    break
        elif fault.startswith('space-'):
            call('tx', 0)
        elif fault == 'flakey-eio':
            for op in range(3, 16):
                if 'ACK' not in tx(op):
                    break
        elif fault == 'damage-D6':
            reply = tx(3, crash=True)
            frame, = [line for line in reply.splitlines() if line.startswith('FRAME ')]
            witness = call('damage', *map(int, frame.split()[1:]))
        elif fault.startswith('dust'):
            # Ensure the pack is in the published manifest before poisoning it.
            call('tx', 0)
        cuts = call('cold').get('cases', [None])
        if fault in ('space-journal', 'space-pack', 'space-manifest', 'space-rename', 'ring', 'flakey-eio'):
            witness = call('witness')
        if not cuts:
            emit('incomplete', native=[], reason='no PREFLUSH/FUA in the marked window')
        for i, case in enumerate(cuts):
            target = directory / f'{name}-{i}'
            raw = call('image', case)
            verify_copy(raw, target, hirsel)
            if fault.startswith('dust'):
                witness = call('dust', fault.split('-')[1])
                emit('pass' if witness else 'incomplete', native=witness)
                continue
            dump = call('dump')
            if fault == 'damage-D6' and witness:
                # Exactly the last transaction's frame is damaged. Its two
                # predecessors were acknowledged before the damaged operation.
                before = History(directory / (name + '.prefix'), init['base'])
                events = [json.loads(line) for line in history.path.read_text().splitlines()][1:]
                for event in events:
                    if event['op'] != 3:
                        event.pop('ns')
                        before.record(event.pop('event'), **event)
                prefix = before.verify(dump)
                emit('pass', native=witness, surviving_prefix=prefix['recovered'], dropped=1)
                continue
            oracle = history.verify(dump)
            native = case if case is not None else witness
            extra = {}
            if not native and fault == 'space-rename':
                extra['reason'] = 'renameat over existing MANIFEST succeeded without ENOSPC'
            emit('pass' if native else 'incomplete', native=native, **extra, **oracle)
    except Exception as e:
        emit('fail', reason=str(e))
        raise
    finally:
        call('close')


def main():
    directory = Path(os.environ['HIRSEL_CHAOS_RECEIPTS']).resolve()
    directory.mkdir(parents=True, exist_ok=True)
    hirsel = Path(os.environ['HIRSEL_CHAOS_DRIVER']).resolve()
    with subprocess.Popen(sys.argv[1:], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as p:
        def call(*args):
            p.stdin.write(json.dumps(args) + '\n')
            p.stdin.flush()
            line = p.stdout.readline()
            if not line:
                raise RuntimeError('Linux guest disconnected')
            reply = json.loads(line)
            if 'error' in reply:
                raise ValueError(reply['error'])
            return reply['ok']
        failures = []
        try:
            for fs in os.getenv('HIRSEL_CHAOS_FILESYSTEMS', 'delalloc nodelalloc xfs').split():
                for fault in os.getenv('HIRSEL_CHAOS_FAULTS', ' '.join(FAULTS)).split():
                    try:
                        qualify(call, fs, fault, directory, hirsel)
                    except Exception as e:
                        failures.append((fs, fault, str(e)))
        finally:
            p.stdin.close()
        require(p.wait() == 0, 'guest command failed')
        for failure in failures:
            print(*failure, file=sys.stderr)
        return int(bool(failures))


if __name__ == '__main__':
    sys.exit(main())
