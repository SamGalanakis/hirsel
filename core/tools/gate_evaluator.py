#!/usr/bin/env python3
"""Gate every evaluator-eligible law through the existing contract walker."""
import argparse
import json
import os
from pathlib import Path
import unittest

import sdk_package
from std_native import ROOT, discover, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--compiler', type=Path)
    parser.add_argument('--runtime', type=Path, default=os.environ.get('HIRSEL_CORE_RT_ARCHIVE'))
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--filter', action='append', default=[])
    parser.add_argument('--node', default='node')
    # The combined core gate asks all harnesses for serial bodies. This harness
    # has one unittest body; accept exactly that same serial setting.
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    if args.compiler and not args.runtime:
        parser.error('--compiler requires --runtime for the build revision law')

    root = args.root.resolve()
    node_dist = os.environ.get('HIRSEL_SDK_NODE_DIST')
    packed = sdk_package.SELECTOR in args.filter or (node_dist and not args.filter)
    filters = [selector for selector in args.filter if selector != sdk_package.SELECTOR]
    if packed and (not node_dist or not args.compiler or not args.runtime):
        parser.error('the packed SDK check requires the Buck SDK toolchain, compiler and runtime')

    class Laws(unittest.TestCase):
        def test_every_evaluator_eligible_law(self):
            if not args.filter or filters:
                self.check_evaluator_laws()
            if packed:
                tools = [Path(os.environ['HIRSEL_SDK_' + name]).resolve()
                         for name in ('TYPESCRIPT', 'NODE_TYPES', 'UNDICI_TYPES')]
                sdk_package.check(root, args.compiler.resolve(), args.runtime.resolve(), Path(node_dist).resolve(), tools)
                print(f'{sdk_package.SELECTOR}: PASS', flush=True)

        def check_evaluator_laws(self):
            contracts = discover(root, filters)
            owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
            rows = run(root, args.compiler.resolve() if args.compiler else None,
                       args.runtime.resolve() if args.runtime else None, contracts, ['evaluator'], None, owners,
                       evaluator=args.evaluator.resolve(), compare_revisions=args.compiler is not None, node=args.node)
            eligible = [row for row in rows if row['applicable']]
            self.assertTrue(eligible, 'zero evaluator-eligible laws selected')
            findings = [f"{row['file']}:{row['line']} {row['test']}: {row['cause']} "
                        f"({row['seconds']:.3f}s, {row['instruction_budget']} instructions)"
                        for row in eligible if not row['ran'] or not row['passed']]
            findings += [f"{row['test']}: native-only law was counted as a pass"
                         for row in rows if not row['applicable']
                         and (not row['native_only'] or row['passed'] is not None)]
            self.assertFalse(findings, '\n' + '\n'.join(findings))

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Laws))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
