#!/usr/bin/env python3
"""STORE-SPEC v2 crash model (lane store-spec-1001), adapted from review 1006's model.py.

Changes from the reviewer's v1 model:
  * every cut is checked against an oracle: the reopened state must be one of the states the
    spec allows at that cut (the v1 model returned pass for any reopen);
  * v2 rules: the journal task writes a segment end file `j-<n>.end` (HEADER + END(d)) after a
    group's sync when its queue is empty, or after 64 groups; recovery refuses any bad frame
    below the durable end d as Corrupt and treats a bad frame at or above d as the torn tail;
  * unknown frames: MUST=0 payload limit 64 KiB, MUST=1 refuses Unsupported;
  * packs are named in the manifest and must exist and decode, or the open is Corrupt.
Each scenario is a list of named primitive steps; a cut named X crashes right after step X.
The power cut keeps synced bytes and synced directory entries (W2's loss model).
The checkpoint family also crashes only the process after MANIFEST rename, then
cuts power during reopen with each subset of its cleanup removals persisted.
"""
from __future__ import annotations
import json, struct, sys
from copy import deepcopy
from dataclasses import dataclass

UNKNOWN_LIMIT = 64 * 1024
K_HEADER, K_COMMIT, K_END, K_NODE, K_MANIFEST = 0x0001, 0x0011, 0x0012, 0x0020, 0x0030
K_FENCE = 0x0070
KNOWN = {K_HEADER, K_COMMIT, K_END, K_NODE, K_MANIFEST}

def crc32c(data: bytes) -> int:
    c = 0xffffffff
    for b in data:
        c ^= b
        for _ in range(8):
            c = (c >> 1) ^ (0x82f63b78 if c & 1 else 0)
    return c ^ 0xffffffff

def frame(kind, payload: bytes, must=True) -> bytes:
    head = struct.pack('<HHI', kind, 1 if must else 0, len(payload))
    return head + struct.pack('<I', crc32c(head + payload)) + payload

def jf(kind, obj, must=True): return frame(kind, json.dumps(obj, sort_keys=True).encode(), must)

class Corrupt(Exception): pass
class Unsupported(Exception): pass

def parse(raw: bytes, durable_end=None, tail_ok=False, fence_support=None):
    """Return (frames, valid_end). Below durable_end (or anywhere when tail_ok is False) a bad
    frame is Corrupt; at or above durable_end in the last segment it is the torn tail."""
    p, out = 0, []
    fence = None
    limit_ok = lambda p: tail_ok and (durable_end is None or p >= durable_end)
    while p < len(raw):
        def bad(why):
            if limit_ok(p): return True
            raise Corrupt(f'{why}@{p}')
        if p + 12 > len(raw):
            if bad('short head'): break
        kind, flags, n, crc = struct.unpack_from('<HHII', raw, p)
        lim = 16 * 2**20 + 64 if kind in KNOWN else UNKNOWN_LIMIT
        if kind == 0 or flags & ~1 or n > lim:
            if bad('head'): break
        if p + 12 + n > len(raw):
            if bad('short payload'): break
        payload = raw[p+12:p+12+n]
        if crc32c(raw[p:p+8] + payload) != crc:
            if bad('crc'): break
        if kind == K_FENCE and fence_support is not None:
            if n != 4 or flags != 1 or fence is not None:
                raise Corrupt('malformed fence')
            fence, = struct.unpack('<I', payload)
            if fence == 0:
                raise Corrupt('zero fence')
            if fence > fence_support:
                raise Unsupported(kind)
            out.append((kind, fence))
        elif kind not in KNOWN:
            if flags & 1: raise Unsupported(kind)
        else:
            out.append((kind, json.loads(payload)))
        p += 12 + n
    if durable_end is not None and p < durable_end:
        raise Corrupt(f'synced bytes missing below {durable_end}')
    return out, p

@dataclass
class File:
    data: bytes = b''; synced: bytes = b''

class FS:
    def __init__(s):
        s.f = {}
        s.durable_names = {}
    def _x(s, p): return s.f.setdefault(p, File())
    def write(s, p, d): s._x(p).data = bytes(d)       # open(Write) truncates
    def append(s, p, d): s._x(p).data += bytes(d)
    def sync(s, p): x = s._x(p); x.synced = x.data
    def sync_dir(s): s.durable_names = dict(s.f)
    def rename(s, a, b): s.f[b] = s.f.pop(a)
    def remove(s, p): s.f.pop(p, None)
    def crash(s, persisted_removals=()):
        for name in persisted_removals:
            s.durable_names.pop(name, None)
        s.f = dict(s.durable_names)
        for x in s.f.values():
            x.data = x.synced
    def read(s, p): x = s.f.get(p); return x.data if x else None

def header(role, seq, prev=0): return jf(K_HEADER, {'magic': 'HIRSTORE', 'format': 1, 'role': role, 'seq': seq, 'prev_end': prev})

class Store:
    """The writer, journal and checkpoint task of the spec, as primitive steps."""
    def __init__(s, fs):
        s.fs = fs; s.state = {'main': {}, 'pins': {}, 'branches': {'0': 0}, 'schema': 1}
        s.ver = 0; s.seg = 1; s.gen = 0; s.pack = 0; s.groups_since_end = 0; s.buf = b''
        s.history = [s.snap()]            # every state a commit boundary produced, in order
        s.durable_index = 0               # index into history of the newest acked state
    def snap(s): return json.loads(json.dumps({'state': s.state, 'ver': s.ver}))
    def seg_path(s, n=None): return f'j-{n or s.seg}'
    # genesis
    def genesis_steps(s):
        return [('seg write', lambda: s.fs.write('j-1', header(2, 1))),
                ('seg sync', lambda: s.fs.sync('j-1')),
                ('end write', lambda: s.fs.write('j-1.end', header(4, 1) + jf(K_END, {'d': len(header(2, 1))}))),
                ('end sync', lambda: s.fs.sync('j-1.end')),
                ('dir sync', s.fs.sync_dir)] + s.manifest_steps(first_live=1, packs=[])
    def manifest_steps(s, first_live, packs, retired=(), prev_first=None, cut=None, fence=None):
        def write():
            s.gen += 1
            c = cut or s.snap()
            st, ver = c['state'], c['ver']
            manifest = header(1, s.gen) + jf(K_MANIFEST, {
                'gen': s.gen, 'first_live': first_live, 'prev_first': prev_first or first_live,
                'packs': packs, 'retired': list(retired), 'next_pack': s.pack + 1, 'state': st, 'ver': ver})
            if fence is not None:
                level, must = fence
                manifest += frame(K_FENCE, struct.pack('<I', level), must=must)
            s.fs.write('MANIFEST.tmp', manifest)
        return [('manifest tmp write', write), ('manifest sync', lambda: s.fs.sync('MANIFEST.tmp')),
                ('manifest rename', lambda: s.fs.rename('MANIFEST.tmp', 'MANIFEST')),
                ('manifest dir sync', s.fs.sync_dir)]
    # one commit: writer publishes, journal task writes the group, syncs, acks; END when idle
    def commit(s, key, value):
        s.ver += 1; s.state['main'][key] = value; s.history.append(s.snap())
        s.buf += jf(K_COMMIT, {'b': 0, 'v': s.ver, 'k': key, 'val': value})
    def group_steps(s, idle=True):
        def write(): s.fs.append(s.seg_path(), s.buf); s.buf = b''
        def ack(): s.durable_index = len(s.history) - 1
        steps = [('group write', write), ('group sync', lambda: s.fs.sync(s.seg_path())), ('group ack', ack)]
        if idle:   # try_receive found the queue empty: record the durable end
            def end_write():
                d = len(s.fs.read(s.seg_path()))
                s.fs.write(s.seg_path() + '.end', header(4, s.seg) + jf(K_END, {'d': d}))
            steps += [('end write', end_write), ('end sync', lambda: s.fs.sync(s.seg_path() + '.end'))]
        return steps
    # a checkpoint cut: rotate, pack, manifest, removals
    def checkpoint_steps(s, change=None, compact=False, fence=None):
        old = s.seg; new = old + 1; ctx = {}
        def seg_write():
            end = len(s.fs.read(s.seg_path(old)))
            s.fs.write(s.seg_path(new), header(2, new, end)); ctx['hdr'] = len(header(2, new, end))
        def end_write(): s.fs.write(s.seg_path(new) + '.end', header(4, new) + jf(K_END, {'d': ctx['hdr']}))
        def switch():
            s.seg = new
            if change: change(); s.history.append(s.snap())
            ctx['cut'] = s.snap()          # §3.5: the Cut carries the roots as of the rotation
        def pack_write():
            s.pack += 1; ctx['p'] = s.pack
            s.fs.write(f'p-{s.pack}.tmp', header(3, s.pack) + jf(K_NODE, ctx['cut']))
        packs = lambda: ([ctx['p']] if compact else s.live_packs() + [ctx['p']])
        retired = lambda: (s.live_packs() if compact else [])
        steps = [('segment header write', seg_write), ('segment sync', lambda: s.fs.sync(s.seg_path(new))),
                 ('segment end write', end_write), ('segment end sync', lambda: s.fs.sync(s.seg_path(new) + '.end')),
                 ('segment dir sync', s.fs.sync_dir), ('rotate', switch),
                 ('pack tmp write', pack_write), ('pack sync', lambda: s.fs.sync(f'p-{ctx["p"]}.tmp')),
                 ('pack rename', lambda: s.fs.rename(f'p-{ctx["p"]}.tmp', f'p-{ctx["p"]}')),
                 ('pack dir sync', s.fs.sync_dir)]
        def mwrite():
            ctx['retired'] = retired(); ctx['packs'] = packs()
            ms = s.manifest_steps(first_live=new, packs=ctx['packs'], retired=ctx['retired'], prev_first=old,
                                  cut=ctx['cut'], fence=fence)
            ms[0][1]()
        def removals():
            for p in ctx['retired']: s.fs.remove(f'p-{p}')
            s.fs.remove(s.seg_path(old)); s.fs.remove(s.seg_path(old) + '.end')
        def ack():   # the cut's version is durable; later commits are durable only through their own groups
            s.durable_index = max(s.durable_index, next(i for i, h in enumerate(s.history) if h == ctx['cut']))
            s.packs_live = ctx['packs']
        steps += [('manifest tmp write', mwrite), ('manifest sync', lambda: s.fs.sync('MANIFEST.tmp')),
                  ('manifest rename', lambda: s.fs.rename('MANIFEST.tmp', 'MANIFEST')),
                  ('manifest dir sync', s.fs.sync_dir), ('retired removal', removals), ('checkpoint ack', ack)]
        return steps
    def live_packs(s): return list(getattr(s, 'packs_live', []))

def recover(fs, fence_support=1):
    """§3.4 v2. Returns ('ok', snapshot) | ('NotFound',) | ('Corrupt', why) | ('Unsupported', kind)."""
    try:
        m = fs.read('MANIFEST')
        if m is None: return ('NotFound',)
        fr, _ = parse(m, fence_support=fence_support)
        man = next(o for k, o in fr if k == K_MANIFEST)
        level = next((o for k, o in fr if k == K_FENCE), None)
        for p in man['packs']:
            raw = fs.read(f'p-{p}')
            if raw is None: raise Corrupt(f'pack p-{p} named but missing')
            parse(raw)
        state = json.loads(json.dumps(man['state'])); ver = man['ver']
        n = man['first_live']; segs = []
        while fs.read(f'j-{n}') is not None: segs.append(n); n += 1
        # a last segment whose header is bad is a crash during creation: treat as absent
        if segs:
            try: parse(fs.read(f'j-{segs[-1]}')[:len(header(2, segs[-1], 0)) + 16], tail_ok=True)
            except Corrupt: pass
            hdr, _ = parse(fs.read(f'j-{segs[-1]}'), durable_end=None, tail_ok=True)
            if not hdr or hdr[0][0] != K_HEADER: segs.pop()
        for i, n in enumerate(segs):
            raw = fs.read(f'j-{n}')
            if i + 1 < len(segs):           # middle segment: next header's prev_end is its durable end
                nxt, _ = parse(fs.read(f'j-{segs[i+1]}'), tail_ok=True)
                end = nxt[0][1]['prev_end']
                if len(raw) < end: raise Corrupt(f'j-{n} shorter than prev_end')
                fr, _ = parse(raw[:end])
            else:                           # last segment: the END file bounds the torn tail
                d = None
                e = fs.read(f'j-{n}.end')
                if e is not None:
                    try:
                        ef, _ = parse(e)
                        d = next(o['d'] for k, o in ef if k == K_END)
                    except (Corrupt, StopIteration):
                        d = None            # a torn or damaged END file only loses detection
                fr, _ = parse(raw, durable_end=d, tail_ok=True)
            for k, o in fr:
                if k == K_COMMIT:
                    if o['v'] != ver + 1: raise Corrupt(f'gap at {o["v"]}')
                    state['main'][o['k']] = o['val']; ver = o['v']
        result = {'state': state, 'ver': ver}
        if level is not None:
            result['fence'] = level
        return ('ok', result)
    except Corrupt as c: return ('Corrupt', str(c))
    except Unsupported as u: return ('Unsupported', u.args[0])

def run(steps, cut_index):
    for i, (_, f) in enumerate(steps):
        f()
        if i == cut_index: return

def scenario(family, compact_checkpoint=False):
    """Build (fs, store, steps). Steps are the spec's primitive steps, each a crash cut."""
    fs = FS(); s = Store(fs)
    if family != 'genesis':
        for _, f in s.genesis_steps(): f()
        s.commit('base', 1)
        for _, f in s.group_steps(): f()
    if family == 'commit':
        s.commit('k', 2); fr = s.buf; s.buf = b''
        def ack(): s.durable_index = len(s.history) - 1
        steps = [('before write', lambda: None),
                 ('during write', lambda: fs.append(s.seg_path(), fr[:len(fr) // 2])),
                 ('after write before sync', lambda: fs.append(s.seg_path(), fr[len(fr) // 2:])),
                 ('after sync before ack', lambda: fs.sync(s.seg_path())),
                 ('after ack', ack)] + s.group_steps()[3:]          # + END write, END sync
    elif family == 'group':
        s.commit('a', 2); s.commit('b', 3)
        steps = [('first handoff', lambda: None)] + s.group_steps()
    elif family == 'checkpoint':
        s.commit('c', 2)                                        # a commit still in the old segment's group
        for _, f in s.group_steps(idle=False): f()
        if compact_checkpoint:
            for _, f in s.checkpoint_steps(): f()               # a listed pack for the next cut to retire
        cp = s.checkpoint_steps(compact=compact_checkpoint)
        post = [('post-cut commit', lambda: s.commit('after', 99))] + s.group_steps()
        steps = cp[:6] + post[:1] + cp[6:] + post[1:]           # a commit lands in the new segment mid-checkpoint
    elif family == 'compaction':
        for _, f in s.checkpoint_steps(): f()                   # pack 1, later retired
        s.commit('c', 9)
        for _, f in s.group_steps(): f()
        steps = s.checkpoint_steps(compact=True)
    elif family in ('pin', 'fork', 'schema'):
        change = {'pin': lambda: s.state['pins'].__setitem__('p', s.ver),
                  'fork': lambda: s.state['branches'].__setitem__('1', s.ver),
                  'schema': lambda: s.state.__setitem__('schema', 2)}[family]
        steps = s.checkpoint_steps(change=change)
    elif family == 'open':
        torn = jf(K_COMMIT, {'b': 0, 'v': 2, 'k': 'torn', 'val': 0})
        valid = len(fs.read(s.seg_path()))
        fs.append(s.seg_path(), torn[:-3]); fs.sync(s.seg_path())     # torn bytes reached the disk
        def rotate_hdr(): fs.write('j-2', header(2, 2, valid)); s.seg = 2
        def rotate_end(): fs.write('j-2.end', header(4, 2) + jf(K_END, {'d': len(header(2, 2, valid))}))
        steps = [('lock', lambda: None), ('manifest read', lambda: None), ('open dir sync', fs.sync_dir)]
        steps += [(n, lambda: None) for n in ('leftovers', 'pack read', 'journal read', 'apply')]
        steps += [('rotate header write', rotate_hdr), ('rotate end write', rotate_end),
                  ('rotate sync', lambda: (fs.sync('j-2'), fs.sync('j-2.end'))), ('rotate dir sync', fs.sync_dir),
                  ('task start', lambda: None)]
        s.commit('X', 5); fr = s.buf; s.buf = b''
        def ack(): s.durable_index = len(s.history) - 1
        steps += [('commit X write', lambda: fs.append('j-2', fr)), ('commit X sync', lambda: fs.sync('j-2')), ('commit X ack', ack)]
        s.history[-1]['ver'] = 2; s.history[-1]['state']['main']['X'] = 5   # X is version 2 after the torn v2 is dropped
        fr = jf(K_COMMIT, {'b': 0, 'v': 2, 'k': 'X', 'val': 5})
    elif family == 'genesis':
        steps = s.genesis_steps()
    return fs, s, steps

FAMILIES = [('genesis', 0), ('commit', 5), ('group', 4), ('checkpoint', 12), ('compaction', 9), ('pin', 10), ('fork', 10), ('schema', 10), ('open', 9)]

def check_cut(family, i):
    fs, s, steps = scenario(family)
    run(steps, i)
    allowed = s.history[s.durable_index:]
    fs.crash()
    got = recover(fs)
    if family == 'genesis' and got == ('NotFound',) and s.gen == 0 or (family == 'genesis' and got == ('NotFound',) and i < len(steps) - 1):
        return True, steps[i][0], got                       # before the manifest is durable there is no store
    ok = got[0] == 'ok' and got[1] in [{'state': h['state'], 'ver': h['ver']} for h in allowed]
    return ok, steps[i][0], got

def checkpoint_reopen(mutant):
    fs, old_process, steps = scenario('checkpoint', compact_checkpoint=True)
    for name, step in steps:
        step()
        if name == 'manifest rename':
            break
    # The process and its tasks are gone. The live filesystem stays as it was.
    expected = old_process.history[old_process.durable_index]
    del old_process, steps, step
    frames, _ = parse(fs.read('MANIFEST'))
    man = next(obj for kind, obj in frames if kind == K_MANIFEST)
    cleanup = [f'p-{p}' for p in man['retired']]
    for seg in range(man['prev_first'], man['first_live']):
        cleanup.extend((f'j-{seg}', f'j-{seg}.end'))
    assert man['retired'] and all(fs.read(name) is not None for name in cleanup)
    if mutant != 'no_open_sync':
        fs.sync_dir()
    cuts = []
    for n in range(len(cleanup) + 1):
        for mask in range(1 << n):
            cut = deepcopy(fs)
            removed = cleanup[:n]
            for name in removed:
                cut.remove(name)
            kept = [name for i, name in enumerate(removed) if mask & (1 << i)]
            cut.crash(persisted_removals=kept)
            got = recover(cut)
            cuts.append((got == ('ok', expected), f'cleanup@{n}, persisted removals {kept}', got))
    return cuts

def damage():
    out = {}
    def base(idle=True):
        fs = FS(); s = Store(fs)
        for _, f in s.genesis_steps(): f()
        s.commit('x', 1)
        for _, f in s.group_steps(idle=idle): f()
        return fs, s
    def flip(fs, p, off):
        x = fs.f[p]; b = bytearray(x.synced); b[off] ^= 1; x.synced = x.data = bytes(b)
    fs, s = base(); flip(fs, 'j-1', -1); fs.crash(); out['D1 flip in synced last frame (idle END written)'] = (recover(fs)[0], 'Corrupt')
    fs, s = base(); s.commit('y', 2); fs.append('j-1', s.buf[:-4]); fs.sync('j-1'); fs.crash()
    out['D2 short last frame (torn)'] = (recover(fs)[0] + ':' + str(recover(fs)[1].get('ver') if recover(fs)[0] == 'ok' else ''), 'ok:1')
    fs, s = base(); s.commit('y', 2); fr = s.buf; fs.append('j-1', fr[:12] + b'\0' * (len(fr) - 12)); fs.sync('j-1'); fs.crash()
    out['D3 full-length frame, payload zeroed, above END (torn sectors)'] = (recover(fs)[0] + ':' + str(recover(fs)[1].get('ver') if recover(fs)[0] == 'ok' else ''), 'ok:1')
    fs, s = base()
    for _, f in s.checkpoint_steps.__func__(s) if False else []: f()
    fs2, s2 = base(); s2.commit('m', 2)
    for _, f in s2.group_steps(): f()
    rot = [x for x in s2.checkpoint_steps()]
    for n, f in rot[:6]: f()                       # rotate only: j-1 becomes a middle segment
    # make the manifest still point at j-1 so j-1 is read as a middle segment
    flip(fs2, 'j-1', len(header(2, 1)) + 14); fs2.crash()
    out['D4 flip in a middle segment frame'] = (recover(fs2)[0], 'Corrupt')
    fs, s = base(); flip(fs, 'j-1.end', -2); fs.crash()
    r = recover(fs); out['D5 flip in the END file'] = (r[0] + ':' + str(r[1]['ver'] if r[0] == 'ok' else ''), 'ok:1')
    fs, s = base(idle=False); flip(fs, 'j-1', -1); fs.crash()
    r = recover(fs); out['D6 flip in a synced frame not yet covered by END (busy window; stated residual)'] = (r[0] + ':' + str(r[1]['ver'] if r[0] == 'ok' else ''), 'ok:0')
    fs, s = base(); fs.f['j-1'].synced = fs.f['j-1'].data = fs.f['j-1'].synced[:-3]; fs.crash()
    out['D7 synced bytes truncated below END'] = (recover(fs)[0], 'Corrupt')
    for must, n, want, name in ((False, 100, 'ok', 'D8 unknown MUST=0 kind, 100 bytes, skipped'),
                                (False, UNKNOWN_LIMIT + 1, 'Corrupt', 'D9 unknown MUST=0 kind over 64 KiB below END'),
                                (True, 10, 'Unsupported', 'D10 unknown MUST=1 kind')):
        fs = FS(); s = Store(fs)
        for _, f in s.genesis_steps(): f()
        fs.append('j-1', frame(0x0200, b'z' * n, must)); s.commit('x', 1)
        for _, f in s.group_steps(): f()
        fs.crash(); out[name] = (recover(fs)[0], want)
    fs, s = base()
    for _, f in s.checkpoint_steps(): f()
    flip(fs, 'p-1', -2); fs.crash(); out['D11 flip in a pack'] = (recover(fs)[0], 'Corrupt')
    fs, s = base(); flip(fs, 'MANIFEST', -2); fs.crash(); out['D12 flip in MANIFEST'] = (recover(fs)[0], 'Corrupt')
    # L10: torn tail, reopen with rotation, new commit X, durable, crash, reopen: X present
    fs, s = base(); s.commit('torn', 2); fs.append('j-1', s.buf[:-4]); s.buf = b''; fs.sync('j-1'); fs.crash()
    r = recover(fs); valid = len(fs.read('j-1')) - (len(jf(K_COMMIT, {'b': 0, 'v': 2, 'k': 'torn', 'val': 2})) - 4)
    fs.write('j-2', header(2, 2, valid)); fs.write('j-2.end', header(4, 2) + jf(K_END, {'d': len(header(2, 2, valid))}))
    fs.sync('j-2'); fs.sync('j-2.end'); fs.sync_dir()
    fs.append('j-2', jf(K_COMMIT, {'b': 0, 'v': 2, 'k': 'X', 'val': 7})); fs.sync('j-2'); fs.crash()
    r2 = recover(fs); out['L10 torn tail, reopen, commit X durable, crash: X present'] = ('ok:X' if r2[0] == 'ok' and r2[1]['state']['main'].get('X') == 7 else str(r2), 'ok:X')
    return out

def activation_fixture(legacy):
    fs = FS()
    store = Store(fs)
    store.state['activation'] = {'current': 'old', 'previous': None, 'releases': {'1': 'old'}} if legacy else None
    store.ver = int(legacy)
    store.history = [store.snap()]
    run(store.genesis_steps(), -1)
    return fs, store


def mutate_release(store, image):
    store.ver += 1
    previous = store.state['activation']
    releases = dict(previous['releases']) if previous else {}
    releases[str(store.ver)] = image
    store.state['schema'] = 2
    store.state['activation'] = {'current': image, 'previous': previous and previous['current'], 'releases': releases}


def activation_cuts(mutant):
    """The first install must recover one complete generation."""
    results = []
    for legacy in (False, True):
        fs, store = activation_fixture(legacy)
        expected_old = store.snap()
        initial_version = store.ver
        new_releases = {'1': 'old', '2': 'new'} if legacy else {'1': 'new'}
        expected_new = deepcopy(expected_old)
        expected_new['ver'] = initial_version + 1
        expected_new['fence'] = 1
        expected_new['state']['schema'] = 2
        expected_new['state']['activation'] = {
            'current': 'new',
            'previous': 'old' if legacy else None,
            'releases': new_releases,
        }
        new_fence = None if mutant == 'split_am2' else (1, mutant != 'fence_optional')
        steps = store.checkpoint_steps(lambda: mutate_release(store, 'new'), compact=True, fence=new_fence)
        for name, step in steps:
            step()
            for durable_rename in (False, True) if name == 'manifest rename' else (False,):
                cut = deepcopy(fs)
                if durable_rename:
                    cut.sync_dir()
                cut.crash()
                got = recover(cut)
                before = got == ('ok', expected_old)
                after = got == ('ok', expected_new)
                if name in ('manifest dir sync', 'retired removal', 'checkpoint ack') or durable_rename:
                    before = False
                older = recover(cut, fence_support=None)
                ok = (before and older == ('ok', expected_old)) or (after and older == ('Unsupported', K_FENCE))
                results.append((ok, f'{legacy=}, {name}, {durable_rename=}', got))
        if mutant == 'split_am2':
            # The fence arrives in a later, separately durable checkpoint.
            run(store.checkpoint_steps(compact=True, fence=(1, True)), -1)
    return results


def probe_activation(mutant):
    """A probe with an older activation version must leave the release alone."""
    fs, store = activation_fixture(False)
    run(store.checkpoint_steps(lambda: mutate_release(store, 'new'), fence=(1, True)), -1)
    before = recover(fs)
    if mutant == 'open_writes_activation':
        run(store.checkpoint_steps(lambda: mutate_release(store, 'old'), fence=(1, True)), -1)
    fs.crash()
    return recover(fs) == before


def refuse_fence_level_two(mutant):
    fs, store = activation_fixture(False)
    run(store.checkpoint_steps(lambda: mutate_release(store, 'new'), fence=(2, True)), -1)
    supported = 2 if mutant == 'no_level_check' else 1
    return recover(fs, fence_support=supported) == ('Unsupported', K_FENCE)


def activation_controls(mutants):
    cuts = activation_cuts(None)
    failures = [name for ok, name, _ in cuts if not ok]
    print(f'activation: {len(cuts) - len(failures)}/{len(cuts)} cuts recovered complete generations')
    good = not failures and probe_activation(None) and refuse_fence_level_two(None)
    for mutant in mutants:
        if mutant == 'no_level_check':
            caught = not refuse_fence_level_two(mutant)
        elif mutant == 'open_writes_activation':
            caught = not probe_activation(mutant)
        else:
            failure = next(((name, got) for ok, name, got in activation_cuts(mutant) if not ok), None)
            caught = failure is not None
            if failure:
                print(f'  {failure[0]} recovered {failure[1]}')
        print(f'[{mutant}] {"CAUGHT" if caught else "SURVIVED"}')
        good = good and caught
    return not good


def install_recovery(mutant):
    fs, store = activation_fixture(False)
    store.ver = 7
    store.state.update(main={'A': 5, 'B': 10}, catalog={'A': 2, 'B': 3},
                       next_table=4, next_node=12,
                       activation={'current': 'old', 'previous': None, 'releases': {'1': 'old'}})
    store.history = [store.snap()]
    run(store.manifest_steps(1, [], fence=(1, True)), -1)
    old = store.snap() | {'fence': 1}
    expected = deepcopy(old)
    expected['ver'] = 8
    expected['state'].update(main={'A': 105, 'B': 210}, catalog={'A': 4, 'B': 5, 'C': 6},
                             next_table=7, next_node=22,
                             activation={'current': 'new', 'previous': 'old', 'releases': {'1': 'old', '8': 'new'}})
    installed_in_memory = deepcopy(old)
    staged = deepcopy(expected)
    if mutant == 'cut_without_advanced_counters':
        staged['state'].update(next_table=4, next_node=12)
    if mutant == 'graft_parents_inside_block':
        staged['state']['next_node'] = 21
    if mutant == 'table_on_schema_appends':
        staged['ver'] += 1
    if mutant == 'activation_from_cutover':
        staged['state']['activation']['current'] = 'third'
    if mutant == 'per_operation_cuts':
        staged['state']['catalog'].update(B=3)
    if mutant == 'publish_before_ack':
        installed_in_memory = staged
    def change():
        store.ver = staged['ver']
        store.state = staged['state']
    prefix = [('lease', lambda: None), ('export', lambda: None), ('prepare', lambda: None),
              ('import', lambda: None), ('settle', lambda: None), ('validate', lambda: None)]
    steps = prefix + store.checkpoint_steps(change, compact=True, fence=(1, True))
    failures = []
    for name, step in steps:
        step()
        for keep_rename in (False, True) if name == 'manifest rename' else (False,):
            cut = deepcopy(fs)
            if keep_rename:
                cut.sync_dir()
            cut.crash()
            observed = recover(cut)
            committed = keep_rename or name in ('manifest dir sync', 'retired removal', 'checkpoint ack')
            desired = expected if committed else old
            if observed != ('ok', desired):
                failures.append((name, 'recovered', observed))
            if not committed and installed_in_memory != old:
                failures.append((name, 'published before ack', installed_in_memory))
    return len(steps) + 1, failures


def install_requests(mutant):
    base = {'attempt': 9, 'digest': 'prepared B', 'version': 7, 'next_table': 4, 'activation': 1}
    inventory = {'new table': 2, 'new empty index': 0, 'dropped table': 0}
    failures = []
    def check(request, live_version, live_next, offered, pack_bytes, budget):
        if (request != (base['attempt'], base['digest']) and mutant != 'base_fence_only') or offered != inventory:
            return 'Import'
        if (live_version != base['version'] and mutant != 'no_v4_version') or live_next != base['next_table']:
            return 'Drift'
        if pack_bytes > budget and mutant != 'no_c4':
            return 'FullNew'
        return 'Installed'
    cases = (
        ('same base different candidate', ((9, 'prepared C'), 7, 4, inventory, 2, 2), 'Import'),
        ('omitted empty index', ((9, 'prepared B'), 7, 4, {'new table': 2, 'dropped table': 0}, 2, 2), 'Import'),
        ('journal drift', ((9, 'prepared B'), 8, 4, inventory, 2, 2), 'Drift'),
        ('schema drift without allocating ids', ((9, 'prepared B'), 8, 4, inventory, 2, 2), 'Drift'),
        ('largest pack exceeds limit', ((9, 'prepared B'), 7, 4, inventory, 3, 2), 'FullNew'),
        ('current candidate exactly fits', ((9, 'prepared B'), 7, 4, inventory, 2, 2), 'Installed'),
    )
    for name, args, want in cases:
        got = check(*args)
        if got != want:
            failures.append((name, 'wanted', want, 'got', got))
    # After a schema cut, its acknowledged row is only in the new manifest and
    # pack. Refreshing the journal from the export's version cannot get it.
    latest = {'updated': 13, 'acknowledged schema row': 41}
    retry = {'updated': 13} if mutant == 'journal_only_refresh' else dict(latest)
    if retry != latest:
        failures.append(('drift retry', retry))
    return len(cases) + 1, failures


def install_controls(mutants):
    cuts, recovery_red = install_recovery(None)
    cases, candidate_red = install_requests(None)
    failures = recovery_red + candidate_red
    for row in failures:
        print('FAIL', row)
    print(f'install: {cuts + cases - len(failures)}/{cuts + cases} recovery cuts and requests passed')
    good = not failures
    for mutant in mutants:
        _, a = install_recovery(mutant)
        _, b = install_requests(mutant)
        failures = a + b
        good = good and bool(failures)
        print(f'[{mutant}] {"CAUGHT" if failures else "SURVIVED"}', failures[:1])
    return not good


def main():
    total = bad = 0; rows = []
    for fam, n in FAMILIES:
        ok_n = 0
        n_steps = len(scenario(fam)[2]); assert n_steps >= n, (fam, n_steps, n); n = n_steps
        for i in range(n):
            ok, name, got = check_cut(fam, i); total += 1; ok_n += ok; bad += (not ok)
            rows.append((fam, name, 'PASS' if ok else f'FAIL {got}'))
        if fam == 'checkpoint':
            double_crashes = checkpoint_reopen(mutant='none')
            for ok, name, got in double_crashes:
                total += 1; ok_n += ok; bad += not ok
                rows.append((fam, 'process crash, reopen, power cut ' + name, 'PASS' if ok else f'FAIL {got}'))
            n += len(double_crashes)
        print(f'{fam}: {ok_n}/{n} cuts match the oracle (review counted {dict(FAMILIES)[fam]})')
    print(f'TOTAL_CUTS={total} FAILED={bad}')
    dmg = damage(); dbad = 0
    for k, (got, want) in dmg.items():
        st = 'PASS' if got == want else 'FAIL'; dbad += st == 'FAIL'
        print(f'{st}  {k}: got {got}, spec says {want}')
    print(f'DAMAGE_CASES={len(dmg)} FAILED={dbad}')
    mutant_failures = [(name, got) for ok, name, got in checkpoint_reopen('no_open_sync') if not ok]
    missing_pack = next(((name, got) for name, got in mutant_failures
                         if got[0] == 'Corrupt' and 'named but missing' in got[1]), None)
    print(f'[no_open_sync] CAUGHT {missing_pack}' if missing_pack else '[no_open_sync] NOT CAUGHT')
    for r in rows:
        if r[2] != 'PASS': print('  ', r)
    return 1 if bad or dbad or not missing_pack else 0

if __name__ == '__main__':
    if sys.argv[1:2] == ['activation']:
        sys.exit(activation_controls(sys.argv[2:]))
    if sys.argv[1:2] == ['install']:
        sys.exit(install_controls(sys.argv[2:]))
    sys.exit(main())
