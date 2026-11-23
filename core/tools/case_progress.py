"""Report inactive test cases without interrupting them."""
import collections
import contextlib
import io
import os
import subprocess
import sys
import threading
import time


STALL_SECONDS = 5 * 60


class Case:
    def __init__(self, name, engine):
        self.label = f'case={name} engine={engine}'
        self.lock = threading.Lock()
        self.done = threading.Event()
        self.lines = collections.deque(maxlen=20)
        self.partial = {}

    def __enter__(self):
        self.started = self.last_output = time.monotonic()
        print(f'START {self.label}', file=sys.stderr, flush=True)
        self.watcher = threading.Thread(target=self.watch, daemon=True)
        self.watcher.start()
        return self

    def __exit__(self, *exc):
        self.done.set()
        self.watcher.join()

    def output(self, stream, chunk):
        with self.lock:
            self.last_output = time.monotonic()
            prior = self.partial.pop(stream, bytearray())
            lines = chunk.split(b'\n')
            if len(lines) == 1:
                prior.extend(chunk)
                self.partial[stream] = prior
            else:
                prior.extend(lines[0])
                self.lines.append(prior)
                self.lines.extend(lines[1:-1])
                if lines[-1]:
                    self.partial[stream] = bytearray(lines[-1])

    def end_stream(self, stream):
        with self.lock:
            if stream in self.partial:
                self.lines.append(self.partial.pop(stream))

    def collect(self, reader, write):
        with reader:
            while chunk := reader.read1(8192):
                self.output(reader, chunk)
                write(chunk)
        self.end_stream(reader)

    def watch(self):
        last_report = self.started
        wait = STALL_SECONDS
        while not self.done.wait(wait):
            with self.lock:
                now = time.monotonic()
                wait = max(self.last_output, last_report) + STALL_SECONDS - now
                if wait > 0:
                    continue
                last_report = now
                wait = STALL_SECONDS
                tail = [line.decode(errors='replace').rstrip('\r')
                        for line in (*self.lines, *self.partial.values())[-20:]]
            print(f'STALL {self.label} elapsed={now - self.started:.1f}s\n'
                  + ('\n'.join(tail) if tail else '(no case output yet)'),
                  file=sys.stderr, flush=True)

    def run(self, command, *, capture_output=False, stdout=None, stderr=None,
            text=False, check=False, on_start=None, **options):
        if capture_output:
            stdout = stderr = subprocess.PIPE
        captured = {}

        def relay(reader, target):
            with os.fdopen(os.dup(target), 'wb') as sink:
                def write(chunk):
                    sink.write(chunk)
                    sink.flush()
                self.collect(reader, write)

        with contextlib.ExitStack() as readers, contextlib.ExitStack() as writers:
            for name, target, inherited in (('stdout', stdout, 1), ('stderr', stderr, 2)):
                if target not in (None, subprocess.PIPE):
                    options[name] = target
                    continue
                read_fd, write_fd = os.pipe()
                reader = os.fdopen(read_fd, 'rb')
                options[name] = writers.enter_context(os.fdopen(write_fd, 'wb'))
                if target is None:
                    worker = threading.Thread(target=relay, args=(reader, inherited))
                else:
                    captured[name] = bytearray()
                    worker = threading.Thread(target=self.collect, args=(reader, captured[name].extend))
                worker.start()
                readers.callback(worker.join)
            if on_start is None:
                result = subprocess.run(command, text=text, **options)
            else:
                with subprocess.Popen(command, text=text, **options) as process:
                    on_start(process.pid)
                    result = subprocess.CompletedProcess(command, process.wait())
        for name, data in captured.items():
            setattr(result, name, io.TextIOWrapper(io.BytesIO(data)).read() if text else bytes(data))
        if check:
            result.check_returncode()
        return result


def run_case(name, engine, command, **options):
    with Case(name, engine) as case:
        return case.run(command, **options)
