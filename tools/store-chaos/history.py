"""External invocation log and exact oracle for the fixture's two-key transaction."""
import hashlib
import json
import os
from pathlib import Path
import time


def require(condition, why):
    if not condition:
        raise ValueError(why)


def digest(state):
    return hashlib.sha256(json.dumps(sorted(state.items()), separators=(',', ':')).encode()).hexdigest()


class History:
    def __init__(self, path, base):
        self.path = Path(path)
        with self.path.open('x'):
            pass
        self.record('base', version=base)

    def record(self, event, **row):
        with self.path.open('a') as f:
            f.write(json.dumps(dict(event=event, ns=time.monotonic_ns(), **row)) + '\n')
            f.flush()
            os.fsync(f.fileno())

    def invoke(self, op):
        a, b = op % 7, (op + 3) % 7
        self.record('invoke', op=op, reads=[a, b],
                    writes=[[a, 'right + op'], [b, 'left + op + 1']])

    def returned(self, op, lines):
        replies = [line for line in lines.splitlines() if line.startswith('ACK ')]
        if not replies:
            self.record('timeout', op=op)
            return
        require(len(replies) == 1, 'more than one durable reply')
        v, echoed, left, right = map(int, replies[0].split()[1:])
        require(echoed == op, 'unknown operation in reply')
        self.record('ack', op=op, version=v, reads=[left, right])

    def verify(self, dump):
        events = [json.loads(s) for s in self.path.read_text().splitlines()]
        base = events[0]['version']
        invoked, ack, recovered, actual, state = {}, {}, {}, {}, {}
        for i, event in enumerate(events[1:], 1):
            op = event['op']
            if event['event'] == 'invoke':
                require(op not in invoked, 'op id reused')
                invoked[op] = i
            elif event['event'] == 'ack':
                require(op in invoked and op not in ack, 'unmatched durable reply')
                ack[op] = (i, event['version'], event['reads'])
        head = None
        for line in dump.splitlines():
            kind, *values = line.split()
            values = list(map(int, values))
            if kind == 'TX':
                v, op, left, right = values
                require(v == base + len(recovered) + 1, 'noncontiguous recovered versions')
                require(op in invoked and op not in recovered, 'phantom or duplicate transaction')
                a, b = op % 7, (op + 3) % 7
                reads = [state.get(a, 0), state.get(b, 0)]
                require([left, right] == reads, f'transaction {op} read differs at v-1')
                if op in ack:
                    require(ack[op][1:] == (v, reads), 'returned version or reads differ')
                require(all(past in recovered for past, (end, _, _) in ack.items()
                            if end < invoked[op]), 'real-time order inverted')
                state[a], state[b] = right + op, left + op + 1
                recovered[op] = v
            elif kind == 'KEY':
                k, n = values
                require(k not in actual, 'duplicate final key')
                actual[k] = n
            elif kind == 'HEAD':
                require(head is None and len(values) == 1, 'duplicate or malformed head')
                head, = values
            else:
                raise ValueError(f'unknown dump row {kind}')
        require(ack and set(ack) <= recovered.keys(), 'lost durable acknowledgement or empty control')
        require(head == base + len(recovered), 'inconsistent recovered head')
        require(state == actual, 'final state differs from ledger replay')
        return dict(acked=len(ack), recovered=len(recovered), acked_loss=0, digest=digest(actual))
