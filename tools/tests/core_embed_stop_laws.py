#!/usr/bin/env python3
"""Slice 2s native link-wrapped fault harness (-Wl,--wrap=exit,_exit,mprotect,munmap,malloc)."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class EmbedStopWrapLaws(unittest.TestCase):
    def test_grep_law_forbids_process_stop_outside_standalone_entry(self):
        src_root = ROOT / 'crates/hirsel-core-rt/src'
        files = sorted(src_root.rglob('*.rs'))
        self.assertTrue(files, f'no .rs files found under {src_root}')
        offenders = []
        for path in files:
            rel = path.relative_to(src_root).as_posix()
            name = path.name
            if (
                rel in ('process.rs', 'standalone_allocator.rs')
                or name.endswith('_tests.rs')
                or name == 'tests.rs'
            ):
                continue
            in_cfg_test = False
            for line_no, raw in enumerate(path.read_text().splitlines(), 1):
                code = raw.split('//', 1)[0]
                if '#[cfg(test)]' in code:
                    in_cfg_test = True
                if not in_cfg_test and 'process::stop' in code:
                    offenders.append(f'{rel}:{line_no}: {raw.strip()}')
        self.assertEqual(
            offenders,
            [],
            'forbidden process::stop outside standalone entry:\n' + '\n'.join(offenders),
        )

    def test_embed_stops_never_exit_and_retire_refused_growth(self):
        archive = Path(os.environ['HIRSEL_EMBED_STOP_FIXTURE']).resolve()
        cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
        source = ROOT / 'crates/hirsel-core-rt/testdata/embed_stop_wrap.c'
        with tempfile.TemporaryDirectory(prefix='hirsel-embed-stop-wrap-') as scratch:
            binary = Path(scratch) / 'embed_stop_wrap'
            subprocess.run(
                [
                    str(cc),
                    '-O2',
                    '-std=c11',
                    str(source),
                    str(archive),
                    '-Wl,--wrap=exit,--wrap=_exit,--wrap=mprotect,--wrap=munmap,--wrap=malloc',
                    '-lgcc_s',
                    '-lpthread',
                    '-ldl',
                    '-lm',
                    '-lrt',
                    '-lutil',
                    '-o',
                    str(binary),
                ],
                check=True,
            )
            run = subprocess.run([binary], capture_output=True, text=True)
            self.assertEqual(
                run.returncode,
                0,
                f'embed_stop_wrap exited {run.returncode}\nstdout:\n{run.stdout}\nstderr:\n{run.stderr}',
            )
            self.assertEqual(run.stderr, '')
            self.assertEqual(run.stdout, 'EMBED_STOP_WRAP_OK\n')


if __name__ == '__main__':
    unittest.main()
