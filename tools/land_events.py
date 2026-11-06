"""Durable submission and completion rows shared by land and its detached wrapper."""
import csv
import fcntl
import os
from pathlib import Path
import time

FIELDS = ('state', 'lane', 'ticket', 'log', 'exit', 'landed', 'submitted', 'at')


def append_event(path, state, record, log, code=''):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('a', newline='') as output:
        fcntl.flock(output, fcntl.LOCK_EX)
        row = dict(record, state=state, log=str(log), exit=code,
                   at=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()))
        csv.writer(output, delimiter='\t').writerow([row.get(key, '') for key in FIELDS])
        output.flush()
        os.fsync(output.fileno())


def report_events(path, output):
    """Print new complete event rows, then durably advance the single round-check reader."""
    path = Path(path)
    if not path.exists():
        return 0
    with Path(str(path) + '.seen').open('a+') as seen:
        fcntl.flock(seen, fcntl.LOCK_EX)
        seen.seek(0)
        offset = int(seen.read() or 0)
        count = 0
        with path.open('rb') as events:
            fcntl.flock(events, fcntl.LOCK_SH)
            events.seek(offset)
            for raw in events:
                if not raw.endswith(b'\n'):
                    break
                [values] = list(csv.reader([raw.decode()], delimiter='\t'))
                if len(values) != len(FIELDS):
                    raise ValueError('malformed land event')
                row = dict(zip(FIELDS, values))
                if row['state'] not in ('SUBMITTED', 'LANDED', 'FAILED'):
                    raise ValueError('unknown land event state')
                suffix = f" EXIT {row['exit']}" if row['exit'] else ''
                if row['landed']:
                    suffix += f" landed {row['landed']}"
                print(f"LAND {row['state']} {row['lane']} ticket {row['ticket'] or '?'}{suffix} log {row['log']}",
                      file=output, flush=True)
                offset += len(raw)
                count += 1
        seen.seek(0)
        seen.truncate()
        seen.write(str(offset))
        seen.flush()
        os.fsync(seen.fileno())
    return count
