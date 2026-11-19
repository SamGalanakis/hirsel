#!/usr/bin/env python3
"""Run the contract laws supported by the selected native engines.

Laws marked `// test-backend: evaluator` run in the evaluator suite.
Dir laws need the host's openat2, so these tests run on the invoking host.
"""
import argparse
import json
import os
from pathlib import Path
import unittest

from std_native import ROOT, discover, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--node', type=Path, required=True)
    parser.add_argument('--model-runtime', type=Path)
    parser.add_argument('--schedule-runtime', type=Path)
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
        def test_every_native_runnable_law(self):
            root = args.root.resolve()
            contracts = [c for c in discover(root, args.filter) if not c.evaluator_only]
            self.assertTrue(contracts, 'zero native-runnable laws selected')
            owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
            rows = run(root, args.compiler.resolve(), args.runtime.resolve(), contracts, engines, None, owners,
                       args.cc.resolve() if args.cc else None,
                       evaluator=args.evaluator.resolve(), node=args.node.resolve(),
                       model_runtime=args.model_runtime.resolve() if args.model_runtime else None,
                       schedule_runtime=args.schedule_runtime.resolve() if args.schedule_runtime else None)
            findings = [f"{row['engine']} {row['file']}:{row['line']} {row['test']}: {row['cause']}"
                        for row in rows if row['applicable'] and not row['passed']]
            # tools/sweep counts this line: one body per law and engine.
            print(f"native laws: {sum(row['applicable'] and row['passed'] for row in rows)} passed, "
                  f"{len(findings)} failed", flush=True)
            self.assertFalse(findings, '\n' + '\n'.join(findings))
            self.assertTrue(any(row['applicable'] for row in rows), 'zero laws ran')

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Laws))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
