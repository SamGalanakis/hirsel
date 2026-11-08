#!/usr/bin/env python3
"""Stream test progress and clean up owned descendants on interruption.

Libtest events follow rust-lang/rust library/test/src/formatters/json.rs:36,
121-177 (a7c37f9f). Keep the usual result lines for counts and cost extraction.
"""
import json
import ctypes
import os
from pathlib import Path
import re
import selectors
import signal
import subprocess
import sys
import tempfile
import time

def report_phases(directory):
    """Read coarse spans outside captured application stdout/stderr."""
    now = time.time()
    for path in sorted(Path(directory).glob('*.jsonl')):
        active, totals = {}, {}
        for line in path.read_text(errors='replace').splitlines():
            try:
                row = json.loads(line)
                timestamp, identity = float(row['timestamp']), row['id']
                thread, event, phase, detail = (row[key] for key in ('thread', 'event', 'phase', 'detail'))
            except (ValueError, TypeError, KeyError):
                continue  # A writer can be interrupted during its last append.
            if event == 'start':
                active[identity] = (timestamp, thread, phase, detail)
            elif event == 'end' and identity in active:
                started, thread, phase, detail = active.pop(identity)
                count, seconds = totals.get(phase, (0, 0))
                totals[phase] = count + 1, seconds + max(0, timestamp - started)
        for phase, (count, seconds) in sorted(totals.items()):
            print('HIRSEL_TEST_PHASE_TOTAL ' + json.dumps(dict(
                pid=int(path.stem), phase=phase, completed=count,
                inclusive_seconds=round(seconds, 6))), file=sys.stderr, flush=True)
        for started, thread, phase, detail in active.values():
            print('HIRSEL_TEST_PHASE_RUNNING ' + json.dumps(dict(
                pid=int(path.stem), thread=thread, phase=phase, detail=detail,
                process_alive=Path(f'/proc/{path.stem}').exists(),
                elapsed_seconds=round(max(0, now - started), 6))),
                file=sys.stderr, flush=True)


def reap_owned_children(child):
    # PR_SET_CHILD_SUBREAPER reparents orphaned descendants here (Linux
    # prctl(2)). Direct children cannot reuse their PID until we reap them.
    # Kill one generation, reap it, then handle its newly adopted children.
    # Preserve the inherited PGID: NativeLink samples/enforces that group.
    child.poll()
    children_file = Path(f'/proc/self/task/{os.getpid()}/children')
    while children := children_file.read_text().split():
        for pid in children:
            try:
                os.kill(int(pid), signal.SIGKILL)
            except ProcessLookupError:
                pass
        if child.returncode is None:
            child.wait()
        for pid in children:
            try:
                # Reap only the children just killed, never a newly adopted
                # (and not yet signalled) grandchild that could still be live.
                os.waitpid(int(pid), 0)
            except ChildProcessError:
                pass


class Progress:
    def __init__(self, raw_json=False):
        self.running = {}
        self.finished = {}
        self.raw_json = raw_json
        self.empty_partition = False
        self.suite_finished = False

    def unittest_line(self, line):
        # unittest's verbose runner writes the start before calling the body,
        # without a newline. Observe that partial line as well as its verdict.
        match = re.match(rb'\S+ \(([^)]+)\) \.\.\. (.*)', line, re.S)
        if match:
            name, result = (part.decode(errors='replace').strip() for part in match.groups())
            if result in ('ok', 'FAIL', 'ERROR') or result.startswith('skipped '):
                self.running.pop(name, None)
                self.finished[name] = {'ok': 'ok', 'FAIL': 'failed', 'ERROR': 'failed'}.get(result, 'ignored')
            elif name not in self.finished:
                self.running.setdefault(name, time.monotonic())

    def line(self, line):
        if line.startswith(b'HIRSEL_TEST_EMPTY_PARTITION '):
            self.empty_partition = True
            return
        try:
            event = json.loads(line)
        except (ValueError, UnicodeError):
            event = None
        if not isinstance(event, dict) or event.get('type') not in ('test', 'suite'):
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()
            return
        kind, status = event['type'], event.get('event')
        if kind == 'suite':
            self.suite_finished = status in ('ok', 'failed')
        if kind == 'test':
            name = event['name']
            if status == 'started':
                self.running[name] = time.monotonic()
            elif status in ('ok', 'failed', 'ignored'):
                self.running.pop(name, None)
                self.finished[name] = status
        if self.raw_json:
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()
        elif kind == 'test':
            if status == 'started':
                print(f'HIRSEL_TEST_STARTED {name}', flush=True)
            elif status in ('ok', 'failed', 'ignored'):
                timing = f" <{event['exec_time']:.6f}s>" if 'exec_time' in event else ''
                print(f"test {name} ... {'FAILED' if status == 'failed' else status}{timing}", flush=True)
                if event.get('stdout'):
                    print(event['stdout'], end='', flush=True)
                if event.get('message'):
                    print(event['message'], flush=True)
        elif status == 'started':
            print(f"running {event['test_count']} tests", flush=True)
        elif status in ('ok', 'failed'):
            print(f"test result: {'ok' if status == 'ok' else 'FAILED'}. "
                  f"{event['passed']} passed; {event['failed']} failed; "
                  f"{event['ignored']} ignored; {event.get('measured', 0)} measured; "
                  f"{event.get('filtered_out', 0)} filtered out; "
                  f"finished in {event.get('exec_time', 0):.2f}s", flush=True)

    def interrupted(self, reason):
        print(f'HIRSEL_TEST_INTERRUPTED {reason}; completed={len(self.finished)} '
              f'running={len(self.running)}',
              file=sys.stderr, flush=True)
        for name, status in sorted(self.finished.items()):
            print(f'HIRSEL_TEST_FINISHED {status} {name}', file=sys.stderr, flush=True)
        for name, started in sorted(self.running.items()):
            print(f'HIRSEL_TEST_RUNNING {name} elapsed={time.monotonic() - started:.3f}s',
                  file=sys.stderr, flush=True)
        if not self.running:
            print('HIRSEL_TEST_RUNNING <launcher setup, discovery, or child teardown>',
                  file=sys.stderr, flush=True)


def supervise(command, *, raw_json=False):
    progress = Progress(raw_json)
    stopped = None

    def stop(signum, _frame):
        nonlocal stopped
        stopped = signum

    previous = {sig: signal.signal(sig, stop) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP)}
    child = None
    # Adopt orphaned descendants so cleanup needs only our direct children,
    # without a new session/group escaping the executor's resource enforcement.
    libc = ctypes.CDLL(None, use_errno=True)
    previous_subreaper = ctypes.c_int()
    if libc.prctl(37, ctypes.byref(previous_subreaper), 0, 0, 0) != 0 or libc.prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), 'cannot establish test descendant ownership')
    phases = tempfile.TemporaryDirectory(prefix='hirsel-test-phases-')
    try:
        env = os.environ | {'HIRSEL_TEST_PHASE_DIR': phases.name}
        child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            selector.register(child.stderr, selectors.EVENT_READ)
            pending = {child.stdout: b'', child.stderr: b''}
            while selector.get_map() or child.poll() is None:
                if stopped:
                    progress.interrupted(f'signal={stopped}')
                    if child.poll() is not None or progress.suite_finished:
                        print('HIRSEL_TEST_PHASE_RUNNING ' + json.dumps(dict(
                            phase='child-teardown', status=child.returncode)),
                            file=sys.stderr, flush=True)
                    elif (not progress.running and not progress.finished
                          and not any(Path(phases.name).iterdir())):
                        print('HIRSEL_TEST_PHASE_RUNNING ' + json.dumps(dict(
                            phase='launcher-setup-or-discovery')),
                            file=sys.stderr, flush=True)
                    report_phases(phases.name)
                    return 128 + stopped
                for key, _ in selector.select(0.1):
                    stream = key.fileobj
                    chunk = os.read(stream.fileno(), 65536)
                    if not chunk:
                        selector.unregister(stream)
                        if pending[stream] and stream is child.stdout:
                            progress.line(pending[stream])
                        pending[stream] = b''
                        continue
                    if stream is child.stderr:
                        sys.stderr.buffer.write(chunk)
                        sys.stderr.buffer.flush()
                    lines = (pending[stream] + chunk).split(b'\n')
                    pending[stream] = lines.pop()
                    for line in lines:
                        if stream is child.stdout:
                            progress.line(line + b'\n')
                        else:
                            progress.unittest_line(line)
                    if stream is child.stderr:
                        progress.unittest_line(pending[stream])
            status = child.wait()
            report_phases(phases.name)
            binary = os.environ.get('HIRSEL_TEST_BINARY')
            discovery = any(arg in ('--list', '--help', '-h') for arg in command)
            if (status == 0 and binary and not discovery and not progress.empty_partition
                    and not any(result in ('ok', 'failed') for result in progress.finished.values())):
                target = os.environ.get('TEST_TARGET', binary)
                print(f'test_launcher.sh: {target} executed zero test bodies', file=sys.stderr, flush=True)
                return 2
            return status if status >= 0 else 128 - status
    finally:
        if child is not None:
            reap_owned_children(child)
            child.stdout.close()
            child.stderr.close()
        phases.cleanup()
        libc.prctl(36, previous_subreaper.value, 0, 0, 0)
        for sig, handler in previous.items():
            signal.signal(sig, handler)


if __name__ == '__main__':
    os.environ['HIRSEL_TEST_REPORT_ACTIVE'] = '1'
    args = sys.argv[1:]
    raw = '--format=json' in args or '--list' in args
    raw = raw or any(pair == ['--format', 'json'] for pair in (args[i:i+2] for i in range(len(args))))
    sys.exit(supervise(args, raw_json=raw))
