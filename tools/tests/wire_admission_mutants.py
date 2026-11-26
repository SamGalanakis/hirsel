#!/usr/bin/env python3
"""Run selected W3c wire controls in temporary source copies, one build per control."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]
CONTROLS = {
    'refuse_early_call': ('hello_order', [
        ('if match checked { None => true, _ => false } && !(service.ready)(service.context) {', 'if false {'),
        ('Call(stream, p, slot, deadline, take arg) => {\n                if s.phase != 3u8 { return Error(1008u16) }',
         'Call(stream, p, slot, deadline, take arg) => {\n                if s.phase != 3u8 { wire_refused(mut s, stream, p, NoEntry)?; return Ok(wire_take_output(mut s)) }'),
    ]),
    'index_panic': ('slot_out_of_range', [
        ('if index >= list.len(s.entries) { return wire_refused(mut s, stream, p, NoEntry) }',
         'if false { return wire_refused(mut s, stream, p, NoEntry) }'),
    ]),
    'rehello_resets_principal': ('second_hello_closes', [
        ('if s.phase != 0u8 { return Error(1008u16) }', 'if false { return Error(1008u16) }'),
    ]),
}


def library_root(work, runner_text):
    """A library root over the checkout whose runner.hirsel is `runner_text`: core and std
    are links, and store, runner, platform and wire are copied members (a package's
    files must lie inside it)."""
    libraries = work / 'core'
    programs = libraries / 'programs'
    programs.mkdir(parents=True)
    for name in ('core', 'std'):
        (libraries / name).symlink_to(ROOT / 'core' / name)
    members = ('store', 'runner', 'platform', 'wire')
    (programs / 'hirsel.toml').write_text('[workspace]\nmembers = [' + ', '.join(f'"{m}"' for m in members) + ']\n')
    for member in members:
        shutil.copytree(ROOT / 'core/programs' / member, programs / member,
                        ignore=shutil.ignore_patterns('.hirsel-*'))
    (programs / 'runner/runner.hirsel').write_text(runner_text)
    return libraries


def build(args, runner_text, work):
    command = [sys.executable, str(ROOT / 'tools/tests/wire_build.py'),
               '--compiler', str(args.compiler.resolve()), '--runtime', str(args.runtime.resolve()),
               '--libraries', str(library_root(work, runner_text)), '--admission-only',
               '--backend', 'cranelift', '--output', str(work / 'build')]
    if args.cc:
        command += ['--cc', str(args.cc.resolve())]
    return subprocess.run(command, cwd=ROOT, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', required=True, type=Path)
    parser.add_argument('--runtime', required=True, type=Path)
    parser.add_argument('--cc', type=Path)
    parser.add_argument('--mutant', action='append', choices=CONTROLS)
    parser.add_argument('--syntax-control', action='store_true')
    parser.add_argument('--report', required=True, type=Path)
    args = parser.parse_args()
    if not args.mutant and not args.syntax_control:
        parser.error('select controls explicitly')
    if len(args.mutant or ()) > 3 or len(set(args.mutant or ())) != len(args.mutant or ()):
        parser.error('select at most three distinct controls per land')
    text = (ROOT / 'core/programs/runner/runner.hirsel').read_text()
    rows = []
    if args.syntax_control:
        with tempfile.TemporaryDirectory(prefix='wire-w3c-syntax-') as directory:
            work = Path(directory)
            marker = 'fn wire_can_read[G: send, H: send, B: send](s: WireSession[G, H, B]) -> bool {'
            assert text.count(marker) == 1
            result = build(args, text.replace(marker, marker[:-1] + '??? {'), work)
            refused = result.returncode != 0 and '"accepted":false' in result.stdout
            rows.append(dict(control='syntax', passed=refused, result=(result.stdout + result.stderr)[-4000:]))
            print('syntax:', 'REFUSED' if refused else 'NOT REFUSED', flush=True)
    for name in args.mutant or ():
        law, edits = CONTROLS[name]
        changed = text
        for old, new in edits:
            assert changed.count(old) == 1, (name, old)
            changed = changed.replace(old, new)
        with tempfile.TemporaryDirectory(prefix='wire-w3c-' + name + '-') as directory:
            work = Path(directory)
            built = build(args, changed, work)
            if built.returncode:
                rows.append(dict(mutant=name, law=law, passed=False, result='control did not build: ' + (built.stdout + built.stderr)[-4000:]))
                print(name + ': BUILD FAILED', flush=True)
                continue
            report = work / 'law.json'
            result = subprocess.run([sys.executable, str(ROOT / 'tools/tests/wire_admission.py'),
                                     '--server', str(work / 'build/admission-cranelift'), '--case', law,
                                     '--report', str(report)], cwd=ROOT, capture_output=True, text=True)
            cases = json.loads(report.read_text()) if report.is_file() else []
            transcript = result.stdout + result.stderr
            caught = (result.returncode == 1 and len(cases) == 1 and cases[0]['law'] == law
                      and cases[0]['passed'] is False and 'AssertionError' in transcript
                      and 'TimeoutError' not in transcript)
            rows.append(dict(mutant=name, law=law, passed=caught, cases=cases, result=transcript[-5000:]))
            print(name + ': ' + ('CAUGHT' if caught else 'NOT CAUGHT'), flush=True)
    args.report.write_text(json.dumps(rows, indent=2) + '\n')
    return int(not all(row['passed'] for row in rows))


if __name__ == '__main__':
    raise SystemExit(main())
