#!/usr/bin/env python3
"""Gate the contract laws on native engines. By default every law a native engine
can run, which is every law not marked `// test-backend: evaluator` (those stay
with the evaluator gate). With --capabilities-only, just the laws whose entry
takes a host capability (Dir, Stdout, Stderr, Clock), which the evaluator walker
reports native-only, and the laws marked `// engine-fact:`, which an engine's
generated code relies on: land runs that set, and the sweep runs them all. The sets
are the runner's own classification, so a new law joins without an edit. Dir
laws need the host's openat2, so these targets execute on the invoking host."""
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
    parser.add_argument('--capabilities-only', action='store_true')
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    engines = args.backend or ['cranelift', 'llvm']

    class Laws(unittest.TestCase):
        def test_every_native_runnable_law(self):
            root = args.root.resolve()
            contracts = [c for c in discover(root, args.filter) if not c.evaluator_only
                         and (host_parameter(c)[0] or c.engine_fact or not args.capabilities_only)]
            self.assertTrue(contracts, 'zero native-runnable laws selected')
            owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
            rows = run(root, args.compiler.resolve(), args.runtime.resolve(), contracts, engines, None, owners,
                       args.cc.resolve() if args.cc else None)
            findings = [f"{row['engine']} {row['file']}:{row['line']} {row['test']}: {row['cause']}"
                        for row in rows if row['applicable'] and not row['passed']]
            self.assertFalse(findings, '\n' + '\n'.join(findings))
            self.assertTrue(any(row['applicable'] for row in rows), 'zero laws ran')

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Laws))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
