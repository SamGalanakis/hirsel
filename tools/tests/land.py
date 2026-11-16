#!/usr/bin/env python3
"""tools/land against a scratch origin with fake Buck2 checks: a push rejected
because main moved goes back to the rebase, and a red check refuses the push."""
import argparse
import ast
import contextlib
import csv
import fcntl
import io
import json
import os
import runpy
import signal
from types import SimpleNamespace
from unittest.mock import patch
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
# The harness removes the randomized delay only; production always uses jitter.
DRIVER = '''import json, os, pathlib, runpy, subprocess, sys
from unittest.mock import patch
module = runpy.run_path('tools/land', run_name='land_test')
module['LandQueue'].HEARTBEAT = .05
module['main'].__globals__['BATCH'] = int(os.environ.get('FAKE_BATCH', module['BATCH']))
real_git = module['git']
pushes = iter(json.loads(os.environ.get('FAKE_PUSH_ERRORS', '[]')))
fetch_error = None
def fake_git(*args):
    global fetch_error
    if args[0] == 'fetch' and fetch_error:
        return subprocess.CompletedProcess(args, 128, '', fetch_error)
    if args[0] == 'push':
        if os.environ.get('FAKE_PUSH_LOG'):
            with pathlib.Path(os.environ['FAKE_PUSH_LOG']).open('a') as log:
                log.write(args[-1] + '\\n')
        error = next(pushes, None)
        if error:
            fetch_error = error.get('fetch_error')
            if error.pop('accept', False):
                accepted = real_git(*args)
                if accepted.returncode:
                    return accepted
                if error.get('advance'):
                    tip = args[-1].split(':')[0]
                    tree = real_git('rev-parse', tip + '^{tree}').stdout.strip()
                    remote = ['git', '--git-dir', os.environ['FAKE_ORIGIN']]
                    child = subprocess.check_output([*remote, 'commit-tree', tree, '-p', tip, '-m', 'other'], text=True).strip()
                    subprocess.run([*remote, 'update-ref', 'refs/heads/main', child], check=True)
            return subprocess.CompletedProcess(args, 128, '', error['stderr'])
    return real_git(*args)
module['main'].__globals__['git'] = fake_git
with patch('random.uniform', return_value=0):
    sys.exit(module['main']())
'''
RECORD = '''import os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['FAKE_CHECKS'])
head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
base = subprocess.check_output(['git', 'rev-parse', 'origin/main'], text=True).strip()
folder = root / head
folder.mkdir(parents=True, exist_ok=True)
(folder / STAGE).write_text(base)
if os.environ.get('FAKE_RED_AFTER_RACE') and STAGE == os.environ['FAKE_RED_AFTER_RACE'] and (root / 'race-0').exists():
    sys.exit(1)
'''

FAKE_BUCK = '''#!/bin/sh
if [ "$1" = test ]; then
  case "$*" in
    *hirsel-core-ir-unittest*)
      if [ -n "$FAKE_TARGET_LOG" ]; then git rev-parse HEAD >> "$FAKE_TARGET_LOG"; fi
      if [ -f main-red ]; then
        echo '✗ Fail: root//:hirsel-core-ir-unittest'
        echo 'error: main fixture is red'
        exit 1
      fi
      if [ -f broken-core-suite ]; then echo 'core evaluator body failed' >&2; exit 1; fi ;;
  esac
  case "$*" in
    *core-front-cost*)
      if [ -n "$FAKE_COST_CRASH" ]; then echo 'cost tool crashed' >&2; exit 1; fi
      echo 'COST_REPORT {"target":"core-front-cost","findings":["parse: 201 exceeds 100"],"ceilings":{"m1":{"instructions_per_token":{"parse":205,"index":50,"resolve":30},"image_per_token":{"bytes_read":1.02}}}}'
      case "$*" in
        *--report-only*) ;;
        *) [ -z "$FAKE_COST_RED" ] || exit 1 ;;
      esac ;;
  esac
  case "$*" in
    *new-required-gate*)
      echo '✗ Fail: root//:new-required-gate'
      echo 'error: the newly required gate was executed'
      exit 1 ;;
  esac
  case "$*" in
    *land-core-evaluator*)
      if [ -n "$FAKE_LAW_GATE" ] && [ -d core/std/tests ]; then
        exec python3 core/tools/gate_evaluator.py --root . --evaluator "$FAKE_CORE_EVALUATOR"
      fi ;;
  esac
  case "$*" in
    *hirsel-cranelift-test-core_aarch64*)
      if [ -f broken-arm-core ]; then echo 'ARM CoreIR body failed' >&2; exit 1; fi ;;
  esac
  case "$*" in
    *land-core-cranelift-custody*|*hirsel-cranelift-test-core_native*)
      if [ -f broken-custody ]; then echo 'custody native agreement failed' >&2; exit 1; fi ;;
    *land-audit-workspace-layering*)
      if [ -f broken-layering ]; then echo 'crate layering: a lower crate depends on a higher one' >&2; exit 1; fi ;;
    *) [ -z "$FAKE_SLOW_TEST" ] || sleep "$FAKE_SLOW_TEST" ;;
  esac
fi
if [ "$1" = build ] && [ -n "$FAKE_BUILD_LOG" ]; then echo build >> "$FAKE_BUILD_LOG"; fi
echo root//:a
'''
FAKES = {
    '.buck2/bin/buck2': FAKE_BUCK,
    'tools/buck2/prepare.py': "from pathlib import Path\np = Path('.buck2/bin/buck2')\np.parent.mkdir(parents=True, exist_ok=True)\np.write_text(" + repr(FAKE_BUCK) + ")\np.chmod(0o755)\n",
    'tools/buck2/sync.py': '',
    'tools/check-hot-files': '',
    'tools/tests/ignored_tests.py': '',
    'tools/buck2/event_logs.py': "PASS, SKIP, OMITTED = 'PASS', 'SKIP', 'OMITTED'\nif __name__ == '__main__':\n import subprocess, sys\n if sys.argv[1] == 'test':\n  sys.exit(subprocess.call(['.buck2/bin/buck2', *sys.argv[1:]]))\n",
    # The first Clippy run lands another lane's commit on origin, as a lane landing meanwhile would.
    'tools/buck2/clippy.py': '''import os, subprocess, sys
if os.environ.get('FAKE_CLIPPY_RED') and os.path.exists('change.txt'):
    print('error[E0063]: missing field')
    sys.exit(1)
other = os.environ.get('FAKE_OTHER_LANE')
if other and not os.path.exists(other):
    subprocess.run(['git', 'clone', '-q', os.environ['FAKE_ORIGIN'], other], check=True)
    open(os.path.join(other, 'other.txt'), 'w').write('other\\n')
    subprocess.run(['git', 'add', 'other.txt'], cwd=other, check=True)
    subprocess.run(['git', 'commit', '-qm', 'other'], cwd=other, check=True)
    subprocess.run(['git', 'push', '-q', 'origin', 'HEAD:main'], cwd=other, check=True)
''',
    'tools/buck2/fmt.py': '''import os, sys
if os.environ.get('FAKE_FMT_RED') and os.path.exists('change.txt'):
    print('checking 3 files')
    print('Diff in crates/a/src/lib.rs at line 1:')
    sys.exit(1)
''',
}


class Land(unittest.TestCase):
    def setUp(self):
        self.top = Path(tempfile.mkdtemp(prefix='land-test-'))
        self.addCleanup(shutil.rmtree, self.top)
        self.origin, self.lane = self.top / 'origin.git', self.top / 'lane'
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(('GIT_', 'HIRSEL_'))} | dict(
            GIT_AUTHOR_NAME='t', GIT_AUTHOR_EMAIL='t@t', GIT_COMMITTER_NAME='t', GIT_COMMITTER_EMAIL='t@t',
            FAKE_ORIGIN=str(self.origin))
        self.git(self.top, 'init', '-q', '--bare', '-b', 'main', str(self.origin))
        self.git(self.top, 'clone', '-q', str(self.origin), str(self.lane))
        files = dict(FAKES, **{'tools/land': Path(os.environ.get('HIRSEL_TEST_LAND', ROOT / 'tools/land')).read_text(),
                               'tools/land-run': (ROOT / 'tools/land-run').read_text(),
                               'tools/check-conflict-markers': (ROOT / 'tools/check-conflict-markers').read_text(),
                               'tools/land_events.py': (ROOT / 'tools/land_events.py').read_text(),
                               'tools/buck2/generated.py': (ROOT / 'tools/buck2/generated.py').read_text(),
                               'tools/buck2/merging.py': (ROOT / 'tools/buck2/merging.py').read_text(),
                               'tools/buck2/run_failures.py': (ROOT / 'tools/buck2/run_failures.py').read_text(),
                               'tools/buck2/rust_environment.py': (ROOT / 'tools/buck2/rust_environment.py').read_text(),
                               'tools/buck2/priorities.bzl': (ROOT / 'tools/buck2/priorities.bzl').read_text(),
                               'core/tools/std_api_index.py': (ROOT / 'core/tools/std_api_index.py').read_text(),
                               'core/tools/cost_ceilings.py': (ROOT / 'core/tools/cost_ceilings.py').read_text(),
                               'core/tools/gate_evaluator.py': (ROOT / 'core/tools/gate_evaluator.py').read_text(),
                               'core/tools/std_native.py': (ROOT / 'core/tools/std_native.py').read_text(),
                               'core/tools/std_native_owners.json': (ROOT / 'core/tools/std_native_owners.json').read_text(),
                               'core/std/example.hirsel': 'pub fn present() -> void {}\n',
                               'core/std/API.md': '# API overview\n\n[Operation index](API-INDEX.md). Contracts: api/<module>.md.\n',
                               'core/std/api/example.md': '# Example\n\nThe example contract.\n',
                               '.gitignore': '.buck2/\n__pycache__/\n'})
        for name, text in files.items():
            (self.lane / name).parent.mkdir(parents=True, exist_ok=True)
            (self.lane / name).write_text(text)
        (self.lane / '.buck2/bin/buck2').chmod(0o755)
        subprocess.run([sys.executable, 'core/tools/std_api_index.py', '--write'],
                       cwd=self.lane, check=True)
        self.git(self.lane, 'checkout', '-q', '-b', 'main')
        self.git(self.lane, 'add', '-A')
        self.git(self.lane, 'commit', '-qm', 'base')
        self.git(self.lane, 'push', '-q', 'origin', 'main')
        self.git(self.lane, 'checkout', '-q', '-b', 'my-lane')
        (self.lane / 'change.txt').write_text('change\n')
        self.git(self.lane, 'add', 'change.txt')
        self.git(self.lane, 'commit', '-qm', 'change')

    def git(self, cwd, *arguments):
        return subprocess.run(['git', *arguments], cwd=cwd, env=self.env, check=True, capture_output=True,
                              text=True).stdout

    def land(self, **env):
        return subprocess.run([sys.executable, '-c', DRIVER], cwd=self.lane, env=self.env | env,
                              capture_output=True, text=True)

    def wrapped_land(self, **env):
        log, events = self.top / 'detached.log', self.top / 'events.tsv'
        done = subprocess.run([sys.executable, 'tools/land-run', str(log), str(events)], cwd=self.lane,
                              env=self.env | env, capture_output=True, text=True)
        with events.open(newline='') as source:
            rows = list(csv.reader(source, delimiter='\t'))
        return done, log.read_text(), rows

    def cost_change(self):
        self.git(self.lane, 'checkout', '-q', 'main')
        directory = self.lane / 'crates/hirsel-core-front'
        directory.mkdir(parents=True)
        (directory / 'cost-ceilings.json').write_text(json.dumps(dict(
            policy='fixture', finished_budget_per_line=60000, corpora={})))
        (directory / 'front-cost-ceilings.json').write_text(json.dumps(dict(
            policy='fixture', history=['old measurements'], suite_targets_instructions={}, corpora=dict(m1=dict(
                observation=dict(tokens=10), source_bytes=50, tokens=10,
                instructions_per_token=dict(parse=100, index=80))))))
        self.git(self.lane, 'add', str(directory))
        self.git(self.lane, 'commit', '-qm', 'base cost ceilings')
        self.git(self.lane, 'push', '-q', 'origin', 'main')
        self.git(self.lane, 'checkout', '-q', 'my-lane')
        self.git(self.lane, 'rebase', 'main')
        (directory / 'source.rs').write_text('compiler source change')
        self.git(self.lane, 'add', str(directory / 'source.rs'))
        self.git(self.lane, 'commit', '-qm', 'exercise a cost-relevant land')

    def test_front_cost_overrun_lands_and_reaches_ticket_event(self):
        self.cost_change()
        accepted, log, rows = self.wrapped_land(FAKE_COST_RED='1')
        self.assertEqual(accepted.returncode, 0, log)
        self.assertIn('parse: 201 exceeds 100', log)
        self.assertIn('parse: 201 exceeds 100', rows[-1][-1])
        cost = json.loads(rows[-1][-1])[0]
        self.assertEqual(cost['tickets'], ['my-lane'])
        self.assertEqual(rows[-1][0], 'LANDED')
        self.assertIn('exercise a cost-relevant land', self.main_log())
        saved = json.loads(self.git(self.origin, 'show', 'main:crates/hirsel-core-front/front-cost-ceilings.json'))
        self.assertEqual(saved['corpora']['m1']['instructions_per_token'], dict(parse=100, index=50, resolve=30))
        self.assertEqual(saved['corpora']['m1']['image_per_token'], dict(bytes_read=1.02))
        self.assertNotIn('observation', json.dumps(saved))
        self.assertNotIn('history', saved)
        source = json.loads((self.lane / 'crates/hirsel-core-front/front-cost-ceilings.json').read_text())
        self.assertEqual(source['corpora']['m1']['instructions_per_token'], dict(parse=100, index=80))


    def red_ticket_after_a_ceilings_push(self, extra=None):
        """Land a cost-relevant change (the train adds its ceilings commit), then queue a red ticket.

        `extra` replaces the train's ceilings commit with one that also touches that path.
        Returns the red ticket's output.
        """
        self.cost_change()
        self.assertEqual(self.land().returncode, 0)
        self.assertEqual(self.main_log()[0], 'land: ratchet measured cost ceilings')
        self.git(self.lane, 'fetch', '-q', 'origin')
        self.git(self.lane, 'checkout', '-q', '-B', 'my-lane', 'origin/main')
        if extra:
            (self.lane / extra).write_text('also changed\n')
            self.git(self.lane, 'add', '-A')
            self.git(self.lane, 'commit', '-q', '--amend', '--no-edit')
            self.git(self.lane, 'push', '-qf', 'origin', 'HEAD:main')
        (self.lane / 'broken-core-suite').write_text('red\n')
        self.git(self.lane, 'add', 'broken-core-suite')
        self.git(self.lane, 'commit', '-qm', 'red ticket')
        red = self.land()
        self.assertEqual(red.returncode, 1, red.stdout + red.stderr)
        self.assertIn('core evaluator body failed', red.stdout)
        return red.stdout

    def test_a_tip_the_train_pushed_with_its_ceilings_commit_is_not_checked_again(self):
        # The red ticket's own check is the only build: its base is the train's tip.
        output = self.red_ticket_after_a_ceilings_push()
        self.assertEqual(output.count('pass  workspace build'), 1, output)
        self.assertIn('is a tree the train checked green', output)

    def test_a_ceilings_commit_touching_another_path_is_checked_as_a_base(self):
        output = self.red_ticket_after_a_ceilings_push(extra='other-path.txt')
        self.assertEqual(output.count('pass  workspace build'), 2, output)
        self.assertNotIn('is a tree the train checked green', output)

    def test_cost_tool_crash_refuses_the_push(self):
        self.cost_change()
        refused = self.land(FAKE_COST_CRASH='1')
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('core cost tool failed', refused.stdout)
        self.assertIn('cost tool crashed', refused.stdout)
        self.assertEqual(self.main_log(), ['base cost ceilings', 'base'])

    def test_lane_ceiling_edits_are_refused(self):
        self.cost_change()
        path = self.lane / 'crates/hirsel-core-front/front-cost-ceilings.json'
        data = json.loads(path.read_text())
        data['corpora']['m1']['instructions_per_token']['parse'] = 999
        path.write_text(json.dumps(data))
        self.git(self.lane, 'add', str(path))
        self.git(self.lane, 'commit', '-qm', 'hand edit a ceiling')
        refused = self.land()
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('land owns measured cost ceilings', refused.stdout)
        self.assertEqual(self.main_log(), ['base cost ceilings', 'base'])

    def test_lane_cannot_delete_cost_ceilings_to_skip_measurement(self):
        self.cost_change()
        self.git(self.lane, 'rm', 'crates/hirsel-core-front/cost-ceilings.json')
        self.git(self.lane, 'commit', '-qm', 'delete cost ceilings')
        refused = self.land()
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('land owns measured cost ceilings', refused.stdout)
        self.assertEqual(self.main_log(), ['base cost ceilings', 'base'])

    def test_reviewed_increase_is_applied_by_land_and_logged(self):
        self.cost_change()
        request = self.lane / 'tools/cost-ceiling-requests/my-lane.json'
        request.parent.mkdir(parents=True)
        request.write_text(json.dumps(dict(file='crates/hirsel-core-front/front-cost-ceilings.json',
            cell=['corpora', 'm1', 'instructions_per_token', 'parse'], previous=100,
            value=250, reason='reviewed new source support')))
        self.git(self.lane, 'add', str(request))
        self.git(self.lane, 'commit', '-qm', 'request an explained increase')
        accepted, log, events = self.wrapped_land(FAKE_COST_RED='1')
        self.assertEqual(accepted.returncode, 0, log)
        saved = json.loads(self.git(self.origin, 'show', 'main:crates/hirsel-core-front/front-cost-ceilings.json'))
        self.assertEqual(saved['corpora']['m1']['instructions_per_token']['parse'], 205)
        self.assertIn('reviewed new source support', events[-1][-1])
        self.assertEqual(self.git(self.origin, 'ls-tree', '--name-only', 'main', str(request.relative_to(self.lane))), '')

    def test_red_core_custody_refuses_the_push(self):
        (self.lane / 'broken-custody').write_text('broken sample\n')
        self.git(self.lane, 'add', 'broken-custody')
        self.git(self.lane, 'commit', '-qm', 'break custody')
        refused = self.land()
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Core suites', refused.stdout)
        self.assertIn('custody native agreement failed', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_red_arm_core_suite_refuses_the_push(self):
        (self.lane / 'broken-arm-core').write_text('broken ARM body\n')
        self.git(self.lane, 'add', 'broken-arm-core')
        self.git(self.lane, 'commit', '-qm', 'break an ARM core body')
        refused = self.land()
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Core suites', refused.stdout)
        self.assertIn('ARM CoreIR body failed', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_conflict_markers_refuse_the_push_with_file_and_line(self):
        for marker in ('<<<<<<< lane\nours\n=======\ntheirs\n>>>>>>> main\n',
                       '>>>>>>> orphaned\n', '<<<<<<< unfinished\n'):
            with self.subTest(marker=marker):
                (self.lane / 'change.txt').write_text('heading\n' + marker)
                self.git(self.lane, 'add', 'change.txt')
                self.git(self.lane, 'commit', '-qm', 'unresolved conflict')
                refused = self.land()
                self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
                self.assertIn('FAIL  screen: conflict markers', refused.stdout)
                self.assertIn('change.txt:2: unresolved conflict marker', refused.stdout)
                self.assertNotIn('pass  workspace build', refused.stdout)
                self.assertEqual(self.main_log(), ['base'])

    def test_setext_underlines_and_quoted_marker_fixtures_land(self):
        (self.lane / 'change.txt').write_text('Heading\n=======\n    <<<<<<< fixture\n    >>>>>>> fixture\n')
        (self.lane / 'binary.dat').write_bytes(b'\0<<<<<<< binary\n')
        (self.lane / 'untracked.txt').write_text('<<<<<<< untracked\n')
        with (self.lane / '.gitignore').open('a') as ignore:
            ignore.write('untracked.txt\n')
        self.git(self.lane, 'add', 'change.txt', 'binary.dat', '.gitignore')
        self.git(self.lane, 'commit', '-qm', 'literal markers and underline')
        accepted = self.land()
        self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)

    def test_stale_std_api_index_is_regenerated_and_committed(self):
        source = self.lane / 'core/std/example.hirsel'
        source.write_text(source.read_text() + 'pub fn added() -> void {}\n')
        self.git(self.lane, 'add', 'core/std/example.hirsel')
        self.git(self.lane, 'commit', '-qm', 'add an operation without updating its index')
        accepted = self.land()
        self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
        index = self.git(self.origin, 'show', 'main:core/std/API-INDEX.md')
        self.assertIn('present, added', index)
        self.assertIn('| **total** | **0 of 1** | **2** | |', index)

    def std_module(self, lane, name, stale=False):
        source, contract = f'core/std/{name}.hirsel', f'core/std/api/{name}.md'
        (lane / source).write_text(f'pub fn {name}_operation() -> void {{}}\n')
        (lane / contract).write_text(f'# {name}\n\nThe {name} contract.\n')
        if not stale:
            subprocess.run([sys.executable, 'core/tools/std_api_index.py', '--write'], cwd=lane, check=True)
        self.git(lane, 'add', source, contract, 'core/std/API-INDEX.md')

    def assert_std_index(self, names):
        index = self.git(self.origin, 'show', 'main:core/std/API-INDEX.md')
        self.assertIn(f'| **total** | **0 of {len(names) + 1}** | **{len(names) + 1}** | |', index)
        for name in names:
            self.assertIn(f'| [{name}](api/{name}.md) |  | 1 | {name}_operation |', index)
            self.assertIn(f'# {name}', self.git(self.origin, 'show', f'main:core/std/api/{name}.md'))
            self.assertIn(f'pub fn {name}_operation', self.git(self.origin, 'show', f'main:core/std/{name}.hirsel'))

    def test_two_std_modules_land_sequentially_without_manual_rebase(self):
        other = self.top / 'other'
        self.git(self.lane, 'worktree', 'add', '-qb', 'other', str(other), 'HEAD')
        (other / '.buck2/bin').mkdir(parents=True)
        shutil.copy(self.lane / '.buck2/bin/buck2', other / '.buck2/bin/buck2')
        for lane, name in ((self.lane, 'alpha'), (other, 'beta')):
            self.std_module(lane, name)
            self.git(lane, 'commit', '-qm', f'add {name}')
        self.finish_land(self.start_land(self.lane))
        self.assert_std_index(['alpha'])
        output = self.finish_land(self.start_land(other))
        self.assertIn('resolved generated conflict in core/std/API-INDEX.md', output)
        self.assert_std_index(['alpha', 'beta'])

    def test_two_std_modules_land_in_one_batch(self):
        checks, lanes, lands, holder = self.batch_lanes(
            2, prepare=lambda lane, i: self.std_module(lane, ('alpha', 'beta')[i]))
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        self.assertIn('batch of 2 tickets', outputs[0])
        self.assertEqual(sum(out.count('pass  workspace build') for out in outputs), 1)
        self.assert_std_index(['alpha', 'beta'])

    def test_stale_index_does_not_hide_a_real_module_contract_conflict(self):
        self.conflicting_lanes('core/std/api/example.md')
        source = self.lane / 'core/std/example.hirsel'
        source.write_text(source.read_text() + 'pub fn added() -> void {}\n')
        self.git(self.lane, 'add', 'core/std/example.hirsel')
        self.git(self.lane, 'commit', '-qm', 'stale generated index and conflicting contract')
        before = self.git(self.origin, 'rev-parse', 'main')
        refused = self.land()
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('conflicts in: core/std/api/example.md', refused.stdout)
        self.assertEqual(self.git(self.origin, 'rev-parse', 'main'), before)

    def test_std_module_requires_its_contract(self):
        (self.lane / 'core/std/api/example.md').unlink()
        checked = subprocess.run([sys.executable, 'core/tools/std_api_index.py', '--write'],
                                 cwd=self.lane, capture_output=True, text=True)
        self.assertEqual(checked.returncode, 1)
        self.assertIn('missing module contract core/std/api/example.md', checked.stderr)

    def test_std_index_links_related_module_contracts(self):
        source = self.lane / 'core/core/clock.hirsel'
        source.parent.mkdir(parents=True)
        source.write_text('pub fn read() -> u64 { 0 }\n')
        (self.lane / 'core/std/api/clock.md').write_text('# Clock\n')
        subprocess.run([sys.executable, 'core/tools/std_api_index.py', '--write'], cwd=self.lane, check=True)
        self.assertIn('- [clock](api/clock.md)', (self.lane / 'core/std/API-INDEX.md').read_text())

    def test_std_index_checks_and_rewrites_the_whole_document(self):
        path = self.lane / 'core/std/API-INDEX.md'
        wanted = path.read_bytes()
        path.write_bytes(wanted + b'=======\n| stale row |\n>>>>>>> orphan\n')
        checked = subprocess.run([sys.executable, 'core/tools/std_api_index.py', '--check'],
                                 cwd=self.lane, capture_output=True, text=True)
        self.assertNotEqual(checked.returncode, 0)
        self.assertIn('operation index is stale', checked.stderr)
        subprocess.run([sys.executable, 'core/tools/std_api_index.py', '--write'], cwd=self.lane, check=True)
        self.assertEqual(path.read_bytes(), wanted)

    def test_red_whole_core_suite_refuses_the_push(self):
        (self.lane / 'broken-core-suite').write_text('broken evaluator body\n')
        self.git(self.lane, 'add', 'broken-core-suite')
        self.git(self.lane, 'commit', '-qm', 'break a core suite')
        refused = self.land()
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Core suites', refused.stdout)
        self.assertIn('core evaluator body failed', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def law_fixture(self, expected):
        # Keep the real walker/helper; only Buck's transport is fake here.
        helper = ROOT / 'buck-out/tmp/cargo-bin-exe/test-land/hirsel-core-contract-eval'
        self.assertTrue(helper.is_file(), 'release evaluator is required test data')
        (self.lane / 'core/std/example.hirsel').write_text('pub fn present()->u64 {7}\n')
        directory = self.lane / 'core/std/tests'
        directory.mkdir()
        oracle = '' if expected is None else f'// expect: run answer() = {expected}\n'
        (directory / 'oracle.hirsel').write_text(oracle + 'fn answer()->u64 {7}\n')
        self.git(self.lane, 'add', 'core')
        self.git(self.lane, 'commit', '-qm', 'library law fixture')
        return dict(FAKE_LAW_GATE='1', FAKE_CORE_EVALUATOR=str(helper.resolve()))

    def test_wrong_library_oracle_refuses_push_until_corrected(self):
        fixture = self.law_fixture(8)
        refused = self.land(**fixture)
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Core suites', refused.stdout)
        self.assertIn("'values': [0]", refused.stdout)
        self.assertIn('oracle:answer', refused.stdout)
        # The surviving land log alone must tell a slow red from a wrong one.
        self.assertRegex(refused.stdout, r'oracle:answer: .*\(\d+\.\d{3}s of the 30s bound\)')
        self.assertEqual(self.main_log(), ['base'])
        law =self.lane / 'core/std/tests/oracle.hirsel'
        law.write_text(law.read_text().replace('answer() = 8', 'answer() = 7'))
        self.git(self.lane, 'commit', '-qam', 'correct the authored expectation')
        accepted = self.land(**fixture)
        self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)

    def test_zero_library_law_selection_refuses_the_push(self):
        refused = self.land(**self.law_fixture(None))
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Core suites', refused.stdout)
        self.assertIn('no expect: run', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_native_only_library_selection_cannot_pass_the_evaluator_gate(self):
        fixture = self.law_fixture(None)
        law = self.lane / 'core/std/tests/oracle.hirsel'
        law.write_text('// expect: run answer(out) = true\nfn answer(out:Stdout)->bool {true}\n')
        self.git(self.lane, 'commit', '-qam', 'only a native law remains')
        refused = self.land(**fixture)
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Core suites', refused.stdout)
        self.assertIn('zero evaluator-eligible laws selected', refused.stdout)
        self.assertIn('1 native-only', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_detached_wrapper_records_submission_and_one_completion_with_the_landed_sha(self):
        done, log, rows = self.wrapped_land()
        self.assertEqual(done.returncode, 0, log + done.stderr)
        self.assertEqual([row[0] for row in rows], ['SUBMITTED', 'LANDED'])
        submitted, landed = rows
        self.assertEqual(submitted[1:4], landed[1:4])
        self.assertEqual(landed[1:3], ['my-lane', '00000000000000000001'])
        self.assertEqual(landed[4], '0')
        self.assertEqual(landed[5], self.git(self.origin, 'rev-parse', 'main').strip())
        self.assertEqual(log.count('EXIT 0'), 1)
        self.assertEqual(self.queued(), [])
        self.assertEqual(list((self.lane / '.git/land-checkouts').iterdir()), [])

    def test_detached_wrapper_records_failure_without_claiming_a_landing(self):
        done, log, rows = self.wrapped_land(FAKE_FMT_RED='1')
        self.assertEqual(done.returncode, 1, log + done.stderr)
        self.assertEqual([row[0] for row in rows], ['SUBMITTED', 'FAILED'])
        self.assertEqual(rows[-1][4:6], ['1', ''])
        self.assertEqual(log.count('EXIT 1'), 1)
        self.assertEqual(self.main_log(), ['base'])

    def test_a_ticket_breaking_a_source_audit_is_refused_at_screen(self):
        # The layering audit supplies the oracle; this test pins only the
        # queue's admission rule, before any workspace build can run.
        (self.lane / 'broken-layering').write_text('wrong dependency\n')
        self.git(self.lane, 'add', 'broken-layering')
        self.git(self.lane, 'commit', '-qm', 'break crate layering')
        builds = self.top / 'builds'
        refused = self.land(FAKE_BUILD_LOG=str(builds))
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('FAIL  screen: source audits', refused.stdout)
        self.assertIn('crate layering: a lower crate depends on a higher one', refused.stdout)
        self.assertFalse(builds.exists(), refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_core_acceptance_input_build_failure_refuses_the_push(self):
        events = self.lane / 'tools/buck2/event_logs.py'
        with events.open('a') as script:
            script.write("\nif __name__ == '__main__':\n import sys\n assert '//:land-core-library-modes[inputs]' in sys.argv\n print('LANG-4')\n sys.exit(1)\n")
        self.git(self.lane, 'add', str(events.relative_to(self.lane)))
        self.git(self.lane, 'commit', '-qm', 'core acceptance inputs refuse')
        refused = self.land()
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('workspace build', refused.stdout)
        self.assertIn('LANG-4', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_detached_wrapper_preserves_one_submission_across_reexec(self):
        other = self.top / 'other'
        self.git(self.top, 'clone', '-q', str(self.origin), str(other))
        with (other / 'tools/land').open('a') as script:
            script.write('\n# Updated land on main.\n')
        self.git(other, 'commit', '-qam', 'update land')
        self.git(other, 'push', '-q', 'origin', 'main')
        done, log, rows = self.wrapped_land()
        self.assertEqual(done.returncode, 0, log + done.stderr)
        self.assertIn('rebased tree has another tools/land', log)
        self.assertEqual([row[0] for row in rows], ['SUBMITTED', 'LANDED'])
        self.assertEqual(rows[0][2], rows[1][2])
        self.assertEqual(list((self.lane / '.git/land-checkouts').iterdir()), [])

    def main_log(self):
        return self.git(self.origin, 'log', '--format=%s', 'main').split('\n')[:-1]

    def ledger_checks(self, text=''):
        (self.lane / 'tools/check-hot-files').write_text((ROOT / 'tools/check-hot-files').read_text())
        (self.lane / '.gitattributes').write_text('tools/known-failures.tsv merge=hirsel\n')
        (self.lane / 'tools/known-failures.tsv').write_text(text)
        self.git(self.lane, 'add', '-A')
        self.git(self.lane, 'commit', '-qm', 'ledger checks')

    def ledger_batch(self, first, second, expected):
        self.ledger_checks('# failures\nkeep\treason\towner\t2026-10-30\n')
        prefix = (self.lane / 'tools/known-failures.tsv').read_text()
        checks, lanes, lands, holder = self.batch_lanes(2, edits={
            0: ('tools/known-failures.tsv', prefix + first),
            1: ('tools/known-failures.tsv', prefix + second),
        })
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        self.assertIn('batch of 2 tickets', outputs[0])
        self.assertEqual(self.git(self.origin, 'show', 'main:tools/known-failures.tsv'), prefix + expected)
        self.assertEqual(len((checks / 'received').read_text().splitlines()), 1)


    def test_equal_date_failure_rows_choose_the_incoming_ticket(self):
        self.ledger_batch('red\tfirst\tfirst\t2026-11-01\n', 'red\tsecond\tsecond\t2026-11-01\n',
                          'red\tsecond\tsecond\t2026-11-01\n')


    def test_a_ticket_with_duplicate_failures_is_refused_unchanged(self):
        original = ('red\tnew\towner\t2026-11-01\n'
                    'other\treason\towner\t2026-10-30\n'
                    'red\told\towner\t2026-10-30\n')
        self.ledger_checks(original)
        landed = self.land()
        self.assertEqual(landed.returncode, 1, landed.stdout + landed.stderr)
        self.assertIn('FAIL  screen: independent line lists', landed.stdout)
        self.assertEqual((self.lane / 'tools/known-failures.tsv').read_text(), original)
        self.assertEqual(self.main_log(), ['base'])

    def test_an_unterminated_duplicate_failure_is_refused_unchanged(self):
        original = ('red\told\towner\t2026-10-30\n'
                    'other\treason\towner\t2026-10-30\n'
                    'red\tnew\towner\t2026-11-01')
        self.ledger_checks('red\told\towner\t2026-10-30\n'
                           'other\treason\towner\t2026-10-30\n')
        with (self.lane / 'tools/known-failures.tsv').open('a') as out:
            out.write('red\tnew\towner\t2026-11-01')
        self.git(self.lane, 'commit', '-qam', 'duplicate unterminated failure')
        landed = self.land()
        self.assertEqual(landed.returncode, 1, landed.stdout + landed.stderr)
        self.assertIn('FAIL  screen: independent line lists', landed.stdout)
        self.assertEqual((self.lane / 'tools/known-failures.tsv').read_text(), original)
        self.assertEqual(self.main_log(), ['base'])

    def ledger_three_way(self, base, main, ticket, expected):
        self.ledger_checks(base)
        self.git(self.lane, 'push', '-q', 'origin', 'HEAD:main')
        self.git(self.lane, 'checkout', '-q', '-b', 'main-edit')
        (self.lane / 'tools/known-failures.tsv').write_text(main)
        self.git(self.lane, 'commit', '-qam', 'main ledger edit')
        self.git(self.lane, 'push', '-q', 'origin', 'HEAD:main')
        self.git(self.lane, 'checkout', '-q', 'my-lane')
        (self.lane / 'tools/known-failures.tsv').write_text(ticket)
        self.git(self.lane, 'commit', '-qam', 'ticket ledger edit')
        landed = self.land()
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(self.git(self.origin, 'show', 'main:tools/known-failures.tsv'), expected)

    def test_a_concurrent_edit_is_kept_when_the_ticket_deletes(self):
        self.ledger_three_way('a\tr\tx\t2026-10-30\nred\tr\tx\t2026-10-30\nz\tr\tx\t2026-10-30\n',
                              'a\tr\tx\t2026-10-30\nred\tr\ty\t2026-11-01\nz\tr\tx\t2026-10-30\n',
                              'a\tr\tx\t2026-10-30\nz\tr\tx\t2026-10-30\n',
                              'a\tr\tx\t2026-10-30\nred\tr\ty\t2026-11-01\nz\tr\tx\t2026-10-30\n')


    def test_malformed_failure_rows_still_refuse_landing(self):
        valid = 'keep\tvalid\towner\t2026-11-01\n'
        self.ledger_checks(valid)
        for malformed in ('red\tmissing date\towner\n', 'red\t\towner\t2026-11-01\n',
                          'red\tbad date\towner\t2026-02-30\n', 'red\tbad format\towner\t2026-1-01\n'):
            with self.subTest(row=malformed):
                (self.lane / 'tools/known-failures.tsv').write_text(valid + malformed)
                self.git(self.lane, 'commit', '-qam', 'malformed failure')
                refused = self.land()
                self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
                self.assertIn('FAIL  screen: independent line lists', refused.stdout)
                self.assertIn(malformed, (self.lane / 'tools/known-failures.tsv').read_text())
                self.assertEqual(self.main_log(), ['base'])

    def test_check_preparation_finds_cargo_without_it_on_the_leaders_path(self):
        cargo_bin = self.top / 'cargo-home/bin'
        cargo_bin.mkdir(parents=True)
        cargo = cargo_bin / 'cargo'
        cargo.write_text('#!/bin/sh\necho cargo-found\n')
        cargo.chmod(0o755)
        (cargo_bin / 'rustc').symlink_to(cargo)
        # The same nested process boundary as prepare -> bootstrap -> sync.
        (self.lane / 'tools/buck2/prepare.py').write_text(FAKES['tools/buck2/prepare.py'] + '''import subprocess, sys
subprocess.run([sys.executable, 'tools/buck2/sync.py'], check=True)
''')
        (self.lane / 'tools/buck2/sync.py').write_text('''import os, subprocess
from pathlib import Path
found = subprocess.check_output(['cargo', '--version'], text=True).strip()
assert found == 'cargo-found', found
Path(os.environ['FAKE_PREPARED']).write_text(found)
''')
        self.git(self.lane, 'commit', '-qam', 'prepare needs cargo')
        stripped = dict(PATH=os.defpath, CARGO='cargo', CARGO_HOME=str(cargo_bin.parent),
                        RUSTUP_HOME=str(self.top / 'rustup-home'))
        self.assertIsNone(shutil.which('cargo', path=stripped['PATH']))
        landed = self.land(**stripped, FAKE_PREPARED=str(self.top / 'prepared'))
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertIn('pass  screen: prepare.py', landed.stdout)
        self.assertEqual((self.top / 'prepared').read_text(), 'cargo-found')

    def test_missing_toolchain_is_refused_before_a_ticket_is_published(self):
        for installed in (False, True):
            with self.subTest(installed=installed):
                if installed:
                    cargo_bin = self.top / 'cargo/bin'
                    cargo_bin.mkdir(parents=True)
                    for name in ('cargo', 'rustc'):
                        tool = cargo_bin / name
                        tool.write_text('#!/bin/sh\nexit 1\n')
                        tool.chmod(0o755)
                landed = self.land(PATH=os.defpath, HOME=str(self.top / 'home'), CARGO='cargo',
                                   CARGO_HOME=str(self.top / 'cargo'), RUSTUP_HOME=str(self.top / 'rustup'))
                self.assertEqual(landed.returncode, 1, landed.stdout + landed.stderr)
                self.assertEqual(len(landed.stdout.splitlines()), 1)
                self.assertIn('Rust toolchain unavailable' if installed else 'no Rust toolchain found', landed.stdout)
                self.assertFalse((self.lane / '.git/land-queue').exists())

    def test_a_tree_without_a_target_mains_tool_asks_for_is_refused_before_queueing(self):
        # A tree that renames a target must not join a batch still requesting it.
        other = self.top / 'other'
        self.git(self.top, 'clone', '-q', str(self.origin), str(other))
        with (other / 'tools/land').open('a') as tool:
            tool.write("\nOLD_CHECK = '//:test-old-name'\n")
        self.git(other, 'commit', '-qam', 'main asks for an old name')
        self.git(other, 'push', '-q', 'origin', 'main')
        buck2 = self.lane / '.buck2/bin/buck2'
        buck2.write_text('#!/bin/sh\ncase "$*" in *test-old-name*) echo "Unknown target" >&2; exit 1;; esac\n'
                         'echo root//:a\n')
        refused = self.land()
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('//:test-old-name', refused.stdout)
        self.assertFalse((self.lane / '.git/land-queue').exists())
        self.assertEqual(self.main_log(), ['main asks for an old name', 'base'])

    def test_a_push_rejected_because_main_moved_rebases_rebuilds_and_lands(self):
        landed = self.land(FAKE_OTHER_LANE=str(self.top / 'other'))
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertIn('main moved during the checks; rebasing again', landed.stdout)
        # Every attempted tree passes all three checks, including a retry.
        self.assertEqual(landed.stdout.count('pass  workspace build'), 2)
        self.assertEqual(landed.stdout.count('pass  Clippy'), 2)
        self.assertEqual(self.main_log(), ['change', 'other', 'base'])
        [row] = [json.loads(line) for line in (self.lane / '.git/hirsel-landings.jsonl').read_text().splitlines()]
        self.assertEqual((row['branch'], row['landed']), ('my-lane', self.git(self.origin, 'rev-parse', 'main').strip()))

    def push_errors(self, errors, **env):
        log = self.top / 'pushes'
        result = self.land(FAKE_PUSH_ERRORS=json.dumps(errors), FAKE_PUSH_LOG=str(log), **env)
        return result, log.read_text().splitlines()

    def test_http_500_then_success_pushes_the_same_checked_tree(self):
        landed, pushes = self.push_errors([dict(stderr='fatal: The requested URL returned error: 500')])
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(pushes, [pushes[0], pushes[0]])
        for stage in ('workspace build', 'Clippy'):
            self.assertEqual(landed.stdout.count('pass  ' + stage), 1)
        self.assertEqual(self.main_log(), ['change', 'base'])
        self.assertEqual(self.queued(), [])

    def test_persistent_http_500_stops_after_one_retry_without_landing(self):
        refused, pushes = self.push_errors([dict(stderr='fatal: HTTP 500') for _ in range(3)])
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertEqual(pushes, [pushes[0]] * 2)
        for stage in ('workspace build', 'Clippy'):
            self.assertEqual(refused.stdout.count('pass  ' + stage), 1)
        self.assertEqual(self.main_log(), ['base'])
        self.assertFalse((self.lane / '.git/hirsel-landings.jsonl').exists())
        self.assertEqual(self.queued(), [])

    def test_ambiguous_push_fetches_and_records_the_accepted_commit_once(self):
        landed, pushes = self.push_errors([dict(stderr='fatal: the remote end hung up unexpectedly', accept=True)])
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(len(pushes), 1)
        self.assertEqual(self.main_log(), ['change', 'base'])
        [row] = [json.loads(line) for line in (self.lane / '.git/hirsel-landings.jsonl').read_text().splitlines()]
        self.assertEqual(row['landed'], self.git(self.origin, 'rev-parse', 'main').strip())
        self.assertEqual(self.queued(), [])

    def test_http_500_with_moved_main_rebases_and_runs_all_gates_again(self):
        landed, pushes = self.push_errors([dict(stderr='fatal: HTTP 500')], FAKE_OTHER_LANE=str(self.top / 'other'))
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(len(pushes), 2)
        self.assertNotEqual(pushes[0], pushes[1])
        for stage in ('workspace build', 'Clippy'):
            self.assertEqual(landed.stdout.count('pass  ' + stage), 2)
        self.assertEqual(self.main_log(), ['change', 'other', 'base'])
        self.assertEqual(self.queued(), [])

    def test_connection_reset_retries_and_lands_without_rechecking(self):
        landed, pushes = self.push_errors([dict(stderr='fatal: unable to access origin: Recv failure: Connection reset by peer')])
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(pushes, [pushes[0], pushes[0]])
        for stage in ('workspace build', 'Clippy'):
            self.assertEqual(landed.stdout.count('pass  ' + stage), 1)
        self.assertEqual(self.main_log(), ['change', 'base'])

    def test_ambiguous_push_is_landed_when_main_contains_it_as_an_ancestor(self):
        landed, pushes = self.push_errors([dict(stderr='fatal: HTTP 502', accept=True, advance=True)])
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(len(pushes), 1)
        self.assertEqual(self.main_log(), ['other', 'change', 'base'])
        [row] = [json.loads(line) for line in (self.lane / '.git/hirsel-landings.jsonl').read_text().splitlines()]
        self.assertEqual(row['landed'], self.git(self.origin, 'rev-parse', 'main^').strip())

    def test_failed_fetch_cannot_confirm_or_retry_an_ambiguous_push(self):
        refused, pushes = self.push_errors([dict(stderr='fatal: HTTP 500', fetch_error='connection reset')])
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertEqual(len(pushes), 1)
        self.assertIn('cannot confirm failed push', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])
        self.assertFalse((self.lane / '.git/hirsel-landings.jsonl').exists())

    def test_http_4xx_and_hook_failures_do_not_retry_even_with_transport_errors(self):
        for error in ('fatal: The requested URL returned error: 403\nRecv failure: Connection reset by peer',
                      'remote: HTTP 500\n! [remote rejected] HEAD -> main (pre-receive hook declined)'):
            with self.subTest(error=error):
                (self.top / 'pushes').unlink(missing_ok=True)
                refused, pushes = self.push_errors([dict(stderr=error)])
                self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
                self.assertEqual(len(pushes), 1)
                self.assertEqual(self.main_log(), ['base'])
                self.assertEqual(self.queued(), [])

    def recording_remote(self):
        checks = self.top / 'checks'
        checks.mkdir()
        self.env['FAKE_CHECKS'] = str(checks)
        # A real receive hook verifies every proposed tree has all checks,
        # and every accepted tree was checked on the remote's exact old tip.
        hook = self.origin / 'hooks/pre-receive'
        hook.write_text("""#!/usr/bin/env python3
import os, pathlib, subprocess, sys
root = pathlib.Path(os.environ['FAKE_CHECKS'])
for line in sys.stdin:
    old, new, ref = line.split()
    # fmt is the screen's check of the same tree, which may sit under another commit.
    stages = [root / new / stage for stage in ('build', 'clippy')]
    if not all(path.is_file() for path in stages):
        print('UNCHECKED TREE', new, file=sys.stderr)
        sys.exit(1)
    if any(path.read_text() != old for path in stages):
        print('WRONG CHECKED BASE', new, file=sys.stderr)
        sys.exit(1)
    with (root / 'received').open('a') as out:
        out.write(new + '\\n')
""")
        hook.chmod(0o755)
        for name, stage in [('event_logs', 'build'), ('clippy', 'clippy'), ('fmt', 'fmt')]:
            text = 'STAGE = ' + repr(stage) + '\n' + RECORD
            if name == 'event_logs':
                text = FAKES['tools/buck2/event_logs.py'] + "if __name__ == '__main__':\n" + ''.join(
                    '    ' + line + '\n' for line in text.splitlines())
            (self.lane / f'tools/buck2/{name}.py').write_text(text)
        self.git(self.lane, 'add', 'tools/buck2')
        self.git(self.lane, 'commit', '-qm', 'record checks')
        return checks

    def wait_for(self, predicate):

        while not predicate():
            time.sleep(.02)

    def queue_lanes(self):
        checks = self.recording_remote()
        # Hold the first check so later, faster lands cannot overtake it.
        with (self.lane / 'tools/buck2/fmt.py').open('a') as out:
            out.write("""
# Only the first lane's own tree: it lands alone, and later batches do not hold.
if pathlib.Path('lane-0.txt').exists() and not pathlib.Path('lane-1.txt').exists():
    import time
    (root / 'holding-pid').write_text(str(os.getpid()))
    (root / 'holding').touch()

    while not (root / 'release').exists():
        time.sleep(.02)
""")
        self.git(self.lane, 'commit', '-qam', 'slow first check')
        lanes = [self.lane]
        # Worktrees share the queue and remote refs, just like host lanes.
        for name in ('second', 'third'):
            lane = self.top / name
            self.git(self.lane, 'worktree', 'add', '-qb', name, str(lane), 'HEAD')
            (lane / '.buck2/bin').mkdir(parents=True)
            shutil.copy(self.lane / '.buck2/bin/buck2', lane / '.buck2/bin/buck2')
            lanes.append(lane)
        for i, lane in enumerate(lanes):
            (lane / f'lane-{i}.txt').write_text(f'{i}\n')
            self.git(lane, 'add', f'lane-{i}.txt')
            self.git(lane, 'commit', '-qm', f'lane {i}')
        return checks, lanes

    def start_land(self, lane, arguments=()):
        log = (self.top / (lane.name + '.log')).open('w+')
        self.addCleanup(log.close)
        proc = subprocess.Popen([sys.executable, '-c', DRIVER, *arguments], cwd=lane,
                                env=self.env, stdout=log, stderr=log, text=True)
        def cleanup():
            if proc.poll() is None:
                proc.kill()
            proc.wait()
        self.addCleanup(cleanup)
        return proc, log

    def finish_land(self, process, code=0):
        proc, log = process
        proc.wait()
        log.seek(0)
        output = log.read()
        self.assertEqual(proc.returncode, code, output)
        return output

    @staticmethod
    def landed(output):
        """The commits a waiting lane says a batch landed for it."""
        [line] = [line for line in output.splitlines() if line.startswith('land: landed ') and ' led by ' in line]
        return line.rsplit(': ', 1)[1].split()

    def batch_lanes(self, count, edits=None, old=(), red=None, hold_build=False, prepare=None, arguments=None):
        """`count` worktrees with one commit each, queued in lane order behind a live ticket this test holds.

        Lanes in `old` are queued as an older tools/land queues them: a ticket
        without its commit, held by a live process that never reads a result.
        """
        checks = self.recording_remote()
        # `red` names the fake check (fmt or build) that fails on a red.txt that says red.
        if red:
            error = {'fmt': 'Diff in red.txt at line 1:', 'build': 'error[E0425]: red.txt is red'}[red]
            fake = {'fmt': 'fmt', 'build': 'event_logs'}[red]
            with (self.lane / f'tools/buck2/{fake}.py').open('a') as out:
                out.write("if pathlib.Path('red.txt').is_file() and pathlib.Path('red.txt').read_text() == 'red\\n':\n"
                          f"    print({error!r})\n    sys.exit(1)\n")
            self.git(self.lane, 'commit', '-qam', f'red {red}')
        if hold_build:
            with (self.lane / 'tools/buck2/event_logs.py').open('a') as out:
                out.write("""
if not (root / 'release-build').exists():
    import time
    (root / 'holding-build').touch()

    while not (root / 'release-build').exists():
        time.sleep(.02)
""")
            self.git(self.lane, 'commit', '-qam', 'hold first build')
        # The fake checks are main's; only each lane's own commit is queued.
        hook = self.origin / 'hooks/pre-receive'
        hook.rename(hook.with_name('held'))
        self.git(self.lane, 'push', '-q', 'origin', 'HEAD:main')
        hook.with_name('held').rename(hook)
        lanes = [self.lane]
        for i in range(1, count):
            lane = self.top / f'lane{i}'
            self.git(self.lane, 'worktree', 'add', '-qb', f'lane{i}', str(lane), 'HEAD')
            (lane / '.buck2/bin').mkdir(parents=True)
            shutil.copy(self.lane / '.buck2/bin/buck2', lane / '.buck2/bin/buck2')
            lanes.append(lane)
        for i, lane in enumerate(lanes):
            if prepare:
                prepare(lane, i)
            else:
                path, text = (edits or {}).get(i, (f'lane-{i}.txt', f'{i}\n'))
                (lane / path).write_text(text)
                self.git(lane, 'add', path)
            self.git(lane, 'commit', '-qm', f'lane {i}')
        queue = runpy.run_path(str(ROOT / 'tools/land'))['LandQueue']
        directory = self.lane / '.git/land-queue'
        directory.mkdir()
        def ticket(number, pid, lane, worktree):
            now = time.time()
            path = directory / f'{number:020d}'
            path.write_text(json.dumps(dict(pid=pid, process_start=queue.process(pid), started=now,
                                            heartbeat=now, lane=lane, worktree=str(worktree))))
            (directory / 'sequence').write_text(f'{number}\n')
            return path
        # As a head running its checks would, this ticket holds every lane behind it.
        holder = ticket(1, os.getpid(), 'holder', self.top / 'holder')
        lands = []
        for i, lane in enumerate(lanes):
            if i in old:
                sleeper = subprocess.Popen(['sleep', '60'])
                self.addCleanup(sleeper.wait)
                self.addCleanup(sleeper.kill)
                ticket(i + 2, sleeper.pid, lane.name if i else 'my-lane', lane)
                lands.append(sleeper)
            else:
                lands.append(self.start_land(lane, (arguments or {}).get(i, ())))
            self.wait_for(lambda i=i: len(self.queued()) == i + 2)
        for i in range(count):
            if i not in old:
                log = self.top / (lanes[i].name + '.log')
                self.wait_for(lambda log=log: 'queue position' in log.read_text())
        return checks, lanes, lands, holder

    def subjects(self, shas):
        return [self.git(self.origin, 'show', '-s', '--format=%s', sha).strip() for sha in shas]

    def add_required_gate(self, lane):
        path = lane / 'tools/land'
        path.write_text(path.read_text().replace('CORE_ACCEPTANCE = (',
                                                "CORE_ACCEPTANCE = ('//:new-required-gate',", 1))
        self.git(lane, 'add', 'tools/land')

    def test_waiting_tickets_own_acceptance_list_checks_its_land(self):
        def prepare(lane, i):
            (lane / f'lane-{i}.txt').write_text(f'{i}\n')
            self.git(lane, 'add', f'lane-{i}.txt')
            if i == 1:
                self.add_required_gate(lane)
        checks, lanes, lands, holder = self.batch_lanes(2, prepare=prepare)
        holder.unlink()
        outputs = [self.finish_land(land, i) for i, land in enumerate(lands)]
        self.assertIn('root//:new-required-gate', outputs[1])
        self.assertIn('newly required gate was executed', outputs[1])
        self.assertNotIn('lane 1', self.main_log())
        self.assertIn('lane 0', self.main_log())
        self.assertEqual(self.queued(), [])

    def test_waiting_tickets_own_validation_logic_checks_its_land(self):
        def prepare(lane, i):
            path = f'lane-{i}.txt'
            (lane / path).write_text(f'{i}\n')
            self.git(lane, 'add', path)
            if i == 1:
                tool = lane / 'tools/land'
                tool.write_text(tool.read_text().replace(
                    'def full(tree, base, screened=False):\n',
                    "def full(tree, base, screened=False):\n"
                    "    if (ROOT / 'lane-1.txt').exists():\n"
                    "        return ['FAIL  merged policy', 'new validation logic ran']\n", 1))
                self.git(lane, 'add', 'tools/land')
        checks, lanes, lands, holder = self.batch_lanes(2, prepare=prepare)
        holder.unlink()
        self.finish_land(lands[0])
        output = self.finish_land(lands[1], 1)
        self.assertIn('new validation logic ran', output)
        self.assertNotIn('lane 1', self.main_log())

    def test_stale_leader_updates_to_mains_acceptance_list(self):
        other = self.top / 'updated-main'
        self.git(self.top, 'clone', '-q', str(self.origin), str(other))
        self.add_required_gate(other)
        self.git(other, 'commit', '-qm', 'main adds a required gate')
        self.git(other, 'push', '-q', 'origin', 'main')
        refused = self.land()
        self.assertNotEqual(refused.returncode, 0, refused.stdout + refused.stderr)
        self.assertIn('rebased tree has another tools/land; running it', refused.stdout)
        self.assertIn('main is red at root//:new-required-gate', refused.stdout)
        self.assertEqual(self.main_log(), ['main adds a required gate', 'base'])

    def test_legacy_batcher_cannot_deliver_new_protocol_ticket(self):
        def prepare(lane, i):
            (lane / f'lane-{i}.txt').write_text(f'{i}\n')
            self.git(lane, 'add', f'lane-{i}.txt')
            if i == 1:
                self.add_required_gate(lane)
            else:
                tool = lane / 'tools/land'
                source = tool.read_text()
                begin, end = source.index('    def claim('), source.index('    def owned(')
                legacy = source[begin:end].replace("if record.get('result') is not None:",
                                                   "if 'result' in record:")
                source = source[:begin] + legacy + source[end:]
                begin, end = source.index('def candidate_check('), source.index('def failed_checks(')
                source = source[:begin] + (
                    "def candidate_check(mode, tree, base, screened=False, target=''):\n"
                    "    return fast(tree, base) if mode == 'fast' else full(tree, base, screened)\n\n\n"
                ) + source[end:]
                tool.write_text(source)
                self.git(lane, 'add', 'tools/land')
        checks, lanes, lands, holder = self.batch_lanes(2, prepare=prepare)
        holder.unlink()
        leader = self.finish_land(lands[0])
        waiter = self.finish_land(lands[1], 1)
        self.assertNotIn('batch of 2 tickets', leader)
        self.assertIn('new-required-gate', waiter)
        self.assertNotIn('lane 1', self.main_log())

    def red_main_fixture(self):
        (self.lane / 'main-red').write_text('a broken accepted target on main\n')
        self.git(self.lane, 'add', 'main-red')
        self.git(self.lane, 'commit', '-qm', 'red main fixture')
        self.env['FAKE_TARGET_LOG'] = str(self.top / 'target-checks')

    def test_red_main_is_checked_once_and_reported_to_the_whole_cohort(self):
        self.red_main_fixture()
        checks, lanes, lands, holder = self.batch_lanes(3)
        base = self.git(self.origin, 'rev-parse', 'main').strip()
        holder.unlink()
        outputs = [self.finish_land(land, 1) for land in lands]
        for output in outputs:
            self.assertIn('main is red at root//:hirsel-core-ir-unittest', output)
            self.assertIn('first seen at ' + base, output)
            self.assertIn('no ticket is blamed', output)
            self.assertNotIn('bisecting', output)
        probes = (self.top / 'target-checks').read_text().splitlines()
        self.assertEqual(probes.count(base), 1)
        self.assertEqual(len(probes), 2, 'one candidate and one bare-main check')
        self.assertEqual(self.git(self.origin, 'rev-parse', 'main').strip(), base)
        # A later cohort sees the same recorded base failure, without running
        # the failing target again or changing its first observed commit.
        output = self.land()
        self.assertNotEqual(output.returncode, 0)
        self.assertIn('first seen at ' + base, output.stdout)
        self.assertEqual((self.top / 'target-checks').read_text().splitlines(), probes)
        self.assertEqual(self.queued(), [])

    def test_verified_main_repair_lands_alone_ahead_of_waiting_cohort(self):
        self.red_main_fixture()
        def prepare(lane, i):
            (lane / f'lane-{i}.txt').write_text(f'{i}\n')
            self.git(lane, 'add', f'lane-{i}.txt')
            if i == 2:
                (lane / 'main-red').unlink()
                self.git(lane, 'add', 'main-red')
        checks, lanes, lands, holder = self.batch_lanes(3, prepare=prepare,
            arguments={2: ['--repair-main', '//:hirsel-core-ir-unittest']})
        base = self.git(self.origin, 'rev-parse', 'main').strip()
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        pushed = (checks / 'received').read_text().splitlines()
        self.assertEqual(len(pushed), 2)
        self.assertEqual(self.subjects(pushed[:1]), ['lane 2'])
        self.assertEqual(self.git(self.origin, 'log', '--format=%s', f'{base}..{pushed[0]}').splitlines(), ['lane 2'])
        self.assertIn('verified main-repair target //:hirsel-core-ir-unittest', outputs[0])
        self.assertIn('checking alone', outputs[0])
        self.assertEqual(self.main_log()[:3], ['lane 1', 'lane 0', 'lane 2'])
        self.assertEqual(self.queued(), [])

    def test_repair_label_does_not_grant_priority_on_green_main(self):
        checks, lanes, lands, holder = self.batch_lanes(3,
            arguments={2: ['--repair-main', '//:hirsel-core-ir-unittest']})
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        pushed = (checks / 'received').read_text().splitlines()
        self.assertEqual(self.subjects(pushed), ['lane 1', 'lane 2'])
        self.assertNotIn('verified main-repair', ''.join(outputs))
        self.assertEqual(self.main_log()[:3], ['lane 2', 'lane 1', 'lane 0'])

    def test_cleared_cached_failure_does_not_grant_repair_priority(self):
        checks, lanes, lands, holder = self.batch_lanes(3,
            arguments={2: ['--repair-main', '//:hirsel-core-ir-unittest']})
        base = self.git(self.origin, 'rev-parse', 'main').strip()
        # A transient failure was observed earlier at the same source commit.
        (holder.parent / 'red-main.json').write_text(json.dumps(dict(
            base=base, first_seen=base, targets=['root//:hirsel-core-ir-unittest'],
            report=['FAIL  Core suites', 'failing targets: root//:hirsel-core-ir-unittest'])))
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        pushed = (checks / 'received').read_text().splitlines()
        self.assertEqual(self.subjects(pushed), ['lane 1', 'lane 2'])
        self.assertIn('cached main failure no longer reproduces; keeping FIFO order', outputs[0])
        self.assertNotIn('verified main-repair', ''.join(outputs))

    def test_main_repair_still_has_to_pass_every_other_gate(self):
        self.red_main_fixture()
        def prepare(lane, i):
            (lane / f'lane-{i}.txt').write_text(f'{i}\n')
            self.git(lane, 'add', f'lane-{i}.txt')
            if i == 2:
                (lane / 'main-red').unlink()
                (lane / 'broken-custody').touch()
                self.git(lane, 'add', 'main-red', 'broken-custody')
        checks, lanes, lands, holder = self.batch_lanes(3, prepare=prepare,
            arguments={2: ['--repair-main', '//:hirsel-core-ir-unittest']})
        base = self.git(self.origin, 'rev-parse', 'main')
        holder.unlink()
        outputs = [self.finish_land(land, 1) for land in lands]
        self.assertIn('main-repair target passed but the standalone gate is red', outputs[2])
        self.assertIn('custody native agreement failed', outputs[2])
        self.assertEqual(self.git(self.origin, 'rev-parse', 'main'), base)
        self.assertFalse((checks / 'received').exists())

    def test_alone_ticket_is_a_fifo_barrier_and_lands_by_itself(self):
        checks, lanes, lands, holder = self.batch_lanes(3, arguments={1: ['--alone']})
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        self.assertEqual(self.subjects((checks / 'received').read_text().splitlines()),
                         ['lane 0', 'lane 1', 'lane 2'])
        self.assertNotIn('batch of', ''.join(outputs))

    def test_repair_label_cannot_bypass_a_still_red_target(self):
        self.red_main_fixture()
        checks, lanes, lands, holder = self.batch_lanes(3,
            arguments={2: ['--repair-main', '//:hirsel-core-ir-unittest']})
        base = self.git(self.origin, 'rev-parse', 'main')
        holder.unlink()
        outputs = [self.finish_land(land, 1) for land in lands]
        self.assertIn('does not fix //:hirsel-core-ir-unittest', ''.join(outputs))
        self.assertEqual(self.git(self.origin, 'rev-parse', 'main'), base)
        self.assertFalse((checks / 'received').exists())
        self.assertEqual(self.queued(), [])

    def test_a_batch_of_three_clean_changes_lands_with_one_check_run(self):
        checks, lanes, lands, holder = self.batch_lanes(3)
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        self.assertIn('batch of 3 tickets', outputs[0])
        self.assertEqual(sum(out.count('pass  workspace build') for out in outputs), 1, outputs)
        # One push of all three commits, each lane's own and in ticket order.
        self.assertEqual(len((checks / 'received').read_text().splitlines()), 1)
        self.assertEqual(self.main_log()[:4], ['lane 2', 'lane 1', 'lane 0', 'record checks'])
        for i in (1, 2):
            self.assertEqual(self.subjects(self.landed(outputs[i])), [f'lane {i}'])
            self.assertEqual(self.git(lanes[i], 'log', '-1', '--format=%s').strip(), f'lane {i}')
        self.assertEqual(self.queued(), [])

    def test_commits_after_submission_stay_in_the_lanes_next_ticket(self):
        checks, lanes, lands, holder = self.batch_lanes(2)
        for i, lane in enumerate(lanes):
            (lane / f'next-{i}.txt').write_text('next ticket\n')
            self.git(lane, 'add', f'next-{i}.txt')
            self.git(lane, 'commit', '-qm', f'next {i}')
        heads = [self.git(lane, 'rev-parse', 'HEAD') for lane in lanes]
        # A dirty edit is also owned by the lane, including when its ticket is delivered.
        (lanes[1] / 'lane-1.txt').write_text('ongoing edit\n')
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        self.assertEqual(sum(out.count('pass  workspace build') for out in outputs), 1)
        self.assertNotIn('next 0', self.main_log())
        self.assertNotIn('next 1', self.main_log())
        for lane, head in zip(lanes, heads):
            self.assertEqual(self.git(lane, 'rev-parse', 'HEAD'), head)
        self.assertEqual((lanes[1] / 'lane-1.txt').read_text(), 'ongoing edit\n')
        tip = self.git(self.origin, 'rev-parse', 'main').strip()
        self.assertEqual((checks / tip / 'build').read_text(),
                         self.git(self.origin, 'rev-parse', 'main~2').strip())

    def test_a_singleton_fmt_failure_stops_before_the_build(self):
        marker = self.top / 'built'
        with (self.lane / 'tools/buck2/event_logs.py').open('a') as script:
            script.write("\nfrom pathlib import Path\nPath(" + repr(str(marker)) + ").touch()\n")
        self.git(self.lane, 'commit', '-qam', 'record build invocation')
        refused = self.land(FAKE_FMT_RED='1')
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('Diff in crates/a/src/lib.rs', refused.stdout)
        self.assertFalse(marker.exists(), refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_a_second_ticket_for_a_live_lane_is_refused(self):
        checks, lanes, lands, holder = self.batch_lanes(1)
        refused = self.land()
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('already has live ticket', refused.stdout)
        holder.unlink()
        self.finish_land(lands[0])

    def test_a_submitted_semantic_conflict_is_reported_even_if_the_owner_keeps_working(self):
        self.conflicting_lanes('source.rs')
        queue = runpy.run_path(str(ROOT / 'tools/land'))['LandQueue']
        directory = self.lane / '.git/land-queue'
        directory.mkdir()
        holder = directory / '00000000000000000001'
        now = time.time()
        holder.write_text(json.dumps(dict(pid=os.getpid(), process_start=queue.process(os.getpid()),
                                          started=now, heartbeat=now, lane='holder', worktree=str(self.top / 'holder'))))
        (directory / 'sequence').write_text('1')
        land = self.start_land(self.lane)
        self.wait_for(lambda: len(self.queued()) == 2)
        # The owner resolves their next submission while the old, conflicting ticket waits.
        self.git(self.lane, 'fetch', '-q', 'origin', 'main')
        self.git(self.lane, 'reset', '--soft', 'origin/main')
        (self.lane / 'source.rs').write_text('main generator output\n')
        self.git(self.lane, 'add', 'source.rs')
        self.git(self.lane, 'commit', '-qm', 'owner resolution for next ticket')
        head = self.git(self.lane, 'rev-parse', 'HEAD')
        holder.unlink()
        output = self.finish_land(land, 1)
        self.assertIn('conflicts in: source.rs', output)
        self.assertEqual(self.git(self.lane, 'rev-parse', 'HEAD'), head)
        self.assertEqual(self.git(self.lane, 'status', '--porcelain'), '')
        self.assertNotIn('owner resolution for next ticket', self.main_log())
        self.assertEqual(self.queued(), [])

    def test_a_formatting_red_change_in_a_batch_of_four_leaves_before_any_build(self):
        checks, lanes, lands, holder = self.batch_lanes(4, edits={1: ('red.txt', 'red\n')}, red='fmt')
        holder.unlink()
        outputs = [self.finish_land(land, 1 if i == 1 else 0) for i, land in enumerate(lands)]
        self.assertIn('Diff in red.txt at line 1:', outputs[1])
        self.assertIn('refusing to push a red tree', outputs[1])
        self.assertNotIn('Diff in', outputs[2] + outputs[3])
        # The screen: all four red, main green, lanes 0-1 red, lane 0 green, then the other three green.
        self.assertEqual(outputs[0].count('screen: formatting'), 5, outputs[0])
        self.assertEqual(outputs[0].count('workspace build'), 1, outputs[0])
        self.assertEqual(outputs[0].count('FAIL  formatting'), 0, outputs[0])
        self.assertEqual(len((checks / 'received').read_text().splitlines()), 1)
        self.assertEqual(self.main_log()[:4], ['lane 3', 'lane 2', 'lane 0', 'red fmt'])
        self.assertEqual(self.queued(), [])

    def test_a_build_red_change_in_a_batch_of_four_bisects_and_the_green_half_lands_first(self):
        checks, lanes, lands, holder = self.batch_lanes(4, edits={2: ('red.txt', 'red\n')}, red='build')
        holder.unlink()
        outputs = [self.finish_land(land, 1 if i == 2 else 0) for i, land in enumerate(lands)]
        self.assertIn('error[E0425]: red.txt is red', outputs[2])
        self.assertIn('refusing to push a red tree', outputs[2])
        # Five candidate builds, plus one bare-main check before blaming any
        # ticket. The passed lanes 0-1 already prove the later base is green.
        self.assertEqual(outputs[0].count('workspace build'), 6, outputs[0])
        self.assertIn('with 5 checks', outputs[0])
        received = (checks / 'received').read_text().splitlines()
        self.assertEqual(self.subjects(received), ['lane 1', 'lane 3'])
        self.assertEqual(self.subjects(self.landed(outputs[1])), ['lane 1'])
        self.assertEqual(self.main_log()[:4], ['lane 3', 'lane 1', 'lane 0', 'red build'])
        self.assertEqual(self.queued(), [])

    def test_a_ticket_withdrawn_during_a_red_check_is_not_built_again_in_the_bisect(self):
        # Ticket 3505 was withdrawn mid-bisect, yet its group was built and failed again.
        checks, lanes, lands, holder = self.batch_lanes(4, edits={2: ('red.txt', 'red\n')}, red='build',
                                                        hold_build=True)
        holder.unlink()
        self.wait_for((checks / 'holding-build').exists)
        lands[2][0].send_signal(signal.SIGTERM)
        self.finish_land(lands[2], 128 + signal.SIGTERM)
        (checks / 'release-build').touch()
        outputs = [self.finish_land(land) for i, land in enumerate(lands) if i != 2]
        # Three candidate builds and one bare-main check; withdrawn lane 2
        # is never built again during attribution.
        self.assertEqual(outputs[0].count('workspace build'), 4, outputs[0])
        self.assertIn('with 3 checks', outputs[0])
        self.assertEqual(self.main_log()[:4], ['lane 3', 'lane 1', 'lane 0', 'hold first build'])
        self.assertEqual(self.queued(), [])

    def test_a_conflict_in_the_middle_ticket_leaves_only_that_ticket(self):
        edits = {0: ('shared.txt', 'zero\n'), 1: ('shared.txt', 'one\n')}
        checks, lanes, lands, holder = self.batch_lanes(3, edits=edits)
        holder.unlink()
        outputs = [self.finish_land(land, 1 if i == 1 else 0) for i, land in enumerate(lands)]
        self.assertIn('conflicts in: shared.txt', outputs[1])
        self.assertEqual(outputs[0].count('pass  workspace build'), 1, outputs[0])
        self.assertEqual(self.main_log()[:3], ['lane 2', 'lane 0', 'record checks'])
        self.assertEqual(self.git(lanes[1], 'status', '--porcelain'), '')
        self.assertEqual(self.queued(), [])

    def test_a_conflict_with_a_red_ticket_ahead_stays_queued_and_lands_next(self):
        edits = {0: ('red.txt', 'red\n'), 1: ('red.txt', 'other\n')}
        checks, lanes, lands, holder = self.batch_lanes(3, edits=edits, red='fmt')
        holder.unlink()
        head = self.finish_land(lands[0], 1)
        self.assertIn('it stands if the batch ahead of it lands', head)
        self.assertIn('ticket 3 for lane1 stays queued', head)
        self.assertIn('refusing to push a red tree', head)
        # Lane 1 never conflicted with main: once the red head leaves, it lands by itself.
        self.assertNotIn('conflicts', self.finish_land(lands[1]))
        self.finish_land(lands[2])
        self.assertEqual(self.main_log()[:3], ['lane 1', 'lane 2', 'red fmt'])
        self.assertEqual(self.queued(), [])

    def test_a_dead_lanes_ticket_is_dropped_from_the_batch(self):
        checks, lanes, lands, holder = self.batch_lanes(3)
        lands[1][0].kill()
        lands[1][0].wait()
        holder.unlink()
        outputs = [self.finish_land(lands[i]) for i in (0, 2)]
        self.assertIn('dropped ticket 00000000000000000003 for lane1 from the batch: dead process', outputs[0])
        self.assertIn('batch of 2 tickets', outputs[0])
        self.assertEqual(self.main_log()[:3], ['lane 2', 'lane 0', 'record checks'])
        self.assertEqual(self.queued(), [])

    def test_a_batch_that_loses_a_push_race_is_checked_again_and_lands_whole(self):
        checks, lanes, lands, holder = self.batch_lanes(3)
        with (self.origin / 'hooks/pre-receive').open('a') as out:
            out.write("""
marker = root / 'race-0'
if not marker.exists():
    marker.touch()
    print('[rejected] simulated sibling won', file=sys.stderr)
    sys.exit(1)
""")
        holder.unlink()
        outputs = [self.finish_land(land) for land in lands]
        self.assertIn('main moved during the checks', outputs[0])
        self.assertEqual(outputs[0].count('pass  workspace build'), 2, outputs[0])
        self.assertEqual(outputs[0].count('batch of 3 tickets'), 2, outputs[0])
        self.assertEqual(self.main_log()[:3], ['lane 2', 'lane 1', 'lane 0'])
        for i in (1, 2):
            self.assertEqual(self.subjects(self.landed(outputs[i])), [f'lane {i}'])

    def batch_with_main_moves(self, moves, newcomer=False, retire=None, ledger_member=1):
        """Move the real remote after each green batch, immediately before its push."""
        (self.lane / 'tools/known-failures.tsv').write_text('base-row\n')
        (self.lane / '.gitattributes').write_text('tools/known-failures.tsv merge=hirsel\n')
        (self.lane / 'source.rs').write_text('fn original() {}\n')
        self.git(self.lane, 'add', '-A')
        self.git(self.lane, 'commit', '-qm', 'ledger and source')
        checks, lanes, lands, holder = self.batch_lanes(
            3, edits={ledger_member: ('tools/known-failures.tsv', 'base-row\nlane-row\n')},
            hold_build=newcomer)
        # This test permits inherited passes; the stage records below prove
        # exactly which trees ran checks. Other tests retain the strict hook.
        (self.origin / 'hooks/pre-receive').write_text('#!/bin/sh\nexit 0\n')
        received = self.origin / 'hooks/post-receive'
        received.write_text('''#!/usr/bin/env python3
import os, pathlib, sys
with (pathlib.Path(os.environ['FAKE_CHECKS']) / 'received').open('a') as out:
    for line in sys.stdin:
        out.write(line.split()[1] + '\\n')
''')
        received.chmod(0o755)
        hook = self.lane / '.git/hooks/pre-push'
        hook.write_text('''#!/usr/bin/env python3
import os, pathlib, subprocess
''' + 'moves = ' + repr(moves) + '''
root = pathlib.Path(os.environ['FAKE_CHECKS'])
env = {k: v for k, v in os.environ.items() if k not in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_INDEX_FILE')}
for i, commits in enumerate(moves):
    marker = root / ('race-' + str(i))
    if marker.exists():
        continue
    marker.touch()
    other = root / ('other-' + str(i))
    def git(*args):
        subprocess.run(['git', *args], cwd=other, env=env, check=True, capture_output=True)
    subprocess.run(['git', 'clone', '-q', os.environ['FAKE_ORIGIN'], str(other)], env=env, check=True)
    for path, text in commits:
        (other / path).write_text(text)
        git('add', path)
        git('commit', '-qm', 'main move')
    git('push', '-q', 'origin', 'HEAD:main')
    break
''')
        if retire is not None:
            with hook.open('a') as out:
                out.write('''import time
if not (root / 'release-retry').exists():
    (root / 'holding-retry').touch()

    while not (root / 'release-retry').exists():
        time.sleep(.02)
''')
        hook.chmod(0o755)
        holder.unlink()
        if newcomer:
            self.wait_for(lambda: (checks / 'holding-build').exists())
            lane = self.top / 'newcomer'
            self.git(self.lane, 'worktree', 'add', '-qb', 'newcomer', str(lane),
                     self.git(lanes[1], 'rev-parse', 'HEAD^').strip())
            (lane / '.buck2/bin').mkdir(parents=True)
            shutil.copy(self.lane / '.buck2/bin/buck2', lane / '.buck2/bin/buck2')
            (lane / 'newcomer.txt').write_text('newcomer\n')
            self.git(lane, 'add', 'newcomer.txt')
            self.git(lane, 'commit', '-qm', 'newcomer')
            lanes.append(lane)
            lands.append(self.start_land(lane))
            self.wait_for(lambda: len(self.queued()) == 4)
            (checks / 'release-build').touch()
        if retire is not None:
            self.wait_for(lambda: (checks / 'holding-retry').exists())
            lands[retire][0].kill()
            lands[retire][0].wait()
            (checks / 'release-retry').touch()
        outputs = {i: self.finish_land(land) for i, land in enumerate(lands) if i != retire}
        expected = (['newcomer'] if newcomer else []) + [f'lane {i}' for i in (2, 1, 0) if i != retire]
        self.assertEqual(self.main_log()[:len(expected)], expected)
        for i in (1, 2):
            if i != retire:
                self.assertEqual(self.subjects(self.landed(outputs[i])), [f'lane {i}'])
        self.assertEqual(self.queued(), [])
        return checks, outputs[0]

    def test_a_checked_prefix_can_reuse_checks_when_only_a_ledger_tail_left(self):
        checks, output = self.batch_with_main_moves([
            [('tools/known-failures.tsv', 'base-row\nmain-row\n')],
        ], retire=2, ledger_member=2)
        self.assertIn('reuses all passed checks', output)
        self.assertEqual(len(list(checks.glob('*/build'))), 1)

    def test_a_checked_prefix_rechecks_when_a_source_tail_left(self):
        checks, output = self.batch_with_main_moves([
            [('tools/known-failures.tsv', 'base-row\nmain-row\n')],
        ], retire=2)
        self.assertNotIn('reuses all passed checks', output)
        self.assertEqual(len(list(checks.glob('*/build'))), 2)

    def test_a_hole_in_the_checked_group_rechecks_even_when_only_ledger_rows_differ(self):
        checks, output = self.batch_with_main_moves([
            [('tools/known-failures.tsv', 'base-row\nmain-row\n')],
        ], retire=1)
        self.assertNotIn('reuses all passed checks', output)
        self.assertEqual(len(list(checks.glob('*/build'))), 2)

    def test_row_only_move_lands_the_checked_batch_while_a_newcomer_waits_for_its_own_checks(self):
        checks, output = self.batch_with_main_moves([
            [('tools/known-failures.tsv', 'base-row\nmain-row\n')],
        ], newcomer=True)
        self.assertEqual(output.count('pass  workspace build'), 1, output)
        self.assertIn('main moved only by known-failures rows', output)
        self.assertNotIn('pass  ', output[output.index('main moved during the checks'):], output)
        received = (checks / 'received').read_text().splitlines()
        # The ledger writer, the checked cohort, then the independently checked newcomer.
        self.assertEqual(len(received), 3, received)
        self.assertEqual(self.subjects(received), ['main move', 'lane 2', 'newcomer'])
        self.assertNotIn('newcomer', self.git(self.origin, 'log', '--format=%s', received[1]).splitlines())
        for stage in ('build', 'clippy'):
            self.assertEqual((checks / received[2] / stage).read_text(), received[1])
        self.assertEqual(len(list(checks.glob('*/build'))), 2)

    def test_row_only_main_moves_rebase_a_passed_batch_without_any_checks(self):
        checks, output = self.batch_with_main_moves([
            [('tools/known-failures.tsv', 'base-row\nmain-row-1\n')],
            [('tools/known-failures.tsv', 'base-row\nmain-row-1\nmain-row-2\n')],
        ])
        self.assertEqual(output.count('main moved only by known-failures rows'), 2, output)
        self.assertEqual(output.count('pass  workspace build'), 1, output)
        retry = output[output.index('main moved during the checks'):]
        self.assertNotIn('pass  ', retry, output)  # Includes prepare, sync, hot files and the fast screen.
        self.assertEqual(len(list(checks.glob('*/build'))), 1)
        self.assertEqual(set(self.git(self.origin, 'show', 'main:tools/known-failures.tsv').splitlines()),
                         {'base-row', 'lane-row', 'main-row-1', 'main-row-2'})
        tip = self.git(self.origin, 'rev-parse', 'main').strip()
        self.assertFalse((checks / tip).exists())

    def test_a_source_main_move_still_rechecks_the_batch(self):
        checks, output = self.batch_with_main_moves([[('source.rs', 'fn changed() {}\n')]])
        self.assertNotIn('reuses all passed checks', output)
        for stage in ('workspace build', 'Clippy', 'screen: formatting'):
            self.assertEqual(output.count('pass  ' + stage), 2, output)
        self.assertEqual(len(list(checks.glob('*/build'))), 2)

    def test_a_source_edit_and_revert_is_not_a_row_only_main_move(self):
        checks, output = self.batch_with_main_moves([[
            ('source.rs', 'fn changed() {}\n'),
            ('source.rs', 'fn original() {}\n'),
            ('tools/known-failures.tsv', 'base-row\nmain-row\n'),
        ]])
        self.assertNotIn('reuses all passed checks', output)
        self.assertEqual(output.count('pass  workspace build'), 2, output)
        self.assertEqual(len(list(checks.glob('*/build'))), 2)

    def test_an_older_lands_waiting_ticket_is_batched_and_later_finds_nothing_to_land(self):
        checks, lanes, lands, holder = self.batch_lanes(2, old=(1,))
        holder.unlink()
        output = self.finish_land(lands[0])
        self.assertIn('batch of 2 tickets', output)
        self.assertEqual(self.main_log()[:2], ['lane 1', 'lane 0'])
        # The older land never reads its result; when it runs again its rebase drops the landed commit.
        [old] = self.queued()
        self.assertEqual(json.loads(old.read_text())['result']['code'], 0)
        lands[1].kill()
        lands[1].wait()
        again = subprocess.run([sys.executable, '-c', DRIVER], cwd=lanes[1], env=self.env,
                               capture_output=True, text=True)
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertIn('nothing to land; HEAD is on origin/main', again.stdout)
        self.assertEqual(self.main_log()[:2], ['lane 1', 'lane 0'])
        self.assertEqual(self.queued(), [])

    def queued(self):
        return sorted((self.lane / '.git/land-queue').glob('[0-9]*'))

    def test_three_concurrent_lands_are_fifo_batch_the_waiters_and_check_exactly_their_push_base(self):
        checks, lanes = self.queue_lanes()
        first = self.start_land(lanes[0])
        self.wait_for(lambda: (checks / 'holding').exists())
        second = self.start_land(lanes[1])
        self.wait_for(lambda: len(self.queued()) == 2)
        third = self.start_land(lanes[2])
        self.wait_for(lambda: len(self.queued()) == 3)
        self.wait_for(lambda: 'queue position 3; head my-lane' in Path(third[1].name).read_text())
        waiting = self.queued()[1]
        before = json.loads(waiting.read_text())['heartbeat']
        self.wait_for(lambda: json.loads(waiting.read_text())['heartbeat'] > before)
        (checks / 'release').touch()
        outputs = [self.finish_land(proc) for proc in (first, second, third)]
        # The holder had no waiters when it started; the second lands the third with its own.
        for out, runs in zip(outputs, (1, 1, 0)):
            for stage in ('workspace build', 'Clippy'):
                self.assertEqual(out.count('pass  ' + stage), runs, out)
            self.assertNotIn('main moved during the checks', out)
        self.assertIn('queue position 2; head my-lane', outputs[1])
        self.assertIn('queue position 3; head my-lane', outputs[2])
        self.assertIn('batch of 2 tickets', outputs[1])
        received = (checks / 'received').read_text().splitlines()
        self.assertEqual(len(received), 2)
        self.assertEqual(self.main_log()[:3], ['lane 2', 'lane 1', 'lane 0'])
        self.assertEqual(self.landed(outputs[2]), [self.git(self.origin, 'rev-parse', 'main').strip()])
        self.assertEqual(self.queued(), [])

    def test_withdraw_checks_two_fake_tickets_and_refuses_a_reused_pid(self):
        queue = runpy.run_path(str(ROOT / 'tools/land'))['LandQueue']
        directory = self.lane / '.git/land-queue'
        directory.mkdir()
        holders = []
        for _ in range(2):
            proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
            holders.append(proc)
            def cleanup(proc=proc):
                if proc.poll() is None:
                    proc.terminate()
                proc.wait()
            self.addCleanup(cleanup)
        own, other = holders
        paths = [directory / f'{i:020d}' for i in (1, 2)]
        for path, proc, worktree in zip(paths, holders, (self.lane, self.top / 'another-lane')):
            # Even a reused lane name cannot override another checkout's path.
            path.write_text(json.dumps(dict(pid=proc.pid, process_start=queue.process(proc.pid),
                                            lane='my-lane', worktree=str(worktree))))
        untouched = paths[1].read_bytes()
        result = subprocess.run([sys.executable, 'tools/land', '--withdraw'], cwd=self.lane,
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'withdrew ticket {paths[0].name} pid {own.pid}', result.stdout)
        self.assertEqual(own.wait(), -signal.SIGTERM)
        self.assertIsNone(other.poll())
        self.assertEqual(paths[1].read_bytes(), untouched)
        # Reuse the surviving process's PID with an old identity in our ticket.
        paths[0].write_text(json.dumps(dict(pid=other.pid, process_start=['old boot', '0'],
                                           lane='my-lane', worktree=str(self.lane))))
        result = subprocess.run([sys.executable, 'tools/land', '--withdraw'], cwd=self.lane,
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('process start does not match', result.stdout)
        self.assertIn('nothing is queued', result.stdout)
        self.assertIsNone(other.poll())
        self.assertEqual(paths[1].read_bytes(), untouched)

    def test_withdraw_waiting_lane_releases_only_its_ticket(self):
        checks, lanes = self.queue_lanes()
        first = self.start_land(lanes[0])
        self.wait_for(lambda: (checks / 'holding').exists())
        second = self.start_land(lanes[1])
        self.wait_for(lambda: len(self.queued()) == 2)
        # Withdrawal must work even with local edits or a detached branch.
        self.git(lanes[1], 'checkout', '-q', '--detach')
        (lanes[1] / 'uncommitted.txt').touch()
        result = subprocess.run([sys.executable, 'tools/land', '--withdraw'], cwd=lanes[1],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f'pid {second[0].pid}', result.stdout)
        self.assertIn('queue ticket released', self.finish_land(second, 143))
        self.assertEqual(len(self.queued()), 1)
        self.assertIsNone(first[0].poll())
        (checks / 'release').touch()
        self.finish_land(first)
        self.assertEqual(self.main_log()[0], 'lane 0')
        self.assertEqual(self.queued(), [])

    def test_withdraw_running_lane_cancels_checks_and_restores_its_branch(self):
        checks, lanes = self.queue_lanes()
        first = self.start_land(lanes[0])
        self.wait_for(lambda: (checks / 'holding').exists())
        checker = int((checks / 'holding-pid').read_text())
        self.addCleanup(lambda: (checks / 'release').touch())
        result = subprocess.run([sys.executable, 'tools/land', '--withdraw'], cwd=lanes[0],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('queue ticket released', self.finish_land(first, 143))
        process = runpy.run_path(str(ROOT / 'tools/land'))['LandQueue'].process
        deadline = time.monotonic() + 5
        while process(checker) is not None and time.monotonic() < deadline:
            time.sleep(.02)
        self.assertIsNone(process(checker), 'withdrawal must stop the validation worker and its checker')
        self.assertEqual(self.queued(), [])
        self.assertEqual(self.git(lanes[0], 'branch', '--show-current').strip(), 'my-lane')
        self.assertEqual(self.main_log(), ['base'])
        self.assertEqual(self.git(lanes[0], 'status', '--porcelain'), '')
        result = subprocess.run([sys.executable, 'tools/land', '--withdraw'], cwd=lanes[0],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('nothing is queued', result.stdout)

    def test_killed_holder_is_reaped_and_the_next_lane_lands(self):
        checks, lanes = self.queue_lanes()
        first = self.start_land(lanes[0])
        self.wait_for(lambda: (checks / 'holding').exists())
        second = self.start_land(lanes[1])
        self.wait_for(lambda: len(self.queued()) == 2)
        first[0].kill()
        first[0].wait()
        # Let the fake checker child finish too; only the holder is killed.
        (checks / 'release').touch()
        output = self.finish_land(second)
        self.assertIn('reaped ticket', output)
        self.assertIn('my-lane', output)
        self.assertIn('dead process', output)
        self.assertEqual(self.main_log()[0], 'lane 1')
        self.assertEqual(self.queued(), [])

    def test_expired_live_holder_resumes_without_pushing(self):
        checks, lanes = self.queue_lanes()
        first = self.start_land(lanes[0])
        self.wait_for(lambda: (checks / 'holding').exists())
        ticket = self.queued()[0]
        before = json.loads(ticket.read_text())['heartbeat']
        self.wait_for(lambda: json.loads(ticket.read_text())['heartbeat'] > before)
        # Stop the process, including its heartbeat thread, while its checker
        # blocks. It is alive but has lost its 15-minute lease when it resumes.
        with (ticket.parent / 'lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            os.kill(first[0].pid, signal.SIGSTOP)
            # Hold the metadata lock until it is stopped, so its heartbeat
            # cannot race this edit or leave a stopped thread holding the lock.
            self.wait_for(lambda: Path(f'/proc/{first[0].pid}/stat').read_text().rsplit(')', 1)[1].split()[0] == 'T')
            record = json.loads(ticket.read_text())
            record['heartbeat'] = time.time() - 901
            ticket.write_text(json.dumps(record))
        second = self.start_land(lanes[1])
        output = self.finish_land(second)
        self.assertIn('heartbeat older than 15 minutes', output)
        (checks / 'release').touch()
        os.kill(first[0].pid, signal.SIGCONT)
        first[0].wait()
        first[1].seek(0)
        refused = first[1].read()
        self.assertEqual(first[0].returncode, 1, refused)
        self.assertIn('queue ownership lost; refusing to push', refused)
        self.assertEqual(len((checks / 'received').read_text().splitlines()), 1)
        self.assertEqual(self.main_log()[0], 'lane 1')
        self.assertEqual(self.queued(), [])

    def test_reexec_preserves_ticket_and_process_identity(self):
        other = self.top / 'other'
        self.git(self.top, 'clone', '-q', str(self.origin), str(other))
        with (other / 'tools/land').open('a') as out:
            out.write('\n# Updated land on main.\n')
        self.git(other, 'commit', '-qam', 'update land')
        self.git(other, 'push', '-q', 'origin', 'main')
        landed = self.land()
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertIn('rebased tree has another tools/land; running it', landed.stdout)
        self.assertEqual(landed.stdout.count('queued ticket 00000000000000000001'), 2)
        self.assertEqual((self.lane / '.git/land-queue/sequence').read_text().strip(), '1')
        self.assertEqual(self.queued(), [])

    def test_ticket_sequence_never_resets_when_the_queue_drains(self):
        for sequence in (1, 2):
            landed = self.land()
            self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
            self.assertIn(f'queued ticket {sequence:020d}', landed.stdout)
            self.assertEqual(self.queued(), [])

    def test_more_than_twenty_lost_races_recheck_all_stages_and_land(self):
        checks = self.recording_remote()
        # The fake remote declines twenty-two fully checked pushes as races.
        hook = self.origin / 'hooks/pre-receive'
        with hook.open('a') as out:
            out.write("""
for i in range(22):
    marker = root / ('race-' + str(i))
    if not marker.exists():
        marker.touch()
        print('[rejected] simulated sibling won', file=sys.stderr)
        sys.exit(1)
""")
        landed = self.land()
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        for stage in ('workspace build', 'Clippy'):
            self.assertEqual(landed.stdout.count('pass  ' + stage), 23)
        self.assertEqual(landed.stdout.count('main moved during the checks'), 22)
        self.assertNotIn('unchecked', landed.stdout)

    def test_a_red_retry_never_pushes_for_each_aggregate(self):
        for stage in ('build', 'clippy', 'fmt'):
            with self.subTest(stage=stage):
                checks = self.recording_remote() if stage == 'build' else self.top / 'checks'
                hook = self.origin / 'hooks/pre-receive'
                with hook.open('a') as out:
                    out.write("""
marker = root / 'race-0'
if not marker.exists():
    marker.touch()
    print('[rejected] simulated sibling won', file=sys.stderr)
    sys.exit(1)
""")
                (checks / 'race-0').unlink(missing_ok=True)
                before = self.git(self.origin, 'rev-parse', 'main')
                refused = self.land(FAKE_RED_AFTER_RACE=stage)
                self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
                self.assertIn('refusing to push a red tree', refused.stdout)
                self.assertEqual(self.git(self.origin, 'rev-parse', 'main'), before)

    def test_head_or_source_change_during_checks_refuses_the_push(self):
        for command in ("open('change.txt', 'a').write('edit')",
                        "subprocess.run(['git', 'commit', '--allow-empty', '-qm', 'moved'], check=True)"):
            with self.subTest(command=command):
                (self.lane / 'tools/buck2/fmt.py').write_text('import subprocess\n' + command + '\n')
                self.git(self.lane, 'add', '-A')
                self.git(self.lane, 'commit', '-qm', 'mutate during check')
                refused = self.land()
                self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
                self.assertIn('worktree changed during checks', refused.stdout)
                self.assertEqual(self.main_log(), ['base'])

    def test_changed_noncanonical_case_is_refused_by_the_real_format_screen(self):
        formatter = str(Path(os.environ['HIRSEL_TEST_CASE_FORMATTER']).resolve())
        (self.lane / 'tools/buck2/fmt.py').write_text((ROOT / 'tools/buck2/fmt.py').read_text())
        # Keep the other gates deterministic while running the actual formatter
        # checker on a real case, through the real merge-train screen.
        buck = self.lane / '.buck2/bin/buck2'
        buck.write_text("#!/usr/bin/env python3\nimport json, os, sys\nif '--output-attribute' in sys.argv:\n print(json.dumps({'root//:a': {'crate_root': 'source.rs', 'edition': '2024'}}))\nelif '--show-json-output' in sys.argv:\n print(json.dumps({'root//:hirsel-core-syntax-bin-hirsel-core-syntax': os.environ['FAKE_CASE_FORMATTER']}))\nelse:\n print('root//:a')\n")
        (self.lane / 'tools/buck2/prepare.py').write_text('')
        rustfmt = self.lane / 'tools/buck2/toolchains/rust-files/bin/rustfmt'
        rustfmt.parent.mkdir(parents=True)
        rustfmt.write_text('#!/bin/sh\nexit 0\n')
        rustfmt.chmod(0o755)
        case = self.lane / 'core/front/cases/not-canonical.hirsel'
        case.parent.mkdir(parents=True)
        case.write_text('fn f(){1}')
        self.git(self.lane, 'add', '.')
        self.git(self.lane, 'add', '-f', '.buck2/bin/buck2')
        self.git(self.lane, 'commit', '-qm', 'noncanonical changed case')
        refused = self.land(FAKE_CASE_FORMATTER=formatter)
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('FAIL  screen: formatting', refused.stdout, refused.stdout + refused.stderr)
        self.assertIn('core/front/cases/not-canonical.hirsel', refused.stdout)
        self.assertNotIn('pass  workspace build', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_legacy_linted_environment_cannot_bypass_checks(self):
        refused = self.land(HIRSEL_LAND_LINTED='1', HIRSEL_LAND_ATTEMPT='30', FAKE_FMT_RED='1')
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertNotIn('pass  workspace build', refused.stdout)
        self.assertNotIn('pass  Clippy', refused.stdout)
        self.assertIn('FAIL  screen: formatting', refused.stdout, refused.stdout + refused.stderr)
        self.assertEqual(self.main_log(), ['base'])

    def conflicting_lanes(self, path):
        # Land has two source commits to replay, both carry a generated edit.
        self.git(self.lane, 'checkout', '-q', 'main')
        target = self.lane / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text('base\n')
        self.git(self.lane, 'add', path)
        self.git(self.lane, 'commit', '-qm', 'common generated base')
        self.git(self.lane, 'checkout', '-q', 'my-lane')
        self.git(self.lane, 'rebase', 'main')
        for i in range(2):
            target.write_text(f'lane {i}\n')
            (self.lane / f'source-{i}').write_text('source change\n')
            self.git(self.lane, 'add', '-A')
            self.git(self.lane, 'commit', '-qm', f'change {i}')
        self.git(self.lane, 'checkout', '-q', 'main')
        target.write_text('main generator output\n')
        self.git(self.lane, 'add', path)
        self.git(self.lane, 'commit', '-qm', 'main generator')
        self.git(self.lane, 'push', '-q', 'origin', 'main')
        self.git(self.lane, 'checkout', '-q', 'my-lane')



    def test_generated_build_inventory_is_untracked_and_regenerated(self):
        self.conflicting_lanes('BUCK')
        (self.lane / 'tools/buck2/sync.py').write_text(
            "from pathlib import Path\nPath('BUCK').write_text('fresh inventory\\n')\n")
        with open(self.lane / '.gitignore', 'a') as ignore:
            ignore.write('BUCK\n')
        self.git(self.lane, 'add', 'tools/buck2/sync.py', '.gitignore')
        self.git(self.lane, 'commit', '-qm', 'generator')
        landed = self.land()
        self.assertEqual(landed.returncode, 0, landed.stdout + landed.stderr)
        self.assertEqual(self.git(self.origin, 'ls-tree', '--name-only', 'main', 'BUCK'), '')
        self.assertEqual(self.git(self.origin, 'ls-tree', '--name-only', 'main', 'BUCK'), '')

    def test_source_conflicts_still_refuse_the_push_and_abort_the_rebase(self):
        self.conflicting_lanes('source.rs')
        refused = self.land()
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('conflicts in: source.rs', refused.stdout)
        self.assertEqual(self.git(self.lane, 'branch', '--show-current').strip(), 'my-lane')

    def test_a_clippy_red_refuses_the_push(self):
        started = time.monotonic()
        refused = self.land(FAKE_CLIPPY_RED='1')
        self.assertLess(time.monotonic() - started, 30, refused.stdout + refused.stderr)
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('FAIL  Clippy', refused.stdout)
        self.assertIn('error[E0063]: missing field', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])

    def test_a_red_check_refuses_the_push_and_prints_its_first_error(self):
        refused = self.land(FAKE_FMT_RED='1')
        self.assertEqual(refused.returncode, 1, refused.stdout + refused.stderr)
        self.assertIn('FAIL  screen: formatting', refused.stdout, refused.stdout + refused.stderr)
        self.assertIn('Diff in crates/a/src/lib.rs', refused.stdout)
        self.assertNotIn('checking 3 files', refused.stdout)
        self.assertIn('refusing to push a red tree', refused.stdout)
        self.assertEqual(self.main_log(), ['base'])


class Units(unittest.TestCase):
    def test_unrelated_base_failure_is_not_attributed_to_the_candidate(self):
        land = runpy.run_path(str(ROOT / 'tools/land'))
        verify = land['Batch'].verify_base
        batch = SimpleNamespace(base_checks={}, queue=SimpleNamespace(
            green_tree=lambda base: False,
            record_red=lambda *args: self.fail('an unrelated base failure is not the same target')))
        candidate = ['FAIL  Core suites', 'failing targets: root//:candidate-target']
        bare_main = ['FAIL  Core suites', 'failing targets: root//:different-target']
        with patch.dict(verify.__globals__, candidate_check=lambda *args: bare_main):
            with self.assertRaisesRegex(land['Refusal'], 'main fails a different check'):
                verify(batch, 'base', candidate)

    def test_a_red_suite_report_names_each_failing_target_with_its_own_error(self):
        # The shape of `buck2 test` output on 2026-11-14: the failing target's section first, then a
        # passing target whose "104 passed" was the whole old excerpt (the last 40 lines of the run).
        stamp = '[2026-11-14T20:15:04.065+01:00] '
        failing = ['✗ Fail: root//:core-driver-sites-pool (1.7s)', '---- STDOUT ----', 'running 2 tests',
                   'test native::tests::failure_sites_cranelift_dev ... FAILED', '',
                   "thread 'native::tests::failure_sites_cranelift_dev' (23) panicked at src/sites.rs:50:5:",
                   'assertion `left == right` failed', '  left: 2', ' right: 0',
                   'test native::tests::failure_sites_llvm_dev ... ok', 'test result: FAILED. 1 passed; 1 failed',
                   '---- STDERR ----', '✗ Fail: root//:land-core-library-modes (3.0s)', '---- STDOUT ----',
                   '---- STDERR ----', 'FAIL: test_cache_identity (__main__.Modes.test_cache_identity)',
                   'Traceback (most recent call last):', '  File "gate.py", line 89, in test_cache_identity',
                   'AssertionError: False != True']
        passing = ['✓ Pass: root//:hirsel-core-driver-unittest (10.0s)', '---- STDOUT ----',
                   *(f'test tests::case_{n} ... ok' for n in range(60)), 'test result: ok. 104 passed; 0 failed']
        summary = ['Tests finished: Pass 1. Fail 2.', '2 TESTS FAILED', '  ✗ root//:core-driver-sites-pool',
                   '  ✗ root//:land-core-library-modes']
        text = '\n'.join([*(stamp + line for line in failing + passing), *summary])
        module = runpy.run_path(str(ROOT / 'tools/land'))
        with tempfile.TemporaryDirectory() as directory:
            script = Path(directory) / 'red.py'
            script.write_text(f'import sys\nsys.stderr.write({text!r})\nsys.exit(32)\n')
            with contextlib.redirect_stdout(io.StringIO()):
                report = '\n'.join(module['check']('Core suites', [sys.executable, str(script)]))
        self.assertIn('failing targets: root//:core-driver-sites-pool, root//:land-core-library-modes', report)
        self.assertIn('failing cases: native::tests::failure_sites_cranelift_dev\n', report)
        self.assertIn('right: 0', report)
        self.assertIn('failing cases: test_cache_identity', report)
        self.assertIn('AssertionError: False != True', report)
        self.assertNotIn('104 passed', report)
        self.assertNotIn(stamp, report)

    def test_land_uses_the_platforms_land_priority(self):
        module = runpy.run_path(str(ROOT / 'tools/land'))
        priority = runpy.run_path(str(ROOT / 'tools/buck2/priorities.bzl'))['LAND_PRIORITY']
        self.assertEqual(priority, 150)
        self.assertEqual([value for value in module['CONFIG'] if value.startswith('hirsel.priority=')],
                         [f'hirsel.priority={priority}'])

    def test_round_check_reports_each_complete_event_once_then_marks_it_seen(self):
        module = runpy.run_path(str(ROOT / 'tools/land_events.py'))
        append, report = module['append_event'], module['report_events']
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'events.tsv'
            record = dict(lane='owner', ticket='1', submitted='submitted')
            append(path, 'SUBMITTED', record, '/tmp/owner.log')
            append(path, 'LANDED', dict(record, landed='checked-sha'), '/tmp/owner.log', 0)
            first = io.StringIO()
            self.assertEqual(report(path, first), 2)
            self.assertIn('LAND SUBMITTED owner ticket 1', first.getvalue())
            self.assertIn('LAND LANDED owner ticket 1 EXIT 0 landed checked-sha', first.getvalue())
            second = io.StringIO()
            self.assertEqual(report(path, second), 0)
            self.assertEqual(second.getvalue(), '')
            append(path, 'FAILED', dict(record, ticket='2'), '/tmp/owner-b.log', 1)
            third = io.StringIO()
            self.assertEqual(report(path, third), 1)
            self.assertIn('LAND FAILED owner ticket 2 EXIT 1', third.getvalue())
            self.assertNotIn('LANDED', third.getvalue())
            # Old and new landers append to this same durable stream.
            with path.open('a', newline='') as output:
                csv.writer(output, delimiter='\t').writerow([
                    'LANDED', 'old', '3', '/tmp/old.log', '0', 'old-sha', 'old-submitted',
                    '2026-11-16T00:00:00Z'])
            cost = dict(target='core-front-cost', findings=['parse: 201 exceeds 100'])
            append(path, 'LANDED', dict(record, ticket='4', landed='new-sha', costs=[cost]), '/tmp/new.log', 0)
            mixed = io.StringIO()
            self.assertEqual(report(path, mixed), 2)
            self.assertIn('LAND LANDED old ticket 3 EXIT 0 landed old-sha log /tmp/old.log', mixed.getvalue())
            self.assertIn('LAND LANDED owner ticket 4 EXIT 0 landed new-sha log /tmp/new.log', mixed.getvalue())
            self.assertIn('parse: 201 exceeds 100', mixed.getvalue())
            with path.open(newline='') as source:
                rows = list(csv.reader(source, delimiter='\t'))
            self.assertEqual(rows[-1][:7], ['LANDED', 'owner', '4', '/tmp/new.log', '0', 'new-sha', 'submitted'])
            self.assertRegex(rows[-1][7], r'^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$')
            self.assertEqual(json.loads(rows[-1][8]), [cost])


    def test_a_live_ticket_must_still_name_the_checked_head(self):
        module = runpy.run_path(str(ROOT / 'tools/land'))
        queue_type = module['LandQueue']
        with tempfile.TemporaryDirectory() as directory:
            queue = queue_type.__new__(queue_type)
            queue.directory = Path(directory)
            record = dict(pid=os.getpid(), process_start=queue.process(os.getpid()),
                          heartbeat=time.time(), head='checked')
            member = module['Member']('1', 'lane', 'checked', record.copy())
            ticket = queue.directory / member.ticket
            for head, expected in (('checked', [member]), ('changed', []), (None, [member])):
                with self.subTest(head=head):
                    if head is None:
                        record.pop('head')  # Older waiting tools/land records have no head.
                    else:
                        record['head'] = head
                    ticket.write_text(json.dumps(record))
                    self.assertEqual(queue.live([member]), expected)

    def test_pid_reuse_is_dead_even_with_a_recent_heartbeat(self):
        queue = runpy.run_path(str(ROOT / 'tools/land'))['LandQueue']
        record = dict(pid=os.getpid(), process_start=('previous boot', '0'), heartbeat=time.time())
        self.assertEqual(queue.stale(queue, record), 'dead process')
        record['process_start'] = queue.process(os.getpid())
        self.assertIsNone(queue.stale(queue, record))

    def test_missing_or_malformed_commit_cannot_become_a_push_refspec(self):
        module = runpy.run_path(str(ROOT / 'tools/land'))
        resolve = module['revision']
        for status, value in ((1, ''), (0, ''), (0, 'HEAD')):
            with self.subTest(status=status, value=value):
                with patch.dict(resolve.__globals__, git=lambda *args: SimpleNamespace(
                        returncode=status, stdout=value, stderr='missing')):
                    with self.assertRaises(ValueError):
                        resolve('HEAD')

    def test_screened_tree_resynchronizes_inputs_before_its_build_queries(self):
        full = runpy.run_path(str(ROOT / 'tools/land'))['full']
        calls = []
        def check(name, command):
            calls.append(('check', command))
        def run(command):
            calls.append(('query', command))
            return SimpleNamespace(returncode=0, stdout='root//:a', stderr='')
        # The land tool's own suite joins exactly the lands that change the land tool.
        for changed, land_suite in (({'core/std/list.hirsel'}, False), ({'tools/land-run'}, True),
                                    ({'tools/tests/land.py'}, True)):
            calls.clear()
            with self.subTest(changed=changed), patch.dict(full.__globals__, check=check, run=run, unchanged=lambda tree: True,
                                                           changed_paths=lambda base, tree: changed):
                self.assertIsNone(full('tree', 'base', screened=True))
                self.assertEqual(calls[0], ('check', [sys.executable, 'tools/buck2/sync.py', '--if-stale']))
                self.assertEqual([kind for kind, _ in calls[:3]], ['check', 'query', 'query'])
                core = calls[-1][1]
                self.assertEqual(core[:3], [sys.executable, 'tools/buck2/event_logs.py', 'test'])
                self.assertIn('//:hirsel-cranelift-test-core_native', core)
                self.assertEqual('//:test-land' in core, land_suite)

    def test_empty_build_query_refuses_the_aggregate(self):
        full = runpy.run_path(str(ROOT / 'tools/land'))['full']
        with patch.dict(full.__globals__, check=lambda *args: None, unchanged=lambda tree: True,
                        run=lambda command: SimpleNamespace(returncode=0, stdout='', stderr='')):
            self.assertEqual(full('tree', 'base', screened=True)[0], 'FAIL  the build set query')

    def test_clippy_rejects_error_diagnostics(self):
        module = runpy.run_path(str(ROOT / 'tools/buck2/clippy.py'))
        main = module['main']
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / 'clippy.txt'
            report.write_text('error[E0063]: missing field\n')
            built = SimpleNamespace(returncode=0, stdout=json.dumps({'root//:a-clippy[clippy.txt]': str(report)}), stderr='')
            with patch.object(sys, 'argv', ['clippy.py']), patch('subprocess.run') as query, \
                    patch.object(main.__globals__['event_logs'], 'run', return_value=built):
                query.return_value = SimpleNamespace(returncode=0, stdout='root//:a-clippy')
                self.assertEqual(main(), 1)

    def test_empty_clippy_or_formatting_query_is_never_green(self):
        for helper, output in (('clippy', ''), ('fmt', '{}')):
            with self.subTest(helper=helper):
                module = runpy.run_path(str(ROOT / f'tools/buck2/{helper}.py'))
                with patch.object(sys, 'argv', [helper]), patch('subprocess.run') as query:
                    query.return_value = SimpleNamespace(returncode=0, stdout=output)
                    with self.assertRaises(SystemExit):
                        module['main']()


class CoreScriptArguments(unittest.TestCase):
    def test_core_scripts_accept_land_serial_argument(self):
        land = runpy.run_path(str(ROOT / 'tools/land'))
        registry = ast.parse((ROOT / 'tools/buck2/sync.py').read_text())
        scripts = set()
        for call in ast.walk(registry):
            if not (isinstance(call, ast.Call) and isinstance(call.func, ast.Name)
                    and call.func.id == 'call' and call.args
                    and isinstance(call.args[0], ast.Constant) and call.args[0].value == 'hirsel_script_test'):
                continue
            fields = {kw.arg: kw.value for kw in call.keywords}
            name = fields['name']
            # Include every loop-generated script target as well as the named Core targets.
            if not isinstance(name, ast.Constant) or '//:' + name.value in land['CORE_ACCEPTANCE']:
                scripts.add(ast.literal_eval(fields['script']))
        self.assertIn('core/tools/recursive_instances.py', scripts)
        for script in sorted(scripts):
            with self.subTest(script=script):
                source = ast.parse((ROOT / script).read_text())
                registrations = [call for call in ast.walk(source)
                                 if isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)
                                 and call.func.attr == 'add_argument' and call.args
                                 and isinstance(call.args[0], ast.Constant) and call.args[0].value == '--test-threads']
                self.assertEqual(len(registrations), 1, script + ' must register land\'s --test-threads argument')
                registration = registrations[0]
                registration.func.value = ast.Name(id='parser', ctx=ast.Load())
                parser = argparse.ArgumentParser()
                exec(compile(ast.fix_missing_locations(ast.Module(body=[ast.Expr(registration)], type_ignores=[])),
                             script, 'exec'), dict(parser=parser, argparse=argparse))
                self.assertEqual(parser.parse_args(['--test-threads', '1']).test_threads, 1)


class CoreCostGuard(unittest.TestCase):
    def test_cost_findings_are_visible_at_land_time_and_unrelated_lands_skip(self):
        report = runpy.run_path(str(ROOT / 'tools/land'))['core_cost_report']
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ceiling = root / 'crates/hirsel-core-front/cost-ceilings.json'
            ceiling.parent.mkdir(parents=True)
            ceiling.write_text('{}')
            messages, commands = [], []
            def run(command):
                commands.append(command)
                return subprocess.CompletedProcess(command, 0,
                    'COST_REPORT {"target":"core-front-cost","findings":["parse: 201 exceeds 100"],"ceilings":{"m1":{"instructions_per_token":{"parse":205,"index":50,"resolve":30},"image_per_token":{"bytes_read":1.02}}}}\n', '')
            with patch.dict(report.__globals__, ROOT=root, run=run, say=messages.append,
                            changed_paths=lambda *_: {'crates/hirsel-core-front/src/body/mod.rs'}):
                rows = report('base', 'tree')
            self.assertIn('//:core-check-cost', commands[0])
            self.assertIn('//:core-front-cost', commands[1])
            self.assertIn('--test-arg=--report-only', commands[0])
            self.assertEqual(rows[0]['findings'], ['parse: 201 exceeds 100'])
            commands.clear()
            with patch.dict(report.__globals__, ROOT=root, run=run, say=messages.append,
                            changed_paths=lambda *_: {'README.md'}):
                report('base', 'tree')
            self.assertEqual(commands, [])

if __name__ == '__main__':
    # Land's Core suites pass --test-threads=1 to every suite; these tests already run one at a time.
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    _, tests = parser.parse_known_args()
    unittest.main(argv=[__file__, *tests])
