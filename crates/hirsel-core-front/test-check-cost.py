#!/usr/bin/env python3
"""The process ceiling must catch startup work outside the driver meter."""
import argparse
import runpy
from pathlib import Path
import tempfile
import unittest

GUARD = runpy.run_path(str(Path(__file__).with_name('check-cost.py')))

FRONT = runpy.run_path(str(Path(__file__).with_name('front-cost.py')))


class FrontStageCeiling(unittest.TestCase):
    def row(self):
        return dict(source_bytes=500, tokens=100,
                    costs={name: 10000 for name in FRONT['STAGES']},
                    image_access={name: 10 for name in FRONT['COUNTS']})

    def test_one_stage_overrun_cannot_hide_in_another_stage_saving(self):
        row = self.row()
        ceiling = FRONT['baseline'](row)
        self.assertEqual(FRONT['compare'](row, ceiling), [])
        row['costs']['parse'] = 10201
        row['costs']['resolve'] = 1
        failures = FRONT['compare'](row, ceiling)
        self.assertEqual(len(failures), 1)
        self.assertIn('parse', failures[0])

    def test_image_work_zero_is_enforced_and_nested_costs_are_not_added(self):
        row = self.row()
        row['image_access'] = {name: 0 for name in FRONT['COUNTS']}
        ceiling = FRONT['baseline'](row)
        row['image_access']['bytes_read'] = 2
        self.assertIn('image bytes_read', FRONT['compare'](row, ceiling)[0])
        metrics = dict(source_bytes=500, tokens=100, counter_refusal=None,
                       stages=[dict(name=name, instructions=10000) for name in
                               ('parse','front/input counts','index','resolve','declarations/layout')],
                       substages=[dict(name='body keys/new', instructions=1000),
                                  dict(name='body keys/for_body', instructions=2000)],
                       front_image_access=dict(records_opened=1, bytes_read=100,
                                               digests=1, digest_bytes=68,
                                               index_calls=3, key_probes=1, row_demands=1, nodes_decoded=1,
                                               index_instructions=700, read_instructions=500,
                                               digest_instructions=100,
                                               counter_refusal=None))
        observed = FRONT['front'](metrics)
        self.assertEqual(observed['costs']['declarations'], 9000)
        self.assertEqual(observed['costs']['body keys'], 3000)
        self.assertEqual(observed['costs']['parse'], 10000)
        del metrics['substages'][1]
        with self.assertRaisesRegex(ValueError, 'missing body key'):
            FRONT['front'](metrics)

    def test_integer_image_margin_admits_one_event_but_catches_two(self):
        row = self.row()
        row['image_access']['records_opened'] = 69
        ceiling = FRONT['baseline'](row)
        self.assertEqual(FRONT['compare'](row, ceiling), [])
        row['image_access']['records_opened'] += 1
        self.assertEqual(FRONT['compare'](row, ceiling), [])
        row['image_access']['records_opened'] += 1
        failures = FRONT['compare'](row, ceiling)
        self.assertEqual(len(failures), 1)
        self.assertIn('image records_opened', failures[0])
        self.assertAlmostEqual(ceiling['image_per_token']['records_opened'], 70.38 / 100)


class SuiteFailure(unittest.TestCase):
    def test_capped_wait_output_does_not_corrupt_the_build_result(self):
        import os
        import subprocess
        from unittest.mock import patch

        suite_edit = FRONT['suite_edit']
        wait = b'capped: waiting 0.0 s for a short slot, 0 ahead; holders:\n'
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)

            def build(command, *, stdout, stderr, **kwargs):
                os.write(stderr.fileno(), wait)
                os.write(stdout.fileno(), b'{"accepted": true, "rechecked": 1, "root": "edit-root"}\n')
                (out / 'edit.metrics.json').write_text('{"counters": {"body_keys": 1}}')
                return subprocess.CompletedProcess(command, 0)

            with patch.object(FRONT['request_cost'], 'manifests'), \
                    patch.dict(FRONT['GUARD'], admit=lambda: None), \
                    patch.dict(suite_edit.__globals__, front=lambda metrics: metrics), \
                    patch('subprocess.run', side_effect=build):
                (out / 'base.snapshot').mkdir()
                _, result = suite_edit(Path('hirsel'), Path('runtime.a'), out,
                                       dict(llvm='/llvm', kit='/kit', clang='/llvm/bin/clang',
                                            linker='/llvm/bin/ld.lld', linker_sha256='fixture'), prepare=False)
            self.assertEqual(result['root'], 'edit-root')
            self.assertEqual((out / 'edit.stderr.log').read_bytes(), wait)

    def test_failed_request_includes_panic_before_temporary_logs_are_deleted(self):
        import subprocess
        from unittest.mock import patch
        suite_edit = FRONT['suite_edit']
        panic = "thread 'main' panicked at compiler.rs:42: bound value missing"
        with tempfile.TemporaryDirectory() as temporary:
            out = Path(temporary)
            def fail(command, **kwargs):
                kwargs['stderr'].write(('request output\n' + panic + '\n').encode())
                return subprocess.CompletedProcess(command, 101)
            with patch.object(FRONT['request_cost'], 'manifests'), \
                    patch.dict(FRONT['GUARD'], admit=lambda: None), \
                    patch('subprocess.run', side_effect=fail):
                with self.assertRaises(ValueError) as failure:
                    suite_edit(Path('hirsel'), Path('runtime.a'), out,
                               dict(llvm='/llvm', kit='/kit', clang='/llvm/bin/clang'))
            message = str(failure.exception)
        self.assertIn('suite base: exit 101', message)
        self.assertIn(panic, message)


class PinnedRequestToolchain(unittest.TestCase):
    def fixture(self, root):
        import hashlib
        llvm = root / 'current-llvm'
        (llvm / 'bin').mkdir(parents=True)
        kit = root / 'kit'
        kit.mkdir()
        linker = llvm / 'bin/ld.lld'
        linker.write_bytes(b'pinned static linker')
        lock = root / 'tools/native-toolchain/llvm-23.lock'
        lock.parent.mkdir(parents=True)
        lock.write_text('static_lld ' + hashlib.sha256(linker.read_bytes()).hexdigest() + ' 20 fixture\n')
        return llvm, kit, linker

    def test_standalone_uses_buck_output_and_records_actual_selected_linker(self):
        import json
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            llvm, kit, linker = self.fixture(root)
            outputs = json.dumps({'root//tools/buck2/native-toolchain:llvm': str(llvm),
                                  'root//tools/buck2/native-toolchain:kit': str(kit)}).encode()
            with patch('subprocess.check_output', side_effect=[outputs, str(linker) + '\n']) as capture, \
                    patch('os.getloadavg', return_value=(0, 0, 0)), \
                    patch.dict('os.environ', {'LLVM_SYS_231_PREFIX': '/stale/config/llvm'}):
                tools = FRONT['request_cost'].toolchain(root)
            self.assertEqual(capture.call_args_list[0].args[0][1], 'build')
            self.assertIn('//tools/buck2/native-toolchain:llvm', capture.call_args_list[0].args[0])
            self.assertEqual(tools['llvm'], str(llvm))
            metrics = root / 'metrics.json'
            metrics.write_text('{"instructions": 123}')
            FRONT['request_cost'].stamp_metrics(metrics, tools)
            self.assertEqual(json.loads(metrics.read_text())['linker_sha256'], tools['linker_sha256'])

    def test_invalid_metrics_name_the_file_and_first_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'edit.metrics.json'
            for contents in (b'', b'warning: metrics unavailable\n', b'\xff{bad UTF-8}'):
                with self.subTest(contents=contents):
                    path.write_bytes(contents)
                    with self.assertRaises(ValueError) as failure:
                        FRONT['request_cost'].stamp_metrics(path, {})
                    self.assertIn(str(path), str(failure.exception))
                    self.assertIn(f'first bytes: {contents!r}', str(failure.exception))

    def test_wrong_linker_is_refused_before_running_any_request(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            llvm, kit, linker = self.fixture(root)
            linker.write_bytes(b'stale dynamic linker')
            with patch('subprocess.check_output', return_value=str(linker) + '\n'):
                with self.assertRaisesRegex(ValueError, 'lock pins'):
                    FRONT['request_cost'].toolchain(root, dict(llvm=llvm, kit=kit))


class ReportOnly(unittest.TestCase):
    def test_overrun_reports_but_missing_counter_still_fails(self):
        from unittest.mock import patch
        import contextlib
        import io
        run = GUARD['run']
        metrics = dict(instructions=100, stages=[dict(name='check/lower', instructions=90)])
        product = dict(diagnostics=[], incomplete=[])
        limits = dict(corpora=dict(m1=dict(code_lines=1, check_lower_per_line=50,
                     whole_request_per_line=60, whole_process_per_line=70)),
                     completion_forecast=dict(m1=dict(complete_bodies=1, bodies=1,
                         missing_check_lower_per_line=0, additional_publication_per_line=0)),
                     finished_budget_per_line=60000, policy='explain the rise')
        def samples(*_, **__):
            return [('m1', 1, product, metrics, 120)]
        with patch.dict(run.__globals__, sample_sources=samples, ANCHOR_RESIDUAL={}):
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                with self.assertRaisesRegex(ValueError, '90.00 instructions/line'):
                    run(None, None, limits)
                run(None, None, limits, report_only=True)
            self.assertIn('whole process: 120.00 instructions/line exceeds 70', output.getvalue())
            metrics['counter_refusal'] = 'counter unavailable'
            with self.assertRaisesRegex(ValueError, 'counter unavailable'):
                run(None, None, limits, report_only=True)
            metrics['counter_refusal'] = None
            product['incomplete'] = ['gap']
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                run(None, None, limits, report_only=True)
            import json
            row = next(line.removeprefix('COST_REPORT ') for line in output.getvalue().splitlines()
                       if line.startswith('COST_REPORT '))
            self.assertEqual(json.loads(row)['ceilings'], {})


class ProcessCeiling(unittest.TestCase):
    def test_fixed_product_ceiling_uses_each_library_token_count(self):
        import contextlib
        import io

        fixed = {'fixed std': dict(instructions=10000, tokens=200),
                 'fixed core': dict(instructions=5001, tokens=100)}
        limits = {'instructions_per_token': {'fixed std': 50, 'fixed core': 50}}
        with contextlib.redirect_stdout(io.StringIO()):
            failures, proposed = GUARD['fixed_product_costs'](fixed, limits)
        self.assertEqual(failures, ['product fixed core: 50.010000 instructions/token exceeds 50'])
        # 10,000 + 2% = 10,200. 5,001 + 2% = 5,101.02, rounded up to 5,200.
        self.assertEqual(proposed, {'fixed std': 51, 'fixed core': 52})

    def test_startup_regression_fails_when_driver_cost_is_unchanged(self):
        metrics = {'instructions': 10000, 'stages': [{'name': 'check/lower', 'instructions': 5000}]}
        ceiling = {'code_lines': 1000, 'check_lower_per_line': 5,
                   'whole_request_per_line': 10, 'whole_process_per_line': 30}
        check = GUARD['check_ceiling']
        self.assertEqual(check(metrics, 1000, ceiling, 30000)[2], [])
        errors = check(metrics, 1000, ceiling, 30001)[2]
        self.assertEqual(len(errors), 1)
        self.assertIn('whole process', errors[0])
        with self.assertRaisesRegex(ValueError, 'does not cover'):
            check(metrics, 1000, ceiling, 9999)

    def test_source_growth_uses_actual_lines_and_keeps_numeric_limits(self):
        metrics = {'instructions': 10000, 'stages': [{'name': 'check/lower', 'instructions': 5000}]}
        ceiling = {'code_lines': 1000, 'check_lower_per_line': 5,
                   'whole_request_per_line': 10, 'whole_process_per_line': 30}
        check = GUARD['check_ceiling']
        self.assertEqual(check(metrics, 1001, ceiling, 30030)[2], [])
        errors = check(metrics, 1001, ceiling, 30031)[2]
        self.assertEqual(len(errors), 1)
        self.assertIn('whole process', errors[0])
        with self.assertRaisesRegex(ValueError, 'no code lines'):
            check(metrics, 0, ceiling, 30000)

    def test_finished_budget_is_independent_of_current_work_ceiling(self):
        forecast = GUARD['finished_forecast']
        projection = {'missing_check_lower_per_line': 1000,
                      'additional_publication_per_line': 500}
        value, failures = forecast(58_500_000, 1000, projection, 60000)
        self.assertEqual(value, 60000)
        self.assertEqual(failures, [])
        # One instruction of additional startup work must reach the forecast too.
        _, failures = forecast(58_500_001, 1000, projection, 60000)
        self.assertEqual(len(failures), 1)
        self.assertIn('budget 60000', failures[0])
        projection['additional_publication_per_line'] = -1
        with self.assertRaisesRegex(ValueError, 'negative missing work'):
            forecast(1, 1000, projection, 60000)

    def test_product_fixed_and_marginal_are_separate_limits(self):
        check = GUARD['product_ceiling']
        self.assertEqual(check(70_000_000, 10_000_000, 1000, 60_000, 60_000), (60_000, []))
        value, errors = check(70_000_001, 10_000_000, 1000, 61_000, 60_000)
        self.assertGreater(value, 60_000)
        self.assertEqual(len(errors), 1)
        self.assertIn('budget 60000', errors[0])
        self.assertEqual(len(check(70_000_001, 10_000_000, 1000, 60_000, 60_000)[1]), 2)
        for process, fixed, lines in [(1, 2, 1), (1, 0, 1), (2, 1, 0)]:
            with self.assertRaisesRegex(ValueError, 'invalid fixed/marginal'):
                check(process, fixed, lines, 60000, 60000)

    def test_named_anchor_residual_does_not_hide_a_new_regression_or_budget_miss(self):
        metrics = {'instructions': 11000, 'stages': [{'name': 'check/lower', 'instructions': 6000}]}
        ceiling = {'check_lower_per_line': 5, 'whole_request_per_line': 10,
                   'whole_process_per_line': 30}
        check = GUARD['check_ceiling']
        self.assertEqual(check(metrics, 1000, ceiling, 31000, 1000)[2], [])
        # The allowance is an absolute frozen-corpus count, not a growing per-line exemption.
        metrics['stages'][0]['instructions'] += 1
        self.assertIn('check/lower', check(metrics, 1000, ceiling, 31000, 1000)[2][0])
        metrics['stages'][0]['instructions'] -= 1
        self.assertIn('whole process', check(metrics, 1000, ceiling, 31001, 1000)[2][0])
        with self.assertRaisesRegex(ValueError, 'negative recorded'):
            check(metrics, 1000, ceiling, 31000, -1)
        projection = {'missing_check_lower_per_line': 0, 'additional_publication_per_line': 0}
        self.assertEqual(len(GUARD['finished_forecast'](60000001, 1000, projection, 60000)[1]), 1)

    def test_a_new_partial_body_cannot_qualify_as_a_cheaper_finished_check(self):
        check = GUARD['completion_errors']
        complete = {'complete_bodies': 10, 'bodies': 10}
        self.assertEqual(check({'diagnostics': [], 'incomplete': []}, complete), [])
        for result in [{'diagnostics': ['refused'], 'incomplete': []},
                       {'diagnostics': [], 'incomplete': ['gap']}]:
            self.assertIn('every body', check(result, complete)[0])
        self.assertEqual(check({'diagnostics': [], 'incomplete': ['gap']},
                               {'complete_bodies': 9, 'bodies': 10}), [])

    def test_process_counter_requires_one_complete_event(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'perf.txt'
            path.write_text('# counter\n30001;;instructions:u;1;100.00\n')
            self.assertEqual(GUARD['process_instructions'](path), 30001)
            for row in ['', '<not counted>;;instructions:u;1;100.00\n',
                        '0;;instructions:u;1;100.00\n',
                        '1;;instructions:u\n2;;instructions:u\n']:
                path.write_text(row)
                with self.assertRaisesRegex(ValueError, 'no complete sample'):
                    GUARD['process_instructions'](path)


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
