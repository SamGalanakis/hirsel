"""The JSON family runtime budget uses the checked Rust twin's count."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import service_build


class RuntimeBudgetTests(unittest.TestCase):
    def test_observed_regression_and_original_code(self):
        for instructions, passed in [(367_326_997, True), (605_042_819, False)]:
            with self.subTest(instructions=instructions):
                budget = service_build.runtime_budget('json', 'llvm', 'hirsel',
                                                      dict(status='ok', instructions=instructions))
                self.assertEqual(budget['instruction_budget'], 386_300_000)
                self.assertEqual(budget['budget_passed'], passed)

    def test_incomplete_or_uncounted_samples_cannot_pass(self):
        for sample in [dict(status='wrong-output', instructions=1),
                       dict(status='memory-censored', instructions=1),
                       dict(status='ok'), dict(status='ok', instructions=0)]:
            with self.subTest(sample=sample):
                self.assertFalse(service_build.runtime_budget('json', 'llvm', 'hirsel', sample)['budget_passed'])

    def test_fresh_stage_counts_execution_and_returns_the_budget_failure(self):
        for instructions, expected_exit in [(367_326_997, 0), (605_042_819, 1)]:
            with self.subTest(instructions=instructions), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                artifacts = root / 'artifacts.json'
                artifacts.write_text(json.dumps(dict(compiler='compiler', runtime='runtime', compiler_build_profile='release')))
                argv = ['service_build.py', '--artifacts', str(artifacts), '--out', str(root / 'out'),
                        '--group', 'json', '--backend', 'llvm', '--release-twins', str(root),
                        '--dev-twins', str(root), '--stage', 'fresh', '--side', 'hirsel']
                commands = []

                def sample(command, directory):
                    commands.append(command)
                    directory.mkdir()
                    if 'build' in command:
                        (directory / 'stdout').write_bytes(b'{"accepted":true,"native":{"linked":true}}')
                        return dict(status='ok', instructions=1_000_000_000)
                    (directory / 'stdout').write_bytes(b'262144\n241502240\n0\n')
                    return dict(status='ok', instructions=instructions)

                with patch.object(sys, 'argv', argv), patch.object(service_build.measurement_artifacts, 'identity', return_value={}), patch.object(service_build, 'execute', side_effect=AssertionError('uncounted execution')), patch.object(service_build, 'sample', side_effect=sample), patch('builtins.print'):
                    self.assertEqual(service_build.main(), expected_exit)
                self.assertIn('build', commands[0])
                self.assertEqual(len(commands), 2)
                self.assertEqual(commands[1], [root / 'out/fresh-hirsel-program'])
                rows = json.loads((root / 'out/results.json').read_text())
                self.assertEqual(rows[0]['budget_passed'], expected_exit == 0)


if __name__ == '__main__':
    unittest.main()
