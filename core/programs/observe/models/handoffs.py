"""Bounded handoff extensions for OBSERVERS-SPEC section 3.

The large models explore scheduling. These finite boundary cases add request
identity, publication generations and shutdown choices to their handoff tables.
Classifiers consume transitions, separately from the allowed tables below.
"""
from itertools import product
import json
import struct

import model_v224_client as store


ALLOWED = {
    'admit': {'Admitted', 'Duplicate.same', 'Abandoned.same-open.same',
              'Abandoned.earlier-open.same', 'RequestMismatch.other',
              'HistoryUnavailable', 'Rejected', 'Full'},
    # AH2 withdraws Superseded. It is not an allowed reply class.
    'reply': {'Returned', 'typed error', 'Failed(Gone)'},
    'join': {'joined-shared', 'RequestMismatch.H1', 'Close1008.V2',
             'own-deadline Abandoned', 'Overloaded'},
    'publication': {'row', 'complete', 'Reset'},
    'wake': {'durable-base row', 'commitless-base-0 row', 'no-owed-send'},
    'cancel': {'own-deadline Abandoned', 'hang-up no Outcome',
               'hung-body Abandoned', 'resend joins'},
}

PAIRS = {
    'admit': ('compare_after_abandoned', 'never_abandon_after_reopen'),
    'reply': ('digest_dropped_on_reply', None),
    'join': ('join_without_digest', 'no_join'),
    'publication': ('reset_keeps_sub', 'no_reset'),
    'wake': ('late_completion_resent', 'no_commitless'),
    'cancel': ('abandon_frees_slot', 'no_hangup'),
}

# None means the handoff distinction has no T1 observation. The open and
# capacity distinctions are kept in the model-only inventory.
PROJECTION = {
    'admit': {'Admitted': None, 'Duplicate.same': 'Returned',
              'Abandoned.same-open.same': 'Abandoned',
              'Abandoned.earlier-open.same': 'Abandoned',
              'RequestMismatch.other': 'RequestMismatch',
              'HistoryUnavailable': 'HistoryUnavailable', 'Rejected': None,
              'Full': 'Overloaded'},
    'reply': {c: c for c in ALLOWED['reply']},
    'join': {'joined-shared': 'shared-or-duplicate', 'RequestMismatch.H1': 'RequestMismatch',
             'Close1008.V2': 'Close1008', 'own-deadline Abandoned': 'Abandoned', 'Overloaded': 'Overloaded'},
    'publication': {c: c for c in ALLOWED['publication']},
    # T1 cannot attribute an answer to head/publisher rather than the session,
    # or show that a slot was freed. Keep those classifier cells model-only.
    'wake': {c: None for c in ALLOWED['wake']},
    'cancel': {'own-deadline Abandoned': 'Abandoned', 'hung-body Abandoned': 'Abandoned',
               'hang-up no Outcome': 'Closed-with-pending', 'resend joins': None},
}


def admit(mutant):
    machine = store.explore(mutant, 'correct', machine=True)
    for latest, state, opened, admitted, room, p, digest in product(
            (None, 0, 1), ('A', 'R'), (0, 1), (0, 1), (False, True), range(3), ((0,), (1,))):
        if admitted > opened:
            continue
        st = dict(machine['init'], latest=latest, sstate=(state, 0) if state == 'R' else ('A',),
                  digest=(0,), bound_r=0, open=opened, aopen=admitted, room=room)
        answer = machine['admit'](st, p, digest)
        yield dict(answer=answer, digest=digest, bound=st['digest'], earlier=opened != admitted)


def classify_admit(e):
    answer = e['answer'][0]
    relation = 'same' if e['digest'] == e['bound'] else 'other'
    if answer == 'Duplicate':
        return 'Duplicate.' + relation
    if answer == 'Abandoned':
        return 'Abandoned.' + ('earlier-open' if e['earlier'] else 'same-open') + '.' + relation
    if answer == 'RequestMismatch':
        return answer + '.' + relation
    return answer


def reply(mutant):
    machine = store.explore(mutant, 'correct', machine=True)
    for result, sent in product(('ok', 'error', 'gone'), (0, 1)):
        st = dict(machine['init'], latest=0, sstate=('R', 0), digest=(0,), bound_r=0)
        if mutant == 'digest_dropped_on_reply':
            st['digest'] = None
        a = machine['admit'](st, 0, (sent,))
        if a[0] == 'Duplicate':
            yield dict(result=result, actual=a[1], sent=sent)


def classify_reply(e):
    if e['actual'] != e['sent']:
        return 'Returned.other'
    return {'ok': 'Returned', 'error': 'typed error', 'gone': 'Failed(Gone)'}[e['result']]


def join(mutant):
    if mutant == 'join_without_digest':
        yield from join('none')
    for seam, same, capacity, expired in product(('H1', 'V2'), (False, True), (False, True), (False, True)):
        # Digest comparisons precede sharing; a full slot or its own timer can
        # refuse a send without sharing a body.
        if not same and mutant != 'join_without_digest':
            result = 'mismatch' if seam == 'H1' else 'close'
        elif not capacity or mutant == 'no_join':
            result = 'full'
        elif expired:
            result = 'abandon'
        else:
            result = 'share'
        yield dict(seam=seam, same=same, result=result)


def classify_join(e):
    return {'mismatch': 'RequestMismatch.H1', 'close': 'Close1008.V2', 'full': 'Overloaded',
            'abandon': 'own-deadline Abandoned',
            'share': 'joined-shared' if e['same'] else 'joined-other'}[e['result']]


def publication(mutant):
    # A complete marker and a row are different observable frame kinds.
    yield dict(kind='complete', generation=0, current=0)
    yield dict(kind='row', generation=0, current=0)
    if mutant == 'no_reset':
        return  # The stronger model closes 1008 instead of publishing a Reset.
    yield dict(kind='Reset', generation=0, current=1)
    if mutant == 'reset_keeps_sub':
        yield dict(kind='row', generation=0, current=1)


def classify_publication(e):
    return 'pre-Reset row' if e['kind'] == 'row' and e['generation'] < e['current'] else e['kind']


def wake(mutant):
    if mutant == 'late_completion_resent':
        yield from wake('none')
    for committed, owed in product((False, True), (0, 1)):
        if not committed and mutant == 'no_commitless':
            continue  # A commitless answer waits for a later durable base.
        sends = owed if mutant != 'late_completion_resent' else max(1, owed)
        yield dict(committed=committed, owed=owed, sends=sends)


def classify_wake(e):
    if not e['owed']:
        return 'row-without-owed-send' if e['sends'] else 'no-owed-send'
    return 'durable-base row' if e['committed'] else 'commitless-base-0 row'


def cancel(mutant):
    if mutant == 'abandon_frees_slot':
        yield from cancel('none')
    for reason in ('deadline', 'hangup', 'hung', 'resend'):
        if reason == 'hangup' and mutant == 'no_hangup':
            continue
        held, heads = True, 1
        closed, answer = reason == 'hangup', reason != 'hangup'
        if reason == 'hangup' and mutant == 'no_hangup_drop':
            answer = True
        if reason == 'resend' and mutant == 'abandon_frees_slot':
            held = False
            heads += 1
        yield dict(reason=reason, closed=closed, answer=answer, held=held, heads=heads)


def classify_cancel(e):
    if e['reason'] == 'hangup':
        return 'hang-up answered' if e['answer'] else 'hang-up no Outcome'
    if e['reason'] == 'resend':
        return 'resend joins' if e['heads'] == 1 and e['held'] else 'resend starts second head'
    return 'hung-body Abandoned' if e['reason'] == 'hung' else 'own-deadline Abandoned'


MODELS = {name: (globals()[name], globals()['classify_' + name]) for name in ALLOWED}


def classes(name, mutant='none'):
    model, classifier = MODELS[name]
    return {classifier(e) for e in model(mutant)}


def visible(name, values):
    return {v for c in values if (v := PROJECTION[name].get(c)) is not None}


def record_visible(trace, observed):
    """Classify only T1. No Pos, Ran, task state, or saved .obs input is read."""
    for name in ALLOWED:
        observed.setdefault(name, set())
    pending, replies = {}, set()
    for e in trace['obs']:
        kind = e['kind']
        if kind == 'Sent':
            key = (e['conn'], tuple(e['stream']), e['p'])
            pending[key] = pending.get(key, 0) + 1
        elif kind == 'Got':
            key = (e['conn'], tuple(e['stream']), e['p'])
            pending[key] = max(0, pending.get(key, 0) - 1)
            outcome = e['outcome']
            if outcome['kind'] == 'Abandoned':
                observed['join'].add('Abandoned')
                observed['cancel'].add('Abandoned')
            if outcome['kind'] == 'Refused':
                tag = outcome['payload'][0]
                if tag in (2, 6):
                    observed['join'].add('Overloaded' if tag == 2 else 'RequestMismatch')
            if outcome['kind'] in ('Returned', 'Failed'):
                signature = (tuple(e['stream']), e['p'], e['version'], json.dumps(outcome, sort_keys=True))
                if signature in replies:
                    observed['join'].add('shared-or-duplicate')
                replies.add(signature)
            # Enable the S5 inventory with its digest-aware driver, not against
            # the old runner's pre-v2.24 store stand-in.
            if trace['header']['has_digest']:
                if outcome['kind'] == 'Returned':
                    observed['admit'].add('Returned')
                    is_error = len(outcome['payload']) >= 4 and int.from_bytes(bytes(outcome['payload'][:4]), 'little') == 1
                    observed['reply'].add('typed error' if is_error else 'Returned')
                elif outcome['kind'] == 'Failed' and outcome['payload'] == [3]:
                    observed['reply'].add('Failed(Gone)')
                elif outcome['kind'] == 'Refused':
                    value = {2: 'Overloaded', 6: 'RequestMismatch', 7: 'HistoryUnavailable'}.get(outcome['payload'][0])
                    if value:
                        observed['admit'].add(value)
                elif outcome['kind'] == 'Abandoned':
                    observed['admit'].add('Abandoned')
        elif kind == 'Closed':
            if e['code'] == 1008:
                observed['join'].add('Close1008')
            if any(n for key, n in pending.items() if key[0] == e['conn']):
                observed['cancel'].add('Closed-with-pending')
            pending = {key: n for key, n in pending.items() if key[0] != e['conn']}
        elif kind == 'Rx':
            data = bytes(e['frame'])
            tag = int.from_bytes(data[:4], 'little')
            if tag == 8:
                observed['publication'].add('Reset')
            elif tag == 6 and len(data) >= 20:
                count = struct.unpack_from('<I', data, 12)[0]
                if count:
                    observed['publication'].add('complete')
                offset = 16 + count * 4
                if offset + 4 <= len(data) and struct.unpack_from('<I', data, offset)[0]:
                    observed['publication'].add('row')


def compare(observed, names):
    good = True
    for name in names:
        allowed = visible(name, classes(name))
        actual = set(observed.get(name, ()))
        extra = actual - allowed
        if extra:
            print(f'OBSERVER inventory FAILED handoff={name} classes={sorted(extra)}')
        print(f'ALLOWED-NOT-OBSERVED {name}: ' + ', '.join(sorted(allowed - actual)))
        good &= not extra
    return good


def inventory_data(names):
    result = []
    for name in names:
        base = classes(name)
        for strength, mutant in zip(('weaker', 'stronger'), PAIRS[name]):
            if mutant is None:
                continue
            reached = classes(name, mutant)
            model, classifier = MODELS[name]
            result.append(dict(handoff=name, mutant=mutant, strength=strength,
                               classes=sorted(reached), added=sorted(reached - base), removed=sorted(base - reached),
                               transitions=[dict(event=e, classified=classifier(e)) for e in model(mutant)]))
    return result


def boundary_traces(model):
    # These are projections of actual boundary-model transitions. No observer
    # implementation or verdict participates in their construction.
    import export
    result = []
    if model == 'calls25':
        for mutant in ('none', 'join_without_digest'):
            for e in join(mutant):
                if e['result'] == 'share' and e['same'] == (mutant == 'none'):
                    digest = (0,) if e['same'] else (1,)
                    events = [export.sent(0, 0, 0, (0,)), export.sent(1, 0, 0, digest),
                              dict(kind='Ran', stream=export.word(0), p=0, digest=[0], version=1),
                              export.got(0, 0, 0, 1, export.answer('ok')),
                              export.got(1, 0, 0, 1, export.answer('ok'))]
                    result.append(dict(name='join_identity' if mutant == 'none' else mutant,
                                       clause=None if mutant == 'none' else 'J1', observer='joins',
                                       trace=export.observed(events, digest=True, ran=True)))
                    break
        for seam, label in (('H1', 'join_h1_mismatch'), ('V2', 'join_v2_close')):
            e = next(e for e in join('none') if e['seam'] == seam and not e['same'])
            conn = 1 if seam == 'H1' else 0
            events = [export.sent(0, 0, 0, (0,)),
                      dict(kind='Pos', stream=export.word(0), latest=0, state=1, digest=[0]),
                      export.sent(conn, 0, 0, (1,))]
            if e['result'] == 'mismatch':
                events += [export.got(conn, 0, 0, 0, export.answer('RequestMismatch')),
                           dict(kind='Closed', conn=0, code=1000, at=0)]
            else:
                events += [dict(kind='Closed', conn=0, code=1008, at=0)]
            result.append(dict(name=label, clause=None, observer='joins',
                               trace=export.observed(events, digest=True, ran=True)))
        machine = store.explore('digest_includes_deadline', 'correct', machine=True)
        history = export.witness(machine)
        result.append(dict(name='join_false_mismatch', clause='J6', observer='joins', trace=export.store_trace(history)))
        result.append(dict(name='join_unbound_mismatch', clause='J6', observer='joins',
                           trace=export.observed([export.sent(0, 0, 0, (0,)), export.got(0, 0, 0, 0, export.answer('RequestMismatch'))], digest=True, ran=True)))
    if model == 'subs25':
        for mutant in ('none', 'reset_keeps_sub'):
            events = [dict(kind='Tx', conn=0, frame=export.subscribe(1))]
            for e in publication(mutant):
                frame = (export.batch(e['current'], complete=(1,)) if e['kind'] == 'complete' else
                         export.batch(e['current'], reset=True) if e['kind'] == 'Reset' else
                         export.batch(e['current'], ((1, [0], [e['current']]),)))
                events.append(dict(kind='Rx', conn=0, frame=frame))
            result.append(dict(name='publication_reset' if mutant == 'none' else mutant,
                               clause=None if mutant == 'none' else 'W5', observer='session',
                               trace=export.observed(events)))
    if model == 'store-v224':
        machine = store.explore('none', 'no_directory', machine=True)
        st = dict(machine['init'], latest=1, sstate=('R', 1), digest=(1,), bound_r=1)
        old = machine['admit'](st, 0, (0,))
        if old[0] != 'HistoryUnavailable':
            raise ValueError('the history coverage driver must reach the handoff table')
        events = []
        for p in (0, 1):
            events += [export.sent(0, 0, p, (p,)),
                       dict(kind='Pos', stream=export.word(0), latest=p, state=2, digest=[p]),
                       dict(kind='Ran', stream=export.word(0), p=p, digest=[p], version=p + 1),
                       export.got(0, 0, p, p + 1, export.answer('ok', p))]
        events += [export.sent(1, 0, 0, (0,)), export.got(1, 0, 0, 2, export.answer(old[0]))]
        result.append(dict(name='history_retired', clause=None, observer='positions',
                           trace=export.observed(events, digest=True, ran=True)))
        result.append(dict(name='history_resent', clause='AH2', observer='positions',
                           trace=export.observed(events + [export.sent(1, 0, 0, (0,))], digest=True, ran=True)))
        result.append(dict(name='unbound_mismatch', clause='M', observer='positions',
                           trace=export.observed([export.sent(0, 0, 0, (0,)), export.got(0, 0, 0, 0, export.answer('RequestMismatch'))], digest=True, ran=True)))
    return result


def check(observed=None, names=None):
    observed = observed or {}
    good = True
    for name, allowed in ALLOWED.items():
        if names is not None and name not in names:
            continue
        reached = classes(name)
        same = reached == allowed
        print(f'INVENTORY {name} I=S={same} classes={len(reached)}')
        if not same:
            print(f'  extra={sorted(reached - allowed)} missing={sorted(allowed - reached)}')
        good &= same
        for strength, mutant in zip(('WEAKER', 'STRONGER'), PAIRS[name]):
            if mutant is None:
                print(f'INVENTORY {name} STRONGER: none; each reply class is forced by the body result')
                continue
            changed = classes(name, mutant)
            added, removed = changed - reached, reached - changed
            bites = bool(added) if strength == 'WEAKER' else bool(removed)
            print(f'INVENTORY {name} {strength} mutant={mutant} added={sorted(added)} removed={sorted(removed)} I=S={changed == allowed}')
            good &= bites and changed != allowed
            good &= reached <= changed if strength == 'WEAKER' else changed <= reached
        if name == 'cancel':
            old = classes(name, 'no_hangup_drop')
            print('INVENTORY cancel SWAP no_hangup_drop -> no_hangup: answering after hang-up adds '
                  + ', '.join(sorted(old - reached)) + '; it is not a stronger model')
        projection = visible(name, reached)
        production = set(observed.get(name, ()))
        good &= production <= projection
        print(f'ALLOWED-NOT-OBSERVED {name}: ' + ', '.join(sorted(projection - production)))
        internal = sorted(c for c in reached if PROJECTION[name][c] is None)
        if internal:
            print(f'INVENTORY {name} INTERNAL: ' + ', '.join(internal))
    return bool(good)


if __name__ == '__main__':
    raise SystemExit(0 if check() else 1)
