"""STORE-SPEC v2.5 pack model: pack naming, unnamed-pack cleanup and the one-NODE oversized pack.

store-build-1010's two S2 gaps (followups.md, "S2 gap 1" and "S2 gap 2").

File system. Directory operations (create, rename, remove) are pending until `sync_dir`. A crash keeps
ANY SUBSET of the pending directory operations, applied in their order; an operation whose source is
gone is skipped. That is weaker than the test provider's all-or-nothing restart, and it is the
partial persistence 1010 named. File data not yet `sync`ed is lost at a power cut (the file reads
torn). A process crash leaves all data and pending directory operations visible to the next opener.

Protocol under test (sections 3.4 and 3.5 as of v2.5):
  cut:  for each pack seq in [next_pack, next_pack + m): write p-seq.tmp (Write truncates), sync,
        rename p-seq.tmp -> p-seq (replacing any file of that name); then sync_dir once; then
        MANIFEST.tmp, sync, rename -> MANIFEST, sync_dir; then remove the retired packs (no sync).
  open: read MANIFEST and sync_dir (O1); remove MANIFEST.tmp; for x = next_pack, ... remove
        p-x and p-x.tmp, stopping at the first x where both are NotFound; remove the retired packs.
        Then load exactly the packs MANIFEST lists (each must exist and hold its cut's bytes).
Packing rule (gap 1): a NODE goes into the open pack unless that would take the pack past pack_bytes
and the pack already holds a NODE; then the pack closes first. So a pack is at most
max(pack_bytes, HEADER + one NODE frame), and a NODE larger than pack_bytes sits in a pack alone.

Scenario: genesis manifest (pack p-1). Cut A writes three packs (one holds an oversized NODE alone),
crashing at every step under every subset of pending directory operations. Recovery's cleanup then runs,
with a second crash at every one of its steps, again under every subset. Then the store opens, runs
cut B (one pack) and cut C (three packs), each to completion, and opens again.
At cut A's MANIFEST rename, also crash only the process, then cut power throughout reopen's cleanup.
Checks at every open:
  P1 every pack the manifest lists exists and holds exactly the content its cut wrote;
  P2 the loaded nodes are exactly the manifest's (nothing adopted from an unlisted pack);
  P3 every p-* file the manifest neither lists nor retires has seq >= next_pack (leftovers sit only where a later cut
     rewrites before listing);
  P4 (gap 1) every pack is at most max(pack_bytes, HEADER + its largest NODE) and no NODE is split.
Mutants (each must be caught):
  adopt_by_probe   open loads every p-x it finds from 1 upward, not the manifest's list
  skip_existing    a cut that finds p-seq already present lists it without rewriting it
  no_dir_sync      the cut omits the sync_dir between the pack renames and MANIFEST
  start_below      a cut starts at next_pack - 1, overwriting a listed pack before MANIFEST moves
  close_after      the pack closes after the NODE that passes pack_bytes, not before it
  no_open_sync     recovery removes files before syncing the selected MANIFEST's directory entry
"""
import itertools
import sys

HEADER = 72
PACK_BYTES = 100
NODES_A = [('a1', 40), ('a2', 40), ('a3', 250), ('a4', 30)]   # a3 alone exceeds pack_bytes
NODES_B = [('b1', 30)]
NODES_C = [('c1', 20), ('c2', 60), ('c3', 60)]   # close_after would make a 152-byte pack


def packs_of(nodes, mutant):
    packs, cur, size = [], [], HEADER
    for name, n in nodes:
        if mutant == 'close_after':
            cur.append((name, n)); size += n
            if size >= PACK_BYTES:
                packs.append(cur); cur, size = [], HEADER
            continue
        if cur and size + n > PACK_BYTES:
            packs.append(cur); cur, size = [], HEADER
        cur.append((name, n)); size += n
    if cur:
        packs.append(cur)
    return packs


class FS:
    def __init__(s):
        s.durable = {}      # name -> inode
        s.pending = []      # directory ops since the last sync_dir
        s.data = {}         # inode -> (content, synced)
        s.next_inode = 1

    def live(s):
        d = dict(s.durable)
        for op in s.pending:
            apply(d, op)
        return d

    def write(s, name, content):
        i = s.next_inode; s.next_inode += 1
        s.data[i] = (content, False)
        s.pending.append(('link', name, i))

    def sync(s, name):
        i = s.live()[name]; c, _ = s.data[i]; s.data[i] = (c, True)

    def rename(s, a, b):
        if a in s.live():
            s.pending.append(('rename', a, b))

    def remove(s, name):
        if name in s.live():
            s.pending.append(('remove', name))

    def sync_dir(s):
        s.durable = s.live(); s.pending = []

    def exists(s, name):
        return name in s.live()

    def read(s, name):
        d = s.live()
        if name not in d:
            return None
        return s.data[d[name]][0]

    def process_crash(s):
        return copy(s)

    def crashes(s):
        n = len(s.pending)
        for mask in range(1 << n):
            f = FS()
            d = dict(s.durable)
            for k, op in enumerate(s.pending):
                if mask >> k & 1:
                    apply(d, op)
            f.durable = d
            f.data = {i: ((c if ok else 'TORN'), ok) for i, (c, ok) in s.data.items()}
            f.next_inode = s.next_inode
            yield f


def apply(d, op):
    if op[0] == 'link':
        d[op[1]] = op[2]
    elif op[0] == 'rename':
        if op[1] in d:
            d[op[2]] = d.pop(op[1])
    elif op[0] == 'remove':
        d.pop(op[1], None)


def manifest_of(fs):
    m = fs.read('MANIFEST')
    assert m not in (None, 'TORN'), 'MANIFEST lost: the genesis manifest is synced and dir-synced'
    return m


def cut_steps(fs_ref, gen, nodes, mutant):
    """Yields after every host operation, so a crash can be cut there."""
    fs = fs_ref
    m = manifest_of(fs)
    start = m['next_pack'] - (1 if mutant == 'start_below' else 0)
    packs = packs_of(nodes, mutant)
    listed = {}
    for k, pk in enumerate(packs):
        seq = start + k
        content = ('pack', gen, k, tuple(pk))
        if mutant == 'skip_existing' and fs.exists('p-%d' % seq):
            listed[seq] = content
            continue
        fs.write('p-%d.tmp' % seq, content); yield
        fs.sync('p-%d.tmp' % seq); yield
        fs.rename('p-%d.tmp' % seq, 'p-%d' % seq); yield
        listed[seq] = content
    if mutant != 'no_dir_sync':
        fs.sync_dir(); yield
    keep = dict(m['packs']) if gen != 'A' else {}
    keep.update(listed)
    retired = [s for s in m['packs'] if s not in keep]
    newm = {'gen': gen, 'packs': keep, 'next_pack': start + len(packs), 'retired': retired}
    fs.write('MANIFEST.tmp', newm); yield
    fs.sync('MANIFEST.tmp'); yield
    fs.rename('MANIFEST.tmp', 'MANIFEST'); yield 'manifest rename'
    fs.sync_dir(); yield
    for s in retired:
        fs.remove('p-%d' % s); yield


def open_check(fs, mutant, fails, trace):
    m = manifest_of(fs)
    loaded = {}
    if mutant == 'adopt_by_probe':
        x, miss = 1, 0
        while miss < 8:
            c = fs.read('p-%d' % x)
            if c is not None:
                loaded[x] = c; miss = 0
            else:
                miss += 1
            x += 1
    else:
        for s in m['packs']:
            loaded[s] = fs.read('p-%d' % s)
    for s, want in m['packs'].items():
        if loaded.get(s) != want:
            fails.append(('P1', 'listed p-%d holds %r, manifest expects %r' % (s, loaded.get(s), want), trace)); return False
    if set(loaded) != set(m['packs']):
        fails.append(('P2', 'adopted unlisted packs %r' % sorted(set(loaded) - set(m['packs'])), trace)); return False
    for name in fs.live():
        if name.startswith('p-') and not name.endswith('.tmp'):
            s = int(name[2:])
            if s not in m['packs'] and s not in m['retired'] and s < m['next_pack']:
                fails.append(('P3', 'unlisted p-%d below next_pack %d' % (s, m['next_pack']), trace)); return False
    for s, c in m['packs'].items():
        nodes = c[3]
        size = HEADER + sum(n for _, n in nodes)
        if size > max(PACK_BYTES, HEADER + max(n for _, n in nodes)):
            fails.append(('P4', 'p-%d is %d bytes' % (s, size), trace)); return False
    return True


def cleanup_steps(fs, mutant):
    m = manifest_of(fs)
    if mutant != 'no_open_sync':
        fs.sync_dir(); yield 'open dir sync'
    fs.remove('MANIFEST.tmp'); yield
    x = m['next_pack']
    while fs.exists('p-%d' % x) or fs.exists('p-%d.tmp' % x):
        fs.remove('p-%d' % x); yield
        fs.remove('p-%d.tmp' % x); yield
        x += 1
    for s in m['retired']:
        fs.remove('p-%d' % s); yield
    if mutant == 'no_open_sync':
        fs.sync_dir(); yield 'late open dir sync'


def copy(fs):
    f = FS(); f.durable = dict(fs.durable); f.pending = list(fs.pending)
    f.data = dict(fs.data); f.next_inode = fs.next_inode
    return f


def run_to_end(gen_iter):
    for _ in gen_iter:
        pass


def finish(fs, mutant, fails, trace, stats):
    """After the crashes: open, cut B, open, cut C, open."""
    if not open_check(fs, mutant, fails, trace + ['open']):
        return
    run_to_end(cleanup_steps(fs, mutant))
    for gen, nodes in (('B', NODES_B), ('C', NODES_C)):
        run_to_end(cut_steps(fs, gen, nodes, mutant))
        for f in fs.crashes():   # the completed cut's last removals are unsynced: every subset
            stats['cuts'] += 1
            if not open_check(f, mutant, fails, trace + ['cut' + gen, 'crash']):
                return
        fs.sync_dir()
        if not open_check(fs, mutant, fails, trace + ['cut' + gen, 'open']):
            return
        run_to_end(cleanup_steps(fs, mutant))


def genesis():
    fs = FS()
    fs.write('p-1', ('pack', 'G', 0, (('g1', 20),))); fs.sync('p-1')
    fs.write('MANIFEST', {'gen': 'G', 'packs': {1: ('pack', 'G', 0, (('g1', 20),))}, 'next_pack': 2, 'retired': []})
    fs.sync('MANIFEST'); fs.sync_dir()
    return fs


def explore(mutant):
    fails, stats, seen = [], {'cuts': 0, 'states': 0}, set()
    base = genesis()
    # Cut A: crash after every step (first crash), under every subset of pending directory ops.
    fs = copy(base)
    steps = cut_steps(fs, 'A', NODES_A, mutant)
    k = 0
    step = None
    while True:
        if step == 'manifest rename':
            trace = ['cutA: manifest rename', 'process crash']
            if not reopen_cuts(fs.process_crash(), mutant, fails, trace, stats, seen):
                return stats, fails
        for f1 in fs.crashes():
            stats['cuts'] += 1
            key = state_key(f1)
            if key in seen:
                continue
            seen.add(key); stats['states'] += 1
            if not reopen_cuts(f1, mutant, fails, ['cutA@%d' % k, 'power cut'], stats, seen):
                return stats, fails
        try:
            step = next(steps); k += 1
        except StopIteration:
            break
    finish(copy(fs), mutant, fails, ['cutA complete'], stats)
    return stats, fails


def state_key(fs):
    return tuple(sorted((n, str(fs.data[i][0])) for n, i in fs.durable.items()))


def reopen_cuts(fs, mutant, fails, trace, stats, seen):
    if not open_check(fs, mutant, fails, trace):
        return False
    cleanup = cleanup_steps(fs, mutant)
    for step, _ in enumerate(itertools.chain([None], cleanup)):
        for crashed in fs.crashes():
            stats['cuts'] += 1
            key = state_key(crashed)
            if key not in seen:
                seen.add(key); stats['states'] += 1
            finish(crashed, mutant, fails, trace + ['cleanup@%d' % step, 'power cut'], stats)
            if fails:
                return False
    return True


def main():
    rows = []
    for m in ('none', 'adopt_by_probe', 'skip_existing', 'no_dir_sync', 'start_below', 'close_after', 'no_open_sync'):
        st, fails = explore(m)
        if m == 'none':
            rows.append('[none] v2.5 rules: states=%d crash_cuts=%d failures=%d' % (st['states'], st['cuts'], len(fails)))
            for f in fails[:3]:
                rows.append('    %s' % (f,))
        else:
            rows.append('[%s] %s' % (m, 'CAUGHT' if fails else 'NOT CAUGHT'))
            if fails:
                rows.append('    %s' % (fails[0],))
    print('\n'.join(rows))
    return 0 if 'NOT CAUGHT' not in ''.join(rows) and 'failures=0' in rows[0] else 1


if __name__ == '__main__':
    sys.exit(main())
