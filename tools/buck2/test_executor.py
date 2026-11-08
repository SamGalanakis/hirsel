#!/usr/bin/env python3
"""Run Buck's stock executor with the repository's shared outer timeout.

Buck2 6507dd15 command.rs:369-415 honours v2_test_executor for external test
providers; timeout_default_s at :696-708 applies only to internal providers.
"""
import argparse
import configparser
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]


def command(arguments, root=ROOT):
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument('--buck-trace-id')
    parser.add_argument('--config-entry', action='append')
    for option in ('executor-fd', 'orchestrator-fd', 'executor-addr', 'orchestrator-addr'):
        parser.add_argument('--' + option)
    parser.add_argument('runner_args', nargs=argparse.REMAINDER)
    args = parser.parse_args(arguments)
    config = configparser.ConfigParser(interpolation=None)
    config.read(root / '.buckconfig')
    seconds = config.getint('test', 'timeout_default_s')
    transport = [value for option in ('executor-fd', 'orchestrator-fd', 'executor-addr', 'orchestrator-addr')
                 if (argument := getattr(args, option.replace('-', '_'))) is not None
                 for value in ('--' + option, argument)]
    separator, program, *runner_args = args.runner_args
    if separator != '--':
        parser.error('expected the Buck executor/runner separator')
    return [str(root / '.buck2/bin/buck2'), 'internal-test-runner', *transport, '--', program,
            '--timeout', str(seconds), *runner_args]


if __name__ == '__main__':
    argv = command(sys.argv[1:])
    print(f'test executor: shared outer bound {argv[argv.index("--timeout") + 1]} s',
          file=sys.stderr, flush=True)
    os.execv(argv[0], argv)
