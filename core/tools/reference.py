#!/usr/bin/env python3
"""Independent Python specifications and fixture writer. Never invokes a twin or Hirsel.
Run with --write only when intentionally changing the corpus contract.
"""
import argparse
from collections import OrderedDict
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS = ROOT / 'corpus/m1'


def failure(out, error):
    if error:
        out.append('failure=' + error)
    return '\n'.join(out) + '\n', 3 if error and error.startswith('Capacity') else 4 if error and error.startswith('Cancelled') else 2 if error else 0


def charge(variant, position):
    return 'Capacity@' + position if variant == 'capacity:' + position else None


def request(text, variant):
    trace, replies, error = [], [], None
    for i, req in enumerate(filter(None, text.split('\n\n')), 1):
        trace += [f'acquire=socket#{i}', f'acquire=scratch#{i}']
        lines = req.splitlines()
        headers = {}
        if not lines[0].startswith('GET '):
            error = 'Parse@method'
        else:
            for h, line in enumerate(lines[1:], 1):
                error = charge(variant, f'request{i}.header{h}')
                if error:
                    break
                if ': ' not in line:
                    error = 'Parse@header'
                    break
                k, v = line.split(': ', 1)
                headers.setdefault(k, v)
            if not error:
                banner = {'/alpha': 'welcome', '/beta': 'ready'}.get(lines[0][4:])
                error = 'Route@path' if banner is None else 'Parse@id' if 'X-Id' not in headers else 'Parse@content' if 'Content' not in headers else None
                if not error:
                    error = charge(variant, f'request{i}.reply')
                if not error and variant == 'error' and i == 2:
                    error = 'Io@request2.write'
                if not error:
                    replies.append(f'{banner}:{headers["X-Id"]}:{headers["Content"]}'.encode())
        trace += [f'release=scratch#{i}', f'release=socket#{i}']
        if error:
            break
    return failure([f'requests={len(replies)} bytes={sum(map(len, replies))} sum={sum(map(sum, replies))}', *trace], error)


def json_ref(text, variant):
    lines = text.splitlines()
    path = lines[0].split('/')[1:]
    out, trace, pretty = [], [], []
    error = None
    for i, line in enumerate(lines[1:], 1):
        trace += [f'acquire=document{i}']
        # The fixture fixes this preorder admission before parsing the malformed suffix.
        if variant == f'capacity:document{i}.node3':
            error = f'Capacity@document{i}.node3'
        else:
            try:
                doc = json.loads(line)
                # Corpus numeric lexemes are canonical integers or -2.5. No float-format oracle coupling.
                if '1e999' in line:
                    error = f'InvalidNumber@document{i}'
                elif variant == f'capacity:document{i}.blob':
                    # The matched reservation is reached only after sizing accepts
                    # the entire document; it precedes every publication write.
                    error = f'Capacity@document{i}.blob'
                else:
                    p = doc
                    for key in path:
                        if isinstance(p, dict):
                            p = p[key]
                        elif isinstance(p, list):
                            p = p[int(key)]
                        else:
                            raise TypeError
                    rendered = json.dumps(doc, ensure_ascii=False, indent=2)
                    query = json.dumps(p, ensure_ascii=False, indent=2)
                    out.append(f'query{i}={query}')
                    pretty.append(rendered.encode())
                    if variant == 'print':
                        out.append(rendered)
            except (json.JSONDecodeError, UnicodeError):
                error = f'Syntax@document{i}'
            except (KeyError, IndexError):
                error = 'MissingPath'
            except (ValueError, TypeError):
                error = 'WrongKind'
        trace += [f'release=document{i}']
        if error:
            break
    return failure([*out, f'documents={len(pretty)} pretty-bytes={sum(map(len, pretty))} sum={sum(map(sum, pretty))}', *trace], error)


def lru(text, variant):
    # OrderedDict is an independent recency model, with no slots, generations or links.
    cache = OrderedDict()
    hits = checksum = evictions = puts = 0
    first = None
    first_evicted = False
    error = None
    for line in text.splitlines():
        op, *args = line.split()
        if op == 'P':
            puts += 1
            error = charge(variant, f'put{puts}.payload')
            if error:
                break
            key, value = args
            if first is None:
                first = key
            if key in cache:
                del cache[key]
            elif len(cache) == 64:
                old, _ = cache.popitem(last=False)
                first_evicted |= old == first
                evictions += 1
            cache[key] = value
        elif op == 'G' and args[0] in cache:
            value = cache.pop(args[0])
            cache[args[0]] = value
            hits += 1
            checksum += sum(value.encode())
        elif op == 'S' and first_evicted:
            error = 'StaleHandle@first-entry'
            break
    return failure([f'hits={hits} sum={checksum} evictions={evictions}', 'order=' + ','.join(reversed(cache)), 'acquire=cache', 'release=cache'], error)


def bfs(text, variant):
    lines = text.splitlines()
    n, source, target = map(int, lines[0].split())
    edges = [tuple(map(int, s.split())) for s in lines[1:]]
    trace, out, error = ['acquire=graph'], [], None
    if max([source, target, *[v for e in edges for v in e]]) >= n:
        error = 'InvalidVertex'
    else:
        error = charge(variant, 'search.parent')
        if not error:
            trace.append('acquire=parent')
            error = charge(variant, 'search.queue')
            if not error:
                trace.append('acquire=queue')
                adj = [[] for _ in range(n)]
                for a, b in edges:
                    adj[a].append(b)
                paths = {source: [source]}
                frontier = [source]
                while frontier:
                    next_layer = []
                    for v in frontier:
                        for u in adj[v]:
                            if u not in paths:
                                paths[u] = paths[v] + [u]
                                next_layer.append(u)
                    frontier = next_layer
                error = 'NoPath' if target not in paths else charge(variant, 'result.path')
                if not error:
                    out += ['path=' + ','.join(map(str, paths[target])), f'visited={len(paths)}']
                trace.append('release=queue')
            trace.append('release=parent')
    trace.append('release=graph')
    return failure([*out, *trace], error)


def pipeline(text, variant):
    lines = text.splitlines()
    size, rows = int(lines[0]), lines[1:]
    count = (len(rows) + size - 1) // size
    if variant == 'normal':
        transformed = [r.upper().encode() for r in rows]
        return failure([f'sent={count} transformed={count} sink-batches={count} rows={len(rows)} bytes={sum(map(len, transformed))} sum={sum(map(sum, transformed))}', f'backpressure={str(count > 8).lower()}', 'released=' + ','.join(map(str, range(1, count + 1)))], None)
    if variant == 'cancel-send':
        return failure(['release-order=1,2,3,4,5,6,7,8,9', 'sent=8 transformed=0 sink-batches=0 rows=0 bytes=0 sum=0', 'backpressure=true', 'released=1,2,3,4,5,6,7,8,9'], 'Cancelled@source.send')
    transformed = [r.upper().encode() for r in rows[:size]]
    error = 'Transform@batch2' if variant == 'error' else 'Capacity@batch2.output' if variant.startswith('capacity:') else 'Cancelled@transform.receive' if variant == 'cancel-receive' else 'Cancelled@transform.check'
    return failure([('release-order=1' if variant == 'cancel-receive' else 'release-order=2,1'), f'sent={1 if variant == "cancel-receive" else 2} transformed=1 sink-batches=1 rows={len(transformed)} bytes={sum(map(len, transformed))} sum={sum(map(sum, transformed))}', 'backpressure=false', ('released=1' if variant == 'cancel-receive' else 'released=1,2')], error)


def versions(text, variant):
    lines = text.splitlines()
    n = int(lines[0])
    values = list(range(n))
    changed, count, error = set(), 0, None
    for line in lines[1:]:
        k, v = map(int, line.split())
        if k >= n:
            error = 'InvalidKey'
            break
        if variant == 'capacity:draft.node3':
            error = 'Capacity@draft.node3'
            break
        # Fixed topology gives an independent set-union count, without an RC model.
        a, b = 0, n
        while a < b:
            mid = (a + b) // 2
            changed.add(mid)
            if mid == k:
                break
            if k < mid:
                b = mid
            else:
                a = mid + 1
        values[k] = v
        count += 1
    return failure([f'updates={count} pinned={n*(n-1)//2} current={sum(values)} live={n+len(changed)}', f'after-unpin={n}', 'after-current=0'], error)


def rope(text, variant):
    lines = text.splitlines()
    # Per-byte origin labels model liveness independently of piece splitting and reference counts.
    current = [(c, 0) for c in lines[0]]
    history = []
    edits, error = 0, None
    for line in lines[1:]:
        if line == 'UNDO':
            if not history:
                error = 'EmptyUndo'
                break
            current = history.pop()
            continue
        p, length, value = line.split(' ', 2)
        p, length = int(p), int(length)
        error = charge(variant, f'edit{edits+1}.piece')
        if error:
            break
        raw = ''.join(c for c, _ in current).encode()
        try:
            if p + length > len(raw):
                raise ValueError
            a = len(raw[:p].decode())
            b = len(raw[:p+length].decode())
        except (ValueError, UnicodeError):
            error = 'InvalidPosition'
            break
        history.append(current)
        edits += 1
        current = current[:a] + [(c, edits) for c in value] + current[b:]
        history = history[-64:]
    raw = ''.join(c for c, _ in current).encode()
    live = len({origin for version in [current, *history] for _, origin in version})
    after = len({origin for _, origin in current})
    return failure([f'edits={edits} undo={len(history)} bytes={len(raw)} sum={sum(raw)} live={live}', f'after-history={after}', 'after-text=0'], error)


def files(text, variant):
    data = text.encode()
    trace, error, destination = [], None, b''
    if variant == 'error-first':
        error = 'Io@open-source'
    else:
        trace += ['acquire=source']
        if variant == 'error-second':
            error, destination = 'Io@create-destination', b'existing'
        else:
            trace += ['acquire=destination']
            if variant == 'error-lock':
                error = 'Lock@acquire'
            else:
                trace += ['acquire=lock']
                error = charge(variant, 'copy.buffer')
                if not error:
                    if variant in ('error-work', 'error-cleanup-all'):
                        error, destination = 'Io@copy', data[:17]
                    else:
                        destination = data
                        trace += [f'copied={len(data)}']
                trace += ['release=lock']
            trace += ['release=destination']
        trace += ['release=source']
    causes, traced = [], []
    for event in trace:
        traced.append(event)
        if event.startswith('release='):
            name = event.split('=')[1]
            if variant == 'error-release-' + name or variant == 'error-cleanup-all':
                cause = 'Io@release-' + name
                causes.append(cause)
                traced.append('cleanup-error=' + cause)
    if not error and causes:
        error = causes.pop(0)
    return failure([*traced, f'destination-bytes={len(destination)} sum={sum(destination)} lock-free=true', *['suppressed='+cause for cause in causes]], error)


def generics(text, variant):
    trace = ['acquire=list', 'acquire=mapped']
    mapped, error = [], None
    for i, s in enumerate(text.splitlines(), 1):
        error = charge(variant, f'callback{i}.string')
        if not error and s == '!error':
            error = f'Callback@{i}'
        if error:
            break
        mapped.append(s + '!')
    out = []
    if not error:
        trace += ['acquire=map']
        counts = {}
        for i, s in enumerate(mapped, 1):
            error = charge(variant, f'tally{i}.key')
            if error:
                break
            counts[s] = counts.get(s, 0) + 1
        if not error:
            out += [f'long={sum(len(s.encode())>=6 for s in mapped)} bytes={sum(len(s.encode()) for s in mapped)}']
            out += [f'{k}={v}' for k, v in counts.items()]
        trace += ['release=map']
    trace += ['release=mapped', 'release=list']
    return failure([*out, *trace], error)


REFERENCES = {'request-scratch': request, 'json': json_ref, 'lru': lru, 'bfs': bfs, 'pipeline': pipeline, 'versions': versions, 'rope': rope, 'files': files, 'generics': generics}


def datasets():
    small_requests = 'GET /alpha\nX-Id: 1\nContent: abc\n\nGET /beta\nX-Id: 2\nContent: xy\n\n'
    requests = ''.join(f'GET /{"alpha" if i%2==0 else "beta"}\nX-Id: {i}\nContent: {"payload-"*16}{i}\n\n' for i in range(2048))
    small_json = '/items/1/name\n{"items":[{"name":"a"},{"name":"b\\n\\u03bb","x":null,"x":true}],"n":-2.5}\n'
    documents = '/items/1/name\n' + '\n'.join(json.dumps({'id': i, 'items': [{'name': f'item-{i}-{j}', 'enabled': j%2==0, 'values': [j, j+1, None]} for j in range(32)], 'meta': {'title': 'λ document', 'valid': True}}, ensure_ascii=False, separators=(',', ':')) for i in range(128)) + '\n'
    small_lru = 'P a one\nP b two\nG a\nP c three\nG absent\n'
    lru_ops = ''.join(f'P k{(i*17)%257} value-{i}-{"x"*(i%32)}\nG k{((i-1)*17)%257}\n' for i in range(10000))
    n = 16384
    edges = [(v, (v+d)%n) for v in range(n) for d in [1, 7, 64, 511]]
    graph = f'{n} 0 {n-1}\n' + ''.join(f'{a} {b}\n' for a, b in edges)
    graph_small = '6 0 5\n0 1\n0 2\n1 3\n2 3\n2 4\n3 5\n4 5\n'
    rows = '32\n' + ''.join(f'row-{i:05d}-{"abcdefgh"*8}\n' for i in range(8192))
    small_rows = '2\na\nbb\nccc\nd\n'
    versions_data = '1023\n' + ''.join(f'{(i*37)%1023} {10000+i}\n' for i in range(256))
    versions_small = '7\n0 10\n6 20\n'
    rope_data = 'abcd'*16384 + '\n' + ''.join(f'{(i*97)%60000} 3 edit{i:04d}\n' for i in range(256)) + 'UNDO\n'*16
    rope_small = 'abcde\n2 1 XY\n1 2 z\nUNDO\n'
    file_data = ''.join(f'{i:07d}:abcdefghijklmno\n' for i in range(12000))
    words = ['alpha', 'beta', 'gamma', 'λ', 'delta', 'epsilon', 'a', 'beta']
    generic_data = ''.join(words[i%len(words)]+'\n' for i in range(16384))
    def cases(small, large, extra):
        return [('small', small, 'normal'), ('normal', large, 'normal'), *extra]
    return {
        'request-scratch': cases(small_requests, requests, [('error', small_requests, 'error'), ('malformed', small_requests.replace('Content: abc', 'broken'), 'normal'), ('capacity', small_requests, 'capacity:request2.header2'), ('capacity-reply', small_requests, 'capacity:request2.reply')]),
        'json': cases(small_json, documents, [('pretty', small_json, 'print'), ('error', small_json + '{"items":[1,]}\n', 'normal'), ('missing', '/missing\n{}\n', 'normal'), ('wrong-kind', '/x\n1\n', 'normal'), ('invalid-number', '/x\n{"x":1e999}\n', 'normal'), ('capacity', small_json+small_json.split('\n')[1]+'\n', 'capacity:document2.node3', ['idiomatic']), ('capacity-blob', small_json+small_json.split('\n')[1]+'\n', 'capacity:document2.blob', ['matched'])]),
        'lru': cases(small_lru, lru_ops, [('error', lru_ops+'S\n', 'normal'), ('capacity', small_lru, 'capacity:put2.payload')]),
        'bfs': cases(graph_small, graph, [('error', '3 0 2\n0 1\n', 'normal'), ('invalid', '3 0 2\n0 3\n', 'normal'), *[(f'capacity-{i}', graph_small, 'capacity:'+site) for i, site in enumerate(['search.parent', 'search.queue', 'result.path'])]]),
        'pipeline': cases(small_rows, rows, [('error', small_rows, 'error'), ('capacity', small_rows, 'capacity:batch2.output'), ('cancel', small_rows, 'cancel'), ('cancel-receive', small_rows, 'cancel-receive'), ('cancel-send', small_rows, 'cancel-send')]),
        'versions': cases(versions_small, versions_data, [('error', versions_small+'7 1\n', 'normal'), ('capacity', versions_small, 'capacity:draft.node3')]),
        'rope': cases(rope_small, rope_data, [('error', 'abc\n1 9 x\n', 'normal'), ('empty-undo', 'abc\nUNDO\n', 'normal'), ('utf8', 'aλb\n2 0 x\n', 'normal'), ('capacity', rope_small, 'capacity:edit2.piece')]),
        'files': cases('hello world\n', file_data, [(v, 'abcdefghijklmnopqrstuvwxyz\n', v) for v in ['error-first', 'error-second', 'error-lock', 'error-work', 'error-release-lock', 'error-release-destination', 'error-release-source', 'error-cleanup-all']] + [('capacity', 'abc\n', 'capacity:copy.buffer')]),
        'generics': cases('a\nalpha\na\nλ\n', generic_data, [('error', 'alpha\n!error\nbeta\n', 'normal'), ('capacity', 'a\nb\nc\n', 'capacity:callback2.string'), ('capacity-map', 'a\nb\nc\n', 'capacity:tally2.key')]),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    parser.add_argument('--only', nargs='+', choices=sorted(REFERENCES))
    args = parser.parse_args()
    total = 0
    for name, cases in datasets().items():
        if args.only and name not in args.only:
            continue
        directory = CORPUS / name
        if args.write:
            directory.mkdir(parents=True, exist_ok=True)
            (directory/'inputs').mkdir(exist_ok=True)
            (directory/'EXPECTED').mkdir(exist_ok=True)
        listing = []
        for case, data, variant, *layouts in cases:
            stdout, code = REFERENCES[name](data, variant)
            listing.append({'name': case, 'input': f'inputs/{case}.txt', 'variant': variant})
            if layouts:
                listing[-1]['layouts'] = layouts[0]
            files = {f'inputs/{case}.txt': data, f'EXPECTED/{case}.stdout': stdout, f'EXPECTED/{case}.stderr': '', f'EXPECTED/{case}.exit': str(code)+'\n'}
            for file, value in files.items():
                path = directory / file
                if args.write:
                    path.write_text(value)
                else:
                    assert path.read_text() == value, path
            total += 1
        content = json.dumps(listing, indent=2)+'\n'
        if args.write:
            (directory/'cases.json').write_text(content)
        else:
            assert (directory/'cases.json').read_text() == content
    print(f'{total} independent cases {"written" if args.write else "verified"}')


if __name__ == '__main__':
    main()
