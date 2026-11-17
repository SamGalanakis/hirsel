"""Shared trace exporters. The observers are independent Hirsel implementations."""
from collections import deque
import json
from pathlib import Path
import random
import re
import struct

import calls25_model as calls
import subs25_model as subs
import model_v224_client as store


def word(n):
    return list(struct.pack('<Q', n))


def sent(conn, stream, p, digest=()):
    return dict(kind='Sent', conn=conn, stream=word(stream), p=p, digest=list(digest), deadline=0, at=0)


def got(conn, stream, p, version, outcome):
    return dict(kind='Got', conn=conn, stream=word(stream), p=p, version=version, outcome=outcome, at=0)


def answer(kind, request=0):
    if kind in ('ok', 'err', 'Returned'):
        raw = struct.pack('<I', int(kind == 'err')) + struct.pack('<Q' if kind != 'err' else '<I', request)
        return dict(kind='Returned', payload=list(raw))
    if kind in ('fault', 'gone'):
        return dict(kind='Failed', payload=[3] if kind == 'gone' else [0, 2])
    if kind.startswith('Refused') or kind in ('Full', 'RequestMismatch', 'HistoryUnavailable'):
        tag = (1 if 'Malformed' in kind else 0 if 'NoEntry' in kind else
               6 if kind == 'RequestMismatch' else 7 if kind == 'HistoryUnavailable' else 2)
        return dict(kind='Refused', payload=[tag, 0, 0] if tag == 1 else [tag])
    return dict(kind=kind, payload=[])


def observed(events, *, digest=False, ran=False, complete=True):
    return dict(header=dict(has_clock=False, has_digest=digest, has_ran=ran, complete=complete), obs=events)


def options(machine, state):
    try:
        if 'check' in machine:
            machine['check'](state)
        choices = machine['successors'](state, []) if 'failures' in machine else machine['successors'](state)
        if machine.get('failures'):
            return [], machine['failures'][0], False
        choices = choices or []
        real = [e for e in choices if e[0] not in ('crash', 'poison', 'poison_exit')]
        if not real and 'terminal' in machine and not ('reqs' in machine and state[6]):
            machine['terminal'](state)
        return choices, None, not choices
    except (calls.Fail, subs.Fail) as e:
        return [], e.args[0], False


def sample(machine, seed):
    rng = random.Random(seed)
    key = machine.get('key', lambda x: x)

    def visit(state, path, active):
        choices, why, complete = options(machine, state)
        if why is not None or not choices:
            return path, state, why, complete
        rng.shuffle(choices)
        for label, nxt in choices:
            if nxt == 'K3':
                return path, state, ('K3 sent past durable',), False
            nk = key(nxt)
            if nk not in active:
                result = visit(nxt, path + [(state, label, nxt)], active | {nk})
                if result is not None:
                    return result
        return None

    result = visit(machine['init'], [], {key(machine['init'])})
    if result is None:
        raise ValueError('no finite maximal history from the model initial state')
    return result


def witness(machine):
    """BFS to the original model oracle, retaining concrete transitions for export."""
    key = machine.get('key', lambda x: x)
    first = key(machine['init'])
    seen = {first: (machine['init'], None, None)}
    queue = deque([first])
    while queue:
        k = queue.popleft()
        state = seen[k][0]
        choices, why, complete = options(machine, state)
        if why is not None:
            path, cur = [], k
            while seen[cur][1] is not None:
                nxt, prev, label = seen[cur]
                path.append((seen[prev][0], label, nxt))
                cur = prev
            return list(reversed(path)), state, why, not choices and ('liveness' in str(why) or 'terminal' in str(why))
        for label, nxt in choices:
            if nxt == 'K3':
                continue
            nk = key(nxt)
            if nk not in seen:
                seen[nk] = (nxt, k, label)
                queue.append(nk)
    return None


def visible_readmit():
    """Keep an original retry in flight until the bad cross-open rebind answers it."""
    machine = store.explore('readmit_rebinds', 'recover_new_bytes', machine=True)
    key = machine['key']
    first = key(machine['init'])
    seen = {first: (machine['init'], None, None)}
    queue = deque([first])
    while queue:
        k = queue.popleft()
        state = seen[k][0]
        choices, _, _ = options(machine, state)
        for label, nxt in choices:
            if (state['latest'] is not None and state['latest'] == nxt['latest']
                    and state['digest'] != nxt['digest'] and label.endswith('rebind')):
                p, r = state['latest'], state['bound_r']
                mismatches, _, _ = options(machine, nxt)
                for ml, mismatched in mismatches:
                    if ml != f'mismatch p{p} r{r}':
                        continue
                    deliveries, _, _ = options(machine, mismatched)
                    for dl, delivered in deliveries:
                        if dl != f'stale p{p} r{r}':
                            continue
                        path, cur = [], k
                        while seen[cur][1] is not None:
                            after, prev, lab = seen[cur]
                            path.append((seen[prev][0], lab, after))
                            cur = prev
                        path = list(reversed(path)) + [(state, label, nxt), (nxt, ml, mismatched), (mismatched, dl, delivered)]
                        return path, delivered, f'M: r{r} at p{p} got RequestMismatch after readmit changed its original binding (B)', False
            nk = key(nxt)
            if nk not in seen:
                seen[nk] = (nxt, k, label)
                queue.append(nk)
    raise ValueError('readmit_rebinds has no retained-retry consequence')


def calls_trace(history, machine):
    path, final, why, complete = history
    reqs, groups, events = machine['reqs'], machine['groups'], []
    for old, label, new in path:
        epoch = old[20] * 16
        if match := re.match(r'send(\d+)_', label):
            se, stream, p, _, _ = reqs[int(match[1])]
            events.append(sent(epoch + se, stream, p))
        for g, (se, k) in enumerate(groups):
            count = new[24][g] - old[24][g]
            if count:
                stream, position = next((r[1], r[2]) for r in reqs if r[0] == se and calls.key(r[1], r[2]) == k)
                out = (old[13][se][0][3] if label.startswith('deliver_s') else
                       next((new[16][i] for i, r in enumerate(reqs)
                             if r[0] == se and calls.key(r[1], r[2]) == k and new[16][i] != old[16][i]), 'Abandoned'))
                events += [got(epoch + se, stream, position, new[15][se], answer(out, k)) for _ in range(count)]
        for op in new[3][len(old[3]):] if len(new[3]) > len(old[3]) else ():
            if op[0] == 'reply' and op[3] in ('ok', 'err'):
                events.append(dict(kind='Ran', stream=word(op[1]), p=op[2], digest=[], version=op[4]))
        if label in ('crash', 'poison_exit'):
            for se in range(len(old[15])):
                events.append(dict(kind='Closed', conn=epoch + se, code=1001, at=0))
    # C5 checks an about-to-be-delivered frame. Export that first visible step,
    # rather than stopping at the model's internal pre-delivery assertion.
    if why and str(why).startswith("('C5"):
        for se, queue in enumerate(final[13]):
            if queue:
                fv, stream, p, out, cv = queue[0]
                v = final[15][se] if fv is None else fv
                if v < final[15][se] or cv is not None and v < cv:
                    events.append(got(final[20] * 16 + se, stream, p, v, answer(out, calls.key(stream, p))))
                    break
    return observed(events, ran=True, complete=complete)


def store_trace(history):
    path, final, why, complete = history
    events, version = [], 0
    for old, label, new in path:
        if label.startswith('send '):
            events.append(sent(old['i'], 0, old['p'], (old['i'],)))
        if label.startswith(('admit ', 'run ')):
            version += 1
            if new['latest'] is not None and new['digest'] is not None:
                events.append(dict(kind='Pos', stream=word(0), latest=new['latest'], state=1 if new['sstate'][0] == 'A' else 2,
                                   digest=[new['bound_r']]))
        if label.startswith('run '):
            p, r = map(int, re.search(r'p(\d+) r(\d+)', label).groups())
            events.append(dict(kind='Ran', stream=word(0), p=p, digest=[r], version=version))
        if label.startswith('recv '):
            kind = label.split()[1]
            response = next((ans for p, r, ans in old['resp']
                             if (p, r) == (old['p'], old['i']) and ans[0] == kind), None)
            if response is not None:
                events.append(got(old['i'], 0, old['p'], version,
                                  answer(kind, response[1] if kind == 'Returned' else old['i'])))
        elif label.startswith('stale '):
            remaining = list(new['resp'])
            for p, r, ans in old['resp']:
                if (p, r, ans) in remaining:
                    remaining.remove((p, r, ans))
                else:
                    events.append(got(r, 0, p, version, answer(ans[0], ans[1] if ans[0] == 'Returned' else r)))
                    break
    law = str(why).split(':', 1)[0] if why else None
    if law == 'H':
        events.append(sent(final['i'], 0, final['p'], (final['i'],)))
    elif law == 'I':
        for p, r, ans in final['resp']:
            if (p, r) == (final['p'], final['i']) and ans[0] == 'Returned':
                events.append(got(r, 0, p, version, answer('Returned', ans[1])))
                break
            if (p, r) == (final['p'], final['i']) and ans[0] == 'Superseded':
                # v2.11.4 has no Superseded on the wire: the same head bug sends the
                # position's stored reply, the reply of the request that ran there.
                events.append(got(r, 0, p, version, answer('Returned', final['hist'][str(p)])))
                break
    elif law == 'M' and not (path and path[-1][1].startswith('stale ')):
        match = re.search(r'r(\d+) at p(\d+)', str(why))
        r, p = map(int, match.groups())
        events.append(got(r, 0, p, version, answer('RequestMismatch')))
    return observed(events, digest=True, ran=True, complete=complete)


def u32(n):
    return struct.pack('<I', n)


def blob(b):
    return u32(len(b)) + bytes(b)


def batch(version, rows=(), complete=(), outcome=False, reset=False):
    data = u32(8 if reset else 6) + struct.pack('<Q', version)
    if not reset:
        data += u32(len(complete)) + b''.join(u32(i) for i in complete)
        data += u32(len(rows))
        for sub, key, value in rows:
            data += u32(sub) + blob(key) + u32(int(value is not None))
            if value is not None:
                data += blob(value)
    data += u32(int(outcome))
    if outcome:
        payload = struct.pack('<IQ', 0, 0)
        data += struct.pack('<IQ', 0, 0) + u32(0) + blob(payload)
    return list(data)


def subscribe(sub):
    return list(u32(2) + u32(sub) + u32(0) + blob(()) + u32(0))


def subs_trace(history):
    path, final, why, complete = history
    events, ids, serial = [], {}, 0
    queued_completions = {0: [], 1: []}

    def delivered(state, se):
        kind, version, rows, outs, ended = state[6][se][0]
        conn = state[11] * 16 + se
        normalized = []
        completed = queued_completions[se][0] if queued_completions[se] else ()
        for (idx, key), value in rows:
            sub = ids.get((conn, idx), idx)
            normalized.append((sub, key.encode(), None if value is None else word(int(value))))
        events.append(dict(kind='Rx', conn=conn, frame=batch(version, normalized, sorted(completed), outs, kind == 'R')))
        if outs:
            events.append(got(conn, 0, 0, version, answer('ok')))
        if kind == 'R':
            for key in list(ids):
                if key[0] == conn:
                    del ids[key]

    for old, label, new in path:
        if label == 'call':
            events.append(sent(old[11] * 16, 0, 0))
            events.append(dict(kind='Ran', stream=word(0), p=0, digest=[], version=len(new[0])))
        elif label.startswith('subscribe'):
            idx = int(label[len('subscribe'):])
            se = subs.SUBS[idx][0]
            conn = old[11] * 16 + se
            serial += 1
            ids[(conn, idx)] = serial
            events.append(dict(kind='Tx', conn=conn, frame=subscribe(serial)))
        elif label.startswith('wire'):
            se = int(label[4:])
            delivered(old, se)
            queued_completions[se] = queued_completions[se][1:]
        elif label == 'crash':
            events += [dict(kind='Closed', conn=old[11] * 16 + se, code=1001, at=0) for se in (0, 1)]
            if new[12]:
                events.append(sent(new[11] * 16, 0, 0))
            queued_completions = {0: [], 1: []}
        if not label.startswith('wire') and label != 'crash':
            for se in (0, 1):
                before, after = old[6][se], new[6][se]
                if before == after:
                    continue
                if len(after) >= len(before) and after[:len(before)] == before:
                    for frame in after[len(before):]:
                        complete_ids = ()
                        if label.startswith('subscribe') and frame[0] == 'B' and not frame[4]:
                            idx = int(label[len('subscribe'):])
                            complete_ids = (ids[(old[11] * 16 + se, idx)],)
                        queued_completions[se].append(complete_ids)
                else:
                    # A Reset replaces the queue and drops every initial result.
                    queued_completions[se] = [()] * len(after)
    # K8 and K2 are checked just before wire delivers a frame.
    if why and (str(why).startswith("('K8") or str(why).startswith("('K2")):
        for se, queue in enumerate(final[6]):
            if queue and (queue[0][3] or queue[0][1] < final[8][se]):
                delivered(final, se)
                break
    return observed(events, ran=True, complete=complete)


def save(path, name, samples, counterexamples):
    import handoffs
    names = {'calls25': ('join', 'wake', 'cancel'), 'store-v224': ('admit', 'reply'), 'subs25': ('publication',)}[name]
    data = dict(model=name, samples=samples, counterexamples=counterexamples,
                inventories=handoffs.inventory_data(names), boundaries=handoffs.boundary_traces(name))
    Path(path).write_text(json.dumps(data, separators=(',', ':')) + '\n')
    return data


def export_calls(path):
    samples = []
    names = sorted(calls.SCENARIOS)
    for seed in range(200):
        scenario = names[seed % len(names)]
        machine = calls.explore(scenario, 'none', machine=True)
        history = sample(machine, seed)
        if history[2] is not None or not history[3]:
            raise ValueError(f'correct calls model did not quiesce: {scenario}, {history[2]}')
        samples.append(dict(seed=seed, scenario=scenario, trace=calls_trace(history, machine)))
    first = dict(deadline_rides_release='review_deadline', direct_commitless='joiners',
                 answer_before_durable='review_deadline', queue_outside_account='review_deadline',
                 poison_answers='poison', gone_refused_at_session='upgrade_gone', decode_after_admit='refusals',
                 store_v22_admit='out_of_order_two_sessions', drop_joiner='joiners', release_at_commit_version='poison',
                 late_completion_resent='review_deadline', one_for_joined='same_session_double_send',
                 join_outside_account='join_overflow', rerun_on_duplicate='failed_resent')
    counterexamples = []
    for mutant in calls.MUTANTS:
        if mutant == 'none':
            continue
        scenario = first[mutant]
        machine = calls.explore(scenario, mutant, machine=True)
        history = witness(machine)
        if history is None:
            raise ValueError(f'no calls counterexample for {mutant}')
        why = str(history[2])
        clause = ('J2' if 'C9 more' in why else 'J3' if 'C9 a send' in why else
                  'V' if why.startswith("('C5") else None)
        counterexamples.append(dict(mutant=mutant, scenario=scenario, why=history[2], clause=clause,
                                    internal=None if clause else 'the violated law reads head, journal or store state',
                                    trace=calls_trace(history, machine)))
    return save(path, 'calls25', samples, counterexamples)


def export_store(path):
    samples = []
    for seed in range(200):
        client = store.CLIENTS[seed % 4]
        machine = store.explore('none', client, machine=True)
        history = sample(machine, seed)
        if history[2] is not None or not history[3]:
            raise ValueError(f'correct store model did not quiesce: {client}, {history[2]}')
        samples.append(dict(seed=seed, client=client, trace=store_trace(history)))
    counterexamples = []
    for mutant in (*store.STORE_MUTANTS, 'advance_on_refusal'):
        found = None
        if mutant == 'readmit_rebinds':
            found = ('recover_new_bytes', visible_readmit())
        for client in (('advance_on_refusal',) if mutant == 'advance_on_refusal' else store.CLIENTS[:4]):
            if found is not None:
                break
            machine = store.explore('none' if mutant == 'advance_on_refusal' else mutant, client, machine=True)
            history = witness(machine)
            if history is not None:
                found = (client, history)
                break
        if found is None:
            # A binding violation need not have an H/I/M consequence in this
            # bounded one-client alphabet. Keep the original B witness internal.
            for client in store.CLIENTS[:4]:
                machine = store.explore(mutant, client, machine='binding')
                history = witness(machine)
                if history is not None:
                    found = (client, history)
                    break
        if found is None:
            raise ValueError(f'no store counterexample for {mutant}')
        client, history = found
        law = str(history[2]).split(':', 1)[0]
        clause = law if law in ('H', 'I', 'M') else None
        counterexamples.append(dict(mutant=mutant, client=client, why=history[2], clause=clause,
                                    internal=None if clause else 'binding-only; the bounded client has no H/I/M consequence',
                                    trace=store_trace(history)))
    return save(path, 'store-v224', samples, counterexamples)


def subs_machine(scenario, mutant):
    main, fork, subscriptions, crash = subs.SCEN[scenario]
    subs.MAIN, subs.SUBS = main, subscriptions
    return subs.explore(mutant, fork_ok=fork, crash_ok=crash, machine=True)


def export_subs(path):
    samples = []
    names = sorted(subs.SCEN)
    for seed in range(200):
        scenario = names[seed % len(names)]
        machine = subs_machine(scenario, 'none')
        history = sample(machine, seed)
        if history[2] is not None or not history[3]:
            raise ValueError(f'correct subscription model did not quiesce: {scenario}, {history[2]}')
        samples.append(dict(seed=seed, scenario=scenario, trace=subs_trace(history)))
    counterexamples = []
    for mutant in subs.MUTANTS:
        if mutant == 'none':
            continue
        for scenario in (('outcome_only',) if mutant == 'outcome_never' else names):
            machine = subs_machine(scenario, mutant)
            history = witness(machine)
            if history is not None:
                break
        else:
            raise ValueError(f'no subscription counterexample for {mutant}')
        why = str(history[2])
        clause = 'J2' if why.startswith("('K8") else 'W3' if why.startswith("('K2") else 'J3' if 'K5 liveness' in why else None
        counterexamples.append(dict(mutant=mutant, scenario=scenario, why=history[2], clause=clause,
                                    internal=None if clause else 'query/reference, queue budget, or durable-publication state',
                                    trace=subs_trace(history)))
    return save(path, 'subs25', samples, counterexamples)
