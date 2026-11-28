#!/usr/bin/env python3
"""tools/capped leases slots while keeping each command in its own capped scope."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
CAPPED = ROOT / 'tools/capped'
loader = importlib.machinery.SourceFileLoader('capped', str(CAPPED))
capped = importlib.util.module_from_spec(importlib.util.spec_from_loader('capped', loader))
loader.exec_module(capped)
# Records the scope's properties, then runs the command as systemd-run would.
FAKE_SYSTEMD_RUN = r'''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
split = args.index('--')
if os.environ.get('CAPPED_NO_BUS'):
    sys.exit('Failed to connect to bus: No medium found')
with open(os.environ['CAPPED_CALLS'], 'a') as calls:
    print(json.dumps(args[:split]), file=calls)
os.execvp(args[split + 1], args[split + 1:])
'''


class Capped(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        (root / 'bin').mkdir()
        fake = root / 'bin/systemd-run'
        fake.write_text(FAKE_SYSTEMD_RUN)
        fake.chmod(0o755)
        self.calls = root / 'calls.jsonl'
        self.env = {key: value for key, value in os.environ.items()
                    if key not in ('HIRSEL_MEMORY_LIMIT', 'HIRSEL_JOBS', 'HIRSEL_CAPPED_SLOTS')}
        self.env |= {'PATH': f'{root}/bin:{os.environ["PATH"]}', 'CAPPED_CALLS': str(self.calls),
                     'XDG_RUNTIME_DIR': str(root), 'HIRSEL_CAPPED_SLOTS': '2'}
        self.root = root

    def tearDown(self):
        self.directory.cleanup()

    def capped(self, *command, env=None):
        return subprocess.run([str(CAPPED), *command], env=self.env | (env or {}),
                              capture_output=True, text=True)

    def properties(self):
        # The scope probe (`systemd-run ... -- true`) sets no properties.
        [call] = [c for c in map(json.loads, self.calls.read_text().splitlines()) if '-p' in c]
        return [call[i + 1] for i, a in enumerate(call) if a == '-p']

    def test_defaults_cap_memory_swap_and_cpus_and_pass_them_on(self):
        result = self.capped('sh', '-c', 'echo "$HIRSEL_MEMORY_LIMIT $HIRSEL_JOBS"; exit 3')
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(result.stdout, '4G 2\n')
        self.assertEqual(self.properties(),
                         ['MemoryMax=4G', 'MemorySwapMax=0', 'OOMPolicy=continue', 'CPUQuota=200%'])
        self.assertRegex(result.stderr, r'capped: peak .* of 4G \(systemd scope\), [0-9.]+ s, exit 3\n')

    def test_stats_come_from_the_harness_without_a_time_child(self):
        stats = self.root / 'usage.json'
        result = self.capped('--stats', str(stats), 'true')
        self.assertEqual(result.returncode, 0, result.stderr)
        usage = json.loads(stats.read_text())
        self.assertGreater(usage['seconds'], 0)
        self.assertGreater(usage['max_rss_kib'], 0)

    def test_the_environment_sets_the_cap_and_the_quota(self):
        result = self.capped('true', env={'HIRSEL_MEMORY_LIMIT': '512M', 'HIRSEL_JOBS': '3'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.properties(),
                         ['MemoryMax=512M', 'MemorySwapMax=0', 'OOMPolicy=continue', 'CPUQuota=300%'])

    def test_parallel_launch_refuses_under_quota_and_runs_unchanged_at_width(self):
        program = self.root / 'launch'
        program.write_text('#!/bin/sh\nprintf "%s\\n" "$HIRSEL_JOBS" "$@"\n')
        program.chmod(0o755)
        record = self.root / 'run.json'
        record.write_text(json.dumps({'placement': {'cores': 4}}))
        for flags in (['--cores', '4'], ['--cores=4'], ['--hirsel-grants', str(record)],
                      ['--run', str(record)]):
            with self.subTest(flags=flags):
                refused = self.capped(str(program), *flags, env={'HIRSEL_JOBS': '2'})
                self.assertEqual(refused.returncode, 2, refused.stderr)
                self.assertIn('raise HIRSEL_JOBS to 4', refused.stderr)
                self.assertEqual(refused.stdout, '')
                accepted = self.capped(str(program), *flags, env={'HIRSEL_JOBS': '4'})
                self.assertEqual(accepted.returncode, 0, accepted.stderr)
                self.assertEqual(accepted.stdout.splitlines(), ['4', *flags])
        refused = self.capped('env', 'HIRSEL_JOBS=4', str(program))
        self.assertEqual(refused.returncode, 2, refused.stderr)
        self.assertIn('raise HIRSEL_JOBS to 4', refused.stderr)

    def test_default_quota_keeps_a_serial_command_runnable_under_one_inherited_cpu(self):
        allowed = os.sched_getaffinity(0) - set(range(12, 16)) - set(range(28, 32))
        if not allowed:
            self.skipTest('requires a non-timing CPU')
        result = subprocess.run(['taskset', '-c', str(min(allowed)), str(CAPPED),
                                 'sh', '-c', 'printf %s "$HIRSEL_JOBS"'],
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '1')
        self.assertEqual(self.properties(),
                         ['MemoryMax=4G', 'MemorySwapMax=0', 'OOMPolicy=continue', 'CPUQuota=100%'])

    def test_inside_also_refuses_a_pool_wider_than_inherited_affinity(self):
        with patch.object(capped.os, 'sched_getaffinity', return_value={0, 1}), \
                patch.object(capped, 'own_cgroup', return_value=None), \
                patch.dict(os.environ, {'HIRSEL_JOBS': '4'}), \
                patch.object(capped.sys, 'argv', [str(CAPPED), '--inside', 'true']), \
                patch.object(capped, 'inside') as inside, \
                contextlib.redirect_stderr(io.StringIO()) as stderr:
            self.assertEqual(capped.main(), 2)
        inside.assert_not_called()
        self.assertIn('width 4 exceeds 2 granted CPUs', stderr.getvalue())

    def test_an_unavailable_python_executable_keeps_the_capped_scope(self):
        copied = self.root / 'tools/capped'
        copied.parent.mkdir()
        copied.write_text(CAPPED.read_text())
        shield = self.root / 'core/tools'
        shield.mkdir(parents=True)
        for module in ('timing_shield.py', 'bench_cpus.py'):
            (shield / module).write_text((ROOT / 'core/tools' / module).read_text())
        with patch.dict(os.environ, self.env, clear=True), \
                patch.object(capped, '__file__', str(copied)), \
                patch.object(capped.sys, 'argv', [str(CAPPED), 'true']), \
                patch.object(capped.sys, 'executable', ''):
            result = capped.main()
        self.assertEqual(result, 0)
        self.assertEqual(self.properties(),
                         ['MemoryMax=4G', 'MemorySwapMax=0', 'OOMPolicy=continue', 'CPUQuota=200%'])

    def test_without_a_user_manager_process_limits_apply(self):
        stats = self.root / 'usage.json'
        result = self.capped('--stats', str(stats), 'sh', '-c',
                             'ulimit -d; nproc; echo "$HIRSEL_MEMORY_LIMIT $HIRSEL_JOBS"',
                             env={'CAPPED_NO_BUS': '1', 'HIRSEL_MEMORY_LIMIT': '512M'})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f'{512 * 1024}\n{min(2, os.cpu_count())}\n512M 2\n')
        self.assertIn('using process limits', result.stderr)
        self.assertRegex(result.stderr, r'capped: peak .* of 512M \(process limits\), [0-9.]+ s, exit 0\n')
        self.assertIsNone(json.loads(stats.read_text())['memory_events'])

    def test_a_malformed_limit_or_job_count_refuses_before_running(self):
        for env in ({'HIRSEL_MEMORY_LIMIT': '4GB'}, {'HIRSEL_MEMORY_LIMIT': '0'}, {'HIRSEL_JOBS': '0'},
                    *({'HIRSEL_CAPPED_SLOTS': count} for count in ('0', '-1', '1.5', '', 'two'))):
            result = self.capped('true', env=env)
            self.assertEqual(result.returncode, 2, env)
            self.assertFalse(self.calls.exists(), env)


    def test_only_an_oom_kill_during_the_run_reports_the_cap(self):
        cgroup = Path(self.directory.name) / 'cgroup'
        cgroup.mkdir()
        events = cgroup / 'memory.events'
        stats = self.root / 'usage.json'
        (cgroup / 'memory.peak').write_text(str(5 << 20))
        start = {'high': 2, 'max': 4, 'oom': 2, 'oom_kill': 2}
        subsequent = [start, {'high': 2, 'max': 7, 'oom': 3, 'oom_kill': 3}]
        for end in subsequent:
            events.write_text(''.join(f'{name} {count}\n' for name, count in start.items()))
            readings = ''.join(f'{name} {count}\n' for name, count in end.items())
            command = [sys.executable, '-c', 'from pathlib import Path; import sys; '
                       'Path(sys.argv[1]).write_text(sys.argv[2])', str(events), readings]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), patch.object(capped, 'own_cgroup', return_value=cgroup):
                self.assertEqual(capped.inside('4G', command, stats=stats), 0)
            self.assertRegex(stderr.getvalue(), r'^capped: peak 5 MiB of 4G \(systemd scope\), [0-9.]+ s, exit 0')
            delta = {name: count - start[name] for name, count in end.items()}
            self.assertEqual(json.loads(stats.read_text())['memory_events'], delta)
            self.assertEqual('memory limit 4G reached' in stderr.getvalue(), bool(delta['oom_kill']))

    def test_two_runs_acquire_distinct_slots_and_a_third_waits(self):
        with capped.slot(self.root, 2, ['first', 'two words']):
            second = capped.slot(self.root, 2, ['second'])
            second.__enter__()
            self.addCleanup(second.__exit__, None, None, None)
            stderr = io.StringIO()
            # A simulated scheduler releases slot two on the first wait tick.
            with contextlib.redirect_stderr(stderr), patch.object(capped.time, 'sleep') as sleep:
                sleep.side_effect = lambda _: second.__exit__(None, None, None)
                with capped.slot(self.root, 2, ['third']):
                    self.assertEqual(sleep.call_count, 1)
            report = stderr.getvalue()
            self.assertRegex(report, r'waiting [0-9.]+ s for a short slot')
            self.assertIn(f'pid={os.getpid()}', report)
            self.assertIn(f'cwd={os.getcwd()!r}', report)
            self.assertIn("command=first 'two words'", report)
            self.assertIn('command=second age=', report)
            self.assertIn('.slot-2.lock after waiting', report)

    def test_one_short_run_starts_within_a_job_turn_while_another_checkout_queues_eight_long_runs(self):
        # Checkout A runs three rounds on all eight slots (alone it may fill
        # them); B then asks for one. B must get the first slot A frees, not
        # wait behind A's next rounds, which re-enter the moment a job ends.
        events, record = [], threading.Lock()
        full = threading.Barrier(9, timeout=20)

        def a(worker):
            for turn in range(3):
                with capped.slot(self.root, 8, [f'a{worker}.{turn}'], checkout='A'):
                    with record:
                        events.append(('A', turn))
                    if turn == 0:
                        full.wait()
                    time.sleep(0.3)

        def b():
            full.wait()
            with capped.slot(self.root, 8, ['b'], checkout='B'):
                with record:
                    events.append(('B', None))

        threads = [threading.Thread(target=a, args=(i,)) for i in range(8)] + [threading.Thread(target=b)]
        with contextlib.redirect_stderr(io.StringIO()):
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join(60)
        self.assertIn(('B', None), events)
        before = events[:events.index(('B', None))]
        self.assertEqual(before, [('A', 0)] * 8, events)

    def test_short_and_long_slots_are_independent_and_long_is_serial(self):
        with capped.slot(self.root, 2, ['short one']), capped.slot(self.root, 2, ['short two']):
            long = capped.slot(self.root, 2, ['long one'], long=True)
            long.__enter__()
            self.addCleanup(long.__exit__, None, None, None)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), patch.object(capped.time, 'sleep') as sleep:
                sleep.side_effect = lambda _: long.__exit__(None, None, None)
                with capped.slot(self.root, 8, ['long two'], long=True):
                    self.assertEqual(sleep.call_count, 1)
            self.assertIn('for a long slot', stderr.getvalue())
            self.assertIn("command='long one'", stderr.getvalue())
        with capped.slot(self.root, 2, ['long'], long=True):
            with capped.slot(self.root, 2, ['short']), patch.object(capped.time, 'sleep') as sleep:
                with capped.slot(self.root, 2, ['another short']):
                    sleep.assert_not_called()

    def test_a_crash_releases_the_slot_despite_stale_pid_metadata(self):
        script = '''
import runpy, sys
from pathlib import Path
slot = runpy.run_path(sys.argv[1])['slot']
with slot(Path(sys.argv[2]), 1, ['crashed']):
    print('locked', flush=True)
    sys.stdin.read()
'''
        with subprocess.Popen(['python3', '-c', script, str(CAPPED), str(self.root)],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as child:
            self.assertEqual(child.stdout.readline(), 'locked\n')
            child.kill()
            child.wait()
            path = self.root / f'hirsel-capped-{os.getuid()}.slot-1.lock'
            self.assertEqual(json.loads(path.read_text())['pid'], child.pid)
            with patch.object(capped.time, 'sleep') as sleep, capped.slot(self.root, 1, ['replacement']):
                sleep.assert_not_called()
                self.assertEqual(json.loads(path.read_text())['pid'], os.getpid())

    def test_exception_releases_slot_without_unlinking_it(self):
        with self.assertRaisesRegex(RuntimeError, 'failed'):
            with capped.slot(self.root, 1, ['fails']):
                raise RuntimeError('failed')
        [path] = self.root.glob('*.lock')
        inode = path.stat().st_ino
        with patch.object(capped.time, 'sleep') as sleep, capped.slot(self.root, 1, ['next']):
            sleep.assert_not_called()
            self.assertEqual(path.stat().st_ino, inode)



    def test_long_jobs_forward_long_to_the_scoped_runner(self):
        result = self.capped('--long', 'true')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.properties(),
                         ['MemoryMax=4G', 'MemorySwapMax=0', 'OOMPolicy=continue', 'CPUQuota=200%'])
        self.assertEqual([p.name for p in self.root.glob('*.lock')],
                         [f'hirsel-capped-{os.getuid()}.long.lock'])

    def test_long_uses_its_own_slot(self):
        result = self.capped('--long', 'true')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([p.name for p in self.root.glob('*.lock')],
                         [f'hirsel-capped-{os.getuid()}.long.lock'])

    def test_slot_count_uses_available_memory_at_start_and_clamps_default(self):
        with patch.object(capped, 'own_cgroup', return_value=None):
            for available, expected in ((0, 1), (1, 1), (3.75, 3), (20, 8)):
                with self.subTest(available=available), patch.object(Path, 'read_text') as read:
                    read.return_value = f'MemTotal: 999999999 kB\nMemAvailable: {int(available * 1024 * 1024)} kB\n'
                    self.assertEqual(capped.slot_count(1 << 30, {}), expected)
            with patch.object(Path, 'read_text', side_effect=OSError):
                self.assertEqual(capped.slot_count(1 << 30, {}), 1)

    def test_slot_override_is_not_clamped_or_dependent_on_meminfo(self):
        with patch.object(Path, 'read_text', side_effect=AssertionError('must not read meminfo')):
            for count in ('1', '12'):
                self.assertEqual(capped.slot_count(1 << 30, {'HIRSEL_CAPPED_SLOTS': count}), int(count))



if __name__ == '__main__':
    unittest.main()
