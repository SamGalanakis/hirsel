#!/usr/bin/env python3
"""Guard build/test entry before handing the original arguments to pinned Buck2."""
import json
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


def explain_llvm_bootstrap_failure(llvm, wanted):
    output = llvm.DEFAULT_OUTPUT
    receipt = output / llvm.RECEIPT
    try:
        installed = json.loads(receipt.read_text(encoding='utf-8'))
    except (json.JSONDecodeError, UnicodeError):
        return f'malformed LLVM receipt {receipt}'
    if not isinstance(installed, dict) or installed.keys() != wanted.keys():
        return f'malformed LLVM receipt {receipt} (expected fields: {", ".join(wanted)})'
    for field, expected in wanted.items():
        actual = installed[field]
        if actual != expected:
            shown = ascii(actual)[1:-1] if isinstance(actual, str) else repr(actual)
            return f'LLVM receipt {receipt}: {field} mismatch (expected {expected}, actual {shown})'
    for path in llvm.REQUIRED:
        required = output / path
        if not required.exists():
            return f'missing required LLVM path {required}'
    return f'LLVM tree {output} failed the private-inode check'


def main():
    if command(sys.argv[1:]) in ('build', 'test'):
        import bootstrap_llvm
        import generated
        problem = None
        try:
            wanted = bootstrap_llvm.identity()
            if not bootstrap_llvm.current(bootstrap_llvm.DEFAULT_OUTPUT, wanted):
                problem = explain_llvm_bootstrap_failure(bootstrap_llvm, wanted)
        except FileNotFoundError as error:
            problem = f'missing LLVM bootstrap path {error.filename}'
        except OSError as error:
            problem = f'cannot verify LLVM bootstrap path {error.filename}: {error.strerror}'
        except UnicodeError:
            problem = f'malformed LLVM receipt {bootstrap_llvm.DEFAULT_OUTPUT / bootstrap_llvm.RECEIPT}'
        if problem:
            sys.exit(f'buck2: {problem}; run python3 tools/buck2/prepare.py')
        if generated.stale(ROOT):
            sys.exit('buck2: generated inputs are stale; run python3 tools/buck2/sync.py --if-stale')
    pin = json.loads((ROOT / 'tools/buck2/pins.json').read_text())['buck2']['executable_sha256']
    executable = ROOT / f'.buck2/bin/buck2-{pin}'
    if not executable.is_file():
        sys.exit('buck2: the pinned Buck2 release is not installed; run python3 tools/buck2/prepare.py')
    os.execv(executable, [str(executable), *sys.argv[1:]])


if __name__ == '__main__':
    main()
