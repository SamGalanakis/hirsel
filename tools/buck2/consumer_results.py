"""Conservative triage of completed consumer results against exact ledger rows.

Buck event decoding and libtest progress follow event_logs.py:test_results and
Rust's JSON protocol, already consumed by test_report.py:Progress. A ledger's
free-form family description is insufficient evidence to suppress a failure.
"""
import json
from pathlib import Path
import re
import sys

import event_logs


def ledger(path):
    rows = {}
    for number, line in enumerate(Path(path).read_text().splitlines(), 1):
        if not line.strip() or line.startswith('#'):
            continue
        fields = line.split('\t')
        if len(fields) != 4 or any(not field.strip() for field in fields):
            raise ValueError(f'{path}:{number}: expected four nonempty ledger fields')
        identity = fields[0].removeprefix('root//:').removeprefix('//:')
        if identity in rows:
            raise ValueError(f'{path}:{number}: duplicate identity {identity}')
        rows[identity] = fields[1]
    return rows


def caps(text):
    result = set()
    for pattern, kind in ((r'action cap=(\d+(?:\.\d+)?)s|RE(\d+)\b', 'action'),
                          (r'(?:after |inner)(\d+(?:\.\d+)?)s?\b', 'inner'),
                          (r'(?<!action )\bcap=(\d+(?:\.\d+)?)s', 'cap')):
        for match in re.finditer(pattern, text):
            result.add((kind, float(next(value for value in match.groups() if value))))
    return result


def normalized(text):
    return ' '.join(text.split())


def evidence(details):
    """Read terminal and active bodies, plus their independent panic payloads."""
    started = set(re.findall(r'^HIRSEL_TEST_STARTED (\S+)$', details, re.M))
    finished = dict(re.findall(r'^test (\S+)(?: - should panic)? \.\.\. (ok|FAILED|ignored)\b', details, re.M))
    for status, name in re.findall(r'^HIRSEL_TEST_FINISHED (ok|failed|ignored) (\S+)$', details, re.M):
        finished[name] = 'FAILED' if status == 'failed' else status
    running = set(re.findall(r'^HIRSEL_TEST_RUNNING (\S+) elapsed=', details, re.M))
    diagnostics = {}
    for line in details.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if not isinstance(event, dict) or event.get('type') != 'test':
            continue
        name = event.get('name')
        status = event.get('event')
        if not name:
            continue
        if status == 'started':
            started.add(name)
        elif status in ('ok', 'failed', 'ignored'):
            finished[name] = 'FAILED' if status == 'failed' else status
            if status == 'failed':
                diagnostics[name] = (event.get('stdout', '') + '\n' + event.get('message', '')).strip()
    # Preserve the whole panic, including left/right assertion values. Rust's
    # optional backtrace note is not part of the assertion diagnostic.
    pattern = r"thread '([^']+)'(?: \(\d+\))? panicked at ([^\n]+)\n(.*?)(?=\n\s*\n|\nnote: run with|\nstack backtrace:|\nfailures:|\Z)"
    for match in re.finditer(pattern, details, re.S):
        name, location, payload = match.groups()
        if name in finished:
            diagnostics[name] = location + '\n' + payload.strip()
    failed = {name for name, status in finished.items() if status == 'FAILED'}
    executed = ({name for name, status in finished.items() if status != 'ignored'} | started | running) - {
        name for name, status in finished.items() if status == 'ignored'}
    return executed, failed | running, diagnostics, running


def classify(target, result, rows):
    """Return known/new/uncertain, or pass only with positive body evidence."""
    target = target.removeprefix('root//:').removeprefix('//:')
    details = result.get('details') or ''
    # unittest's own terminal census also proves bodies for script targets.
    # Skipped bodies count in "Ran N", so subtract them before accepting it.
    python_summaries = list(re.finditer(r'^Ran (\d+) tests? in [\d.]+s$', details, re.M))
    python_oks = list(re.finditer(r'^OK(?: \(([^\n]+)\))?$', details, re.M))
    python_summary = python_summaries[-1] if python_summaries else None
    python_ok = python_oks[-1] if python_oks else None
    if (result.get('status') == event_logs.PASS and python_summary and python_ok
            and python_ok.start() > python_summary.end()):
        skipped = re.search(r'\bskipped=(\d+)', python_ok[1] or '')
        count = int(python_summary[1]) - (int(skipped[1]) if skipped else 0)
        return ('pass', f'{count} executed bodies') if count > 0 else ('uncertain', 'zero executed bodies')
    executed, active, diagnostics, running = evidence(details)
    if not executed:
        return 'uncertain', 'zero executed bodies'
    if result.get('status') == event_logs.PASS:
        summaries = re.findall(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed;', details, re.M)
        summary = summaries[-1] if summaries else None
        successful = re.findall(r'^test (\S+)(?: - should panic)? \.\.\. ok\b', details, re.M)
        if (active or not summary or summary[0] != 'ok' or int(summary[1]) == 0
                or int(summary[2]) != 0 or int(summary[1]) != len(set(successful))):
            return 'uncertain', 'missing complete passing body result'
        return 'pass', f'{len(executed)} executed bodies'
    if not active:
        return 'uncertain', 'failure has no localized active body'
    if '<launcher setup, discovery, or child teardown>' in details:
        return 'uncertain', 'deadline has no localized active body'
    if (result.get('status') == event_logs.TIMED_OUT or 'HIRSEL_TEST_INNER_DEADLINE ' in details) and not running:
        return 'uncertain', 'deadline has no localized active body'
    if executed - active - set(re.findall(r'^test (\S+)(?: - should panic)? \.\.\. (?:ok|ignored)\b', details, re.M)):
        return 'uncertain', 'unknown active body at failure'
    binary = re.sub(r'(?:-cpus-\d+)?-shard-\d+$', '', target)
    verdicts = []
    for body in sorted(active):
        candidates = [(key, rows[key]) for key in (target, binary + '::' + body) if key in rows]
        if not candidates:
            verdicts.append(('new', f'{body}: no exact selector row'))
            continue
        # Prior receipts remain evidence, but cannot establish that the current
        # assertion and cap still describe this failure.
        current = []
        for key, reason in candidates:
            reason = re.split(r'\bPrior(?: evidence)?:', reason, maxsplit=1)[0]
            if key == target:
                # A target row may describe several failing cases. Its other
                # case's assertion is never evidence for this active body.
                named = set(re.findall(r"(?:\bFailed\s+|HIRSEL_TEST_RUNNING\s+|thread ')([\w:]+)", reason))
                named.update(re.findall(r'\b[A-Za-z_]\w*(?:::[A-Za-z_]\w*)+\b', reason))
                named = {name.removeprefix(binary + '::') for name in named}
                if named != {body}:
                    continue
            current.append(reason)
        candidates = current
        diagnostic = diagnostics.get(body)
        if body in running:
            # A localized deadline needs the exact active body and both the
            # inner and action caps. An aggregate timeout alone never matches.
            diagnostic = next((line for line in details.splitlines() if line.startswith('HIRSEL_TEST_INNER_DEADLINE ')), None)
        observed_caps = caps(details)
        deadline_caps = {kind for kind, _ in observed_caps}
        if not diagnostic or not observed_caps or body in running and not {'inner', 'action'} <= deadline_caps:
            verdicts.append(('uncertain', f'{body}: diagnostic or cap evidence missing'))
            continue
        matched = any(re.search(r'(?<![\w:])' + re.escape(body) + r'(?![\w:])', reason)
                      and normalized(diagnostic) in normalized(reason)
                      and observed_caps == caps(reason) for reason in candidates)
        verdicts.append(('known' if matched else 'uncertain',
                         f'{body}: ' + ('exact selector, diagnostic and cap match' if matched else 'body, assertion or cap differs from row')))
    # A known member never hides another unrecognized failure in its shard.
    state = 'new' if any(state == 'new' for state, _ in verdicts) else 'uncertain' if any(state == 'uncertain' for state, _ in verdicts) else 'known'
    return state, '; '.join(reason for _, reason in verdicts)


def report(paths, table):
    rows = ledger(table)
    counts = dict(known=0, new=0, uncertain=0, passed=0)
    for path in paths:
        results = event_logs.test_results(path, history=event_logs.NoTestHistory(), all_results=True)
        if not results:
            raise ValueError(f'{path}: no Buck test results')
        for target, result in results:
            state, reason = classify(target, result, rows)
            counts['passed' if state == 'pass' else state] += 1
            if state in ('new', 'uncertain'):
                print(f'{state}\t{target}\t{reason}')
    print('consumers: ' + ', '.join(f'{count} {state}' for state, count in counts.items()), file=sys.stderr)
    return 1 if counts['new'] or counts['uncertain'] else 0
