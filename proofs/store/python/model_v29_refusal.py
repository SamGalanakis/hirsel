"""STORE-SPEC v2.9 refusal model: a checkpoint cut refused with Capacity part way, then retried.

store-build-1010's S2 task-capacity gap (followups.md, "S2 task-capacity gap").

What is modelled.
  The on-disk node graph: nodes with ids, sizes and children, built by path-copy commits.
  The checkpoint task's per-node table: id -> (pack seq, size, reference count).
  A cut (section 3.5): Write (new nodes, children first, into packs from the manifest's next_pack, each
  p-seq.tmp written, synced, renamed), Count (increment children of new nodes and the new root; decrement
  the old root and cascade zeros), Compact (retire packs with no live bytes), Publish (sync_dir,
  MANIFEST.tmp, sync, rename, sync_dir), Remove (retired packs, no sync).
  Every step that allocates is a refusal point: each new record, each pack buffer, each count update
  (the table is persistent, so an update allocates a path copy), the manifest buffer.

Rule under test (v2.9, Q1):
  (a) a cut allocates only before its MANIFEST rename; names and the ack are built before it;
  (b) the cut works on a new version of the table (shared structure) and the task installs it only
     after the MANIFEST's sync_dir; a refusal drops that version, so the table is exactly as before;
  (c) a refused cut changes no durable state: its packs are leftovers at seq >= next_pack (H2); the
     task keeps no counter of its own, so the next cut starts from the manifest's next_pack again;
  (d) the task stays alive and acks Refused(g, c); the next cut retries from the current roots.
File system: as model_v25_packs.py. A crash keeps ANY SUBSET of pending directory operations; unsynced
file data is lost.

Scenario. Genesis: root r0 -> (l1, l2) in p-1. Commits: c1 replaces l1 by l3 (root r1), c2 replaces l2
by l4 (root r2), c3 replaces l3 by l5 (root r3). Cut X takes r2 and is refused at each of its refusal
points in turn (and once runs to the end). A crash is cut right after the refusal under every subset.
Then cut Y takes r3 and runs to the end with a crash after every step under every subset, and the
store opens.
Checks:
  P1 every listed pack exists and holds what its cut wrote;
  P3 an unlisted, unretired p-file has seq >= next_pack;
  P5 every node reachable from the manifest's root is in a listed pack (no lost node);
  P6 after each cut, refused or completed, the task's table equals the table rebuilt from the manifest
     (each reachable node's pack, size and in-degree, +1 for the root);
  L  after a refusal, cut Y completes and its manifest names r3.
Mutants (each must be caught):
  inplace_counts       the cut updates the live table in place and a refusal leaves the partial work
  advance_next_pack    the task keeps its own next_pack and advances it past a refused cut's packs
  alloc_after_publish  a refusal can happen after the MANIFEST rename (at the removals) and drops the
                       new table although the new manifest is durable
  refusal_ends_task    the task ends on a refusal, as it does on an IoError
"""
import sys

PACK_BYTES = 60

NODES = {
    'r0': (20, ('l1', 'l2')), 'l1': (30, ()), 'l2': (30, ()),
    'l3': (30, ()), 'r1': (20, ('l3', 'l2')),
    'l4': (30, ()), 'r2': (20, ('l3', 'l4')),
    'l5': (30, ()), 'r3': (20, ('l5', 'l4')),
}


class Refused(Exception):
    pass


class FS:
    def __init__(s):
        s.durable, s.pending, s.data, s.next_inode = {}, [], {}, 1

    def live(s):
        d = dict(s.durable)
        for op in s.pending:
            apply(d, op)
        return d

    def write(s, name, content):
        i = s.next_inode; s.next_inode += 1
        s.data[i] = (content, False); s.pending.append(('link', name, i))

    def sync(s, name):
        i = s.live()[name]; s.data[i] = (s.data[i][0], True)

    def rename(s, a, b):
        if a in s.live():
            s.pending.append(('rename', a, b))

    def remove(s, name):
        if name in s.live():
            s.pending.append(('remove', name))

    def sync_dir(s):
        s.durable = s.live(); s.pending = []

    def read(s, name):
        d = s.live()
        return s.data[d[name]][0] if name in d else None

    def crashes(s):
        for mask in range(1 << len(s.pending)):
            f = FS(); d = dict(s.durable)
            for k, op in enumerate(s.pending):
                if mask >> k & 1:
                    apply(d, op)
            f.durable = d
            f.data = {i: ((c if ok else 'TORN'), ok) for i, (c, ok) in s.data.items()}
            f.next_inode = s.next_inode
            yield f

    def copy(s):
        f = FS(); f.durable = dict(s.durable); f.pending = list(s.pending)
        f.data = dict(s.data); f.next_inode = s.next_inode
        return f


def apply(d, op):
    if op[0] == 'link':
        d[op[1]] = op[2]
    elif op[0] == 'rename':
        if op[1] in d:
            d[op[2]] = d.pop(op[1])
    elif op[0] == 'remove':
        d.pop(op[1], None)


class Task:
    def __init__(s, table, mutant):
        s.table = table          # id -> (seq, size, count); the installed version
        s.mutant = mutant
        s.alive = True
        s.next_pack = None       # only the advance_next_pack mutant keeps one


def reachable(root):
    out, st = [], [root]
    while st:
        n = st.pop()
        if n in out:
            continue
        out.append(n); st.extend(NODES[n][1])
    return out


def children_first(root, skip):
    out = []

    def go(n):
        if n in skip or n in out:
            return
        for c in NODES[n][1]:
            go(c)
        out.append(n)
    go(root)
    return out


def cut(fs, task, gen, root, refuse_at, events):
    """Runs one cut. Allocation points call alloc(); a crash point is a yield."""
    point = [0]

    def alloc():
        point[0] += 1
        if point[0] == refuse_at:
            raise Refused()

    m = fs.read('MANIFEST')
    old_root = m['root']
    # (b): a new version of the table. The inplace_counts mutant writes the installed one.
    t = task.table if task.mutant == 'inplace_counts' else dict(task.table)
    start = m['next_pack']
    if task.mutant == 'advance_next_pack' and task.next_pack is not None:
        start = task.next_pack
    try:
        # 1. Write
        new = children_first(root, set(t))
        packs, cur, size = [], [], 0
        for n in new:
            sz = NODES[n][0]
            if cur and size + sz > PACK_BYTES:
                packs.append(cur); cur, size = [], 0
            cur.append(n); size += sz
        if cur:
            packs.append(cur)
        listed = {}
        for k, pk in enumerate(packs):
            seq = start + k
            if task.mutant == 'advance_next_pack':
                task.next_pack = seq + 1
            alloc()                                       # the pack buffer
            content = ('pack', gen, k, tuple((n, NODES[n][0], NODES[n][1]) for n in pk))
            for n in pk:
                alloc()                                   # the node's record
                t[n] = (seq, NODES[n][0], 0)
            fs.write('p-%d.tmp' % seq, content); yield
            fs.sync('p-%d.tmp' % seq); yield
            fs.rename('p-%d.tmp' % seq, 'p-%d' % seq); yield
            listed[seq] = content
        # 2. Count
        def bump(n, d):
            alloc()                                       # a path copy of the table
            seq, sz, c = t[n]; t[n] = (seq, sz, c + d)
        for n in new:
            for c in NODES[n][1]:
                bump(c, +1)
        bump(root, +1)
        dead, st = [], [old_root]
        while st:
            n = st.pop()
            bump(n, -1)
            if t[n][2] == 0:
                dead.append(n); st.extend(NODES[n][1])
        for n in dead:
            alloc(); del t[n]
        # 3. Compact: retire packs with no live node
        packs_now = dict(m['packs']); packs_now.update(listed)
        live_seqs = {v[0] for v in t.values()}
        retired = [s for s in packs_now if s not in live_seqs]
        keep = {s: c for s, c in packs_now.items() if s in live_seqs}
        alloc()                                           # the manifest buffer and the removal names
        # 4. Publish
        fs.sync_dir(); yield
        newm = {'gen': gen, 'root': root, 'packs': keep, 'next_pack': start + len(packs), 'retired': retired}
        fs.write('MANIFEST.tmp', newm); yield
        fs.sync('MANIFEST.tmp'); yield
        fs.rename('MANIFEST.tmp', 'MANIFEST'); yield
        fs.sync_dir(); yield
        if task.mutant == 'alloc_after_publish':
            alloc()                                       # a removal name built only now
        task.table = t                                    # (b): install after the sync_dir
        task.next_pack = None if task.mutant != 'advance_next_pack' else newm['next_pack']
        # 5. Remove
        for s in retired:
            fs.remove('p-%d' % s); yield
        events.append(('Checkpointed', gen))
    except Refused:
        events.append(('Refused', gen))
        if task.mutant == 'refusal_ends_task':
            task.alive = False


def open_check(fs, trace, fails):
    m = fs.read('MANIFEST')
    if m in (None, 'TORN'):
        fails.append(('P0', 'MANIFEST lost', trace)); return False
    for s, want in m['packs'].items():
        got = fs.read('p-%d' % s)
        if got != want:
            fails.append(('P1', 'listed p-%d holds %r' % (s, got), trace)); return False
    for name in fs.live():
        if name.startswith('p-') and not name.endswith('.tmp'):
            s = int(name[2:])
            if s not in m['packs'] and s not in m['retired'] and s < m['next_pack']:
                fails.append(('P3', 'unlisted p-%d below next_pack %d' % (s, m['next_pack']), trace)); return False
    have = {n for c in m['packs'].values() for (n, _, _) in c[3]}
    for n in reachable(m['root']):
        if n not in have:
            fails.append(('P5', 'node %s of root %s is in no listed pack' % (n, m['root']), trace)); return False
    return True


def table_of(fs):
    m = fs.read('MANIFEST')
    where = {n: s for s, c in m['packs'].items() for (n, _, _) in c[3]}
    t = {}
    for n in reachable(m['root']):
        t[n] = (where.get(n), NODES[n][0], 0)
    for n in reachable(m['root']):
        for c in NODES[n][1]:
            s, sz, k = t[c]; t[c] = (s, sz, k + 1)
    s, sz, k = t[m['root']]; t[m['root']] = (s, sz, k + 1)
    return t


def genesis():
    fs = FS()
    content = ('pack', 'G', 0, tuple((n, NODES[n][0], NODES[n][1]) for n in ('l1', 'l2', 'r0')))
    fs.write('p-1', content); fs.sync('p-1')
    fs.write('MANIFEST', {'gen': 'G', 'root': 'r0', 'packs': {1: content}, 'next_pack': 2, 'retired': []})
    fs.sync('MANIFEST'); fs.sync_dir()
    return fs


def run(gen_iter):
    for _ in gen_iter:
        pass


def explore(mutant):
    fails, stats = [], {'refusals': 0, 'cuts': 0, 'states': 0}
    base = genesis()
    base_task = Task(table_of(base), mutant)
    # Count cut X's refusal points with a dry run.
    n_points = 0
    while True:
        f, tk, ev = base.copy(), Task(dict(base_task.table), 'none'), []
        run(cut(f, tk, 'X', 'r2', n_points + 1, ev))
        if ev[-1][0] == 'Checkpointed':
            break
        n_points += 1
    extra = 1 if mutant == 'alloc_after_publish' else 0
    for refuse_at in list(range(1, n_points + 1 + extra)) + [0]:   # 0: X runs to the end
        fs, task, events = base.copy(), Task(dict(base_task.table), mutant), []
        run(cut(fs, task, 'X', 'r2', refuse_at, events))
        trace = ['X refused@%d' % refuse_at if refuse_at else 'X complete']
        if refuse_at:
            stats['refusals'] += 1
        for f in fs.crashes():                          # a crash right after the refusal
            stats['cuts'] += 1; stats['states'] += 1
            if not open_check(f, trace + ['crash'], fails):
                return stats, fails
        if task.table != table_of(fs):
            fails.append(('P6', 'table after X (%s) differs from the manifest' % events[-1][0], trace)); return stats, fails
        if not task.alive:
            fails.append(('L', 'the task ended on a refusal; cut Y is never acked', trace)); return stats, fails
        # Cut Y: crash after every step, under every subset.
        g = fs.copy(); gt = Task(dict(task.table), mutant); gt.next_pack = task.next_pack
        steps = cut(g, gt, 'Y', 'r3', None, events)
        k = 0
        while True:
            for f in g.crashes():
                stats['cuts'] += 1; stats['states'] += 1
                if not open_check(f, trace + ['Y@%d' % k, 'crash'], fails):
                    return stats, fails
            try:
                next(steps); k += 1
            except StopIteration:
                break
            except KeyError as e:
                fails.append(('P6', 'cut Y found no record for %s: the table disagrees with the manifest' % e, trace))
                return stats, fails
        g.sync_dir()
        if events[-1] != ('Checkpointed', 'Y') or g.read('MANIFEST')['root'] != 'r3':
            fails.append(('L', 'cut Y did not complete: %r' % (events[-1],), trace)); return stats, fails
        if gt.table != table_of(g):
            fails.append(('P6', 'table after Y differs from the manifest', trace)); return stats, fails
        if not open_check(g, trace + ['Y complete', 'open'], fails):
            return stats, fails
    stats['points'] = n_points
    return stats, fails


def main():
    rows = []
    for m in ('none', 'inplace_counts', 'advance_next_pack', 'alloc_after_publish', 'refusal_ends_task'):
        st, fails = explore(m)
        if m == 'none':
            rows.append('[none] v2.9 rules: refusal_points=%d refusals=%d crash_cuts=%d failures=%d'
                        % (st.get('points', -1), st['refusals'], st['cuts'], len(fails)))
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
