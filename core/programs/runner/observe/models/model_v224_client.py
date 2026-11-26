"""STORE-SPEC v2.24 request-identity model: section 1.8, `admit` with a request digest.

One stream, one client, and the store's v2.24 admit table as the server. `admit` now takes the request digest and
the stream value is (p, digest, Admitted | Replied(reply)). At an equal p the store compares the digest before
answering Duplicate or Abandoned (a mismatch is RequestMismatch, nothing committed, nothing run); below latest it
answers HistoryUnavailable (terminal; the client retires the stream).

Store: latest position, its bound digest and its state; the open number that admitted it; per-position history of
the request that ran (for the oracle only). A delivered send is refused before admission (a choice per delivery,
nothing recorded) or goes to `admit`. Admitted attempts reply later only while the stream is Admitted(p) with
their own digest (else Missing("reply")), or fault. One server reopen: running attempts die, the stream state
stays, and an Admitted position of the earlier open answers Abandoned with a re-commit (digest compared first, kept).
Client: requests 0..R-1 with distinct bytes; durable pending (p, request, sends so far); at most S sends per request;
one client crash. Every send carries a new deadline and slot (the send counter); the digest covers only the
request's bytes (entry Key and args), so a retry of the same request has the same digest.

Client configurations. `correct` is the client of section 1.8 (P1). The other three are the v2.23 client mutants,
here run as BUGGY CLIENTS: under v2.23 law I caught each of them; under v2.24 the store must keep law I true for
them anyway, which is the request-identity guarantee. `advance_on_refusal` breaks law H (a hole), which no store check can repair.
  reuse_while_live    after a refusal, p is reused for the next request while an earlier send may still be admitted
  recover_new_bytes   after a crash, the next request is sent at the committed p
  no_directory        after losing its durable state, the client starts again at p = 0 on the same stream
  advance_on_refusal  p advances after a pre-admission refusal (H)

Laws (oracle: the rule text of section 1.8 v2.24):
  H  no hole: a client never sends p > latest + 1 (a client law; only advance_on_refusal breaks it).
     Sends that reach the store after their client moved on (stale) may get HistoryUnavailable; that answer
     goes to no pending request, as v2.23's Superseded did.
  I  identity: every reply any client accepts for request r at p is the reply of a run of r's own bytes at p.
  B  binding: while p is the stream's latest, the digest bound at p never changes, through reply, typed error,
     reopen and Abandoned re-commit; no answer but Admitted (p = latest + 1, or a new stream) binds a digest.
  M  no false mismatch: a send whose request is the one bound at p never gets RequestMismatch, whatever its
     deadline or slot.
  Q  the correct client never sees RequestMismatch or HistoryUnavailable (they are for buggy or lost clients).
Store mutants (each must be caught under some client configuration):
  compare_after_duplicate  at p = latest with a reply, Duplicate is answered before the digest is compared
  compare_after_abandoned  at p = latest admitted and unreplied, Abandoned is answered before comparing
  digest_includes_deadline the digest covers the send's deadline too (M: a retry mismatches)
  digest_dropped_on_reply  the reply writes the stream value without the digest (B), and an absent digest matches
  mismatch_mutates         a mismatch answers RequestMismatch but re-binds the stream to the new digest (B)
  readmit_rebinds          the Abandoned re-commit of an earlier open binds the sender's digest without comparing (B)
  superseded_below_latest  below latest answers Rejected(latest) as v2.23 did, read by the client as Superseded (I);
                           exported as the stored reply of the request that ran at p
"""
import sys

sys.setrecursionlimit(200000)

R, S, CRASHES, REOPENS = 3, 2, 1, 1


def explore(store_mut, client, machine=False):
    fails = []
    seen = set()
    hits = {}

    def hit(name):
        hits[name] = hits.get(name, 0) + 1

    def fail(law, msg, trace):
        if not fails:
            fails.append('%s: %s | %s' % (law, msg, ' ; '.join(trace[-12:])))

    def digest(r, deadline):
        return (r, deadline) if store_mut == 'digest_includes_deadline' else (r,)

    def admit_table(st, p, d):
        lat, s, bound = st['latest'], st['sstate'], st['digest']
        if lat is None:
            return ('Admitted',) if p == 0 else ('Rejected', 0)
        if p == lat + 1:
            return ('Admitted',) if s[0] == 'R' else ('Rejected', lat)
        if p > lat + 1:
            return ('Rejected', lat)
        if p < lat:
            return ('Rejected', lat) if store_mut == 'superseded_below_latest' else ('HistoryUnavailable',)
        # p == lat
        same = bound is None or bound == d if store_mut == 'digest_dropped_on_reply' else bound == d
        if s[0] == 'R':
            if store_mut == 'compare_after_duplicate' or same:
                return ('Duplicate', s[1])
            return ('RequestMismatch',)
        earlier = st['aopen'] != st['open']
        if store_mut == 'readmit_rebinds' and earlier:
            return ('Abandoned', 'rebind')
        if store_mut == 'compare_after_abandoned' or same:
            return ('Abandoned', 'earlier' if earlier else 'same')
        return ('RequestMismatch',)

    def admit(st, p, d):
        answer = admit_table(st, p, d)
        earlier = answer[0] == 'Abandoned' and st['aopen'] != st['open']
        if store_mut == 'never_abandon_after_reopen' and earlier:
            return ('Full',)
        if not st.get('room', True) and (answer[0] == 'Admitted' or earlier):
            return ('Full',)
        return answer

    def key(st):
        return (st['latest'], st['sstate'], st['digest'], st['bound_r'], st['aopen'], st['open'], st['net'], st['running'],
                tuple(sorted(st['resp'], key=repr)), st['p'], st['i'], st['sends'], st['crashes'], st['done'],
                st['lost'], tuple(sorted(st['hist'].items())))

    def bind_ok(st, st2, trace, why):
        if machine is True and store_mut in ('digest_dropped_on_reply', 'mismatch_mutates', 'readmit_rebinds'):
            return True  # Extend B's counterexample to its first client-visible consequence.
        # law B: while latest stays, its digest stays
        if st['latest'] is not None and st2['latest'] == st['latest'] and st2['digest'] != st['digest']:
            fail('B', 'p%d re-bound %s -> %s by %s' % (st['latest'], st['digest'], st2['digest'], why), trace)
            return False
        return True

    def successors(st, trace=()):
        fails.clear()
        succ = []
        if st['i'] < R and st['sends'] < S and st['lost'] is not True:
            if True:
                if st['latest'] is not None and st['p'] > st['latest'] + 1 or st['latest'] is None and st['p'] > 0:
                    fail('H', 'client sends p%d over latest %s' % (st['p'], st['latest']), trace); return
            succ.append(('send p%d r%d dl%d' % (st['p'], st['i'], st['sends']),
                         dict(st, net=st['net'] + ((st['p'], st['i'], st['sends']),), sends=st['sends'] + 1)))
        for n, (p, r, dl) in enumerate(st['net']):
            rest = st['net'][:n] + st['net'][n + 1:]
            succ.append(('refuse p%d r%d' % (p, r), dict(st, net=rest, resp=st['resp'] + ((p, r, ('Refused',)),))))
            d = digest(r, dl)
            # law M: the request bound at p never mismatches
            a = admit(st, p, d)
            if a[0] == 'RequestMismatch' and st['latest'] == p and st['bound_r'] == r:
                fail('M', 'r%d at p%d (deadline %d) mismatched its own binding %s' % (r, p, dl, st['digest']), trace); return
            if a[0] == 'Admitted' or a[0] == 'Abandoned':
                kind = a[1] if a[0] == 'Abandoned' else 'new'
                hit('Abandoned (%s open)' % kind if a[0] == 'Abandoned' else 'Admitted')
                if kind == 'same':
                    nxt = dict(st, net=rest, running=st['running'] + ((p, r, st['digest']),))
                else:
                    nd = st['digest'] if kind == 'earlier' else d
                    br = st['bound_r'] if kind == 'earlier' else r
                    nxt = dict(st, net=rest, latest=p, sstate=('A',), digest=nd, bound_r=br, aopen=st['open'],
                               running=st['running'] + ((p, r, nd),))
                    if kind == 'rebind' and not bind_ok(st, nxt, trace + ['readmit p%d r%d' % (p, r)], 'readmit'):
                        return
                succ.append(('admit p%d r%d %s' % (p, r, kind), nxt))
            elif a[0] == 'Duplicate':
                hit('Duplicate')
                succ.append(('dup p%d r%d' % (p, r), dict(st, net=rest, resp=st['resp'] + ((p, r, ('Returned', a[1])),))))
            elif a[0] == 'RequestMismatch':
                hit('RequestMismatch')
                nxt = dict(st, net=rest, resp=st['resp'] + ((p, r, ('RequestMismatch',)),))
                if store_mut == 'mismatch_mutates':
                    nxt = dict(nxt, digest=d, bound_r=r)
                    if not bind_ok(st, nxt, trace + ['mismatch p%d r%d' % (p, r)], 'mismatch'):
                        return
                succ.append(('mismatch p%d r%d' % (p, r), nxt))
            elif a[0] == 'Full':
                hit('Full')
                succ.append(('full p%d r%d' % (p, r), dict(st, net=rest, resp=st['resp'] + ((p, r, ('Refused',)),))))
            elif a[0] == 'HistoryUnavailable':
                hit('HistoryUnavailable')
                succ.append(('histun p%d r%d' % (p, r), dict(st, net=rest, resp=st['resp'] + ((p, r, ('HistoryUnavailable',)),))))
            else:
                l = a[1]
                ans = ('Superseded', l) if p < l else ('Rejected', l)
                succ.append(('rej p%d r%d' % (p, r), dict(st, net=rest, resp=st['resp'] + ((p, r, ans),))))
        for n, (p, r, d) in enumerate(st['running']):
            rest = st['running'][:n] + st['running'][n + 1:]
            if st['latest'] == p and st['sstate'] == ('A',) and st['digest'] == d:
                hit('reply committed')
                nd = None if store_mut == 'digest_dropped_on_reply' else st['digest']
                nxt = dict(st, running=rest, sstate=('R', r), digest=nd, hist=dict(st['hist'], **{str(p): r}),
                           resp=st['resp'] + ((p, r, ('Returned', r)),))
                if not bind_ok(st, nxt, trace + ['reply p%d r%d' % (p, r)], 'reply'):
                    return
                succ.append(('run p%d r%d' % (p, r), nxt))
            else:
                hit('Missing("reply")')
                succ.append(('late p%d r%d' % (p, r), dict(st, running=rest)))
            succ.append(('fault p%d r%d' % (p, r), dict(st, running=rest)))
        for n, (p, r, ans) in enumerate(st['resp']):
            rest = st['resp'][:n] + st['resp'][n + 1:]
            base = dict(st, resp=rest)
            if (p, r) != (st['p'], st['i']) or st['lost'] is True:
                succ.append(('stale p%d r%d' % (p, r), base)); continue
            if ans[0] == 'Refused':
                if client == 'advance_on_refusal':
                    nxt = dict(base, p=p + 1, i=r + 1, sends=0)
                elif st['sends'] == 1 or client == 'reuse_while_live':
                    nxt = dict(base, i=r + 1, sends=0)
                else:
                    nxt = base
                succ.append(('recv Refused p%d r%d' % (p, r), nxt)); continue
            if ans[0] in ('RequestMismatch', 'HistoryUnavailable'):
                if client == 'correct':
                    fail('Q', 'correct client got %s for r%d at p%d' % (ans[0], r, p), trace + ['recv']); return
                # a buggy client: RequestMismatch ends this request; HistoryUnavailable retires the stream
                nxt = dict(base, i=r + 1, sends=0) if ans[0] == 'RequestMismatch' else dict(base, i=R, lost=True)
                succ.append(('recv %s p%d r%d' % (ans[0], p, r), nxt)); continue
            if ans[0] == 'Returned':
                if ans[1] != r:
                    fail('I', 'r%d at p%d accepted the reply of r%d' % (r, p, ans[1]), trace + ['recv']); return
                hit('accept Returned')
                succ.append(('recv Returned p%d r%d' % (p, r), dict(base, p=p + 1, i=r + 1, sends=0, done=st['done'] + (r,))))
                continue
            if ans[0] == 'Superseded':
                ran = st['hist'].get(str(p))
                if ran != r:
                    fail('I', 'r%d at p%d accepted Superseded but p%d ran r%s' % (r, p, p, ran), trace + ['recv']); return
                succ.append(('recv Superseded p%d r%d' % (p, r), dict(base, p=p + 1, i=r + 1, sends=0, done=st['done'] + (r,))))
                continue
            succ.append(('recv Rejected p%d r%d' % (p, r), base))
        for n in range(len(st['resp'])):
            succ.append(('lose', dict(st, resp=st['resp'][:n] + st['resp'][n + 1:])))
        if st['crashes'] < CRASHES and st['i'] < R and not st['lost']:
            c = dict(st, resp=(), crashes=st['crashes'] + 1)
            if client == 'recover_new_bytes':
                c = dict(c, i=st['i'] + 1, sends=0)
            succ.append(('crash', c))
        if not st['lost'] and st['i'] < R and st['done']:
            if client == 'no_directory':
                l = dict(st, resp=(), lost='restart', p=0, i=st['i'] + 1 if st['i'] + 1 < R else st['i'], sends=0)
            else:
                l = dict(st, resp=(), lost=True, i=R)
            succ.append(('lose state', l))
        # server reopen: running attempts die; stream state (with its digest) is what the journal replays
        if st['open'] < REOPENS:
            succ.append(('reopen', dict(st, running=(), open=st['open'] + 1)))
        return succ

    def step(st, trace):
        k = key(st)
        if k in seen or fails:
            return
        seen.add(k)
        for name, s2 in successors(st, trace) or ():
            step(s2, trace + [name])

    st0 = dict(latest=None, sstate=None, digest=None, bound_r=None, aopen=0, open=0, net=(), running=(), resp=(),
               p=0, i=0, sends=0, crashes=0, done=(), hist={}, lost=False)
    if machine:
        return dict(init=st0, successors=successors, failures=fails, key=key, admit=admit)
    step(st0, [])
    return len(seen), fails, hits


CLIENTS = ('correct', 'reuse_while_live', 'recover_new_bytes', 'no_directory', 'advance_on_refusal')
STORE_MUTANTS = ('compare_after_duplicate', 'compare_after_abandoned', 'digest_includes_deadline',
                 'digest_dropped_on_reply', 'mismatch_mutates', 'readmit_rebinds', 'superseded_below_latest')


def main():
    rows, ok = [], True
    for c in CLIENTS:
        n, fails, hits = explore('none', c)
        want_fail = c == 'advance_on_refusal'
        laws = sorted({f.split(':')[0] for f in fails})
        good = (bool(fails) and laws == ['H']) if want_fail else not fails
        ok &= good
        rows.append('[store v2.24, client %s] states=%d failures=%d %s%s' % (
            c, n, len(fails), 'laws=' + ','.join(laws) if fails else '', '' if good else '  UNEXPECTED'))
        rows.extend('    ' + f[:400] for f in fails[:1])
        rows.append('    answers reached: ' + ', '.join('%s %d' % kv for kv in sorted(hits.items())))
    for m in STORE_MUTANTS:
        caught = []
        for c in CLIENTS[:4]:
            n, fails, _ = explore(m, c)
            if fails:
                caught.append((c, fails[0]))
        ok &= bool(caught)
        rows.append('[%s] %s' % (m, ('CAUGHT under %s' % ', '.join(c for c, _ in caught)) if caught else 'NOT CAUGHT'))
        if caught:
            rows.append('    ' + caught[0][1][:400])
    print('\n'.join(rows))
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())


def export_obs(path):
    """Export counterexamples and 200 seeded maximal histories in the shared Obs vocabulary."""
    from export import export_store
    return export_store(path)
