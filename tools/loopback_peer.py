"""Identify a Linux loopback client by the process that owns its socket."""

import os
import socket
import sys
import time
from pathlib import Path


def _process_tree(pid):
    yield pid
    for thread in Path(f'/proc/{pid}/task').glob('*'):
        try:
            children = (thread / 'children').read_text().split()
        except FileNotFoundError:
            continue
        for child in children:
            yield from _process_tree(int(child))


def owned_by(pid, peer, *, descendants=False):
    family = peer.family
    ends = []
    for host, port, *_ in (peer.getpeername(), peer.getsockname()):
        packed = socket.inet_pton(family, host)
        address = ''.join(f'{int.from_bytes(packed[i:i + 4], sys.byteorder):08X}'
                          for i in range(0, len(packed), 4))
        ends.append(f'{address}:{port:04X}')
    with open('/proc/net/tcp6' if family == socket.AF_INET6 else '/proc/net/tcp') as table:
        next(table)
        inode = next((row[9] for row in map(str.split, table)
                      if row[1:3] == ends and row[9] != '0'), None)
    if inode is None:
        return False
    target = f'socket:[{inode}]'
    for owner in _process_tree(pid) if descendants else (pid,):
        for fd in Path(f'/proc/{owner}/fd').glob('*'):
            try:
                if os.readlink(fd) == target:
                    return True
            except FileNotFoundError:
                continue
    return False


def log_stray(address):
    print(f'skipped unexpected loopback connection from {address}', file=sys.stderr, flush=True)


def accept_from(listener, pid, *, descendants=False):
    """Close unrelated connections without inspecting the test's wire bytes."""
    timeout = listener.gettimeout()
    deadline = time.monotonic() + timeout if timeout else None
    try:
        while True:
            if deadline is not None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError('expected loopback client did not connect')
                listener.settimeout(remaining)
            peer, address = listener.accept()
            try:
                if owned_by(pid, peer, descendants=descendants):
                    return peer
            except BaseException:
                peer.close()
                raise
            peer.close()
            log_stray(address)
    finally:
        if timeout:
            listener.settimeout(timeout)
