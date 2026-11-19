#!/usr/bin/env python3
"""Observed memory figures are JSON, exact at checkpoints and behavior-neutral."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class MemoryReport(unittest.TestCase):
    def test_native_phases_charge_backing_slack_and_cleanup(self):
        archive = Path(os.environ['HIRSEL_CORE_RT_OBSERVED_ARCHIVES']).resolve() / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
        cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        with tempfile.TemporaryDirectory(prefix='hirsel-memory-report-') as scratch:
            binary = Path(scratch) / 'run'
            subprocess.run([str(cc), '-O2', '-std=c11', '-I', str(ROOT / 'crates/hirsel-core-rt/include'),
                            str(ROOT / 'crates/hirsel-core-rt/testdata/memory_report.c'), str(archive),
                            '-lgcc_s', '-lpthread', '-ldl', '-lm', '-lrt', '-lutil', '-o', str(binary)], check=True)
            run = subprocess.run([binary], capture_output=True, check=True)
            self.assertEqual(run.stderr, b'')
            report = json.loads(run.stdout)
            self.assertEqual(report['metric_version'], 2)
            self.assertEqual(report['peak_root_reserved_bytes'], 296)
            self.assertEqual(report['peak_charged_bytes'], 296)
            self.assertEqual([report[x] for x in ['charged_bytes', 'retained_bytes', 'slack_bytes']], [0, 0, 0])
            phases = {x['name']: x for x in report['phases']}
            allocated = phases['allocated\n"λ"']
            trimmed = phases['trimmed']
            closed = phases['closed']
            self.assertEqual(allocated['charged_bytes'], 296)
            self.assertEqual(trimmed['charged_bytes'], 288)
            self.assertEqual(allocated['retained_bytes'], trimmed['retained_bytes'])
            self.assertGreater(allocated['retained_bytes'], closed['retained_bytes'] + 128)
            self.assertEqual([allocated['slack_bytes'], trimmed['slack_bytes'], closed['slack_bytes']], [4088, 4088, 0])
            self.assertEqual(closed['peak_charged_bytes'], 288)
            self.assertEqual(phases['run_end']['retained_bytes'], 0)
            for phase in report['phases']:
                self.assertLessEqual(phase['slack_bytes'], phase['retained_bytes'])
                for field in ['charged_bytes', 'retained_bytes', 'slack_bytes']:
                    self.assertGreaterEqual(phase['peak_' + field], phase[field])

    def test_driver_option_preserves_status_and_output_on_both_engines(self):
        compiler = Path(os.environ['HIRSEL_REPORT_COMPILER']).resolve()
        evaluator = Path(os.environ['HIRSEL_REPORT_EVALUATOR']).resolve()
        cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        with tempfile.TemporaryDirectory(prefix='hirsel-memory-driver-') as scratch:
            scratch = Path(scratch)
            source = scratch / 'source'
            source.mkdir()
            main = source / 'main.hirsel'
            main.write_text('''pub fn main() -> Result[(), Exit] { Ok(()) }
pub fn code() -> Result[(), Exit] { Error(Exit(status: 7)) }
pub fn caught() -> Result[(), Exit] {
    match isolate(fn() -> u64 { option.unwrap(None) }) {
        Ok(Error(_)) => Ok(()),
        _ => Error(Exit(status: 1)),
    }
}
''')
            caught_balances = {}
            caught_main_balances = {}
            for engine in ['llvm', 'cranelift']:
                for entry, limit, expected, peak in [
                    ('main', 4096, 0, 288), ('code', 4096, 7, 288),
                    ('main', 0, 103, 0), ('caught', 4096, 0, 320),
                ]:
                    outcomes = []
                    for observed in [False, True]:
                        key = f'{engine}-{entry}-{limit}-{observed}'
                        archive_env = 'HIRSEL_CORE_RT_OBSERVED_ARCHIVES' if observed else 'HIRSEL_CORE_RT_DEFAULT_ARCHIVES'
                        archive = Path(os.environ[archive_env]).resolve() / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
                        binary, report_path = scratch / key, scratch / (key + '.json')
                        args = [str(compiler), 'build', str(source), '--entry', 'app@1/main/' + entry,
                                '--backend', engine, '--limit', str(limit), '--runtime', str(archive),
                                '--cc', str(cc), '--store', str(scratch / 'store'), '--json', '-o', str(binary)]
                        if engine == 'llvm':
                            args.append('--release')
                        if observed:
                            args += ['--logical-charge-report', str(report_path)]
                        build = subprocess.run(args, capture_output=True, text=True)
                        self.assertEqual(build.returncode, 0, build.stdout + build.stderr)
                        run = subprocess.run([binary], capture_output=True)
                        self.assertEqual(run.returncode, expected, run.stderr)
                        outcomes.append((run.returncode, run.stdout, run.stderr))
                        if observed:
                            report = json.loads(report_path.read_text())
                            self.assertTrue(report['observation_mode'])
                            self.assertEqual(report['peak_root_reserved_bytes'], peak)
                            self.assertEqual([report[x] for x in ['retained_bytes', 'slack_bytes']], [0, 0])
                            if entry == 'caught':
                                caught_balances[engine] = report['charged_bytes']
                                caught_main_balances[engine] = next(p['charged_bytes'] for p in report['phases'] if p['name'] == 'main')
                            else:
                                self.assertEqual(report['charged_bytes'], 0)
                    self.assertEqual(outcomes[0], outcomes[1])

            core = {'name': 'core', 'version': '1'}
            inputs = scratch / 'inputs.json'
            inputs.write_text(json.dumps({'core': core, 'packages': [
                {'package': core, 'dependencies': [], 'sources': [
                    {'file': str(p), 'modules': [p.stem], 'signatures': True}
                    for p in sorted((ROOT / 'core/core').glob('*.hirsel'))
                ]},
                {'package': {'name': 'app', 'version': '1'}, 'dependencies': [core], 'sources': [
                    {'file': str(main), 'modules': ['main'], 'signatures': False}
                ]},
            ]}))
            result = subprocess.check_output([evaluator, inputs, 'app@1/main/caught'])
            report = json.loads(result)
            self.assertEqual(report['outcome'], {'kind': 'returned', 'values': [1]}, report)
            caught_balances['evaluator'] = report['used']
            self.assertEqual(caught_balances, {'cranelift': 0, 'llvm': 0, 'evaluator': 0})
            # The startup cell was refunded with the caught chain. Main still
            # owns its task record and the replacement cell at this checkpoint.
            self.assertEqual(caught_main_balances, {'cranelift': 288, 'llvm': 288})

    def test_each_report_path_is_written_from_one_store(self):
        """The charge-report path is a link-plan input the environment does not
        cover: two Cranelift development requests that differ only in it share
        every instance and entry record, and each executable writes its own
        path. A plan key without the path links the first request's executable
        again, which writes the first path."""
        compiler = Path(os.environ['HIRSEL_REPORT_COMPILER']).resolve()
        cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        archive = Path(os.environ['HIRSEL_CORE_RT_OBSERVED_ARCHIVES']).resolve() / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
        with tempfile.TemporaryDirectory(prefix='hirsel-memory-path-') as scratch:
            scratch = Path(scratch)
            source = scratch / 'source'
            source.mkdir()
            (source / 'main.hirsel').write_text('pub fn main() -> Result[(), Exit] { Error(Exit(status:7)) }\n')
            for name in ['one', 'two']:
                binary, report_path = scratch / name, scratch / (name + '.json')
                build = subprocess.run([str(compiler), 'build', str(source), '--entry', 'app@1/main/main',
                                        '--backend', 'cranelift', '--runtime', str(archive), '--cc', str(cc),
                                        '--store', str(scratch / 'store'), '--json', '-o', str(binary),
                                        '--logical-charge-report', str(report_path)], capture_output=True, text=True)
                self.assertEqual(build.returncode, 0, build.stdout + build.stderr)
                self.assertEqual(subprocess.run([binary]).returncode, 7)
                self.assertTrue(json.loads(report_path.read_text())['observation_mode'], name)
            self.assertEqual(json.loads(build.stdout)['native']['compiled'], 0, 'the second request hits every unit')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
