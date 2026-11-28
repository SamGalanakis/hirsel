"""The CTF oracle: one run's transcript in, one verdict out.

It judges what the host process did and said. It never trusts a host line that claims
success: a verdict is DEFENDED only when no breach signal is present, the canary answered
a fresh challenge after the attack, the host exited cleanly, and the attack ended by one
of the mechanisms its challenge declared. Everything else is BREACH, UNEXPECTED or
ERROR, and none of those pass.
"""
import base64
from dataclasses import dataclass, field
import hashlib
import re
import signal

DEFENDED, BREACH, UNEXPECTED, ERROR = 'DEFENDED', 'BREACH', 'UNEXPECTED', 'ERROR'
# The shortest run of secret bytes that counts as a leak: more than 8.
WINDOW = 9
# hirsel_guard's exit status for a process that reached its memory limit.
GUARD_LIMIT_STATUS = 71

LOAD_REFUSALS = ['NotAdmitted', 'Malformed', 'Mismatch', 'Import', 'Unbound', 'Fixup',
                 'NoEntry', 'Capacity', 'Quarantined', 'NotPrelinked', 'Unsupported']
CAUSES = ['failed:Overflow', 'failed:Bounds', 'failed:Division', 'failed:Unwrap', 'failed:Fail',
          'failed:Cancelled', 'bounded:Capacity', 'bounded:OutOfTime', 'bounded:Stack']
RULE = re.compile(r'\b([A-Z]{2,8})-(\d+)\b')
# `<file>:<line>:<column>[-<end>]: <first word of the message>`
DIAGNOSTIC = re.compile(r':\d+:\d+(?:-[\d:]+)?: (\S+)')
FIELD = re.compile(r'^([a-z]+)=(.*)$')


def needles(secret):
    """Every spelling of every WINDOW-byte run of `secret` the scan looks for."""
    found = set()
    for at in range(len(secret) - WINDOW + 1):
        run = secret[at:at + WINDOW]
        found.update((run, run.hex().encode(), run.hex().upper().encode(),
                      base64.b64encode(run), base64.urlsafe_b64encode(run)))
    return found


def unhex(text):
    try:
        return bytes.fromhex(text) if text and len(text) % 2 == 0 else None
    except ValueError:
        return None


def haystacks(stdout, stderr):
    """Everything the attack could have written to: both streams as they are, and every
    hex field of every host line decoded."""
    hay = [stdout, stderr]
    for token in stdout.split():
        value = token.split(b'=', 1)[-1].decode('ascii', 'replace')
        decoded = unhex(value)
        if decoded:
            hay.append(decoded)
    return hay


def leaks(secret, stdout, stderr):
    hay = haystacks(stdout, stderr)
    return any(needle in part for needle in needles(secret) for part in hay)


def answer(secret, nonce):
    """What a live canary holding `secret` answers to `nonce`."""
    return hashlib.sha256(secret + nonce).hexdigest()


def fields(line):
    out = {}
    for token in line.split(' ')[1:]:
        match = FIELD.match(token)
        if match:
            out[match[1]] = match[2]
    return out


def shown(data):
    """Returned bytes as a mechanism suffix: the text itself when it is plain."""
    if data and all(0x21 <= byte < 0x7f and byte != 0x2c for byte in data):
        return data.decode('ascii')
    return 'hex:' + data.hex()


def text_of(value):
    decoded = unhex(value or '')
    return decoded.decode('utf-8', 'replace') if decoded is not None else ''


def refusal(text):
    """The rule a compile refusal names, or `resource` for the implementation's resource
    refusal, which names none (PROG-6)."""
    first = DIAGNOSTIC.search(text)
    if first:
        rule = RULE.fullmatch(first[1])
        if rule:
            return 'refused:' + first[1]
        if first[1] == 'resource':
            return 'refused:resource'
    rule = RULE.search(text)
    return 'refused:' + rule[0] if rule else 'refused:unnamed'


def detail(line):
    """The words the host gave with its reply: a diagnostic, a failure text or a refusal."""
    if not line:
        return ''
    got = fields(line)
    if line.startswith(('admit-refused ', 'raw-refused ')):
        return text_of(line.split(' ', 1)[1])
    if line.startswith('compile-refused '):
        return re.sub(r'\S*/(main\.hirsel)', r'\1', text_of(got.get('text')).strip())[:240]
    if line.startswith('report ') and got.get('kind') != '1':
        said = text_of(got.get('out'))
        numbers = ' '.join(f'{name}={got[name]}' for name in ('a', 'b', 'causes') if got.get(name, '0') != '0')
        return (numbers + ' ' + said).strip()[:240]
    if line.startswith(('run-stop ', 'host-stop ')):
        return ' '.join(f'{name}={got[name]}' for name in ('fault', 'quarantined') if name in got)
    return ''


def number(got, name):
    try:
        return int(got.get(name, ''))
    except ValueError:
        return -1


def mechanism(line):
    """How the attack ended, from the host's reply line. `unknown` for anything this
    oracle does not recognise in full, so a new host line can never pass by accident."""
    if line is None:
        return 'none'
    head = line.split(' ', 1)[0]
    got = fields(line)
    if head == 'compile-refused':
        kind = got.get('kind')
        if kind == 'Rejected':
            return refusal(text_of(got.get('text')))
        if kind == 'Limit':
            return 'compile-limit:' + re.sub(r'\W.*', '', text_of(got.get('text')))
        if kind == 'ToolRefusal' and refusal(text_of(got.get('text'))) == 'refused:resource':
            # The checker's own resource refusal, which the supervisor reports as the tool's.
            return 'refused:resource'
        return {'Unsupported': 'compile-unsupported', 'ToolRefusal': 'compile-tool-refusal',
                'Unavailable': 'compile-unavailable', 'Busy': 'compile-busy'}.get(kind, 'unknown')
    if head == 'admit-refused':
        return 'admit-refused'
    if head == 'report':
        stage, kind, first = (number(got, name) for name in ('stage', 'kind', 'a'))
        if stage == 1 and 1 <= kind <= len(LOAD_REFUSALS):
            return 'load-refused:' + LOAD_REFUSALS[kind - 1]
        if stage != 2:
            return 'unknown'
        if kind == 1:
            data = unhex(got.get('out', ''))
            return 'unknown' if data is None else 'returned:' + shown(data)
        if kind == 2:
            return 'bounded:OutOfTime'
        if kind in (3, 5):
            return 'bounded:Capacity'
        if kind == 4 and 1 <= first <= len(CAUSES):
            return CAUSES[first - 1]
        return 'unknown'
    if head == 'escaped':
        return 'escaped:' + got.get('status', '?')
    if head == 'run-stop':
        return 'stopped:' + (text_of(got.get('name')) or '?')
    if head == 'host-stop':
        return 'bounded:Wall'
    if head == 'raw-refused':
        name = re.match(r'\w+', text_of(line.split(' ', 1)[1] if ' ' in line else ''))
        return 'raw-refused:' + name[0] if name else 'unknown'
    if head == 'raw-loaded':
        return 'raw-loaded'
    return 'unknown'


def matches(found, expected):
    """`refused:OWN` admits every OWN rule; every other expectation is exact."""
    for want in expected:
        if found == want:
            return True
        if want.startswith('refused:') and '-' not in want and found.startswith(want + '-'):
            return True
    return False


@dataclass
class Step:
    """One offer to the host and how the challenge says it must end."""
    reply: str | None
    expected: tuple


@dataclass
class Transcript:
    secret: bytes
    decoy: bytes
    ready: bool = False
    sealed: str | None = None
    before: tuple | None = None   # (nonce, reply) of the probe before the attack
    steps: list = field(default_factory=list)
    after: tuple | None = None    # (nonce, reply) of the probe after the attack
    bye: str | None = None
    exit_code: int | None = None  # negative: the signal that ended the host
    killed: str | None = None     # 'timeout' or 'rss' when the harness ended the host
    stdout: bytes = b''
    stderr: bytes = b''
    peak_rss_kb: int = 0
    rss_ceiling_kb: int = 0


@dataclass
class Verdict:
    verdict: str
    breaches: list
    mechanisms: list
    notes: list


def judge(run):
    breaches, notes = [], []
    if leaks(run.secret, run.stdout, run.stderr):
        breaches.append('leak:canary')
    if leaks(run.decoy, run.stdout, run.stderr):
        breaches.append('leak:decoy')
    if run.killed == 'timeout':
        breaches.append('hang')
    if run.killed == 'rss' or run.peak_rss_kb > run.rss_ceiling_kb:
        breaches.append('rss')
    code = run.exit_code
    if run.killed is None:
        if code is None:
            breaches.append('hang')
        elif code < 0:
            try:
                breaches.append('crash:' + signal.Signals(-code).name)
            except ValueError:
                breaches.append(f'crash:signal-{-code}')
        elif code == GUARD_LIMIT_STATUS:
            breaches.append('oom')
        elif code != 0:
            breaches.append(f'exit:{code}')

    attacked = bool(run.steps)
    started = (run.ready and run.sealed == 'sealed' and run.before is not None
               and run.before[1] == 'answer ' + answer(run.secret, run.before[0]))
    if not started:
        notes.append('the host did not reach a sealed, answering canary before the attack')
    if attacked:
        if run.after is None or run.after[1] != 'answer ' + answer(run.secret, run.after[0]):
            breaches.append('canary:probe')
        if run.bye is None or not run.bye.startswith('bye canary=retired '):
            breaches.append('canary:retire')

    found = [mechanism(step.reply) for step in run.steps]
    for step, name in zip(run.steps, found):
        if (step.reply or '').startswith('bad-command'):
            notes.append('the host refused a harness command: ' + step.reply)
        elif not matches(name, step.expected):
            notes.append(f'ended by {name}, not by one of: ' + ', '.join(step.expected))
    if breaches:
        verdict = BREACH
    elif not started or not attacked or any(note.startswith('the host') for note in notes):
        verdict = ERROR
    elif notes:
        verdict = UNEXPECTED
    else:
        verdict = DEFENDED
    return Verdict(verdict, sorted(set(breaches)), found, notes)
