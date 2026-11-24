#!/usr/bin/env python3
"""Run the opt-in Lean extraction, proof, sensitivity and differential checks."""

import argparse
import copy
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(ROOT / 'core/tools'))
import store_tests  # noqa: E402

TARGETS = {
    'Triggers': (
        'runner@1/triggers', 'core/programs/runner/triggers.hirsel',
        ('stored', 'copy_bytes', 'allow', 'listed', 'allocate_id', 'write_at', 'write',
         'revision_mismatch', 'read', 'parked', 'running', 'current_attempt', 'current',
         'fence', 'start', 'hold', 'release', 'request_cancel', 'consume', 'finish',
         'finish_cancel', 'finish_transact'),
        'int.checked_add(row.attempt, 1u64)', 'int.checked_add(row.attempt, 2u64)',
        'TriggersProofs',
    ),
    'OMap': (
        'std@1/omap', 'core/std/omap.hirsel',
        ('new', 'snapshot', 'len', 'root', 'from_root', 'node_id', 'node_level',
         'node_count', 'node_len'),
        'OMap(head: root(m))', 'OMap(head: None)', 'OMapProofs',
    ),
    'Register': (
        'store@1/store', 'core/programs/store/store-register.hirsel',
        ('history_claim_same',),
        'list.slice(candidate, 112, list.len(candidate)) {\n        return false',
        'list.slice(candidate, 112, list.len(candidate)) {\n        return true',
        'RegisterProofs',
    ),
}


def run(*args, directory, timeout=300, stdin=None, check=True):
    result = subprocess.run(args, cwd=directory, env=dict(os.environ, TMPDIR=str(directory)),
                            input=stdin, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(result.stdout or f'{args[0]} exited {result.returncode}')
    return result


def manifest(path, inputs):
    path.write_text(json.dumps(inputs))
    return path


def add_source(inputs, package, module, file):
    row = next(p for p in inputs['packages'] if p['package']['name'] == package)
    row['sources'].append(dict(file=str(file), modules=[module], signatures=False))


def extract(hirsel, inputs, target, output, store, directory, action='extract'):
    module, _, functions, *_ = TARGETS[target]
    return run(hirsel, 'lean', action, module, '--inputs', inputs, '--store', store,
               '-o', output, '--', *functions, directory=directory).stdout


def check_axioms(output):
    forbidden = re.compile(r'\b(sorry|admit|native_decide|partial|unsafe|implemented_by|extern)\b')
    proofs = [p for p in HERE.glob('*.lean') if p.stem.endswith('Proofs')]
    for source in proofs:
        code = re.sub(r'(?s)/-.*?-/', '', source.read_text())
        code = re.sub(r'--[^\n]*', '', code)
        if forbidden.search(code):
            raise RuntimeError(f'{source.name} uses a forbidden proof or execution primitive')
    for line in output.splitlines():
        if "declaration uses 'sorry'" in line:
            raise RuntimeError(line)
        match = re.search(r"\bdepends on axioms: \[([^\]]*)\]", line)
        if match and set(match[1].split(', ')) - {'propext', 'Classical.choice', 'Quot.sound'}:
            raise RuntimeError(line)
        if 'depends on axioms' in line or 'does not depend on any axioms' in line:
            print(line)
    for theorem in ('terminal_never_changes', 'start_issues_next', 'snapshot_history',
                    'claim_rejects_different_length'):
        if not re.search(rf"'[\w.]*\b{theorem}' (depends on axioms|does not depend on any axioms)",
                         output):
            raise RuntimeError(f'no axiom audit for {theorem}')


def verify_extractions(hirsel, inputs, work, project):
    plain = manifest(work / 'plain.json', inputs)
    for target in TARGETS:
        generated = project / 'Generated' / f'{target}.lean'
        second = work / f'{target}.lean'
        log = extract(hirsel, plain, target, generated, work / 'store-a', work)
        repeated = extract(hirsel, plain, target, second, work / 'store-b', work)
        if generated.read_bytes() != second.read_bytes() or log != repeated:
            raise RuntimeError(f'{target} extraction changed across fresh stores')
        total = next(line for line in log.splitlines() if line.startswith('TOTAL '))
        print(f'{target} {total}', flush=True)

    probe = copy.deepcopy(inputs)
    add_source(probe, 'runner', 'triggers', HERE / 'diff/probe.hirsel')
    probe_path = manifest(work / 'probe.json', probe)
    for name, reason in (
        ('probe_cancel', 'runtime operation CheckCancel'),
        ('probe_spawn', 'runtime operation Spawn'),
        ('next_pending', 'indirect call'),
        ('first', 'drops store@1/store/Reader (Resource'),
    ):
        outcome = run(hirsel, 'lean', 'probe', f'runner@1/triggers/{name}', '--inputs',
                      probe_path, '--store', work / 'store-a', directory=work).stdout
        if reason not in outcome:
            raise RuntimeError(f'exclusion did not fire ({reason}): {outcome}')
    print('KC1 PASS', flush=True)


def sensitivity(hirsel, inputs, work, project, lake):
    for target, (_, source, _, before, after, theorem_file) in TARGETS.items():
        original = ROOT / source
        contents = original.read_text()
        if contents.count(before) != 1:
            raise RuntimeError(f'{source}: expected one location for the sensitivity edit')
        scratch = work / f'{target}.hirsel'
        scratch.write_text(contents.replace(before, after))
        seeded = copy.deepcopy(inputs)
        matches = [s for p in seeded['packages'] for s in p['sources']
                   if Path(s['file']) == original]
        if len(matches) != 1:
            raise RuntimeError(f'{source}: expected one entry in the manifest')
        matches[0]['file'] = str(scratch)
        seeded_path = manifest(work / f'{target}.json', seeded)
        generated = project / 'Generated' / f'{target}.lean'
        faithful = generated.read_bytes()
        try:
            extract(hirsel, seeded_path, target, generated, work / 'store-a', work)
            status = extract(hirsel, manifest(work / 'plain.json', inputs), target, generated,
                             work / 'store-b', work, action='status')
            if not status.endswith('stale\n'):
                raise RuntimeError(f'{target} did not report advisory stale status')
            red = run(lake, 'build', theorem_file, directory=project, timeout=600, check=False)
            if red.returncode != 1 or not re.search(rf'^error: .*{theorem_file}\.lean:', red.stdout, re.M):
                raise RuntimeError(f'{target}: expected Lean proof rejection\n{red.stdout}')
            reason = next(line for line in red.stdout.splitlines()
                          if line.startswith('error: ') and f'{theorem_file}.lean:' in line)
            print(f'{target} scratch break rejected. {reason}', flush=True)
        finally:
            generated.write_bytes(faithful)
    print('KC3 PASS', flush=True)


def differential(evaluator, inputs, work, project):
    traces = copy.deepcopy(inputs)
    for package, module, file in (
        ('runner', 'triggers', 'diff_traces.hirsel'),
        ('store', 'store', 'register.hirsel'),
        ('runner', 'lean_diff', 'omap.hirsel'),
    ):
        add_source(traces, package, module, HERE / 'diff' / file)
    path = manifest(work / 'diff.json', traces)
    entries = ('runner@1/triggers/diff_main', 'runner@1/lean_diff/lean_diff_omap',
               'store@1/store/lean_diff_register')
    requests = ''.join(json.dumps(dict(manifest=str(path), entry=entry, instructions=20_000_000_000))
                       + '\n' for entry in entries)
    output = run(evaluator, directory=work, stdin=requests).stdout
    observations = [json.loads(line) for line in output.splitlines()]
    if len(observations) != len(entries):
        raise RuntimeError('the evaluator did not return three observations')
    errors = []
    for entry, observation in zip(entries, observations):
        if observation.get('outcome') != dict(kind='returned', values=[1]):
            errors.append(f'{entry}: {observation.get("outcome", observation)}')
    bins = project / '.lake/build/bin'
    if 'stdout' in observations[0]:
        trigger_text = bytes(observations[0]['stdout']).decode()
        if len(re.findall(r'^trace ', trigger_text, re.M)) != 240:
            errors.append('expected 240 trigger traces')
        else:
            reference = work / 'triggers.txt'
            reference.write_text(trigger_text)
            result = run(bins / 'diffrunner', reference, directory=work, check=False)
            if result.returncode:
                errors.append(result.stdout.strip())
            else:
                print(result.stdout.strip(), flush=True)
    for which, observation, size in zip(('omap', 'register'), observations[1:], (65, 122)):
        if 'stdout' not in observation:
            continue
        report = bytes(observation['stdout']).decode()
        generated = run(bins / 'diffprojections', which, directory=work).stdout
        if len(report.splitlines()) != size or report != generated:
            errors.append(f'{which}: generated observations disagree with the evaluator')
        else:
            print(f'{which} {size} observations agree', flush=True)
    for error in errors:
        print(error, flush=True)
    print('KC4 FAIL' if errors else 'KC4 PASS', flush=True)
    return not errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hirsel', type=lambda x: Path(x).resolve(), required=True)
    parser.add_argument('--evaluator', type=lambda x: Path(x).resolve(), required=True)
    parser.add_argument('--scratch', type=Path, default=Path.home() / '.cache/hirsel/lean')
    args = parser.parse_args()
    lake = shutil.which('lake')
    if not lake or not shutil.which('lean'):
        print('Lean proofs skipped. Install Lean and Lake for the version in tools/lean/lean-toolchain.')
        return
    scratch = args.scratch.resolve()
    if scratch.is_relative_to(ROOT):
        parser.error('the Lake cache must be outside the repository')
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        work = Path(directory)
        project = work / 'proofs'
        project.mkdir()
        (project / 'Generated').mkdir()
        for source in [*HERE.glob('*.lean'), HERE / 'lean-toolchain', HERE / 'lakefile.toml']:
            shutil.copyfile(source, project / source.name)
        inputs = store_tests.inputs(ROOT, package='runner')
        verify_extractions(args.hirsel, inputs, work, project)
        proofs = run(lake, 'build', directory=project, timeout=600)
        check_axioms(proofs.stdout)
        print('KC2 PASS', flush=True)
        sensitivity(args.hirsel, inputs, work, project, lake)
        if not differential(args.evaluator, inputs, work, project):
            raise SystemExit(1)


if __name__ == '__main__':
    main()
