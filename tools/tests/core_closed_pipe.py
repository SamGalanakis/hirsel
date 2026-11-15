#!/usr/bin/env python3
"""A compiler command whose reader has closed ends quietly, killed by SIGPIPE.

Law: every stdout and stderr write of `hirsel` goes to a pipe whose reader may
close early (`hirsel explain --list | head`). The process then ends with the
conventional SIGPIPE status: no panic (status 101), no error line, and no later
output on its other stream. Each case closes the read end before the command
starts, so the first write meets a closed pipe. Bug caught: Rust's startup
ignores SIGPIPE, so each print returns EPIPE and panics.
"""
import argparse
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import unittest


def closed_pipe_run(args, stream):
    """Run `args` with `stream` ('stdout' or 'stderr') on a pipe whose reader is
    already closed; return (returncode, the other stream's bytes)."""
    read, write = os.pipe()
    os.close(read)
    other = 'stderr' if stream == 'stdout' else 'stdout'
    try:
        run = subprocess.run(args, **{stream: write, other: subprocess.PIPE}, stdin=subprocess.DEVNULL)
    finally:
        os.close(write)
    return run.returncode, getattr(run, other)


class ClosedPipe(unittest.TestCase):
    def setUp(self):
        self.compiler = str(Path(os.environ['HIRSEL_PIPE_COMPILER']).resolve())

    def assert_quiet_sigpipe(self, args, stream):
        status, other = closed_pipe_run([self.compiler] + args, stream)
        self.assertEqual(status, -signal.SIGPIPE, other.decode(errors='replace'))
        self.assertEqual(other, b'')

    def test_explain_list_on_stdout(self):
        self.assert_quiet_sigpipe(['explain', '--list'], 'stdout')

    def test_help_on_stdout(self):
        self.assert_quiet_sigpipe(['help'], 'stdout')

    def test_check_diagnostics_on_stderr(self):
        # 2000 malformed literals: a diagnostic list far past one pipe buffer;
        # the summary line on stdout comes after them and must not appear.
        with tempfile.TemporaryDirectory(prefix='hirsel-closed-pipe-') as scratch:
            source = Path(scratch) / 'source'
            source.mkdir()
            (source / 'main.hirsel').write_text(
                'pub fn main() -> Result[(), Exit] { Ok(()) }\n'
                + ''.join(f'pub fn f{i}() -> u64 {{ 0u128 }}\n' for i in range(2000)))
            self.assert_quiet_sigpipe(['check', str(source), '--store', str(Path(scratch) / 'store')], 'stderr')


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.parse_args()
    unittest.main(argv=[__file__], verbosity=2)
