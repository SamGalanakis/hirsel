#!/usr/bin/env python3
"""FAIL-15 standalone archive: bookkeeping OOM never enters Rust's abort handler."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class StandaloneBookkeeping(unittest.TestCase):
    def test_address_space_refusal_stops_without_cleanup(self):
        root = Path(__file__).resolve().parents[2]
        archive = Path(os.environ['HIRSEL_CORE_RT_ARCHIVES']).resolve()
        archive = archive / 'x86_64-unknown-linux-gnu/libhirsel_core_rt.a'
        compiler = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        with tempfile.TemporaryDirectory(prefix='hirsel-bookkeeping-stop-') as scratch:
            binary = Path(scratch) / 'run'
            subprocess.run([
                str(compiler), '-O2', '-std=c11', '-I', str(root / 'crates/hirsel-core-rt/include'),
                str(root / 'crates/hirsel-core-rt/testdata/bookkeeping_stop.c'), str(archive),
                '-lgcc_s', '-lpthread', '-ldl', '-lm', '-lrt', '-lutil', '-o', str(binary),
            ], check=True)
            result = subprocess.run([binary], capture_output=True, check=False)
            self.assertEqual(result.returncode, 102, result)
            self.assertEqual(result.stdout, b'entered\n')
            self.assertEqual(result.stderr, b'run stop: HostMemory\n')


if __name__ == '__main__':
    unittest.main()
