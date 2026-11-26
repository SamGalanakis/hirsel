"""STORE-SPEC v2.21 scope model: section 1.4 key scopes under the S5 rulings AE4 (a)-(e).

No scope model existed before v2.21 (none under store-spec-1001/model/, none in store-build-1010).

The store side (`Impl`) works as section 1.4 and 1.3 say the store does: an entry is (table, lo bytes, hi bytes,
access) with bounds encoded as section 1.3 encodes a span (Included(k) high is Key(k) || 0x00, Excluded(k) low is
Key(k) || 0x00, Unbounded is the table's prefix ends), and every check is a byte compare of encoded bounds.
The oracle (`Rule`) never compares bounds: it enumerates the primary keys each entry covers and reads the ruling
text at key level.

Key domain: two tables t and u; keys are every byte string of length 0, 1 or 2 over {0, 1, 2} (13 per table), so
Key(k) || 0x00 is itself a key and byte coverage equals key coverage: every byte gap between entries holds a key, so
the model cannot see (b)'s gap case (no key of K in the gap, yet OutOfScope); STORE-SPEC L14 holds that as a hand case. Bounds name length-1 keys. Table t has one
index, J(k) = sum(k) mod 3, rows ordered by (J, primary key).

Laws, each checked exhaustively over every scope of at most two entries (inner scopes for narrow: up to two):
  a  fetch, put and delete of (table, k) succeed exactly when an entry covers k (put and delete: a ReadWrite one).
  b  range(table, lo, hi) is OutOfScope, with no row read, exactly when some key of the span is not covered;
     otherwise it reads every row of the span. Adjacent entries together cover a span.
  c  lookup on t's index is OutOfScope up front, no row read, exactly when the scope has no entry for t; then
     next_indexed returns rows in (J, key) order until the first row whose primary key is not covered, which is
     OutOfScope and returns nothing. (What a later call gives is stated in the spec text, not modelled.)
  d  union(a, b) covers exactly the keys either covers, and a key is ReadWrite iff some entry of a or b that
     covers it is ReadWrite.
  e  narrow(outer, inner) is accepted exactly when every inner entry's keys lie within one outer entry's keys
     with equal or greater access. Soundness: an accepted narrow never gives a key more access than outer does.
     The count of narrows that are sound at key level but refused by (e) is reported (the two-entry cover case).
Mutants (each must be caught):
  range_lazy        range checks rows as it reads them, not the span up front (b: a row read before OutOfScope)
  range_one_entry   range needs the whole span inside one entry (b: a span two adjacent entries cover refused)
  lookup_no_upfront lookup with no entry for t still starts and refuses only at the first row (c)
  lookup_skips      next_indexed skips an uncovered row instead of stopping with OutOfScope (c)
  index_exempt      next_indexed returns a row whose primary key is not covered (c)
  put_readonly      a put on a key covered only ReadOnly is admitted (a)
  union_last_wins   union takes b's access where both cover a key (d)
  union_hull        union replaces two entries of one table by their hull (d: a gap key becomes covered)
  narrow_ignores_access  narrow checks only containment (e, and unsound)
  narrow_union      narrow accepts an inner entry covered by the union of outer entries (e)
"""
import functools
import itertools
import sys

TABLES = ('t', 'u')
ALPHA = (0, 1, 2)
KEYS = [b''] + [bytes([a]) for a in ALPHA] + [bytes([a, b]) for a in ALPHA for b in ALPHA]
KEYS.sort()
LO_END, HI_END = b'', b'\xff'          # the table prefix ends (Unbounded)
RO, RW = 0, 1


def J(k):
    return sum(k) % 3


INDEX_ROWS = sorted(KEYS, key=lambda k: (J(k), k))   # table t's index, in index order

BOUNDS = [('U', None)] + [(s, bytes([a])) for a in ALPHA for s in ('I', 'E')]


def enc_lo(b):
    s, k = b
    return LO_END if s == 'U' else (k if s == 'I' else k + b'\x00')


def enc_hi(b):
    s, k = b
    return HI_END if s == 'U' else (k + b'\x00' if s == 'I' else k)


@functools.lru_cache(maxsize=None)
def keyset(lo, hi):
    """The oracle's reading of a bound pair: the keys it names, by the bound's meaning, not its bytes."""
    def above(k):
        s, x = lo
        return s == 'U' or (k >= x if s == 'I' else k > x)

    def below(k):
        s, x = hi
        return s == 'U' or (k <= x if s == 'I' else k < x)
    return frozenset(k for k in KEYS if above(k) and below(k))


# Every nonempty entry: (table, lo bound, hi bound, access).
ENTRIES = [(tb, lo, hi, a) for tb in TABLES for lo in BOUNDS for hi in BOUNDS for a in (RO, RW)
           if keyset(lo, hi)]
# One entry per distinct (table, keyset, access): bounds that name the same keys encode to the same bytes here.
_seen, _uniq = set(), []
for e in ENTRIES:
    sig = (e[0], keyset(e[1], e[2]), e[3])
    if sig not in _seen:
        _seen.add(sig); _uniq.append(e)
ENTRIES = _uniq
SPANS = [(lo, hi) for lo in BOUNDS for hi in BOUNDS]


class Rule:
    """The oracle: AE4 (a)-(e) read at key level."""

    @staticmethod
    def access(scope, tb, k):
        acc = [a for (t2, lo, hi, a) in scope if t2 == tb and k in keyset(lo, hi)]
        return max(acc) if acc else None

    @staticmethod
    def point(scope, tb, k, write):
        a = Rule.access(scope, tb, k)
        return a is not None and (a == RW or not write)

    @staticmethod
    def range(scope, tb, lo, hi):
        span = sorted(keyset(lo, hi))
        if all(Rule.access(scope, tb, k) is not None for k in span):
            return ('Ok', span)
        return ('OutOfScope', [])

    @staticmethod
    def lookup(scope):
        if not any(t2 == 't' for (t2, _, _, _) in scope):
            return ('OutOfScope', [])
        out = []
        for k in INDEX_ROWS:
            if Rule.access(scope, 't', k) is None:
                return ('OutOfScope at row', out)
            out.append(k)
        return ('Ok', out)

    @staticmethod
    def union_access(a, b, tb, k):
        xs = [x for x in (Rule.access(a, tb, k), Rule.access(b, tb, k)) if x is not None]
        return max(xs) if xs else None

    @staticmethod
    def narrow(outer, inner):
        return all(any(o[0] == i[0] and keyset(i[1], i[2]) <= keyset(o[1], o[2]) and o[3] >= i[3]
                       for o in outer) for i in inner)


class Impl:
    """The store side: encoded bounds and byte compares only."""

    def __init__(self, mutant):
        self.m = mutant

    @staticmethod
    def entry(e):
        tb, lo, hi, a = e
        return (tb, enc_lo(lo), enc_hi(hi), a)

    def covers(self, scope, tb, k):
        acc = [a for (t2, lo, hi, a) in map(self.entry, scope) if t2 == tb and lo <= k < hi]
        return max(acc) if acc else None

    def point(self, scope, tb, k, write):
        a = self.covers(scope, tb, k)
        if write and self.m == 'put_readonly':
            return a is not None
        return a is not None and (a == RW or not write)

    def span_covered(self, scope, tb, slo, shi):
        """Is the byte span [slo, shi) inside the union of tb's entries? A sweep over merged intervals."""
        ivs = sorted((lo, hi) for (t2, lo, hi, _) in map(self.entry, scope) if t2 == tb)
        if self.m == 'range_one_entry':
            return any(lo <= slo and shi <= hi for lo, hi in ivs)
        at = slo
        for lo, hi in ivs:
            if lo > at:
                break
            at = max(at, hi)
            if at >= shi:
                return True
        return at >= shi

    def range(self, scope, tb, lo, hi):
        slo, shi = enc_lo(lo), enc_hi(hi)
        rows = [k for k in KEYS if slo <= k < shi]
        if self.m == 'range_lazy':
            read = []
            for k in rows:
                read.append(k)
                if self.covers(scope, tb, k) is None:
                    return ('OutOfScope', read)
            return ('Ok', read)
        if slo >= shi or self.span_covered(scope, tb, slo, shi):
            return ('Ok', rows)
        return ('OutOfScope', [])

    def lookup(self, scope):
        if self.m != 'lookup_no_upfront' and not any(t2 == 't' for (t2, _, _, _) in scope):
            return ('OutOfScope', [])
        out = []
        for k in INDEX_ROWS:                      # next_indexed, one call per row
            if self.covers(scope, 't', k) is None:
                if self.m == 'lookup_skips':
                    continue
                if self.m != 'index_exempt':
                    return ('OutOfScope at row', out)
            out.append(k)
        if self.m == 'lookup_no_upfront' and not any(t2 == 't' for (t2, _, _, _) in scope):
            return ('OutOfScope at row', out)
        return ('Ok', out)

    def union(self, a, b):
        if self.m == 'union_hull':
            ents = list(a) + list(b)
            for tb in TABLES:
                mine = [e for e in ents if e[0] == tb]
                if len(mine) == 2:
                    lo = min(mine, key=lambda e: enc_lo(e[1]))[1]
                    hi = max(mine, key=lambda e: enc_hi(e[2]))[2]
                    ents = [e for e in ents if e[0] != tb] + [(tb, lo, hi, max(e[3] for e in mine))]
            return ents
        if self.m == 'union_last_wins':
            return ('last', list(a), list(b))
        return list(a) + list(b)

    def union_access(self, u, tb, k):
        if isinstance(u, tuple) and u[0] == 'last':
            _, a, b = u
            x = self.covers(b, tb, k)
            return x if x is not None else self.covers(a, tb, k)
        return self.covers(u, tb, k)

    def narrow(self, outer, inner):
        O = list(map(self.entry, outer))
        for i in map(self.entry, inner):
            if self.m == 'narrow_union':
                ks = [k for k in KEYS if i[1] <= k < i[2]]
                if not all(self.covers(outer, i[0], k) is not None and self.covers(outer, i[0], k) >= i[3]
                           for k in ks):
                    return False
                continue
            ok = any(o[0] == i[0] and o[1] <= i[1] and i[2] <= o[2] and
                     (self.m == 'narrow_ignores_access' or o[3] >= i[3]) for o in O)
            if not ok:
                return False
        return True


def scopes(max_entries):
    yield from ([e] for e in ENTRIES)
    if max_entries >= 2:
        yield from (list(p) for p in itertools.combinations(ENTRIES, 2))


def explore(mutant):
    imp = Impl(mutant)
    fails, n = [], [0]

    def fail(law, msg):
        fails.append('%s: %s' % (law, msg))

    all_scopes = list(scopes(2))
    for S in all_scopes:
        for tb in TABLES:
            for k in KEYS:
                for w in (False, True):
                    n[0] += 1
                    if imp.point(S, tb, k, w) != Rule.point(S, tb, k, w):
                        fail('a', 'scope %s %s %s write=%s' % (S, tb, k.hex(), w)); return n[0], fails, 0
            for lo, hi in SPANS:
                n[0] += 1
                if imp.range(S, tb, lo, hi) != Rule.range(S, tb, lo, hi):
                    fail('b', 'scope %s range %s %s %s: got %s want %s' % (S, tb, lo, hi, imp.range(S, tb, lo, hi),
                                                                         Rule.range(S, tb, lo, hi)))
                    return n[0], fails, 0
        n[0] += 1
        if imp.lookup(S) != Rule.lookup(S):
            fail('c', 'scope %s lookup: got %s want %s' % (S, imp.lookup(S), Rule.lookup(S))); return n[0], fails, 0
    singles = [[e] for e in ENTRIES]
    for a, b in itertools.product(singles, singles):
        u = imp.union(a, b)
        for tb in TABLES:
            for k in KEYS:
                n[0] += 1
                if imp.union_access(u, tb, k) != Rule.union_access(a, b, tb, k):
                    fail('d', 'union %s %s at %s %s' % (a, b, tb, k.hex())); return n[0], fails, 0
    sound_refused = 0
    for outer in all_scopes:
        for inner in (singles if len(outer) == 2 else all_scopes):
            n[0] += 1
            got, want = imp.narrow(outer, inner), Rule.narrow(outer, inner)
            sound = all(Rule.access(outer, tb, k) is not None and Rule.access(outer, tb, k) >= Rule.access(inner, tb, k)
                        for tb in TABLES for k in KEYS if Rule.access(inner, tb, k) is not None)
            if got and not sound:
                fail('e', 'unsound narrow %s within %s' % (inner, outer)); return n[0], fails, 0
            if got != want:
                fail('e', 'narrow %s within %s: got %s want %s' % (inner, outer, got, want)); return n[0], fails, 0
            if sound and not want:
                sound_refused += 1
    return n[0], fails, sound_refused


def main():
    rows = []
    muts = ('none', 'range_lazy', 'range_one_entry', 'lookup_no_upfront', 'lookup_skips', 'index_exempt',
            'put_readonly', 'union_last_wins', 'union_hull', 'narrow_ignores_access', 'narrow_union')
    for m in muts:
        n, fails, sr = explore(m)
        if m == 'none':
            rows.append('[none] v2.21 rules: entries=%d checks=%d failures=%d sound_but_refused_narrows=%d'
                        % (len(ENTRIES), n, len(fails), sr))
            rows.extend('    ' + f for f in fails[:3])
        else:
            rows.append('[%s] %s' % (m, 'CAUGHT' if fails else 'NOT CAUGHT'))
            if fails:
                rows.append('    ' + fails[0][:300])
    print('\n'.join(rows))
    return 0 if 'NOT CAUGHT' not in ''.join(rows) and 'failures=0' in rows[0] else 1


if __name__ == '__main__':
    sys.exit(main())
