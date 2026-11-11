#!/usr/bin/env python3
"""Guard build/test entry before handing the original arguments to pinned Buck2."""
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
# Buck's universal options may precede the command. Values can themselves be
# command names (notably --isolation-dir test); never scan subcommand arguments.
VALUE_OPTIONS = {'--isolation-dir', '-v', '--verbose', '--oncall', '--client-metadata',
                 '--setting', '--agent-context'}


def command(arguments):
    arguments = iter(arguments)
    for argument in arguments:
        if argument in VALUE_OPTIONS:
            next(arguments, None)
        elif not argument.startswith('-'):
            return argument
    return None


def main():
    if command(sys.argv[1:]) in ('build', 'test'):
        import bootstrap_llvm
        import generated
        try:
            ready = bootstrap_llvm.current(bootstrap_llvm.DEFAULT_OUTPUT, bootstrap_llvm.identity())
        except OSError:
            ready = False
        if not ready:
            sys.exit('buck2: pinned LLVM tree or hermetic C++ headers missing/stale; run python3 tools/buck2/prepare.py')
        if generated.stale(ROOT):
            sys.exit('buck2: generated inputs are stale; run python3 tools/buck2/sync.py --if-stale')
    executable = ROOT / '.buck2/bin/buck2-real'
    os.execv(executable, [str(executable), *sys.argv[1:]])


if __name__ == '__main__':
    main()
