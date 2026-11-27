#!/usr/bin/env python3
"""Check that each Store declaration due by a lane landed in its assigned module."""

import argparse
from collections import defaultdict
import csv
from pathlib import Path
import sys

from store_public_contract import declarations


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('lane', type=int, choices=range(1, 15))
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument('--lower', action='append', default=[], metavar='NAME=MODULE',
                        help='a lower-owner move recorded for this lane under MODULES.md §3.5')
    args = parser.parse_args()
    overrides = {}
    for value in args.lower:
        name, _, module = value.partition('=')
        if not name or not module or name in overrides:
            parser.error(f'invalid lower-owner move {value!r}')
        overrides[name] = module

    actual = defaultdict(set)
    for _, module, item, _ in declarations(args.root, include_tests=True):
        actual[item['name']].add(module)

    errors = []
    with Path(__file__).with_name('placement.tsv').open() as inventory:
        for row in csv.DictReader(inventory, delimiter='\t'):
            landing = row['landing']
            if not landing.startswith('L') or int(landing[1:]) > args.lane:
                continue
            name = row['name']
            expected = overrides.pop(name, row['v31_owner'])
            owners = sorted(actual[name])
            if expected not in owners:
                errors.append(f'{name} belongs in {expected}, found {", ".join(owners) or "no declaration"}')
    for name in overrides:
        errors.append(f'{name} is absent from the placement inventory through L{args.lane}')
    for error in errors:
        print(error, file=sys.stderr)
    return bool(errors)


if __name__ == '__main__':
    sys.exit(main())
