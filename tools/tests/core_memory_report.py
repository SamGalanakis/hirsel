#!/usr/bin/env python3
"""Observed memory figures are JSON, exact at checkpoints and behavior-neutral."""
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
            self.assertEqual([allocated['slack_bytes'], trimmed['slack_bytes'], closed['slack_bytes']], [120, 120, 0])
            self.assertEqual(closed['peak_charged_bytes'], 288)
            self.assertEqual(phases['run_end']['retained_bytes'], 0)
            for phase in report['phases']:
                self.assertLessEqual(phase['slack_bytes'], phase['retained_bytes'])
                for field in ['charged_bytes', 'retained_bytes', 'slack_bytes']:
                    self.assertGreaterEqual(phase['peak_' + field], phase[field])

    def test_driver_option_preserves_status_and_output_on_both_engines(self):
        compiler = Path(os.environ['HIRSEL_REPORT_COMPILER']).resolve()
        cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        with tempfile.TemporaryDirectory(prefix='hirsel-memory-driver-') as scratch:
            scratch = Path(scratch)
            source = scratch / 'source'
            source.mkdir()
            (source / 'main.hirsel').write_text('pub fn main() -> Result[(), Exit] { Ok(()) }\npub fn code() -> Result[(), Exit] { Error(Exit(status:7)) }\n')
            for engine in ['llvm', 'cranelift']:
                for entry, limit, expected in [('main', 4096, 0), ('code', 4096, 7), ('main', 0, 103)]:
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
                            self.assertEqual(report['peak_root_reserved_bytes'], 288 if limit else 0)
                            self.assertEqual([report[x] for x in ['charged_bytes', 'retained_bytes', 'slack_bytes']], [0, 0, 0])
                    self.assertEqual(outcomes[0], outcomes[1])


if __name__ == '__main__':
    unittest.main()
