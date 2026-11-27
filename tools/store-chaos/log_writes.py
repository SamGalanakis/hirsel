"""Read kernel dm-log-writes v1 and reconstruct crash images from a clean backing image."""
from dataclasses import dataclass
import os
import struct

FLUSH, FUA, DISCARD, MARK = 1, 2, 4, 8


@dataclass(frozen=True)
class Entry:
    block: int
    count: int
    flags: int
    offset: int
    name: str


def entries(path):
    with path.open('rb') as log:
        magic, version, count, size = struct.unpack('<QQQI', log.read(28))
        if magic != 0x6a736677736872 or version != 1 or size not in (512, 4096):
            raise ValueError('invalid dm-log-writes superblock')
        log.seek(size)
        records = []
        for _ in range(count):
            header = log.read(size)
            block, sectors, flags, length = struct.unpack_from('<4Q', header)
            if length > size - 32:
                raise ValueError('oversize log marker')
            label = header[32:32 + length].rstrip(b'\0').decode() if flags & MARK else ''
            records.append(Entry(block, sectors, flags, log.tell(), label))
            if not flags & DISCARD:
                log.seek(sectors * size, 1)
        if log.tell() > path.stat().st_size:
            raise ValueError('truncated dm-log-writes log')
        return size, records


def cases(records):
    start, = [i for i, e in enumerate(records) if e.name == 'ack-floor']
    end, = [i for i, e in enumerate(records) if e.name == 'end-window']
    previous = start + 1
    for i in range(previous, end):
        e = records[i]
        if not e.flags & (FLUSH | FUA):
            continue
        yield dict(stop=i + 1, epoch=previous, variant='barrier', flags=e.flags)
        # A cut just before this barrier can expose only part of this epoch.
        # FUA completes its own write, whereas PREFLUSH orders all earlier writes.
        writes = [j for j in range(previous, i) if records[j].count and not records[j].flags & DISCARD]
        if writes:
            yield dict(stop=i, epoch=previous, variant='omit-sector', omit=writes[-1], flags=e.flags)
        if len(writes) >= 2:
            # Replay a later sector before an earlier one and cut at that point.
            yield dict(stop=i, epoch=previous, variant='later-sector-first', later=writes[-1], flags=e.flags)
        if e.flags & FLUSH:
            previous = i + 1
        elif e.flags & FUA:
            # Conservatively keep completed FUA writes and their predecessors.
            previous = i + 1


def replay(target, logpath, size, records, case):
    with logpath.open('rb') as log, target.open('r+b') as image:
        for i, entry in enumerate(records[:case['stop']]):
            if entry.flags & MARK or not entry.count:
                continue
            sectors = entry.count
            if case.get('omit') == i:
                sectors -= 1
            if case['variant'] == 'later-sector-first' and i >= case['epoch']:
                if i != case['later']:
                    continue
                sectors = 1
            image.seek(entry.block * size)
            log.seek(entry.offset)
            remain = sectors * size
            while remain:
                n = min(remain, 1 << 20)
                data = bytes(n) if entry.flags & DISCARD else log.read(n)
                if len(data) != n:
                    raise ValueError('truncated device log payload')
                image.write(data)
                remain -= n
        image.flush()
        os.fsync(image.fileno())
