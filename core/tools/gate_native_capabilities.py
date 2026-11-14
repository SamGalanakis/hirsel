#!/usr/bin/env python3
"""Gate the contract laws the evaluator cannot run: every law whose entry takes a
host capability (Dir, Stdout, Stderr, Clock), which the evaluator walker reports
native-only. The set is the walker's own classification, so a new
capability-taking law joins without an edit. Dir laws need the host's openat2,
so this target executes on the invoking host."""
import argparse
import json
import os
from pathlib import Path
import unittest

from std_native import ROOT, discover, host_parameter, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--cc', type=Path, default=(Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang')
                        if 'LLVM_SYS_231_PREFIX' in os.environ else None)
    parser.add_argument('--backend', action='append', choices=['cranelift', 'llvm'])
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    engines = args.backend or ['cranelift', 'llvm']

    class Laws(unittest.TestCase):
        def test_every_native_only_law(self):
            root = args.root.resolve()
            # The evaluator walker marks exactly these rows native-only.
            contracts = [c for c in discover(root, args.filter) if host_parameter(c)[0]]
            self.assertTrue(contracts, 'zero native-only laws selected')
            owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
            rows = run(root, args.compiler.resolve(), args.runtime.resolve(), contracts, engines, None, owners,
                       args.cc.resolve() if args.cc else None)
            findings = [f"{row['engine']} {row['file']}:{row['line']} {row['test']}: {row['cause']}"
                        for row in rows if row['applicable'] and not row['passed']]
            self.assertFalse(findings, '\n' + '\n'.join(findings))

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Laws))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
