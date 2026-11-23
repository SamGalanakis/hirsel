"""Independent AP3/AP14 frame accounting for the fixed R6 workload."""
from collections import Counter
from dataclasses import dataclass
from struct import unpack_from

PACK_HEADER = 72
FRAME_HEADER = 16
PAGE = 0x0021
TARGET = 4096
COMMITS = 64


@dataclass
class Page:
    place: tuple
    width: int
    role: str
    level: int
    children: list


def frames(raw):
    offset = PACK_HEADER
    while offset < len(raw):
        kind, version, width = unpack_from('<HHI', raw, offset)
        end = offset + FRAME_HEADER + width
        if kind != PAGE or version != 1 or end > len(raw):
            raise ValueError('R6 uses only PAGE frames with inline values')
        yield offset, raw[offset + FRAME_HEADER:end]
        offset = end


class Manifest:
    def __init__(self, directory):
        self.directory = directory
        self.packs = {}
        self.pages = {}
        self.pack_rows = {}
        self.retired_rows = {}
        raw = (directory / 'MANIFEST').read_bytes()
        kind, version, width = unpack_from('<HHI', raw, PACK_HEADER)
        disk_format, = unpack_from('<I', raw, 24)
        if disk_format != 4 or (kind, version, width) != (0x0031, 1, len(raw) - PACK_HEADER - FRAME_HEADER):
            raise ValueError('R6 expected a format-4 MANIFEST')
        manifest = raw[PACK_HEADER + FRAME_HEADER:]
        self.next_stamp = unpack_from('<Q', manifest, 24)[0]
        self.next_pack = unpack_from('<Q', manifest, 40)[0]
        branches, branch_id, name_length = unpack_from('<IIH', manifest, 308)
        roots = 346 + name_length
        if (branches, branch_id) != (1, 0) or manifest[roots + 108:] != bytes(5):
            raise ValueError('R6 expected only the main branch with no pins')
        self.walk(manifest, 236, 'pack_list')
        self.walk(manifest, 272, 'retirement')
        self.walk(manifest, roots, 'map')
        self.walk(manifest, roots + 36, 'streams')
        self.walk(manifest, roots + 72, 'catalog')

    def pack(self, seq):
        if seq not in self.packs:
            raw = (self.directory / f'p-{seq}').read_bytes()
            self.packs[seq] = dict(frames(raw))
        return self.packs[seq]

    def walk(self, parent, field, role):
        pack, offset, width, _, stamp = unpack_from('<QQIQQ', parent, field)
        if stamp == 0:
            return None
        if stamp in self.pages:
            existing = self.pages[stamp]
            if (existing.place, existing.width, existing.role) != ((pack, offset), width, role):
                raise ValueError(f'R6 conflicting references to stamp {stamp}')
            return stamp
        raw = self.pack(pack)[offset]
        level, _, count, prefix, _, found = unpack_from('<BBHHHQ', raw)
        if (width, stamp) != (len(raw), found):
            raise ValueError(f'R6 location does not resolve stamp {stamp}')
        children = []
        for slot in range(count):
            entry = unpack_from('<H', raw, len(raw) - 2 * (count - slot))[0]
            suffix = unpack_from('<H', raw, entry)[0]
            cell = entry + 2 + suffix
            if level:
                children.append(self.walk(raw, cell, role))
                continue
            tag, size = unpack_from('<BH', raw, cell)
            if tag != 0:
                raise ValueError('R6 expected an inline leaf value')
            value = cell + 3
            if role == 'map':
                child = self.walk(raw, value, 'collection')
                if child is not None:
                    children.append(child)
            elif role in ('pack_list', 'retirement'):
                rows = self.pack_rows if role == 'pack_list' else self.retired_rows
                key = raw[24:24 + prefix] + raw[entry + 2:cell]
                rows[key] = raw[value:value + size]
        self.pages[stamp] = Page((pack, offset), width, role, level, children)
        return stamp


def height(before, after, role):
    return max((p.level + 1 for image in (before, after)
                for p in image.pages.values() if p.role == role), default=0)


def relocation_closure(before, after):
    # Project the surviving stamps back into the old files and compute
    # AP16's sparse-pack ratio from frame widths, including dead frames.
    live = Counter()
    for stamp, page in before.pages.items():
        if stamp in after.pages:
            live[page.place[0]] += page.width
    sparse = {
        seq for seq, pack in before.packs.items()
        if len(pack) > 1 and 0 < 2 * live[seq] < sum(map(len, pack.values()))
    }
    # MANIFEST traversal records children before parents. Fresh ancestors
    # already count toward their own path's L5 bound.
    closure = set()
    for stamp, page in after.pages.items():
        old = before.pages.get(stamp)
        if old and (old.place[0] in sparse or closure.intersection(page.children)):
            closure.add(stamp)
    return closure


def check_r6(before, after):
    closure = relocation_closure(before, after)
    emitted = Counter()
    written = 0
    for seq in range(before.next_pack, after.next_pack):
        pack = after.pack(seq)
        written += PACK_HEADER
        for offset, raw in pack.items():
            stamp = unpack_from('<Q', raw, 8)[0]
            page = after.pages.get(stamp)
            if page is None or page.place != (seq, offset):
                raise ValueError(f'R6 emitted an unreachable or duplicate PAGE, stamp {stamp}')
            if stamp < before.next_stamp:
                if stamp not in closure:
                    raise ValueError(f'R6 relocated stamp {stamp} outside the AP16 closure')
                emitted['relocation'] += 1
            else:
                emitted[page.role] += 1
            written += FRAME_HEADER + len(raw)

    edits = sum(before.pack_rows.get(key) != after.pack_rows.get(key)
                for key in before.pack_rows.keys() | after.pack_rows.keys())
    # R6 overwrites fixed-size values at existing keys. The optional index
    # uses id % 4, which does not change when the balance changes (i = 0).
    bounds = {
        'collection': COMMITS * (3 * height(before, after, 'collection') + 1),
        'map': COMMITS * (3 * height(before, after, 'map') + 1),
        'pack_list': edits * (3 * height(before, after, 'pack_list') + 1),
        'retirement': 2 * len(after.retired_rows) + 1,
        'streams': 0,
        'catalog': 0,
        'relocation': len(closure),
    }
    num_pages = emitted.total()
    byte_bound = num_pages * (TARGET + FRAME_HEADER) + PACK_HEADER * (after.next_pack - before.next_pack)
    violations = [f'{role} pages {emitted[role]} > {bound}'
                  for role, bound in bounds.items() if emitted[role] > bound]
    if written > byte_bound:
        violations.append(f'pack bytes {written} > {byte_bound}')
    return dict(page_counts={role: emitted[role] for role in bounds}, page_bounds=bounds,
                pack_bytes=written, new_pages=num_pages, pack_byte_bound=byte_bound,
                pack_list_row_edits=edits, retirement_rows=len(after.retired_rows),
                r6_violations=violations)
