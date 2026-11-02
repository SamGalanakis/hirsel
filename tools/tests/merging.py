#!/usr/bin/env python3
"""Corpus ratchets accept real lane append shapes without losing removals."""
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'buck2'))
import merging


class PathLists(unittest.TestCase):
    def test_unsorted_appends_next_to_comments_and_twin_rows_keep_both_lanes(self):
        base = 'conformance/Z/existing.hir\n# new lane cases\n'
        a = base + 'conformance/B/ok.hir\nconformance/B/bad.hir\n'
        b = base + 'conformance/A/ok.hir\nconformance/A/bad.hir\n'
        for left, right in [(a, b), (b, a)]:
            text = merging.resolve('conformance/passing.txt', base, left, right)
            self.assertIsNotNone(text)
            self.assertEqual(set(text.splitlines()), set((a + b).splitlines()))
            entries = [line for line in text.splitlines() if not line.startswith('#')]
            self.assertEqual(entries, sorted(entries))

    def test_removal_is_not_resurrected_when_other_lane_adds(self):
        base = 'conformance/A/old.hir\nconformance/Z/keep.hir\n'
        a = 'conformance/Z/keep.hir\nconformance/B/new.hir\n'
        b = base + '# append\nconformance/C/new.hir\n'
        text = merging.resolve('conformance/passing.txt', base, a, b)
        self.assertNotIn('old.hir', text)
        self.assertIn('conformance/B/new.hir', text)
        self.assertIn('conformance/C/new.hir', text)

    def test_diverging_expectations_and_delete_versus_edit_stay_conflicts(self):
        base = 'CAP/a.hir # old reason\n'
        a, b = 'CAP/a.hir # new reason\n', 'CAP/a.hir # other reason\n'
        for left, right in [(a, b), ('', b)]:
            self.assertIsNone(merging.resolve('crates/hirsel-authority/src/corpus/pending.txt',
                                              base, left, right))

    def test_duplicate_or_non_path_entries_are_never_hidden(self):
        row = 'conformance/A/new.hir\n'
        for invalid in [row + row, '../outside.hir\n', 'not a path\n']:
            self.assertIsNone(merging.resolve('conformance/passing.txt', '', invalid, row))


class LineSets(unittest.TestCase):
    def test_every_row_obeys_three_way_set_membership(self):
        # Exhaust all eight base/ours/theirs membership combinations, including
        # deletion against unchanged, shared additions and both deletions.
        from itertools import product
        combinations = list(product([False, True], repeat=3))
        texts = [''.join(f'row{i}\n' for i, flags in enumerate(combinations) if flags[side])
                 for side in range(3)]
        expected = {f'row{i}' for i, (base, ours, theirs) in enumerate(combinations)
                    if (base and ours and theirs) or (ours and not base) or (theirs and not base)}
        self.assertEqual(set(merging.resolve('crates/example/tests/it/modules.rs', *texts).splitlines()), expected)

    def test_unsorted_lines_keep_order_and_normalize_missing_final_newline(self):
        self.assertEqual(merging.resolve('crates/example/tests/it/modules.rs', 'R\nkeep',
                                         'keep\nzed', 'R\nkeep\nalpha'), 'keep\nzed\nalpha\n')


class Ledgers(unittest.TestCase):
    def test_unchanged_side_uses_the_other_sides_row_or_deletion(self):
        cases = [
            ('id\told\n', 'id\told\n', 'id\ttheirs\n', 'id\ttheirs\n'),
            ('id\told\n', 'id\tours\n', 'id\told\n', 'id\tours\n'),
            ('id\told\n', 'id\told\n', '', ''),
            ('id\told\n', '', 'id\told\n', ''),
            ('', 'id\tours\n', '', 'id\tours\n'),
            ('', '', 'id\ttheirs\n', 'id\ttheirs\n'),
        ]
        for base, ours, theirs, expected in cases:
            with self.subTest(base=base, ours=ours, theirs=theirs):
                self.assertEqual(merging.resolve('tools/known-failures.tsv', base, ours, theirs), expected)

    def test_both_added_rows_keep_the_newer(self):
        self.assertEqual(
            merging.resolve(
                'tools/known-failures.tsv',
                '',
                'same\tolder\tmain\t2026-10-30\n',
                'same\tnewer\tticket\t2026-11-01\n',
            ),
            'same\tnewer\tticket\t2026-11-01\n',
        )

    def test_diverging_edits_keep_the_newer(self):
        base = 'same\tbase\towner\t2026-10-31\n'
        ours = 'same\tnewer\tmain\t2026-11-01\n'
        theirs = 'same\tolder\tticket\t2026-10-30\n'
        self.assertEqual(
            merging.resolve('tools/known-failures.tsv', base, ours, theirs),
            'same\tnewer\tmain\t2026-11-01\n',
        )

    def test_concurrent_edit_and_delete_keep_the_edit_in_either_direction(self):
        base = 'id\tbase\towner\t2026-10-30\n'
        edited = 'id\tchanged\towner\t2026-11-01\n'
        for ours, theirs in [('', edited), (edited, '')]:
            with self.subTest(ours=ours, theirs=theirs):
                self.assertEqual(merging.resolve('tools/known-failures.tsv', base, ours, theirs), edited)

    def test_ours_comments_and_sorted_rows_are_normalized(self):
        base = '# base header\nz\tbase\towner\t2026-10-30\n'
        ours = '# ours first\n\n# ours second\nz\tbase\towner\t2026-10-30\nq\tours\towner\t2026-10-30'
        theirs = '# theirs header\nz\tbase\towner\t2026-10-30\na\ttheirs\towner\t2026-10-30'
        self.assertEqual(
            merging.resolve('tools/known-failures.tsv', base, ours, theirs),
            '# ours first\n# ours second\na\ttheirs\towner\t2026-10-30\n'
            'q\tours\towner\t2026-10-30\nz\tbase\towner\t2026-10-30\n',
        )

    def test_same_identity_added_twice_keeps_the_newer_row(self):
        # Two lanes recording one red must leave a single row: the merge keeps
        # the row with the newer date, whichever side holds it.
        self.assertEqual(
            merging.resolve(
                'tools/known-failures.tsv',
                '',
                'same\tnewer\tlane-a\t2026-11-01\n',
                'same\tolder\tlane-b\t2026-10-30\n',
            ),
            'same\tnewer\tlane-a\t2026-11-01\n',
        )

    def test_duplicate_ids_in_any_input_stay_conflicts(self):
        duplicate = 'id\tfirst\towner\t2026-10-30\nid\tsecond\towner\t2026-11-01\n'
        cases = [
            (duplicate, '', ''),
            ('', duplicate, ''),
            ('', '', duplicate),
        ]
        for base, ours, theirs in cases:
            with self.subTest(base=base, ours=ours, theirs=theirs):
                self.assertIsNone(merging.resolve('tools/known-failures.tsv', base, ours, theirs))


if __name__ == '__main__':
    unittest.main()
