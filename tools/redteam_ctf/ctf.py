#!/usr/bin/env python3
"""Run the red-team containment CTF: hostile Hirsel modules against a co-resident canary.

Each case starts one pack host process (crates/hirsel-pack/examples/ctf_host.rs) holding
a Canary Place with a fresh random 256-bit secret, offers it one challenge module as a
zero-capability Attacker Place, and hands the transcript to oracle.py. See README.md.

    tools/redteam_ctf/ctf.py --build                     # the whole corpus, both engines
    tools/redteam_ctf/ctf.py --build --self-check        # every breach mode must be caught
    tools/redteam_ctf/ctf.py --build --sabotage leak     # one broken host: BREACH, exit 1

Exit status: 0 when every case is DEFENDED (or, for --self-check, every broken mode was
caught), 1 on any BREACH or UNEXPECTED, 2 on a harness error.
"""
import argparse
from dataclasses import dataclass, field
import hashlib
import json
import os
from pathlib import Path
import queue
import re
import resource
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))

import oracle

ENGINES = ('cranelift', 'llvm')
DIRECTIVE = re.compile(r'^// ctf: (\w+) (.+)$')
TARGETS = ('//:hirsel-pack-example-ctf_host', '//:hirsel-core-driver-bin-hirsel')
# What each broken host mode must be caught as. `None`: any verdict but DEFENDED.
SABOTAGE = {
    'leak': 'leak:canary', 'leak-window': 'leak:canary', 'leak-hex': 'leak:canary',
    'leak-cause': 'leak:canary', 'leak-stderr': 'leak:canary',
    'crash-abort': 'crash:SIGABRT', 'crash-segv': 'crash:SIGSEGV',
    'hang': 'hang', 'rss': 'rss', 'canary-swap': 'canary:probe', 'canary-kill': 'canary:probe',
}


@dataclass
class Challenge:
    name: str
    path: Path
    group: str = ''
    about: str = ''
    arena: str = 'plain'
    pool: str = 'pure'
    expect: tuple = ()
    steps: int = 2_000_000
    bytes: int = 1 << 20
    root: int = 16 << 20
    wall: int = 10_000
    tampers: list = field(default_factory=list)
    digest: str = ''


def load(path):
    """A challenge is a Hirsel module whose leading `// ctf:` lines say how to offer it
    and how the attack must end."""
    source = path.read_bytes()
    challenge = Challenge(path.stem, path, digest=hashlib.sha256(source).hexdigest())
    for line in source.decode().splitlines():
        match = DIRECTIVE.match(line)
        if not match:
            if line.startswith('//') or not line.strip():
                continue
            break
        key, value = match[1], match[2].strip()
        names = lambda text: tuple(part.strip() for part in text.split(',') if part.strip())
        if key == 'class':
            challenge.group = value
        elif key == 'about':
            challenge.about = (challenge.about + ' ' + value).strip()
        elif key == 'arena' and value in ('plain', 'held'):
            challenge.arena = value
        elif key == 'pool' and value in ('pure', 'provider'):
            challenge.pool = value
        elif key == 'expect':
            challenge.expect += names(value)
        elif key in ('steps', 'bytes', 'root', 'wall'):
            setattr(challenge, key, int(value))
        elif key == 'tamper' and ' expect ' in value:
            plan, expected = value.split(' expect ', 1)
            challenge.tampers.append((plan.strip(), names(expected)))
        else:
            raise ValueError(f'{path}: unknown directive: {line}')
    if not challenge.group or not (challenge.expect or challenge.tampers):
        raise ValueError(f'{path}: a challenge names its class and what it expects')
    return challenge


def corpus(directory=HERE / 'challenges'):
    return [load(path) for path in sorted(directory.glob('*.hirsel'))]


def tamper(module, plan):
    """An altered copy of an admitted module. Every plan is a fixed function of the
    module's bytes, so a case is the same attack on every run."""
    kind, _, argument = plan.partition(':')
    amount = int(argument, 0) if argument else 0
    data = bytearray(module)
    if kind == 'keep':          # the first `amount` bytes only
        return bytes(data[:amount])
    if kind == 'drop':          # all but the last `amount` bytes
        return bytes(data[:len(data) - amount])
    if kind == 'flip':          # one byte inverted; a negative offset counts from the end
        data[amount] ^= 0xff
        return bytes(data)
    if kind == 'grow':          # `amount` zero bytes appended
        return bytes(data) + bytes(amount)
    if kind == 'length':        # section `amount`'s length field set to the largest u32
        at = 132 + 8 * amount
        data[at:at + 4] = b'\xff\xff\xff\xff'
        return bytes(data)
    if kind == 'noise':         # the magic, then `amount` bytes of a fixed hash stream
        out = bytearray(data[:8])
        block = 0
        while len(out) < 8 + amount:
            out += hashlib.sha256(b'redteam-ctf noise %d' % block).digest()
            block += 1
        return bytes(out[:8 + amount])
    if kind == 'bloat':         # the real header, then `amount` MiB of zeros
        return bytes(data[:192]) + bytes(amount << 20)
    raise ValueError(f'unknown tamper plan: {plan}')


@dataclass
class Config:
    host: list                   # argv prefix of the host
    hirsel: str                  # the compile worker the host spawns
    scratch: Path
    trusted: Path = HERE / 'trusted'
    rss_ceiling_kb: int = 256 << 10
    startup_s: float = 180.0     # two capped compiles of the trusted modules
    compile_s: float = 25.0      # PACK-3's wall cap on one compile, and change
    slack_s: float = 15.0


class Host:
    """One host process, its reply lines, and a watch on its resident memory."""

    def __init__(self, config, engine, pool, sabotage, work):
        argv = [*config.host, config.hirsel, str(work / 'host'), str(config.trusted), engine]
        if pool == 'provider':
            argv.append('--provider')
        if sabotage:
            argv += ['--sabotage', sabotage]
        (work / 'host').mkdir()
        # A host that crashes is a verdict, not a core file.
        resource.setrlimit(resource.RLIMIT_CORE, (0, resource.getrlimit(resource.RLIMIT_CORE)[1]))
        self.errors = open(work / 'stderr', 'wb')
        env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'TMPDIR': str(work), 'LANG': 'C'}
        # Its own session, so ending it ends its compile worker too and nothing else.
        self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=self.errors, env=env, start_new_session=True)
        self.pid = self.process.pid
        self.config, self.work = config, work
        self.lines, self.stdout = queue.Queue(), bytearray()
        self.killed, self.exit_code, self.peak_rss_kb, self.tree_rss_kb = None, None, 0, 0
        self.gone = threading.Event()
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        threading.Thread(target=self.watch, daemon=True).start()

    def read(self):
        for raw in self.process.stdout:
            self.stdout += raw
            self.lines.put(raw.decode('utf-8', 'replace').rstrip('\n'))
        self.lines.put(None)

    def resident_kb(self):
        try:
            status = Path(f'/proc/{self.pid}/status').read_text()
        except OSError:
            return 0
        match = re.search(r'^VmHWM:\s+(\d+) kB', status, re.M)
        return int(match[1]) if match else 0

    def watch(self):
        while not self.gone.wait(0.02):
            seen = self.resident_kb()
            self.peak_rss_kb = max(self.peak_rss_kb, seen)
            if seen > self.config.rss_ceiling_kb:
                self.kill('rss')
                return

    def kill(self, why):
        if self.killed is None and self.exit_code is None:
            self.killed = why
            try:
                os.killpg(self.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    def reply(self, timeout):
        try:
            line = self.lines.get(timeout=timeout)
        except queue.Empty:
            self.kill('timeout')
            return None
        if line is None:
            self.lines.put(None)
        self.peak_rss_kb = max(self.peak_rss_kb, self.resident_kb())
        return line

    def ask(self, command, timeout):
        try:
            self.process.stdin.write(command.encode() + b'\n')
            self.process.stdin.flush()
        except OSError:
            return None
        return self.reply(timeout)

    def finish(self, timeout):
        try:
            self.process.stdin.close()
        except OSError:
            pass
        deadline = time.monotonic() + timeout
        # Wait without reaping, so the pid still names this host when its session is ended.
        while not os.waitid(os.P_PID, self.pid, os.WEXITED | os.WNOWAIT | os.WNOHANG):
            if time.monotonic() > deadline:
                self.kill('timeout')
            time.sleep(0.01)
        self.gone.set()
        try:
            os.killpg(self.pid, signal.SIGKILL)  # a compile worker the host left behind
        except (ProcessLookupError, PermissionError):
            pass
        _, status, usage = os.wait4(self.pid, 0)
        self.exit_code = os.waitstatus_to_exitcode(status)
        self.process.returncode = self.exit_code
        # wait4 counts the reaped compile workers too, which PACK-3 caps on their own.
        self.tree_rss_kb = usage.ru_maxrss // (1024 if sys.platform == 'darwin' else 1)
        if not self.peak_rss_kb:
            self.peak_rss_kb = self.tree_rss_kb  # no /proc here: the tree is all there is
        self.reader.join(timeout)
        self.process.stdout.close()
        self.errors.close()
        return (self.work / 'stderr').read_bytes()


def run_case(config, challenge, engine, sabotage=''):
    """One host process, one challenge. Returns the transcript, the attack's milliseconds
    and the peak resident KiB of the host with its compile workers."""
    secret, decoy = secrets.token_bytes(32), secrets.token_bytes(32)
    run = oracle.Transcript(secret, decoy, rss_ceiling_kb=config.rss_ceiling_kb)
    config.scratch.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix=f'{challenge.name}-{engine}-', dir=config.scratch))
    host = Host(config, engine, challenge.pool, sabotage, work)
    attack_s = config.compile_s + challenge.wall / 1000 + config.slack_s
    elapsed, tree_rss_kb = 0.0, 0

    def probe():
        nonce = secrets.token_bytes(23)
        return nonce, host.ask('probe ' + nonce.hex(), config.slack_s)

    try:
        run.ready = (host.reply(config.startup_s) or '').startswith('ready pid=')
        if run.ready:
            run.sealed = host.ask(f'seal {secret.hex()} {decoy.hex()}', config.slack_s)
        if run.sealed == 'sealed':
            run.before = probe()
        if run.before and run.before[1]:
            began = time.monotonic()
            if challenge.tampers:
                module = work / 'admitted.hirsel-mod'
                emitted = host.ask(f'emit {challenge.path} {module}', attack_s)
                if not (emitted or '').startswith('emitted bytes='):
                    run.steps.append(oracle.Step(emitted, ('emitted',)))
                else:
                    for index, (plan, expected) in enumerate(challenge.tampers):
                        altered = work / f'altered-{index}.hirsel-mod'
                        altered.write_bytes(tamper(module.read_bytes(), plan))
                        run.steps.append(oracle.Step(host.ask(f'raw {altered}', attack_s), expected))
            else:
                command = (f'attack {challenge.arena} {challenge.steps} {challenge.bytes} '
                           f'{challenge.root} {challenge.wall} {challenge.path}')
                run.steps.append(oracle.Step(host.ask(command, attack_s), challenge.expect))
            elapsed = time.monotonic() - began
            run.after = probe()
            run.bye = host.ask('quit', config.slack_s)
    finally:
        run.stderr = host.finish(config.slack_s)
        run.stdout = bytes(host.stdout)
        run.exit_code, run.killed, run.peak_rss_kb = host.exit_code, host.killed, host.peak_rss_kb
        tree_rss_kb = host.tree_rss_kb
        shutil.rmtree(work, ignore_errors=True)
    return run, round(elapsed * 1000), tree_rss_kb


def record(challenge, engine, run, elapsed_ms, tree_rss_kb):
    verdict = oracle.judge(run)
    if verdict.verdict == oracle.ERROR or not run.steps:
        # A host that never took the attack says why on its own stderr.
        verdict.notes.append('host stderr: ' + run.stderr[-400:].decode('utf-8', 'replace').strip())
    return {
        'challenge': challenge.name, 'class': challenge.group, 'engine': engine,
        'verdict': verdict.verdict, 'breaches': verdict.breaches,
        'mechanisms': verdict.mechanisms,
        'details': [oracle.detail(step.reply) for step in run.steps],
        'expected': [list(step.expected) for step in run.steps] or [list(challenge.expect)],
        'notes': verdict.notes, 'elapsed_ms': elapsed_ms, 'peak_rss_kb': run.peak_rss_kb,
        'tree_peak_rss_kb': tree_rss_kb,
        'exit': run.exit_code, 'killed': run.killed, 'source_sha256': challenge.digest,
    }


def receipt(cases, base, config):
    cases = sorted(cases, key=lambda case: (case['challenge'], case['engine']))
    counts = {name: sum(case['verdict'] == name for case in cases)
              for name in (oracle.DEFENDED, oracle.BREACH, oracle.UNEXPECTED, oracle.ERROR)}
    return {'schema': 'hirsel.redteam-ctf.receipt.v1', 'base': base,
            'rss_ceiling_kb': config.rss_ceiling_kb, 'leak_window_bytes': oracle.WINDOW,
            'summary': counts, 'cases': cases}


def scoreboard(report, abouts):
    """The receipt as a page. `abouts` is each challenge's own account of its attack."""
    counts, cases = report['summary'], report['cases']
    lines = [
        '# Red-team containment CTF scoreboard', '',
        'Written by `tools/redteam_ctf/ctf.py --write-scoreboard`. Do not edit by hand.', '',
        f'Base: `{report["base"]}`. ' + ', '.join(f'{count} {name}' for name, count in counts.items())
        + f' of {len(cases)} cases.', '',
        'DEFENDED means what README.md says it means and no more: one owner per pack pool,',
        'no speculative or timing channels, and only the attacks in this corpus.', '',
    ]
    failed = [case for case in cases if case['verdict'] != oracle.DEFENDED]
    if failed:
        lines += ['## Not defended', '']
        for case in failed:
            detail = '; '.join(case['breaches'] + case['notes'] + [d for d in case['details'] if d]) or 'no detail'
            ended = {'rss': f' The harness ended the host at {case["peak_rss_kb"]} KiB resident.',
                     'timeout': ' The harness ended the host when it stopped answering.'}.get(case['killed'], '')
            lines.append(f'- **{case["verdict"]}** `{case["challenge"]}` on {case["engine"]}: {detail}.{ended}')
        lines.append('')
        for name in sorted({case['challenge'] for case in failed}):
            lines += [f'`{name}`: {abouts.get(name, "")}', '',
                      f'    tools/redteam_ctf/ctf.py --build --only {name} --verbose', '']
    lines += ['| challenge | class | engine | verdict | ended by | ms | peak RSS KiB |',
              '|---|---|---|---|---|---:|---:|']
    for case in cases:
        ended = ', '.join(case['mechanisms']) or '-'
        lines.append(f'| `{case["challenge"]}` | {case["class"]} | {case["engine"]} | {case["verdict"]} '
                     f'| `{ended}` | {case["elapsed_ms"]} | {case["peak_rss_kb"]} |')
    return '\n'.join(lines) + '\n'


def self_check(config, modes, control, say=print):
    """Every broken host must be caught, and the unbroken one must not be."""
    failures = []
    run, _, _ = run_case(config, control, ENGINES[0])
    clean = oracle.judge(run)
    say(f'self-check control: {clean.verdict}')
    if clean.verdict != oracle.DEFENDED:
        failures.append(f'the unbroken host is {clean.verdict}: {clean.breaches + clean.notes}')
    for mode, wanted in modes.items():
        run, _, _ = run_case(config, control, ENGINES[0], sabotage=mode)
        got = oracle.judge(run)
        caught = got.verdict == oracle.BREACH and wanted in got.breaches
        if wanted is None:
            caught = got.verdict != oracle.DEFENDED
        say(f'self-check {mode}: {got.verdict} {got.breaches}' + ('' if caught else '  <- NOT CAUGHT'))
        if not caught:
            failures.append(f'{mode}: wanted {wanted}, got {got.verdict} {got.breaches} {got.notes}')
    return failures


def head():
    """The checkout's commit, for a receipt whose caller did not name one."""
    try:
        return subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL).stdout.strip() or 'unknown'
    except OSError:
        return 'unknown'


def built():
    command = [str(ROOT / '.buck2/bin/buck2'), 'build', *TARGETS, '--show-full-output']
    out = subprocess.run(command, cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True).stdout
    paths = dict(line.split(' ', 1) for line in out.splitlines() if ' ' in line)
    return [paths['root' + TARGETS[0]].strip()], paths['root' + TARGETS[1]].strip()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--host', help='the built ctf_host example')
    parser.add_argument('--hirsel', help='the built hirsel binary the host compiles with')
    parser.add_argument('--build', action='store_true', help='build both with Buck2 first')
    parser.add_argument('--engine', choices=(*ENGINES, 'both'), default='both')
    parser.add_argument('--only', action='append', default=[], help='run this challenge only')
    parser.add_argument('--challenges', type=Path, default=HERE / 'challenges', help='the corpus directory')
    parser.add_argument('--scratch', type=Path)
    parser.add_argument('--rss-ceiling-mib', type=int, default=256,
                        help="the host process's own resident ceiling (its compile workers are capped by PACK-3)")
    parser.add_argument('--receipt', type=Path, help='write the JSON receipt here')
    parser.add_argument('--write-scoreboard', action='store_true', help='rewrite SCOREBOARD.md')
    parser.add_argument('--base', default='', help='the commit the host was built from')
    parser.add_argument('--self-check', action='store_true')
    parser.add_argument('--sabotage', choices=sorted(SABOTAGE), help='run one broken host')
    parser.add_argument('--list', action='store_true')
    parser.add_argument('--verbose', action='store_true', help='print what the host said with each reply')
    args = parser.parse_args(argv)

    challenges = corpus(args.challenges)
    if args.list:
        for challenge in challenges:
            print(f'{challenge.name}\t{challenge.group}\t{", ".join(challenge.expect) or "tamper"}')
        return 0
    if args.build:
        host, hirsel = built()
    elif args.host and args.hirsel:
        host, hirsel = [str(Path(args.host).resolve())], str(Path(args.hirsel).resolve())
    else:
        parser.error('give --build, or both --host and --hirsel')
    scratch = args.scratch or Path(tempfile.mkdtemp(prefix='redteam-ctf-'))
    config = Config(host, hirsel, scratch, rss_ceiling_kb=args.rss_ceiling_mib << 10)
    control = load(HERE / 'challenges/control_benign.hirsel')
    try:
        if args.self_check:
            failures = self_check(config, SABOTAGE, control)
            for failure in failures:
                print('SELF-CHECK FAILED ' + failure)
            print('self-check: ' + ('every breach mode is caught' if not failures else 'FAILED'))
            return 1 if failures else 0
        if args.sabotage:
            case = record(control, ENGINES[0], *run_case(config, control, ENGINES[0], sabotage=args.sabotage))
            print(f'{case["verdict"]} {case["breaches"]} {case["notes"]}')
            return 0 if case['verdict'] == oracle.DEFENDED else 1
        wanted = [c for c in challenges if not args.only or c.name in args.only]
        engines = ENGINES if args.engine == 'both' else (args.engine,)
        cases = []
        for challenge in wanted:
            for engine in engines:
                case = record(challenge, engine, *run_case(config, challenge, engine))
                elapsed = case['elapsed_ms']
                cases.append(case)
                print(f'{case["verdict"]:10} {challenge.name:34} {engine:9} '
                      f'{", ".join(case["mechanisms"]):34} {elapsed:6} ms {case["peak_rss_kb"]:8} KiB'
                      + ''.join(f'\n    {line}' for line in case['breaches'] + case['notes']
                                + ([d for d in case['details'] if d] if args.verbose else [])), flush=True)
        report = receipt(cases, args.base or head(), config)
        if args.receipt:
            args.receipt.write_text(json.dumps(report, indent=1, sort_keys=True) + '\n')
        if args.write_scoreboard:
            (HERE / 'SCOREBOARD.md').write_text(scoreboard(report, {c.name: c.about for c in challenges}))
        print(' '.join(f'{name}={count}' for name, count in report['summary'].items()))
        verdicts = {case['verdict'] for case in cases}
        if oracle.ERROR in verdicts or not cases:
            return 2
        return 0 if verdicts == {oracle.DEFENDED} else 1
    finally:
        if not args.scratch:
            shutil.rmtree(scratch, ignore_errors=True)


if __name__ == '__main__':
    sys.exit(main())
