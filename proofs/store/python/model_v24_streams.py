"""STORE-SPEC v2.4 streams model (v2.3 plus N11: a call record kept with an aborted reply): section 1.8 (admit, reply, S7) and section 1.3 (Aborted keeps its reply).

Store-side only: no runner. One stream, positions 0..P-1, and a data tree with one counter per position
(the effect). Every store commit is one COMMIT frame on a prefix-ordered journal (section 3.3).
A crash keeps the synced prefix plus any longer prefix of the written frames (the torn-tail cuts).
`durable(t)` returns only once frame t is synced. Exhaustive DFS over interleavings of:
  admit(p) for every p, run(p, kind) for kind in ok | err | err_silent | fault | refused after Admitted
  or Abandoned, sync, crash+reopen (at most C crashes).
Laws (the oracle is the rule text, not this store code):
  A  S7: in every recovered and live state, latest = q implies every p < q has a reply.
  B  atomic effect: the data effect of p is present iff p's reply is an ok reply; an err reply has no effect.
  C  a reply whose ticket `durable` acknowledged survives every later crash, unchanged.
  D  Rejected(l) with p < l is returned only when p has a reply (the proof RUNNER H4 relies on).
  E  Abandoned(t): t is a ticket of the current open; an earlier open's unreplied admit is re-committed.
  F  Aborted with a reply call: one frame, a streams section only, and u.ticket names it; without a reply
     call, nothing is committed and u.ticket is None.
  G  (N11) every body records its call. A frame carries a call record iff it carries a reply written by a run
     (Committed or Aborted); a call record never appears without its stream's reply op in the same frame.
Mutants (each must be caught):
  no_s7             admit commits latest+1 while latest is admitted and unreplied (v2.2 table)
  abort_keeps_data  an Aborted body's data writes are committed with its reply
  abort_drops_reply an Aborted body's reply is discarded (v2.2; the reply-on-abort rule's own mutant)
  abort_no_ticket   the reply is committed but u.ticket is None, so `durable` cannot be asked for it
  abort_drops_call  an Aborted body's call record is discarded while its reply is kept (v2.3's rule; N11)
  call_without_reply an Aborted body with no reply call commits its call record alone
"""
import sys

P, C, DEPTH = 3, 2, 14


def explore(mutant):
    fails, states, cuts = [], set(), [0]

    def replay(frames):
        data, stream = {}, None   # stream: (pos, ('A', open) | ('R', reply))
        for f in frames:
            for k, v in f['data'].items():
                data[k] = data.get(k, 0) + v
            if f['stream'] is not None:
                stream = f['stream']
        return data, stream

    def law_g(tag, st, trace):
        for i, f in enumerate(st['written']):
            replied = f['stream'] is not None and f['stream'][1][0] == 'R'
            if f.get('call') and not replied:
                fails.append((tag, 'G: frame %d has a call record without its reply' % i, trace)); return False
            if replied and not f.get('call'):
                fails.append((tag, 'G: frame %d has a reply without its call record' % i, trace)); return False
        return True

    def check(tag, st, trace):
        if not law_g(tag, st, trace):
            return False
        frames = st['written']
        data, stream = replay(frames)
        if stream is not None:
            q, s = stream
            replied = st['replies']
            for p in range(q):
                if p not in replied:
                    fails.append((tag, 'A: latest %d but %d has no reply' % (q, p), trace)); return False
        for p in range(P):
            r = st['replies'].get(p)
            eff = data.get(p, 0)
            if (r == 'ok') != (eff == 1) or eff > 1:
                fails.append((tag, 'B: p%d reply %s effect %d' % (p, r, eff), trace)); return False
        for p, r in st['acked'].items():
            if st['replies'].get(p) != r:
                fails.append((tag, 'C: acked reply of p%d lost or changed' % p, trace)); return False
        return True

    def recompute(st):
        # Replies visible in the current written journal (the store's streams tree plus history).
        reps = {}
        for f in st['written']:
            if f['stream'] is not None and f['stream'][1][0] == 'R':
                reps[f['stream'][0]] = f['stream'][1][1]
        st['replies'] = reps

    def commit(st, data, stream, call=False):
        st = dict(st, written=st['written'] + [{'data': data, 'stream': stream, 'call': call}])
        recompute(st)
        return st, len(st['written']) - 1

    def head(st):
        return replay(st['written'])[1]

    def admit(st, p):
        h = head(st)
        if h is None:
            if p == 0:
                st, t = commit(st, {}, (0, ('A', st['open'])))
                return st, ('Admitted', t)
            return st, ('Rejected', None)
        q, s = h
        if p == q + 1 and (s[0] == 'R' or mutant == 'no_s7'):
            st, t = commit(st, {}, (p, ('A', st['open'])))
            return st, ('Admitted', t)
        if p == q and s[0] == 'R':
            return st, ('Duplicate', s[1])
        if p == q and s[0] == 'A' and s[1] != st['open']:
            st, t = commit(st, {}, (p, ('A', st['open'])))
            return st, ('Abandoned', t)
        if p == q and s[0] == 'A':
            return st, ('Abandoned', st['admit_ticket'].get(p))
        return st, ('Rejected', q)

    def run(st, p, kind):
        h = head(st)
        if h is None or h[0] != p or h[1][0] != 'A':
            return st, ('Missing', None)
        if kind == 'ok':
            st, t = commit(st, {p: 1}, (p, ('R', 'ok')), call=True)
            return st, ('Committed', t)
        if kind == 'err':
            if mutant == 'abort_drops_reply':
                return st, ('Aborted', None)
            st, t = commit(st, {p: 1} if mutant == 'abort_keeps_data' else {}, (p, ('R', 'err')),
                           call=mutant != 'abort_drops_call')
            return st, ('Aborted', None if mutant == 'abort_no_ticket' else t)
        if kind == 'err_silent' and mutant == 'call_without_reply':
            st, t = commit(st, {}, None, call=True)
            return st, ('Aborted', t)
        return st, (kind, None)   # err_silent (no reply call), fault, refused: nothing committed

    seen = set()

    def key(st):
        return (tuple((tuple(sorted(f['data'].items())), f['stream'], f['call']) for f in st['written']),
                st['synced'], st['open'], st['crashes'], tuple(sorted(st['acked'].items())),
                tuple(sorted(st['admit_ticket'].items())), tuple(sorted(st['pending'].items())))

    def dfs(st, trace):
        k = key(st)
        if k in seen or len(trace) > DEPTH:
            return
        seen.add(k); states.add(k)
        if not check('live', st, trace):
            return
        moves = []
        for p in range(P):
            moves.append(('admit%d' % p, p, None))
            for kind in ('ok', 'err', 'err_silent', 'fault', 'refused'):
                moves.append(('run%d%s' % (p, kind), p, kind))
        for name, p, kind in moves:
            if kind is None:
                s2, (res, v) = admit(st, p)
                if res == 'Rejected' and v is not None and p < v and p not in s2['replies']:
                    fails.append(('live', 'D: Rejected(%d) for unreplied %d' % (v, p), trace + [name])); return
                if res in ('Admitted', 'Abandoned'):
                    if v is None or v < 0 or (res == 'Abandoned' and s2['written'][v]['stream'][1] != ('A', s2['open'])):
                        fails.append(('live', 'E: Abandoned ticket not of this open', trace + [name])); return
                    s2 = dict(s2, admit_ticket={**s2['admit_ticket'], p: v})
                if res == 'Rejected' or res == 'Duplicate':
                    continue
                dfs(s2, trace + [name])
            else:
                if st['admit_ticket'].get(p) is None:
                    continue
                s2, (res, t) = run(st, p, kind)
                if res == 'Missing':
                    continue
                if res == 'Aborted':
                    if t is None and p in s2['replies'] and s2['replies'][p] == 'err' and p not in st['replies']:
                        fails.append(('live', 'F: Aborted committed a reply without a ticket', trace + [name])); return
                    if t is None and mutant == 'abort_drops_reply':
                        fails.append(('live', 'F: Aborted dropped its reply', trace + [name])); return
                if t is not None:
                    s2 = dict(s2, pending={**s2['pending'], p: t})
                dfs(s2, trace + [name])
        # sync: everything written is now durable; `durable(t)` acks pending tickets
        if st['synced'] < len(st['written']):
            n = len(st['written'])
            acked = dict(st['acked'])
            for p, t in st['pending'].items():
                if t < n:
                    acked[p] = st['replies'].get(p)
            dfs(dict(st, synced=n, acked=acked, pending={}), trace + ['sync'])
        # crash: keep the synced prefix plus any longer prefix of written frames (prefix-ordered, section 3.3)
        if st['crashes'] < C:
            for keep in range(st['synced'], len(st['written']) + 1):
                cuts[0] += 1
                s2 = dict(st, written=st['written'][:keep], synced=keep, open=st['open'] + 1,
                          crashes=st['crashes'] + 1, admit_ticket={}, pending={})
                recompute(s2)
                if not check('recovered', s2, trace + ['crash@%d' % keep]):
                    return
                dfs(s2, trace + ['crash@%d' % keep])

    st0 = {'written': [], 'synced': 0, 'open': 1, 'crashes': 0, 'acked': {}, 'admit_ticket': {},
           'pending': {}, 'replies': {}}
    dfs(st0, [])
    return len(states), cuts[0], fails


def main():
    rows = []
    for m in ('none', 'no_s7', 'abort_keeps_data', 'abort_drops_reply', 'abort_no_ticket',
              'abort_drops_call', 'call_without_reply'):
        n, cuts, fails = explore(m)
        if m == 'none':
            rows.append('[none] v2.4 rules: states=%d crash_cuts=%d failures=%d' % (n, cuts, len(fails)))
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
