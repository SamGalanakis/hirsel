#!/usr/bin/env python3
"""tools/compiler-bench: instruction regression verdicts, the scoreboard's
pairing, reading `hirsel --timings=json`, corpus determinism and edits, and the
budget tracking every interval and phase the compiler reports."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import socket

ROOT = Path(__file__).resolve().parents[2]
BENCH = ROOT / 'tools/compiler-bench'
sys.path.insert(0, str(BENCH))
import corpus  # noqa: E402


def script(name):
    loader = importlib.machinery.SourceFileLoader(name.replace('-', '_'), str(BENCH / name))
    module = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(module)
    return module


compare = script('compare')
run = script('run')
BUDGET = json.loads((BENCH / 'budget.json').read_text())


def cell(cid, walls, status='ok', phases=None, language='hirsel', mode='check', program='p', jobs=1, intervals=None):
    runs = [{'status': status, 'wall_s': w, 'cpu_s': w, 'rss_mib': 100.0, 'instructions_u': w * 1000000} for w in walls]
    summary = {m: run.summary([r[m] for r in runs]) for m in ('instructions_u', 'wall_s', 'cpu_s', 'rss_mib')}
    for part, values in (('phases', phases), ('intervals', intervals)):
        if values:
            summary[part] = {name: run.summary(v) for name, v in values.items()}
    return {'id': cid, 'program': program, 'language': language, 'mode': mode, 'jobs': jobs,
            'status': status, 'runs': runs, 'summary': summary}


def result(*cells):
    return {'contract': {'profile': 'release', 'workloads': ['fixture'], 'counter': 'instructions:u',
                         'toolchain': {'rustc': 'fixture'}},
            'schema': 1, 'suite': 'compiler-bench', 'cells': list(cells), 'pairs': run.pairs(list(cells))}


class Verdicts(unittest.TestCase):
    def verdict(self, before, after, metric='instructions_u'):
        rows, _ = compare.compare(result(before), result(after), BUDGET)
        return {r['metric']: r for r in rows}[metric]


    def test_one_percent_threshold_is_counted_without_a_time_noise_bound(self):
        self.assertFalse(self.verdict(cell('c', [1.0] * 3), cell('c', [1.009] * 3))['regressed'])
        self.assertTrue(self.verdict(cell('c', [1.0] * 3), cell('c', [1.011] * 3))['regressed'])

    def test_times_rss_and_phases_do_not_change_the_verdict(self):
        before = cell('c', [5.0] * 3, phases={'verify': [2.0] * 3}, intervals={'units': [2.0] * 3})
        after = cell('c', [50.0] * 3, phases={'verify': [30.0] * 3}, intervals={'units': [30.0] * 3})
        after['summary']['instructions_u'] = before['summary']['instructions_u']
        rows, _ = compare.compare(result(before), result(after), BUDGET)
        self.assertEqual([r['metric'] for r in rows], ['instructions_u'])
        self.assertFalse(rows[0]['regressed'])

    def test_legacy_baseline_without_counts_is_unusable(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / 'legacy.json'
            legacy = cell('c', [1.0])
            del legacy['summary']['instructions_u']
            path.write_text(json.dumps(result(legacy)))
            with self.assertRaisesRegex(ValueError, 'no instructions:u'):
                compare.load(path)

    def test_a_cell_that_stops_completing_regresses(self):
        rows, notes = compare.compare(result(cell('c', [1.0] * 3)), result(cell('c', [9.0], status='cap')), BUDGET)
        self.assertEqual([r['metric'] for r in rows if r['regressed']], ['status'])
        self.assertIn('c: status ok -> cap', notes)

    def test_incomplete_and_incompatible_runs_fail_closed(self):
        baseline = result(cell('c', [1.0]))
        cases = [result(), result(cell('c', [], status='skipped'))]
        for field in ('profile', 'workloads', 'counter', 'toolchain'):
            changed = result(cell('c', [1.0]))
            changed['contract'][field] = 'different'
            cases.append(changed)
        no_metric = result(cell('c', [1.0]))
        del no_metric['cells'][0]['summary']['instructions_u']
        cases.append(no_metric)
        with tempfile.TemporaryDirectory() as scratch:
            base, current = (Path(scratch) / n for n in ('base.json', 'current.json'))
            base.write_text(json.dumps(baseline))
            for changed in cases:
                with self.subTest(changed=changed):
                    current.write_text(json.dumps(changed))
                    self.assertNotEqual(subprocess.run(
                        [sys.executable, str(BENCH / 'compare'), str(base), str(current)],
                        capture_output=True).returncode, 0)
        with self.assertRaisesRegex(ValueError, 'zero compared metrics'):
            compare.compare(result(cell('c', [], status='skipped')),
                            result(cell('c', [], status='skipped')), BUDGET)
        with self.assertRaisesRegex(ValueError, 'required budget cell missing'):
            compare.compare(baseline, baseline, dict(BUDGET, cells=['required']))

    def test_exit_status_follows_the_verdict(self):
        with tempfile.TemporaryDirectory() as scratch:
            base, slow, same = (Path(scratch) / n for n in ('base.json', 'slow.json', 'same.json'))
            base.write_text(json.dumps(result(cell('c', [2.0] * 3))))
            slow.write_text(json.dumps(result(cell('c', [3.0] * 3))))
            same.write_text(json.dumps(result(cell('c', [2.02] * 3))))
            status = [subprocess.run([sys.executable, str(BENCH / 'compare'), str(base), str(other)],
                                     capture_output=True).returncode for other in (slow, same)]
            self.assertEqual(status, [1, 0])


class Timings(unittest.TestCase):
    REPORT = {'schema': 1, 'command': 'build', 'wall_ns': 712000000, 'untimed_ns': 9000000,
              'intervals': [{'interval': 'image open', 'wall_ns': 101000000}, {'interval': 'S1', 'wall_ns': 0}],
              'phases': [{'phase': 'parse', 'work_ns': 3000000, 'cpu_ns': 2900000, 'calls': 128},
                         {'phase': 'codegen', 'work_ns': 0, 'cpu_ns': 0, 'calls': 0}],
              'comptime_verify': {'work_ns': 0, 'cpu_ns': 0, 'calls': 0}, 'wait_ns': 0,
              'counts': {'units': 168}}

    def test_startup_metadata_after_the_report_does_not_hide_phases(self):
        stderr = json.dumps(self.REPORT) + '\ncompiler-startup: {"status":"ready"}\n'
        self.assertEqual(run.timings(stderr)['phases'], {'parse': .003})

    def test_the_last_stderr_line_is_the_report(self):
        stderr = 'warning: something\n' + json.dumps(self.REPORT) + '\n'
        found = run.timings(stderr)
        self.assertEqual(found['intervals'], {'image open': 0.101, 'S1': 0.0, '(untimed)': 0.009})
        self.assertEqual(found['phases'], {'parse': 0.003})
        self.assertEqual(found['counts'], {'units': 168})
        self.assertIsNone(run.timings('error: no such file\n'))
        self.assertIsNone(run.timings(json.dumps(dict(self.REPORT, schema=2))))

class Scoreboard(unittest.TestCase):
    def test_pairs_divide_hirsel_by_the_paired_rust_mode(self):
        cells = [cell('p/hirsel/clif', [6.0] * 3, mode='clif'),
                 cell('p/rust/debug/j1', [2.0] * 3, language='rust', mode='debug', jobs=1),
                 cell('p/rust/check/j1', [1.0] * 3, language='rust', mode='check', jobs=1)]
        [pair] = run.pairs(cells)
        self.assertEqual((pair['rust'], pair['ratio']), ('p/rust/debug/j1', 3.0))


class ResidentPlacement(unittest.TestCase):
    def test_selected_cpus_pin_server_and_client(self):
        resident = script('resident')
        with tempfile.TemporaryDirectory() as work:
            directory = Path(work)
            args = SimpleNamespace(toolchain=directory, server_cpu=2, client_cpu=3)
            def start(*a, **kw):
                (directory / 'server.log').write_bytes(b'serving test\n')
                return SimpleNamespace(poll=lambda: None, terminate=lambda: None, wait=lambda **kw: None)
            with patch.object(resident.subprocess, 'Popen', side_effect=start) as launched:
                server = resident.Server(args, directory, pinned=True)
            try:
                self.assertEqual(launched.call_args.args[0][:3], ['taskset', '-c', '2'])
                with patch.object(resident.subprocess, 'run', return_value=SimpleNamespace(returncode=0)) as client:
                    server.command(['check', 'fixture.hir'], daemon='off', perf=directory / 'perf')
                argv = client.call_args.args[0]
                at = argv.index('taskset')
                self.assertEqual(argv[at:at+3], ['taskset', '-c', '3'])
            finally:
                server.close()

    def test_campaign_forwards_placement_to_capped_session(self):
        resident = script('resident')
        with tempfile.TemporaryDirectory() as work:
            args = SimpleNamespace(mode='measure', select='^tasks$', work=Path(work),
                                   output=Path(work) / 'result.json', scale=(3000,), rounds=3,
                                   seed=42, toolchain=Path(work), native=False,
                                   allow_rejected_checks=False, server_cpu=2, client_cpu=3)
            with patch.object(resident.subprocess, 'run', return_value=SimpleNamespace(returncode=0)) as child:
                resident.campaign(args)
            argv = child.call_args.args[0]
            self.assertEqual(argv[argv.index('--server-cpu') + 1], '2')
            self.assertEqual(argv[argv.index('--client-cpu') + 1], '3')


class QuickMatrix(unittest.TestCase):

    def test_measurements_disable_the_native_program_cache(self):
        with tempfile.TemporaryDirectory() as scratch:
            runner = run.Runner.__new__(run.Runner)
            runner.work = Path(scratch)
            runner.base_env = {'HIRSEL_PROGRAM_CACHE': 'on'}
            runner.native_env = {'HIRSEL_PROGRAM_CACHE': 'on'}
            for native in (False, True):
                self.assertEqual(runner.env(native=native)['HIRSEL_PROGRAM_CACHE'], 'off')


class Corpus(unittest.TestCase):
    def tree(self, root):
        return {str(p.relative_to(root)): p.read_bytes() for p in sorted(root.rglob('*')) if p.is_file()}

    def test_generation_is_deterministic_and_pairs_share_modules(self):
        with tempfile.TemporaryDirectory() as one, tempfile.TemporaryDirectory() as two:
            first = corpus.corpus(Path(one), scale=(3000,))
            corpus.corpus(Path(two), scale=(3000,))
            self.assertEqual(self.tree(Path(one)), self.tree(Path(two)))
            names = {(p.name, p.language) for p in first}
            for p in first:
                if 'pairs' in p.groups:
                    self.assertIn((p.name, 'rust' if p.language == 'hirsel' else 'hirsel'), names, p.name)
            sources = json.loads((Path(one) / 'hirsel/syn3k/hirsel.json').read_text())['sources']
            hirsel = sorted(s['module'][0] for s in sources if s['module'])
            rust = sorted(p.stem for p in (Path(one) / 'rust/syn3k/src').glob('*.rs') if p.stem != 'main')
            self.assertEqual(hirsel, rust)


    def test_each_edit_changes_exactly_one_marked_text(self):
        with tempfile.TemporaryDirectory() as scratch:
            corpus.corpus(Path(scratch), scale=(10000,))
            for language, copy in (('hirsel', Path(scratch) / 'hirsel/syn10k'), ('rust', Path(scratch) / 'rust/syn10k')):
                path, base, variants = corpus.edits(copy, language)
                self.assertEqual(set(variants), set(run.EDITS))
                for text in variants.values():
                    self.assertNotEqual(text, base)
                    self.assertEqual(len(text.splitlines()), len(base.splitlines()))


class Matrix(unittest.TestCase):
    def test_pairing_keeps_cold_and_warm_separate(self):
        cells = []
        for language, mode, temp, cost in [('hirsel', 'release', 'cold', 9), ('hirsel', 'release', 'warm', 8),
                                            ('rust', 'release', 'cold', 3), ('rust', 'release', 'warm', 1)]:
            c = cell(f'p/{language}/{temp}', [cost], language=language, mode=mode)
            c['temperature'] = temp
            cells.append(c)
        self.assertEqual([p['ratio'] for p in run.pairs(cells)], [3, 8])

    def test_release_clean_removes_release_artifacts(self):
        runner = object.__new__(run.Runner)
        with patch.object(run.subprocess, 'run', return_value=SimpleNamespace(stdout='{"packages":[{"name":"p"}]}')) as command:
            runner.clean_rust(Path('/tmp/project'), {}, 'release')
            self.assertIn('--release', command.call_args.args[0])
            runner.clean_rust(Path('/tmp/project'), {}, 'check')
            self.assertNotIn('--release', command.call_args.args[0])

    def test_new_cold_cells_still_compare_against_the_existing_baseline(self):
        before = cell('p/hirsel/check', [1.0])
        after = cell('p/hirsel/check/cold', [2.0])
        rows, _ = compare.compare(result(before), result(after), BUDGET)
        self.assertTrue(rows[0]['regressed'])


    def test_native_selects_release_rust_and_std_precomputes_products(self):
        rust = corpus.Program('p', 'rust', Path('/tmp'), 3)
        std = corpus.Program('std@1', 'hirsel', Path('/tmp'), 3, check_only=True)
        self.assertEqual(run.modes(rust, ['native']), ['release'])
        self.assertEqual(run.modes(std, ['check', 'native']), ['precompute'])

    def test_cold_warm_are_adjacent_and_failure_does_not_drop_the_rust_pair(self):
        with tempfile.TemporaryDirectory() as work:
            args = SimpleNamespace(toolchain=work, work=work, groups=None, modes='check', select=None,
                                   jobs='1', repeat=2, seed=10, output=work+'/r.json', quick=False, checkpoint_identity={})
            with patch.object(run.subprocess, 'run', return_value=SimpleNamespace(stdout='--timings')):
                runner = run.Runner(args, {'base': {}, 'native': {}})
            seen = []
            def sample(p, mode, jobs, temperature, cache):
                seen.append((p.language, temperature))
                return {'status': 'failed' if p.language == 'hirsel' else 'ok', 'wall_s': 1}
            runner.sample = sample
            runner.untimed = lambda *a: None
            programs = [corpus.Program('p', lang, Path(work), 3) for lang in ('hirsel', 'rust')]
            cells = runner.go(programs)
            self.assertEqual(len(cells), 4)
            self.assertEqual(seen.count(('hirsel', 'cold')), 1)
            self.assertEqual(seen.count(('rust', 'cold')), 2)
            for i in range(0, len(seen), 2):
                self.assertEqual(seen[i][0], seen[i+1][0])
                self.assertEqual((seen[i][1], seen[i+1][1]), ('cold', 'warm'))

    def test_resume_keeps_finished_inputs_and_rounds_without_repeating_them(self):
        with tempfile.TemporaryDirectory() as work:
            args = SimpleNamespace(toolchain=work, work=work, groups=None, modes='check', select=None,
                                   jobs='1', repeat=2, seed=10, output=work+'/r.json', quick=False,
                                   checkpoint_identity={'fixture': True})
            def runner():
                with patch.object(run.subprocess, 'run', return_value=SimpleNamespace(stdout='--timings')):
                    r = run.Runner(args, {'base': {}, 'native': {}})
                r.untimed = lambda *a: None
                return r
            programs = [corpus.Program('done', 'hirsel', Path(work), 3),
                        corpus.Program('p', 'hirsel', Path(work), 3), corpus.Program('p', 'rust', Path(work), 3)]
            seen = []
            def first(p, mode, jobs, temperature, cache):
                if p.name == 'p' and sum(n == 'p' for n, _ in seen) == 4:
                    raise RuntimeError('interrupted after checkpoint')
                seen.append((p.name, temperature))
                return {'status': 'ok', 'wall_s': 1}
            before = runner()
            before.sample = first
            with self.assertRaisesRegex(RuntimeError, 'interrupted'):
                before.go(programs)
            args.checkpoint = json.loads(Path(args.output + '.partial').read_text())
            seen.clear()
            after = runner()
            def second(p, mode, jobs, temperature, cache):
                seen.append((p.name, temperature))
                return {'status': 'ok', 'wall_s': 1}
            after.sample = second
            cells = after.go(programs)
            self.assertEqual(len(seen), 4)
            self.assertTrue(all(name == 'p' for name, _ in seen))
            self.assertEqual(len(cells), 6)
            self.assertTrue(all(len(c['runs']) == 2 for c in cells))

    def test_round_lock_is_released_after_each_cold_warm_round(self):
        from contextlib import contextmanager
        with tempfile.TemporaryDirectory() as work:
            args = SimpleNamespace(toolchain=work, work=work, groups=None, modes='check', select=None,
                                   jobs='1', repeat=2, seed=10, output=work+'/r.json', quick=False,
                                   checkpoint_identity={}, lock_scope='round', cpu=31)
            with patch.object(run.subprocess, 'run', return_value=SimpleNamespace(stdout='--timings')):
                r = run.Runner(args, {'base': {}, 'native': {}})
            events = []
            @contextmanager
            def lock(cpu):
                events.append('enter')
                try:
                    yield
                finally:
                    events.append('exit')
            def sample(*a):
                self.assertEqual(events[-1], 'enter')
                return {'status': 'ok', 'wall_s': 1}
            r.sample = sample
            with patch.object(run, 'timing_lock', lock):
                r.go([corpus.Program('p', 'hirsel', Path(work), 3)])
            self.assertEqual(events, ['enter', 'exit', 'enter', 'exit'])


class Startup(unittest.TestCase):
    def probe(self, code, endpoint='stdout'):
        return subprocess.run([sys.executable, str(BENCH/'startup.py'), '--endpoint', endpoint,
                               '--', sys.executable, '-c', code], capture_output=True, text=True, timeout=5)

    def report(self, done):
        line = next(l for l in done.stderr.splitlines() if l.startswith('compiler-startup: '))
        return json.loads(line.removeprefix('compiler-startup: '))

    def test_first_output_precedes_process_completion(self):
        done = self.probe('import time; print("hello", flush=True); time.sleep(.3)')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertLess(self.report(done)['wall_s'], .25)
        self.assertEqual(self.report(done)['status'], 'ready')

    def test_silent_exit_is_not_reported_as_first_output(self):
        done = self.probe('pass')
        self.assertEqual(done.returncode, 0)
        self.assertEqual(self.report(done)['status'], 'no-output')
        self.assertIsNone(self.report(done)['wall_s'])

    def test_failure_is_preserved(self):
        self.assertEqual(self.probe('raise SystemExit(7)').returncode, 7)

    def test_a_listening_announcement_allows_graceful_shutdown(self):
        done = self.probe('import sys; print("listening on http://127.0.0.1:1/", flush=True); '
                          'assert sys.stdin.readline() == "quit\\n"', 'listening')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertFalse(self.report(done)['forced_stop'])

    def test_stopping_the_native_child_keeps_the_compiler_report(self):
        code = ('import subprocess, sys; '
                'p = subprocess.Popen([sys.executable, "-c", '
                '"import time; print(123, flush=True); time.sleep(30)"]); '
                'p.wait(); print("compiler phases", file=sys.stderr)')
        done = self.probe(code)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertIn('compiler phases', done.stderr)
        self.assertTrue(self.report(done)['forced_stop'])

    def test_output_followed_by_failure_is_not_an_accepted_startup(self):
        done = self.probe('print("ready", flush=True); raise SystemExit(7)')
        self.assertEqual(done.returncode, 7)
        self.assertEqual(self.report(done)['status'], 'ready-then-failed')

    def test_http_readiness_sends_a_valid_request_and_drains_the_response(self):
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            port = reservation.getsockname()[1]
        code = (f'import socket, time; s=socket.socket(); s.bind(("127.0.0.1", {port})); s.listen(); '
                'c,_=s.accept(); data=c.recv(4096); '
                'assert data.startswith(b"GET / HTTP/1.1\\r\\n"), data; '
                'c.sendall(b"HTTP/1.1 200 OK\\r\\nContent-Length: 2\\r\\nConnection: close\\r\\n\\r\\nOK"); '
                'c.close(); time.sleep(30)')
        done = self.probe(code, f'http:127.0.0.1:{port}')
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.report(done)['status'], 'ready')

    def test_a_preexisting_listener_is_refused(self):
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            listener.listen()
            done = self.probe('print("bad readiness")', f'tcp:127.0.0.1:{listener.getsockname()[1]}')
            self.assertNotEqual(done.returncode, 0)
            self.assertNotIn('compiler-startup:', done.stderr)

trend_loader = importlib.machinery.SourceFileLoader('cost_trend', str(ROOT / 'tools/cost-trend'))
trend = importlib.util.module_from_spec(importlib.util.spec_from_loader(trend_loader.name, trend_loader))
trend_loader.exec_module(trend)


class CostTrend(unittest.TestCase):
    def test_refusal_stops_that_workload_and_never_publishes_its_partial_median(self):
        calls = []
        def sample(toolchain, path, cpu, scratch, fresh):
            calls.append(path)
            if path == 'refused' and calls.count(path) == 2:
                return None, 'CPU cap reached'
            return {'instructions:u': 100, 'peak_rss_kib': 10, 'work/parse': 3}, ''
        observations, refused = trend.collect(Path('/unused'), 0,
                                              {'fresh-std': 'refused', 'csvjson': 'ok'}, sample)
        self.assertEqual((calls.count('refused'), calls.count('ok')), (2, 3))
        rows = trend.rows_for('commit', 'contract', observations, refused)
        bad = [r for r in rows if r['workload'] == 'fresh-std']
        self.assertTrue(bad)
        self.assertTrue(all(r['status'] == 'refused' and r['value'] == '' for r in bad))
        self.assertEqual([r['value'] for r in rows if r['metric'] == 'instructions:u' and r['status'] == 'ok'], [100])

    def test_trend_diff_never_crosses_contracts_or_hides_a_previous_refusal(self):
        observations = {'csvjson': [{'instructions:u': n} for n in (9, 10, 50)]}
        [row] = trend.rows_for('new', 'release', observations, {})
        self.assertEqual(row['value'], 10)
        prior = [dict(row, commit='old', value='5'),
                 dict(row, contract='checked', commit='different', value='100')]
        self.assertIn('+100.000%', trend.text_diff([row], prior)[0])
        self.assertIn('previous old', trend.text_diff([row], prior)[0])
        prior.append(dict(row, commit='refusal', value='', status='refused'))
        self.assertIn('first successful row', trend.text_diff([row], prior)[0])

    def test_missing_counters_or_signalled_compiler_are_refused_even_if_perf_exits_zero(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            def process(*args, **kwargs):
                (directory / 'perf').write_text('100;;instructions:u;1;100.00;\n200;;instructions;1;100.00;\n')
                (directory / 'status').write_text(json.dumps({'exit': -9, 'rss_kib': 100}))
                return SimpleNamespace(returncode=0)
            with patch.object(trend.subprocess, 'run', side_effect=process):
                values, reason = trend.measure(directory, Path('input.hir'), 0, directory, True)
            self.assertIsNone(values)
            self.assertIn('compiler -9', reason)
            def missing(*args, **kwargs):
                (directory / 'status').write_text(json.dumps({'exit': 0, 'rss_kib': 100}))
                return SimpleNamespace(returncode=0)
            with patch.object(trend.subprocess, 'run', side_effect=missing):
                values, reason = trend.measure(directory, Path('input.hir'), 0, directory, True)
            self.assertIsNone(values)
            self.assertIn('work counters missing', reason)


TIMINGS = ROOT / 'crates/hirsel-intern/src/timings.rs'


class Budget(unittest.TestCase):
    @unittest.skipUnless(TIMINGS.exists(), 'hirsel_intern::timings is not on this tree yet')
    def test_every_reported_interval_and_phase_is_tracked_and_documented(self):
        source = TIMINGS.read_text()
        reported = {kind: set(re.findall(kind + r'::\w+ => "([^"]+)"', source)) for kind in ('Interval', 'Phase')}
        self.assertEqual(set(BUDGET['intervals']['names']), reported['Interval'] | {'(untimed)'})
        self.assertEqual(set(BUDGET['phases']['names']), reported['Phase'])

if __name__ == '__main__':
    unittest.main()
