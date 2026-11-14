#!/usr/bin/env python3
"""Gate every evaluator-eligible law through the existing contract walker."""
import argparse
import json
from pathlib import Path
import unittest

from std_native import ROOT, discover, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evaluator', type=Path, required=True)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--filter', action='append', default=[])
    # The combined core gate asks all harnesses for serial bodies. This harness
    # has one unittest body; accept exactly that same serial setting.
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()

    class Laws(unittest.TestCase):
        def test_every_evaluator_eligible_law(self):
            root = args.root.resolve()
            contracts = discover(root, args.filter)
            owners = json.loads(Path(__file__).with_name('std_native_owners.json').read_text())
            rows = run(root, None, None, contracts, ['evaluator'], None, owners,
                       evaluator=args.evaluator.resolve())
            eligible = [row for row in rows if row['applicable']]
            self.assertTrue(eligible, 'zero evaluator-eligible laws selected')
            findings = [f"{row['file']}:{row['line']} {row['test']}: {row['cause']}"
                        for row in eligible if not row['ran'] or not row['passed']]
            findings += [f"{row['test']}: native-only law was counted as a pass"
                         for row in rows if not row['applicable']
                         and (not row['native_only'] or row['passed'] is not None)]
            self.assertFalse(findings, '\n' + '\n'.join(findings))

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(Laws))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
