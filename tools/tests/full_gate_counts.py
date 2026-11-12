#!/usr/bin/env python3
"""A full-gate log must cover the compiled case inventory exactly once."""
import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest

CHECKER = Path(__file__).resolve().parents[1] / 'buck2/full_gate_counts.py'


def block(binary, expected, executed):
    return '\n'.join(['---- STDOUT ----', f'HIRSEL_FULL_GATE_BINARY {binary}',
                      *[f'HIRSEL_FULL_GATE_EXPECT {binary} {name}' for name in expected],
                      *[f'test {name} ... {status}' for name, status in executed]]) + '\n'


class FullGateCounts(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location('full_gate_counts', CHECKER)
        self.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.module)

    def check(self, text):
        counts = self.module.Counts()
        for line in text.splitlines():
            counts.observe(line)
        return counts.finish()

    def test_host_pool_and_empty_shards_cover_one_shared_inventory(self):
        names = ['arm::full_gate_a', 'heap::full_gate_b']
        log = block('it', names, [(names[0], 'ok')]) + block('it', names, [])
        log += block('it', names, [(names[1], 'ok')])
        self.assertEqual(self.check(log), 2)

    def test_duplicate_missing_unexpected_failed_and_unlabelled_are_rejected(self):
        name = 'full_gate_a'
        for log in [
            block('it', [name], [(name, 'ok'), (name, 'ok')]),
            block('it', [name], []),
            block('it', [name], [(name, 'ok'), ('full_gate_other', 'ok')]),
            block('it', [name], [(name, 'FAILED')]),
            block('it', [name], [(name, 'ignored')]),
            'test full_gate_a ... ok\n',
            block('it', [name], [(name, 'ok')]) + '---- STDOUT ----\ntest full_gate_a ... ok\n',
            '',
        ]:
            with self.subTest(log=log), self.assertRaises(ValueError):
                self.check(log)

    def test_identical_names_in_different_binaries_are_distinct(self):
        log = ''.join(block(binary, ['full_gate_same'], [('full_gate_same', 'ok')])
                      for binary in ['a-test-it', 'b-unittest'])
        self.assertEqual(self.check(log), 2)

    def test_buck_timestamps_ansi_should_panic_and_long_running_notices(self):
        log = block('it', ['full_gate_a'], [])
        log += 'test full_gate_a has been running for over 60 seconds\n'
        log += 'test full_gate_a - should panic ... \x1b[32mok\x1b[0m\n'
        self.assertEqual(self.check('\n'.join('[2026-10-15T23:11:06.536+02:00] ' + line
                                              for line in log.splitlines())), 1)

    def test_run_forwards_the_buck_test_runner_separator_and_options(self):
        script = ('import sys; assert sys.argv[1:] == ["--", '
                  '"--test-arg=full_gate_", "--test-arg=--ignored"]; '
                  f'print({block("it", ["full_gate_a"], [("full_gate_a", "ok")])!r})')
        run = subprocess.run([sys.executable, str(CHECKER), '--run', sys.executable,
                              '-c', script, '--',
                              '--test-arg=full_gate_', '--test-arg=--ignored'],
                             capture_output=True, text=True)
        self.assertEqual(run.returncode, 0, run.stderr)

    def test_run_preserves_child_failure_and_rejects_success_without_coverage(self):
        for script, expected in [('raise SystemExit(19)', 19), ('pass', 1),
                                  (f'print({block("it", ["full_gate_a"], [("full_gate_a", "ok")])!r})', 0)]:
            with self.subTest(script=script):
                run = subprocess.run([sys.executable, str(CHECKER), '--run', sys.executable,
                                      '-c', script], capture_output=True, text=True)
                self.assertEqual(run.returncode, expected, run.stderr)

    def test_retained_counts_require_each_exact_body_once(self):
        target = 'hirsel-fixture-test-it-retained-1'
        inventory = {target: ['module::a', 'module::b']}
        output = 'test module::a ... ok\ntest module::b ... ok\n'
        self.assertEqual(self.module.retained_counts({target: output}, inventory), 2)
        for changed in ['', output.replace('module::a', 'module::other'), output + output,
                        output.replace('... ok', '... ignored', 1),
                        output.replace('... ok', '... FAILED', 1)]:
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                self.module.retained_counts({target: changed}, inventory)

    def test_retained_counts_refuse_missing_or_unexpected_shards(self):
        target = 'hirsel-fixture-test-it-retained-1'
        inventory = {target: ['module::a']}
        for outputs in [{}, {target + '-retained-2': 'test module::a ... ok\n'}]:
            with self.assertRaises(ValueError):
                self.module.retained_counts(outputs, inventory)


if __name__ == '__main__':
    unittest.main()
