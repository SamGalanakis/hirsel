#!/usr/bin/env python3
"""Build and run one Hirsel entry on the evaluator or a native engine.

Shared by the std contract walker (std_native.py) and the containment fuzz driver
(containment_fuzz.py): the resident evaluator, the source manifest and the native build.
"""
import contextlib
import json
import os
import subprocess
import tempfile
import threading
import time

from case_progress import Case


class Evaluator:
    """One checked-product store per gate, one fresh evaluator world per request."""
    def __init__(self, binary):
        self.binary = binary
        self.errors = tempfile.TemporaryFile()
        self.active_case = None
        self.process = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE)
        self.stderr_reader = threading.Thread(target=self.read_errors)
        self.stderr_reader.start()

    def read_errors(self):
        with self.process.stderr as errors:
            while chunk := errors.read1(8192):
                self.errors.write(chunk)
                if progress := self.active_case:
                    progress.output(errors, chunk)

    def __enter__(self):
        return self

    def evaluate(self, manifest, entry, program_entry=False, instructions=None, sdk_out=None, sdk_describe=None, events=False, progress=None, meter=False, meter_limit=None, root_limit=None):
        request = dict(manifest=str(manifest), entry=entry, program_entry=program_entry)
        if events:
            # CONC-10 events are recorded only on request (22f95498).
            request["events"] = True
        if instructions is not None:
            request["instructions"] = instructions
        if meter:
            request.update(meter=True, meter_limit=meter_limit, root_limit=root_limit)
        elif root_limit is not None:
            request["root_limit"] = root_limit
        if sdk_out is not None:
            request.update(sdk_out=str(sdk_out), sdk_describe=str(sdk_describe))
        with (contextlib.nullcontext(progress) if progress else Case(entry, 'evaluator')) as active:
            self.active_case = active
            line = bytearray()
            try:
                with contextlib.suppress(BrokenPipeError):
                    self.process.stdin.write(json.dumps(request).encode() + b'\n')
                    self.process.stdin.flush()
                    while chunk := self.process.stdout.peek(8192):
                        line_end = chunk.find(b'\n') + 1
                        read = self.process.stdout.read(line_end or len(chunk))
                        active.output(self.process.stdout, read)
                        line.extend(read)
                        if line_end:
                            break
                    active.end_stream(self.process.stdout)
                if line:
                    return subprocess.CompletedProcess([str(self.binary)], 0, bytes(line), b'')
                status = self.process.wait()
                self.stderr_reader.join()
                self.errors.seek(0)
                return subprocess.CompletedProcess([str(self.binary)], status or 1, b'', self.errors.read())
            finally:
                self.active_case = None

    def __exit__(self, *exc):
        with contextlib.suppress(BrokenPipeError):
            self.process.stdin.close()
        self.process.stdout.close()
        self.process.wait()
        self.stderr_reader.join()
        self.errors.close()


def library_sources(directory, signatures):
    return [
        {'file': str(p.resolve()), 'modules': list(relative.with_suffix('').parts), 'signatures': signatures}
        for p in sorted(directory.rglob('*.hirsel'))
        if (relative := p.relative_to(directory)).parts[0] != 'tests'
    ]


def source_manifest(root, package, modules, source, *, replace=False):
    core = {'name': 'core', 'version': '1'}
    std = {'name': 'std', 'version': '1'}
    packages = [
        {'package': core, 'sources': library_sources(root / 'core/core', True), 'dependencies': []},
        {'package': std, 'sources': library_sources(root / 'core/std', False), 'dependencies': [core]}]
    if package == 'runner':
        from store_tests import inputs
        packages = inputs(root, package='runner')['packages']
    files = next(p['sources'] for p in packages if p['package']['name'] == package)
    if replace:
        original = next(f for f in files if f['modules'] == modules)
        original.update(file=str(source), signatures=package == 'core')
    else:
        files.append({'file': str(source), 'modules': modules, 'signatures': False})
    return {'core': core, 'packages': packages}


def first_stop(product):
    reached = (product.get('reachable') or {}).get('first_stops', [])
    stops = reached or product.get('diagnostics', []) or product.get('incomplete', [])
    if stops:
        stop = stops[0]
        site = stop['primary']
        span = site['span']
        return f"{stop.get('rule', 'GAP')} {site['module']}:{span['start']}..{span['end']}: {stop.get('construct', stop.get('message'))}"
    return product.get('stopped') or 'native product unavailable without a diagnostic'


def wait_load():
    printed = False
    while os.getloadavg()[0] > 32:
        if not printed:
            print('waiting: host load exceeds 32; no build started', flush=True)
            printed = True
        time.sleep(10)


def native_build(compiler, runtime, inputs, entry, backend, store, output, *, limit, release=False,
                 grants=(), cc=None, cwd=None, progress=None):
    """Build `entry` of a source manifest natively, reading the manifest's core sources."""
    command = [str(compiler), 'build', '--source-core', '--inputs', str(inputs), '--entry', entry,
               '--backend', backend, '--runtime', str(runtime), '--store', str(store),
               '--json', '--limit', str(limit), '-o', str(output)]
    if release:
        command.append('--release')
    for grant in grants:
        command.extend(['--grant', grant])
    if cc:
        command.extend(['--cc', str(cc)])
    run = progress.run if progress else subprocess.run
    return run(command, capture_output=True, cwd=cwd)
