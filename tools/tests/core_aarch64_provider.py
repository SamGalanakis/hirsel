#!/usr/bin/env python3
"""Execute openat2 and a real-file core provider through declared ARM/QEMU inputs."""
import argparse
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class Aarch64Provider(unittest.TestCase):
    def test_openat2_and_real_file_provider(self):
        arm = Path(os.environ['HIRSEL_TOOLCHAIN_ARM']).resolve()
        emulator = Path(os.environ['HIRSEL_TOOLCHAIN_QEMU']).resolve() / 'qemu-aarch64'
        sysroot = arm / 'aarch64-none-linux-gnu/libc'
        runtime = (Path(os.environ['HIRSEL_CORE_RT_ARCHIVES']).resolve()
                   / 'aarch64-unknown-linux-gnu/libhirsel_core_rt.a')
        with tempfile.TemporaryDirectory(prefix='hirsel-core-arm-provider-') as temporary:
            scratch = Path(temporary)
            binary = scratch / 'provider'
            built = subprocess.run([
                str(arm / 'bin/aarch64-none-linux-gnu-gcc'), '-O0', '-std=c11', '-fstack-clash-protection',
                '-I', str(ROOT / 'crates/hirsel-core-rt/include'),
                str(ROOT / 'tools/tests/fixtures/core_aarch64_provider.c'),
                str(runtime), '-lgcc_s', '-lpthread', '-ldl', '-lm', '-lrt', '-lutil',
                '-o', str(binary),
            ], capture_output=True)
            self.assertEqual(built.returncode, 0, built.stderr.decode(errors='replace'))
            command = [str(emulator), '-L', str(sysroot), str(binary)]
            probe = subprocess.run([*command, 'probe'], cwd=scratch, capture_output=True)
            self.assertEqual(probe.returncode, 0, probe)
            self.assertRegex(probe.stdout, rb'^openat2=\d+ errno=0\n$')
            self.assertEqual(probe.stderr, b'')
            provider = subprocess.run(command, cwd=scratch, capture_output=True)
            self.assertEqual(provider.returncode, 0, provider)
            self.assertEqual(provider.stdout, b'provider=532\n')
            self.assertEqual(provider.stderr, b'')
            self.assertEqual((scratch / 'provider.txt').read_bytes(), b'hello')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
