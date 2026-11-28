#!/usr/bin/env python3
"""A stand-in for ctf_host that speaks its line protocol and builds nothing.

test_oracle.py runs the harness against it, so the oracle's laws are checked without a
compiler or a pack host. It keeps the secret, as only a sabotaged host does, answers
probes as a live canary would, and ends each attack the way its mode says.

usage: stub_host.py <hirsel> <scratch> <trusted> <engine> [--provider] [--sabotage mode]
"""
import hashlib
import os
import resource
import signal
import sys
import time

CONTROL = 'report stage=2 kind=1 a=0 b=0 causes=0 total=7 out=' + b'control'.hex()


def crash(number):
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    signal.signal(number, signal.SIG_DFL)
    os.kill(os.getpid(), number)
    time.sleep(60)


def attack(mode, state):
    """The reply to one attack, or None to say nothing."""
    secret = state['secret']
    returned = lambda data: f'report stage=2 kind=1 a=0 b=0 causes=0 total={len(data)} out={data.hex()}'
    if mode == 'leak':
        return returned(secret)
    if mode == 'leak-window':
        return returned(secret[11:20])
    if mode == 'short-window':     # 8 bytes: not a leak
        return returned(secret[11:19])
    if mode == 'leak-hex':
        return returned(secret.hex().encode())
    if mode == 'leak-cause':
        return f'report stage=2 kind=4 a=5 b=0 causes=1 total=32 out={secret.hex()}'
    if mode == 'leak-diagnostic':
        return 'compile-refused kind=Rejected text=' + (b'main.hirsel:1:1-2: LANG-7 ' + secret).hex()
    if mode == 'leak-stop-name':
        return f'run-stop name={secret[3:20].hex()} fault=0 quarantined=0'
    if mode == 'leak-stderr':
        sys.stderr.buffer.write(secret)
        sys.stderr.flush()
    if mode == 'leak-decoy':
        return returned(state['decoy'])
    if mode in ('crash-abort', 'crash-segv', 'crash-bus', 'crash-ill'):
        crash({'crash-abort': signal.SIGABRT, 'crash-segv': signal.SIGSEGV,
               'crash-bus': signal.SIGBUS, 'crash-ill': signal.SIGILL}[mode])
    if mode == 'hang':
        time.sleep(3600)
    if mode == 'rss':
        hoard = []
        while True:
            hoard.append(bytearray(b'\x5a') * (8 << 20))
            time.sleep(0.01)
    if mode == 'guard-limit':
        os._exit(71)
    if mode == 'exit-unclean':
        os._exit(9)
    if mode == 'exit-silent':      # a clean status with the attack unanswered
        os._exit(0)
    if mode == 'canary-swap':
        state['secret'] = bytes(32)
    if mode == 'canary-kill':
        state['lost'] = True
    if mode == 'new-line':         # a reply this oracle has never seen
        return 'contained status=fine'
    if mode == 'out-of-time':
        return 'report stage=2 kind=2 a=2000000 b=0 causes=0 total=0 out='
    if mode == 'run-stop':
        return 'run-stop name=' + b'Stack'.hex() + ' fault=0 quarantined=0'
    if mode == 'refused':
        return 'compile-refused kind=Rejected text=' + b'main.hirsel:9:3-4: OWN-5 `x` was moved'.hex()
    return CONTROL


def main():
    arguments = sys.argv[5:]
    mode = arguments[arguments.index('--sabotage') + 1] if '--sabotage' in arguments else ''
    state = {'secret': b'', 'decoy': b'', 'lost': False}
    if mode == 'startup-crash':
        crash(signal.SIGSEGV)
    print(f'ready pid={os.getpid()}', flush=True)
    for line in sys.stdin:
        parts = line.split()
        if parts[:1] == ['seal'] and len(parts) == 3:
            state['secret'], state['decoy'] = bytes.fromhex(parts[1]), bytes.fromhex(parts[2])
            reply = 'sealed'
        elif parts[:1] == ['probe'] and len(parts) == 2:
            digest = hashlib.sha256(state['secret'] + bytes.fromhex(parts[1])).hexdigest()
            reply = 'canary-lost stopped' if state['lost'] else 'answer ' + digest
        elif parts[:1] == ['attack'] and len(parts) == 7:
            reply = attack(mode, state)
        elif parts[:1] == ['emit'] and len(parts) == 3:
            with open(parts[2], 'wb') as module:
                module.write(b'HIRMOD\0\x01' + bytes(248))
            reply = 'emitted bytes=256'
        elif parts[:1] == ['raw'] and len(parts) == 2:
            reply = 'raw-loaded' if mode == 'raw-loads' else 'raw-refused ' + b'Malformed(Header)'.hex()
        elif parts == ['quit']:
            break
        else:
            reply = 'bad-command unknown'
        if reply is not None:
            print(reply, flush=True)
    lost = state['lost']
    print('bye canary=' + ('canary-lost stopped' if lost else 'retired') + ' live=0', flush=True)
    sys.exit(1 if lost else 0)


if __name__ == '__main__':
    main()
