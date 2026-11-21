#!/usr/bin/env python3
"""Caller selection, explicit inputs and shard coverage at the launcher seam."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest
import sys
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
LAUNCHER = Path(os.environ.get('HIRSEL_TEST_LAUNCHER', ROOT / 'tools/buck2/test_launcher.sh')).resolve()
SCRATCH_OWNER = Path(os.environ.get('HIRSEL_SCRATCH_OWNER', ROOT / 'tools/buck2/scratch.py')).resolve()
CASES = {
    'pool::full_gate_heap': True,
    'host::full_gate_arm': True,
    'pool::full_gate_prefix': True,
    'pool::full_gate_prefix_host': True,
    'host::smoke': False,
    'pool::smoke': False,
    'pool::other': False,
}
# Emulate the external libtest CLI with argparse, independently of the shell
# selector. Real Buck2 --list probes also exercise the pinned Rust harness.
HARNESS = '''#!/usr/bin/env python3
import argparse, json, os, sys
parser = argparse.ArgumentParser()
for option in ('ignored', 'include-ignored', 'exact', 'list', 'nocapture', 'show-output', 'quiet', 'report-time'):
    parser.add_argument('--' + option, action='store_true')
parser.add_argument('--skip', action='append', default=[])
for option in ('format', 'color', 'test-threads', 'logfile', 'shuffle-seed'):
    parser.add_argument('--' + option)
parser.add_argument('-Z')
parser.add_argument('filters', nargs='*')
# libtest rejects a repeated exact flag, unlike argparse's store_true action.
options = sys.argv[1:sys.argv.index('--')] if '--' in sys.argv else sys.argv[1:]
if options.count('--exact') > 1:
    parser.error("Option 'exact' given more than once")
args = parser.parse_args()
cases = json.loads(os.environ['CASES'])
match = lambda name, pattern: name == pattern if args.exact else pattern in name
selected = [name for name, ignored in cases.items()
            if (not args.filters or any(match(name, f) for f in args.filters))
            and not any(match(name, f) for f in args.skip)
            and (not args.ignored or ignored)]
if args.list:
    for name in selected: print(name + ': test')
else:
    with open(os.environ['OBSERVED'], 'w') as output:
        json.dump(dict(argv=sys.argv[1:], bootstrap=os.environ.get('RUSTC_BOOTSTRAP'), threads=os.environ.get('RUST_TEST_THREADS'), jobs=os.environ.get('HIRSEL_JOBS'), llvm=os.environ.get('LLVM_SYS_231_PREFIX'), qemu=os.environ.get('HIRSEL_QEMU_AARCH64'), kit=os.environ.get('HIRSEL_LINK_KIT'), cases=[name for name in selected
                  if args.ignored or args.include_ignored or not cases[name]]), output)
    if args.format == 'json':
        print(json.dumps(dict(type='suite', event='started', test_count=len(selected))))
    for name in selected:
        status = 'ok' if args.ignored or args.include_ignored or not cases[name] else 'ignored'
        if args.format == 'json':
            print(json.dumps(dict(type='test', event=status, name=name)))
        else:
            print('test ' + name + ' ... ' + status)
'''


class TestExecutor(unittest.TestCase):
    def test_shared_bound_preserves_stock_runner_arguments(self):
        path = Path(os.environ.get('HIRSEL_TEST_EXECUTOR', ROOT / 'tools/buck2/test_executor.py'))
        namespace = {'__file__': str(path), '__name__': 'test_executor'}
        exec(compile(path.read_text(), str(path), 'exec'), namespace)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / '.buckconfig').write_text('[test]\ntimeout_default_s = 86400\n')
            actual = namespace['command']([
                '--executor-fd', '7', '--orchestrator-fd', '8',
                '--buck-trace-id', 'trace', '--config-entry', 'host=linux',
                '--config-entry', 'config=hirsel.tests=focus',
                '--', 'ignored', '--buck-test-info', 'ignored',
                '--env', 'FIXTURE=value', '--test-arg=--exact', '--test-arg=case',
            ], root)
        self.assertEqual(actual, [
            str(root / '.buck2/bin/buck2'), 'internal-test-runner',
            '--executor-fd', '7', '--orchestrator-fd', '8', '--', 'ignored',
            '--timeout', '86400', '--buck-test-info', 'ignored',
            '--env', 'FIXTURE=value', '--test-arg=--exact', '--test-arg=case',
        ])


class CpuRequests(unittest.TestCase):
    def platforms(self, mode, priority=None):
        source_dir = ROOT / 'tools/buck2'
        paths = {name: Path(os.environ.get('HIRSEL_TEST_' + name.upper(), source_dir / (name + '.bzl')))
                 for name in ('platforms', 'test_requirements', 'test_sizes', 'exec_sizes')}
        config = {'tests': mode, 'remote_properties': 'kiln_executor_runtime=fixture-runtime'}
        if priority is not None:
            config['priority'] = str(priority)
        def fail(message):
            raise ValueError(message)
        namespace = dict(read_root_config=lambda section, key, default: config.get(key, default),
                         fail=fail,
                         CommandExecutorConfig=lambda **kwargs: kwargs,
                         ConfigurationInfo=object(), rule=lambda **kwargs: None,
                         attrs=SimpleNamespace(**{name: lambda *args, **kwargs: None
                                                  for name in ('dict', 'string', 'dep', 'list')}))
        def load(label, *names):
            imported = {}
            exec(paths[label.removeprefix(':').removesuffix('.bzl')].read_text(), imported)
            namespace.update({name: imported[name] for name in names})
        namespace['load'] = load
        # This module uses the Python-compatible Starlark subset. Evaluate the
        # production request functions; Buck2 cquery also validates Starlark.
        exec(paths['platforms'].read_text(), namespace)
        return namespace

    def test_priority_bounds_reach_the_executors(self):
        for priority in (None, 0, 40, 50, 99):
            with self.subTest(priority=priority):
                module = self.platforms('pool', priority)
                expected = 50 if priority is None else priority
                self.assertEqual(module['test_executor'](1, 2097152, '')['priority'], expected)
                self.assertEqual(module['doctest_properties']()['priority'], expected)
                platforms = []
                module.update(ConfigurationInfo=lambda **kwargs: SimpleNamespace(**kwargs),
                              ExecutionPlatformInfo=lambda **kwargs: kwargs,
                              ExecutionPlatformRegistrationInfo=lambda **kwargs: platforms.extend(kwargs['platforms']),
                              DefaultInfo=lambda: None)
                class Budget:
                    label = SimpleNamespace(raw_target=lambda: 'pool')
                    def __getitem__(self, key):
                        return SimpleNamespace(constraints={})
                module['_platforms'](SimpleNamespace(attrs=SimpleNamespace(
                    constraints=[], budgets={'pool_1_1048576|1|1048576|': Budget()},
                    host=Budget())))
                self.assertEqual(platforms[0]['executor_config']['priority'], expected)
        for priority in (-1, 100, 150):
            with self.subTest(priority=priority), self.assertRaisesRegex(ValueError, '0 to 99'):
                self.platforms('pool', priority)


    def test_ordinary_targets_keep_measured_and_focus_defaults(self):
        for mode in ('pool', 'focus'):
            module = self.platforms(mode)
            # A per-case binary's undeclared cases keep its measured request.
            for binary in ('hirsel-rt-test-it', 'unknown-test', 'hirsel-cli-test-cli'):
                measured = module['TEST_REQUESTS'].get(binary, module['DEFAULT_TEST_REQUEST'])
                request = module['test_request'](binary)
                self.assertEqual(request, [1 if mode == 'focus' else measured[0], *measured[1:]])
                props = module['test_executor'](*request)['remote_execution_properties']
                self.assertNotIn('cpu_affinity', props)
                self.assertNotIn('exact_cpus', props)


class LauncherFixture(unittest.TestCase):
    """A fake harness under the launcher, as a test action runs it."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'llvm/bin').mkdir(parents=True)
        (self.root / 'llvm/lib').mkdir()
        harness = self.root / 'harness'
        harness.write_text(HARNESS)
        harness.chmod(0o755)
        self.observed = self.root / 'observed.json'
        self.env = os.environ | dict(
            HIRSEL_TEST_TMPDIR=str(self.root / 'tmp'), HIRSEL_TEST_CWD=str(self.root),
            LLVM_SYS_231_PREFIX=str(self.root / 'llvm'), CASES=json.dumps(CASES),
            OBSERVED=str(self.observed), HIRSEL_TEST_BINARY='fixture-test-it',
            HIRSEL_SCRATCH_OWNER=str(SCRATCH_OWNER), TMPDIR=str(self.root / 'scratch'),
            TEST_TARGET='root//:fixture-test-it')
        for key in ('HIRSEL_TEST_POOL', 'HIRSEL_TEST_CPUS', 'HIRSEL_TEST_EXACT_CPUS', 'TEST_SHARD_INDEX', 'TEST_TOTAL_SHARDS', 'HIRSEL_TEST_HOST_ONLY', 'HIRSEL_TEST_REPORT_ACTIVE', 'HIRSEL_TEST_REPORTER'):
            self.env.pop(key, None)


class PrivateNativeInputs(LauncherFixture):
    def helper(self):
        return Path(os.environ.get('HIRSEL_TEST_PRIVATE_NATIVE_INPUTS', ROOT / 'tools/buck2/private_native_inputs.py')).resolve()

    def source_files(self):
        library = self.root / 'llvm/lib/libshared.so'
        library.write_bytes(b'original immutable SDK bytes')
        library.chmod(0o444)
        (library.parent / 'alias.so').hardlink_to(library)
        (self.root / 'llvm/include').mkdir()
        (self.root / 'llvm/include/header').write_bytes(b'header')
        return library

    def test_private_inodes_survive_source_hardlink_churn_and_preserve_aliases(self):
        library = self.source_files()
        destination = self.root / 'private'
        copied = subprocess.run(['python3', str(self.helper()), str(self.root / 'llvm'), str(destination)], capture_output=True, text=True)
        self.assertEqual(copied.returncode, 0, copied.stderr)
        private = destination / 'lib/libshared.so'
        self.assertNotEqual(library.stat().st_ino, private.stat().st_ino)
        self.assertEqual(private.stat().st_ino, (destination / 'lib/alias.so').stat().st_ino)
        before = private.stat()
        for index in range(4):
            alias = self.root / f'other-materialization-{index}'
            alias.hardlink_to(library)
            self.assertEqual(private.stat().st_nlink, before.st_nlink)
            alias.unlink()
        after = private.stat()
        self.assertEqual((after.st_ino, after.st_ctime_ns, after.st_mtime_ns, after.st_mode),
                         (before.st_ino, before.st_ctime_ns, before.st_mtime_ns, before.st_mode))
        self.assertEqual(private.read_bytes(), b'original immutable SDK bytes')
        self.assertTrue((destination / 'include').is_symlink())
        self.assertEqual((destination / 'include/header').read_bytes(), b'header')

    def test_launcher_uses_private_sdk_for_bodies_and_removes_it_after_exit(self):
        self.source_files()
        environment = self.root / 'native-environment.tsv'
        environment.write_text('')
        self.env.update(HIRSEL_NATIVE_ENVIRONMENT=str(environment), HIRSEL_PRIVATE_NATIVE_INPUTS=str(self.helper()))
        result = subprocess.run(['sh', str(LAUNCHER), 'harness', '--exact', 'pool::smoke'], cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        observed = json.loads(self.observed.read_text())
        self.assertNotEqual(observed['llvm'], str(self.root / 'llvm'))
        self.assertFalse(Path(observed['llvm']).exists())
        self.assertTrue((self.root / 'llvm/lib/libshared.so').exists())

    def test_native_environment_exports_a_path_inside_each_named_toolchain(self):
        self.source_files()
        environment = self.root / 'native-environment.tsv'
        rows = [line for line in Path(os.environ.get('HIRSEL_TEST_NATIVE_ENVIRONMENT', ROOT / 'tools/buck2/native-toolchain/environment.tsv')).read_text().splitlines()
                if line.startswith(('HIRSEL_QEMU_AARCH64\t', 'HIRSEL_LINK_KIT\t'))]
        self.assertEqual(len(rows), 2)
        environment.write_text('\n'.join(rows) + '\n')
        for _, target, _ in (row.split('\t') for row in rows):
            package = self.root / target
            package.mkdir()
            self.env['HIRSEL_TOOLCHAIN_' + target.replace('-', '_').upper()] = str(package)
        self.env.update(HIRSEL_NATIVE_ENVIRONMENT=str(environment), HIRSEL_PRIVATE_NATIVE_INPUTS=str(self.helper()))
        result = subprocess.run(['sh', str(LAUNCHER), 'harness', '--exact', 'pool::smoke'], cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        observed = json.loads(self.observed.read_text())
        self.assertEqual(Path(observed['qemu']), (self.root / 'qemu').resolve() / 'qemu-aarch64')
        self.assertEqual(Path(observed['kit']).resolve(), (self.root / 'kit').resolve())

    def test_listing_uses_original_sdk_without_materialization(self):
        self.source_files()
        environment = self.root / 'native-environment.tsv'
        environment.write_text('')
        # An invalid helper would fail immediately if a listing tried to copy.
        self.env.update(HIRSEL_NATIVE_ENVIRONMENT=str(environment), HIRSEL_PRIVATE_NATIVE_INPUTS='/bin/false')
        result = subprocess.run(['sh', str(LAUNCHER), 'harness', '--list'], cwd=self.root, env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('pool::smoke: test', result.stdout)


class ScratchOwnership(LauncherFixture):
    def helper(self):
        import runpy
        return runpy.run_path(str(SCRATCH_OWNER))

    def test_sweep_keeps_live_owner_and_removes_dead_and_reused_owners(self):
        helper = self.helper()
        owner = f'{os.getpid()}-{helper["start_time"](os.getpid())}'
        parent = self.root / 'scratch'
        parent.mkdir()
        for prefix in helper['PREFIXES']:
            with self.subTest(prefix=prefix):
                live = parent / f'{prefix}live'
                dead = parent / f'{prefix}dead'
                reused = parent / f'{prefix}reused'
                retained = parent / f'{prefix}retained'
                recent = parent / f'{prefix}recent'
                foreign = parent / f'{prefix}unregistered'
                unregistered = parent / f'{prefix}4294967295-0-interrupted'
                for path, identity in ((live, owner), (dead, '4294967295-0'),
                                       (reused, f'{os.getpid()}-0'), (retained, '4294967295-0'),
                                       (recent, '4294967295-0')):
                    (path / '.hirsel-owners').mkdir(parents=True)
                    (path / '.hirsel-owners' / identity).touch()
                (retained / '.hirsel-keep').touch()
                unregistered.mkdir()
                foreign.mkdir()
                stale = time.time() - 2 * helper['STALE_SECONDS']
                for path in (live, dead, reused, retained, unregistered, foreign):
                    os.utime(path, (stale, stale))
                helper['sweep'](parent, prefix)
                self.assertTrue(live.is_dir())
                self.assertTrue(retained.is_dir())
                self.assertTrue(recent.is_dir())
                self.assertTrue(foreign.is_dir())
                self.assertFalse(dead.exists())
                self.assertFalse(reused.exists())
                self.assertFalse(unregistered.exists())

    def test_shared_scratch_lives_until_last_launcher_exits(self):
        probe = self.root / 'probe'
        probe.write_text("""#!/usr/bin/env python3
import os, sys
from pathlib import Path
scratch = Path('tmp').resolve()
(scratch / os.environ['TOKEN']).write_text('owned')
print(scratch, flush=True)
sys.stdin.readline()
assert (scratch / os.environ['TOKEN']).read_text() == 'owned'
""")
        probe.chmod(0o755)
        runs = []
        try:
            for token in ('first', 'second'):
                run = subprocess.Popen(['sh', str(LAUNCHER), 'probe'], cwd=self.root,
                                       env=self.env | {'TOKEN': token}, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                runs.append(run)
                path = Path(run.stdout.readline().strip())
                self.assertTrue((path / token).is_file())
            first_path = Path(os.readlink(self.root / 'tmp'))
            runs[0].stdin.write('done\n'); runs[0].stdin.flush()
            self.assertEqual(runs[0].wait(), 0, runs[0].stderr.read())
            self.assertTrue((first_path / 'second').is_file())
            runs[1].stdin.write('done\n'); runs[1].stdin.flush()
            self.assertEqual(runs[1].wait(), 0, runs[1].stderr.read())
            self.assertFalse(first_path.exists())
        finally:
            for run in runs:
                if run.poll() is None:
                    run.communicate('done\n')
                for pipe in (run.stdin, run.stdout, run.stderr):
                    pipe.close()

    def test_failure_after_native_creation_cleans_both_patterns_and_keep_retains(self):
        environment = self.root / 'native-environment.tsv'
        environment.write_text('')
        materializer = self.root / 'materialize.py'
        materializer.write_text("import sys\nfrom pathlib import Path\np=Path(sys.argv[2]); p.mkdir(); print(p)\n")
        harness = self.root / 'harness'
        harness.write_text("#!/bin/sh\nprintf '%s\\n' \"$LLVM_SYS_231_PREFIX\" \"$XDG_CACHE_HOME\"\nexit \"$FIXTURE_STATUS\"\n")
        harness.chmod(0o755)
        for status, keep in ((17, ''), (0, '1'), (17, '1')):
            with self.subTest(status=status, keep=keep):
                env = self.env | dict(HIRSEL_NATIVE_ENVIRONMENT=str(environment),
                                      HIRSEL_PRIVATE_NATIVE_INPUTS=str(materializer), HIRSEL_KEEP_TEST_SCRATCH=keep)
                env.pop('HIRSEL_TEST_BINARY', None)
                env['FIXTURE_STATUS'] = str(status)
                run = subprocess.run(['sh', str(LAUNCHER), 'harness'], cwd=self.root,
                                     env=env, capture_output=True, text=True)
                self.assertEqual(run.returncode, status, run.stderr)
                native, cache = map(Path, run.stdout.splitlines())
                scratch = Path(os.readlink(self.root / 'tmp'))
                retained = bool(status and keep)
                for path in (native, cache, scratch):
                    self.assertEqual(path.exists(), retained)
                self.assertEqual('Kept failed test scratch' in run.stderr, retained)


class LauncherReporting(LauncherFixture):
    def reporter(self):
        return Path(os.environ.get('HIRSEL_TEST_REPORTER', ROOT / 'tools/buck2/test_report.py')).resolve()

    def run_report(self, args=()):
        code = ("import runpy,sys; m=runpy.run_path(sys.argv[1]); "
                "sys.exit(m['supervise'](sys.argv[2:]))")
        env = self.env | {'HIRSEL_TEST_REPORT_ACTIVE': '1',
                          'HIRSEL_TEST_REPORTER': str(self.reporter())}
        return subprocess.run([sys.executable, '-c', code, str(self.reporter()),
                               'sh', str(LAUNCHER), 'harness', *args],
                              cwd=self.root, env=env, capture_output=True, text=True)

    def test_zero_body_pass_is_refused_and_names_target(self):
        self.env['TEST_TARGET'] = 'root//:fixture-test-it'
        for cases, args in [({}, ()), ({'host::unsupported': True}, ()),
                            ({'host::unsupported': True}, ('host::unsupported',)),
                            ({'pool::only': False}, ('--ignored',))]:
            with self.subTest(cases=cases, args=args):
                self.env['CASES'] = json.dumps(cases)
                result = self.run_report(args)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('root//:fixture-test-it', result.stderr)
                self.assertIn('executed zero test bodies', result.stderr)

    def test_filtered_ignored_only_body_is_not_an_empty_partition(self):
        self.env['CASES'] = json.dumps({'pool::ignored': True})
        statuses = []
        for index in ('0', '1'):
            self.env.update(TEST_TOTAL_SHARDS='2', TEST_SHARD_INDEX=index)
            result = self.run_report(('pool::ignored',))
            statuses.append(result.returncode)
            if result.returncode:
                self.assertRegex(result.stderr, 'executed zero test bodies|matched no test bodies')
        self.assertCountEqual(statuses, [2, 2])



    def fixture(self):
        # Same started/result JSON events emitted by pinned libtest. The Rust
        # fixture below additionally exercises the real formatter remotely.
        (self.root / 'harness').write_text(r'''#!/usr/bin/env python3
import json, os, signal, sys, time
from pathlib import Path
if '--list' in sys.argv:
    print('finished: test\nforever: test\nqueued: test'); sys.exit(0)
assert '--format=json' in sys.argv, sys.argv
for event in [dict(type='suite',event='started',test_count=3),
              dict(type='test',event='started',name='finished'),
              dict(type='test',event='ok',name='finished',exec_time=0.01),
              dict(type='test',event='started',name='forever')]:
    print(json.dumps(event), flush=True)
phase_dir = os.environ.get('HIRSEL_TEST_PHASE_DIR')
if phase_dir:
    # Completed preparation must remain distinct from an active replay, even
    # when the body captures all of its child process's standard output.
    now = time.time()
    events = [dict(timestamp=now - 0.2, id=0, thread='ThreadId(1)', event='start',
                   phase='compiler-request', detail='fixture.hir'),
              dict(timestamp=now - 0.1, id=0, thread='ThreadId(1)', event='end', phase='', detail=''),
              dict(timestamp=now, id=1, thread='ThreadId(1)', event='start',
                   phase='replay', detail='fixture/main')]
    Path(phase_dir, str(os.getpid()) + '.jsonl').write_text(
        ''.join(json.dumps(event) + '\n' for event in events))
signal.signal(signal.SIGTERM, signal.SIG_IGN)
print('fixture ready', flush=True)
while True: signal.pause()
''')





    def test_real_libtest_results(self):
        fixture = os.environ.get('HIRSEL_TEST_REPORT_FIXTURE')
        if not fixture:
            self.skipTest('real libtest fixture supplied by Buck2')
        import shutil
        shutil.copy2(Path(fixture).resolve(), self.root / 'harness')
        result = self.run_report(('--include-ignored',))
        self.assertEqual(result.returncode, 101, result.stderr)
        self.assertIn('test a_pass ... ok', result.stdout)
        self.assertIn('test b_fail ... FAILED', result.stdout)
        self.assertIn('fixture failure output', result.stdout)
        self.assertIn('test result: FAILED. 1 passed; 1 failed; 0 ignored;', result.stdout)
        result = self.run_report(('--exact', 'b_fail'))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('fixture-test-it executed zero test bodies', result.stderr)
    def test_executor_process_group_is_preserved_and_orphans_are_reaped(self):
        self.env['EXPECTED_PGID'] = str(os.getpgrp())
        (self.root / 'harness').write_text(r'''#!/usr/bin/env python3
import json, os, subprocess, sys
assert os.getpgrp() == int(os.environ['EXPECTED_PGID']), 'escaped executor sampling group'
print(json.dumps(dict(type='test', event='started', name='orphan')), flush=True)
child = subprocess.Popen([sys.executable, '-c', 'import signal; signal.pause()'],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
with open('orphan.pid', 'w') as output: output.write(str(child.pid))
print(json.dumps(dict(type='test', event='ok', name='orphan')), flush=True)
''')
        result = self.run_report()
        self.assertEqual(result.returncode, 0, result.stderr)
        pid = int((self.root / 'orphan.pid').read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def test_sigterm_flushes_running_names(self):
        self.fixture()
        import selectors
        import signal
        env = self.env | {'HIRSEL_TEST_REPORTER': str(self.reporter())}
        with subprocess.Popen(['sh', str(LAUNCHER), 'harness'], cwd=self.root, env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as run:
            try:
                with selectors.DefaultSelector() as ready:
                    ready.register(run.stdout, selectors.EVENT_READ)
                    self.assertTrue(ready.select(), 'no started event')
                # Read one character at a time: TextIO's read-ahead would hide
                # later lines from select while waiting for the hanging name.
                output = ''
                while 'fixture ready\n' not in output:
                    output += run.stdout.read(1)
                run.send_signal(signal.SIGTERM)
                _, stderr = run.communicate()
                self.assertEqual(run.returncode, 143, stderr)
                self.assertIn('HIRSEL_TEST_RUNNING forever', stderr)
                self.assertIn('HIRSEL_TEST_FINISHED ok finished', stderr)
                self.assertNotIn('HIRSEL_TEST_RUNNING finished', stderr)
                self.assertNotIn('HIRSEL_TEST_RUNNING queued', stderr)
                records = [json.loads(line.split(' ', 1)[1]) for line in stderr.splitlines()
                           if line.startswith('HIRSEL_TEST_PHASE_')]
                prepared = next(row for row in records if row['phase'] == 'compiler-request')
                self.assertEqual(prepared['completed'], 1)
                self.assertAlmostEqual(prepared['inclusive_seconds'], 0.1, places=4)
                replay = next(row for row in records if row['phase'] == 'replay')
                self.assertEqual(replay['detail'], 'fixture/main')
                self.assertGreater(replay['elapsed_seconds'], 0)

            finally:
                if run.poll() is None:
                    run.kill()


class LauncherSelection(LauncherFixture):
    def test_a_stale_selector_fails_instead_of_running_zero_bodies(self):
        for args in [('missing_case',), ('--exact', 'pool::missing_case'),
                     ('--', 'missing_case'), ('--skip', 'pool::', '--skip', 'host::')]:
            for shard in ({}, *[dict(TEST_TOTAL_SHARDS='3', TEST_SHARD_INDEX=str(i)) for i in range(3)]):
                with self.subTest(args=args, shard=shard):
                    run = subprocess.run(['sh', str(LAUNCHER), 'harness', *args],
                                         cwd=self.root, env=self.env | shard,
                                         capture_output=True, text=True)
                    self.assertEqual(run.returncode, 2, run.stderr)
                    self.assertIn('matched no test bodies', run.stderr)
                    self.assertIn(args[-1], run.stderr)
                    self.assertFalse(self.observed.exists())



    def test_stale_dedicated_case_cannot_turn_into_an_empty_partition(self):
        self.env['HIRSEL_TEST_DEDICATED_CASE'] = 'removed_body'
        run = subprocess.run(['sh', str(LAUNCHER), 'harness'], cwd=self.root,
                             env=self.env, capture_output=True, text=True)
        self.assertEqual(run.returncode, 2, run.stderr)
        self.assertIn('removed_body', run.stderr)
        self.assertFalse(self.observed.exists())

    def test_empty_discovery_keeps_its_status(self):
        for cases in (CASES, {}):
            self.env['CASES'] = json.dumps(cases)
            run = subprocess.run(['sh', str(LAUNCHER), 'harness', '--list', 'missing_case'],
                                 cwd=self.root, env=self.env, capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(run.stdout, '')


    def test_dedicated_exact_target_accepts_an_exact_caller_selection(self):
        self.env['HIRSEL_TEST_DEDICATED_CASE'] = 'pool::smoke'
        observed, _ = self.run_cases('--exact', 'pool::smoke', '--exact', 'pool::smoke')
        self.assertEqual(observed['cases'], ['pool::smoke'])
        self.assertEqual(observed['argv'].count('--exact'), 1)

    def test_host_required_cases_never_run_on_a_pool_shard_even_with_positive_filters(self):
        before = self.run_cases()[0]['cases']
        self.env['HIRSEL_TEST_HOST_CASES'] = 'pool::smoke'
        after = self.run_cases()[0]['cases']
        self.assertCountEqual(after, [case for case in before if case != 'pool::smoke'])
        explicit = self.run_cases('pool::')[0]['cases']
        self.assertNotIn('pool::smoke', explicit)
        self.assertIn('pool::other', explicit)
        self.env['HIRSEL_TEST_DEDICATED_CASE'] = 'pool::smoke'
        dedicated, _ = self.run_cases('--exact', 'pool::smoke')
        self.assertEqual(dedicated['cases'], ['pool::smoke'])


    def launcher_with_quota(self, quota):
        # Keep the real launcher's quota logic, with deterministic kernel files.
        # A wide affinity mask does not imply a wide ancestor cpu.max quota.
        (self.root / 'cgroup').write_text('0::/child\n')
        (self.root / 'groups/child').mkdir(parents=True, exist_ok=True)
        (self.root / 'groups/child/cpu.max').write_text('max 100000\n')
        (self.root / 'groups/cpu.max').write_text(f'{quota} 100000\n')
        launcher = self.root / 'launcher.sh'
        launcher.write_text(LAUNCHER.read_text()
                            .replace('/proc/self/cgroup', str(self.root / 'cgroup'))
                            .replace('/sys/fs/cgroup', str(self.root / 'groups')))
        return launcher

    def run_cases(self, *args, shard='', expected_status=0):
        self.observed.unlink(missing_ok=True)
        env = self.env.copy()
        if shard:
            # `<i>/<n>` with i from 1, as Buck2 target names count shards;
            # the launcher reads Bazel's 0-based variables.
            index, total = shard.split('/')
            env |= {'TEST_SHARD_INDEX': str(int(index) - 1), 'TEST_TOTAL_SHARDS': total}
        run = subprocess.run(['sh', str(LAUNCHER), 'harness', *args], cwd=self.root,
                             env=env, capture_output=True, text=True)
        self.assertEqual(run.returncode, expected_status, run.stderr)
        result = json.loads(self.observed.read_text()) if self.observed.exists() else {'cases': [], 'argv': []}
        return result, run.stdout + run.stderr

    def test_source_only_bodies_run_without_an_llvm_input(self):
        self.env.pop('LLVM_SYS_231_PREFIX', None)
        result, _ = self.run_cases('host::smoke', '--exact')
        self.assertEqual(result['cases'], ['host::smoke'])
        self.assertIsNone(result['llvm'])

    def test_host_scratch_lives_outside_the_project_and_survives_concurrent_links(self):
        # Buck2 stops at a non-UTF-8 name in the project tree; native tests make one.
        scratch = self.root / 'host-tmp'
        self.env['TMPDIR'] = str(scratch)
        (self.root / 'tmp').mkdir()
        (self.root / 'tmp/stale').write_text('from a run before the link')
        runs = [subprocess.Popen(['sh', str(LAUNCHER), 'harness', '--list'], cwd=self.root,
                                 env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                for _ in range(8)]
        for run in runs:
            _, stderr = run.communicate()
            self.assertEqual(run.returncode, 0, stderr)
        link = self.root / 'tmp'
        self.assertTrue(link.is_symlink())
        target = Path(os.readlink(link))
        self.assertEqual(target.parent, scratch)
        self.assertTrue(target.name.startswith('hirsel-test-'))
        self.assertFalse(target.exists(), 'the last concurrent launcher removes shared scratch')

    def test_two_cpu_reservation_bounds_threads_and_workers(self):
        self.env.update(HIRSEL_TEST_CPUS='2', RUST_TEST_THREADS='8', HIRSEL_JOBS='8')
        result, _ = self.run_cases()
        self.assertEqual(result['threads'], '2')
        self.assertEqual(result['jobs'], '2')




    def test_pool_without_a_reservation_fails_before_running_the_harness(self):
        run = subprocess.run(['sh', str(LAUNCHER), 'harness'], cwd=self.root,
                             env=self.env | {'HIRSEL_TEST_POOL': '1'}, capture_output=True, text=True)
        self.assertEqual(run.returncode, 2, run.stderr)
        self.assertIn('pool action requires HIRSEL_TEST_CPUS', run.stderr)
        self.assertFalse(self.observed.exists())

    def test_caller_can_choose_less_than_the_reservation(self):
        self.env.update(HIRSEL_TEST_CPUS='2', RUST_TEST_THREADS='1', HIRSEL_JOBS='1')
        result, _ = self.run_cases('--test-threads=1')
        self.assertEqual((result['threads'], result['jobs']), ('1', '1'))
        self.assertIn('1', result['argv'])

    def test_reservation_also_sets_defaults_without_caller_settings(self):
        self.env.update(HIRSEL_TEST_CPUS='2')
        for key in ('RUST_TEST_THREADS', 'HIRSEL_JOBS'):
            self.env.pop(key, None)
        result, _ = self.run_cases()
        self.assertEqual((result['threads'], result['jobs']), ('2', '2'))

    def test_thread_arguments_cannot_exceed_the_reservation(self):
        self.env.update(HIRSEL_TEST_CPUS='2')
        for args in [('--test-threads', '8'), ('--test-threads=8',)]:
            for shard in ['', '1/2']:
                result, _ = self.run_cases(*args, shard=shard)
                argv = result['argv']
                threads = (argv[argv.index('--test-threads') + 1] if '--test-threads' in argv
                           else next(a.split('=')[1] for a in argv if a.startswith('--test-threads=')))
                self.assertEqual(threads, '2')

    def test_flag_shaped_values_remain_values(self):
        self.env.pop('HIRSEL_TEST_BINARY')  # This argv probe is a script, not libtest.
        self.env['HIRSEL_TEST_CPUS'] = '2'
        probe = self.root / 'probe'
        probe.write_text('#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n')
        probe.chmod(0o755)
        for args in [('--logfile', '--test-threads=8'), ('--skip', '--test-threads=8'),
                     ('--', '--test-threads=8')]:
            run = subprocess.run(['sh', str(LAUNCHER), 'probe', *args], cwd=self.root,
                                 env=self.env, capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(json.loads(run.stdout), list(args))

    def test_every_native_case_runs_once_on_the_pool_even_under_exact(self):
        for case in CASES:
            actual = self.run_cases(case, '--exact', '--include-ignored')[0]['cases']
            self.assertEqual(actual, [case])




    def test_flags_and_values_do_not_select_cases(self):
        result, _ = self.run_cases('-Z', 'unstable-options', '--test-threads', '2',
                                   '--include-ignored')
        self.assertCountEqual(result['cases'], CASES)



    def test_values_and_spaces_survive_selection(self):
        args = ('--test-threads', '2', '--logfile', 'a path with spaces', '--color=never',
                '--format', 'pretty', '--shuffle-seed', '123', '-Z', 'unstable-options',
                'full_gate_', '--ignored')
        result, _ = self.run_cases(*args)
        for value in ['a path with spaces', '2', '123', 'unstable-options']:
            self.assertIn(value, result['argv'])
        self.assertEqual(set(result['cases']), {name for name in CASES if 'full_gate_' in name})


    def test_end_of_options_and_equals_skip_preserve_selection(self):
        result, _ = self.run_cases('--ignored', '--skip=prefix', '--', 'full_gate_')
        self.assertEqual(set(result['cases']), {'host::full_gate_arm', 'pool::full_gate_heap'})

    def test_unpartitioned_command_passes_original_argv(self):
        result, _ = self.run_cases('pool::other', '--exact')
        self.assertEqual(result['cases'], ['pool::other'])
        self.assertEqual(result['argv'], ['pool::other', '--exact'])


class LauncherMemory(LauncherFixture):
    def limits(self, limit):
        probe = self.root / 'probe'
        probe.write_text('#!/bin/sh\necho "$(ulimit -S -d) $(ulimit -H -d) $HIRSEL_MEMORY_LIMIT"\n')
        probe.chmod(0o755)
        env = self.env | ({'HIRSEL_MEMORY_LIMIT': limit} if limit else {})
        if not limit:
            env.pop('HIRSEL_MEMORY_LIMIT', None)
        run = subprocess.run(['sh', str(LAUNCHER), 'probe'], cwd=self.root, env=env,
                             capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)
        return run.stdout.split()

    def test_the_grant_caps_the_run_data_in_every_unit(self):
        hard = subprocess.run(['sh', '-c', 'ulimit -H -d'], capture_output=True, text=True).stdout.strip()
        # A soft limit: a sanitizer's child may lift it back to the hard one.
        self.assertEqual(self.limits('1048576K'), ['1048576', hard, '1048576K'])
        self.assertEqual(self.limits('512M'), ['524288', hard, '512M'])
        self.assertEqual(self.limits('2G'), ['2097152', hard, '2G'])


class LauncherCache(LauncherFixture):
    def launch_probe(self, body, *, env=None, args=()):
        probe = self.root / 'probe'
        probe.write_text('#!/usr/bin/env python3\n' + body)
        probe.chmod(0o755)
        return subprocess.run(['sh', str(LAUNCHER), 'probe', *args], cwd=self.root,
                              env=self.env if env is None else env,
                              capture_output=True, text=True)

    def test_unset_empty_and_unwritable_cache_variables_are_action_owned(self):
        shared = self.root / 'shared'
        shared.mkdir()
        blocked = self.root / 'blocked'
        blocked.write_text('not a directory')
        for value in (None, '', str(blocked), str(shared)):
            env = self.env.copy()
            for key in ('HOME', 'XDG_CACHE_HOME'):
                env.pop(key, None)
                if value is not None:
                    env[key] = value
            env['XDG_DATA_HOME'] = str(shared / 'data')
            run = self.launch_probe('''
import json, os, tempfile
from pathlib import Path
cache = Path(os.environ['XDG_CACHE_HOME'])
assert cache.is_absolute() and cache.is_dir()
assert cache.stat().st_mode & 0o777 == 0o700
for key in ('XDG_CACHE_HOME', 'XDG_DATA_HOME'):
    path = Path(os.environ[key]); path.mkdir(parents=True, exist_ok=True)
    (path / 'written').write_text('owned')
leaked = tempfile.mkdtemp(prefix='.tmp')
print(json.dumps(dict(cache=str(cache), data=os.environ['XDG_DATA_HOME'], leaked=leaked)))
''', env=env)
            self.assertEqual(run.returncode, 0, run.stderr)
            paths = json.loads(run.stdout)
            action = Path(paths['cache']).parent
            self.assertEqual(Path(paths['data']).parent, action)
            self.assertEqual(Path(paths['leaked']).parent.parent, action)
            self.assertFalse(action.exists())
            self.assertEqual(list(shared.iterdir()), [])
            self.assertEqual(blocked.read_text(), 'not a directory')

    def test_failure_and_filtered_empty_shard_clean_up_and_preserve_exit(self):
        for args, env in [((), {}), (('only-this',), dict(HIRSEL_TEST_DEDICATED_CASE='removed'))]:
            run = self.launch_probe("import os, sys\nif '--list' in sys.argv:\n    print('only-this: test'); sys.exit(0)\nif '' in sys.argv: sys.exit(0)\nprint(os.environ['XDG_CACHE_HOME'])\nsys.exit(17)\n",
                                    args=args, env=self.env | env)
            if not args:
                self.assertEqual(run.returncode, 17, run.stderr)
                self.assertFalse(Path(run.stdout.strip()).parent.exists())
            else:
                self.assertEqual(run.returncode, 2, run.stderr)
                self.assertIn('only-this', run.stderr)
            self.assertEqual(list(Path(self.env['TMPDIR']).iterdir()), [])

    def test_parallel_launchers_never_share_a_cache(self):
        probe = self.root / 'probe'
        probe.write_text('''#!/usr/bin/env python3
import os
from pathlib import Path
cache=Path(os.environ['XDG_CACHE_HOME'])
(cache / 'owner').write_text(str(os.getpid()))
print(cache)
''')
        probe.chmod(0o755)
        runs = [subprocess.Popen(['sh', str(LAUNCHER), 'probe'], cwd=self.root,
                                 env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                for _ in range(8)]
        caches = []
        for run in runs:
            stdout, stderr = run.communicate()
            self.assertEqual(run.returncode, 0, stderr)
            cache = Path(stdout.strip())
            caches.append(cache)
            self.assertFalse(cache.parent.exists())
        self.assertEqual(len(set(caches)), len(caches))


class LauncherBinaries(unittest.TestCase):
    """CARGO_BIN_EXE_<name> is compiled in under HIRSEL_TEST_BIN_EXE_DIR and
    linked by the launcher to the binary the run carries (tools/buck2/defs.bzl)."""

    def test_package_binaries_resolve_from_the_package_directory(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / 'llvm/bin').mkdir(parents=True)
            (root / 'llvm/lib').mkdir()
            (root / 'crates/pkg').mkdir(parents=True)
            binary = root / 'buck-out/art/__pkg-bin-tool__/tool'
            binary.parent.mkdir(parents=True)
            binary.write_text('#!/bin/sh\necho "tool ran $1"\n')
            binary.chmod(0o755)
            links = root / 'buck-out/tmp/cargo-bin-exe/pkg-test-it-harness'
            links.mkdir(parents=True)
            (links / 'tool').symlink_to(root / 'stale')
            harness = root / 'harness'
            harness.write_text('#!/bin/sh\nenv | grep "^HIRSEL_TEST_BIN" || true\n'
                               'exec ../../buck-out/tmp/cargo-bin-exe/pkg-test-it-harness/tool "$(pwd)"\n')
            harness.chmod(0o755)
            env = os.environ | dict(
                HIRSEL_TEST_TMPDIR='buck-out/tmp/cargo-target-tmpdir', HIRSEL_TEST_CWD='crates/pkg',
                HIRSEL_SCRATCH_OWNER=str(SCRATCH_OWNER),
                LLVM_SYS_231_PREFIX=str(root / 'llvm'),
                HIRSEL_TEST_BIN_EXE_DIR='buck-out/tmp/cargo-bin-exe/pkg-test-it-harness',
                HIRSEL_TEST_BIN_EXES='tool=buck-out/art/__pkg-bin-tool__/tool')
            for key in ('HIRSEL_TEST_POOL', 'TEST_SHARD_INDEX', 'TEST_TOTAL_SHARDS', 'HIRSEL_TEST_HOST_CASES', 'HIRSEL_TEST_BINARY'):
                env.pop(key, None)
            run = subprocess.run(['sh', str(LAUNCHER), 'harness'], cwd=root, env=env,
                                 capture_output=True, text=True)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual(run.stdout, f'tool ran {root / "crates/pkg"}\n')
            self.assertEqual(os.readlink(links / 'tool'), str(binary))
            self.assertEqual(sorted(p.name for p in links.iterdir()), ['tool'])


if __name__ == '__main__':
    unittest.main()
