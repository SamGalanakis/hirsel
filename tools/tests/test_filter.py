#!/usr/bin/env python3
"""Filtered gates follow live case identities through cost and width ownership."""
from pathlib import Path
from contextlib import contextmanager
from types import SimpleNamespace
import runpy
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


class FilterOwners(unittest.TestCase):
    def setUp(self):
        self.tool = runpy.run_path(str(ROOT / 'tools/test-filter'))
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        (self.root / 'tools/buck2').mkdir(parents=True)
        (self.root / 'tools/buck2/test_requirements.bzl').write_text('EXACT_CASE_CPUS = {}\n')
        (self.root / 'costs.tsv').write_text('other\t9\nx\t1\n')
        self.graph = {f'root//:suite-shard-{i + 1}': dict(test_binary='suite', shard_index=i,
                      shards=2, costs='root//:costs.tsv', exact_cpus=0) for i in range(2)}
        self.globals = self.tool['plan'].__globals__
        self.patcher = patch.dict(self.globals, ROOT=self.root)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)

    def test_renamed_case_leaves_its_stale_cost_row_and_moves_to_its_live_owner(self):
        # Greedy costs put x on 2. A renamed z has no row and hashes as ASCII
        # 122, so its two-way owner is 1. The stale x row must not admit x.
        for live, target in [('x', 'root//:suite-shard-2'), ('z', 'root//:suite-shard-1')]:
            with self.subTest(live=live), patch.dict(self.globals,
                    graph_for=lambda *_: self.graph,
                    listing=lambda *_: {live}):
                targets, cases = self.tool['plan'](['suite'], [], [live, '--include-ignored'])
                self.assertEqual(targets, [target])
                self.assertEqual(cases, {'suite': {live}})
        with patch.dict(self.globals, graph_for=lambda *_: self.graph, listing=lambda *_: set()):
            with self.assertRaisesRegex(ValueError, 'matched no runnable test bodies'):
                self.tool['plan'](['suite'], [], ['x', '--include-ignored'])

    def test_host_required_body_has_one_host_owner_even_under_a_positive_filter(self):
        host = 'root//:suite-openat2-1'
        self.graph[host] = dict(test_binary='suite', labels=['requires-openat2'],
                                extra_env={'HIRSEL_TEST_DEDICATED_CASE': 'x'})
        with patch.dict(self.globals, graph_for=lambda *_: self.graph,
                        listing=lambda _b, _g, _c, args: set() if '--ignored' in args else {'x'}):
            targets, cases = self.tool['plan'](['suite'], [], ['x'])
        self.assertEqual(targets, [host])
        self.assertEqual(cases, {'suite': {'x'}})
        homes = self.tool['owners'](self.graph, {'suite': {'x', 'z'}})
        self.assertEqual(homes[('suite', 'x')], host)
        self.assertNotEqual(homes[('suite', 'z')], host)

    def test_width_owner_and_substring_selection_cover_each_live_case_once(self):
        self.graph['root//:suite-cpus-4-shard-1'] = dict(test_binary='suite', shard_index=0,
                                                       shards=1, costs='root//:costs.tsv', exact_cpus=4)
        homes = self.tool['owners'](self.graph, {'suite': {'x', 'z'}}, {'suite': {'z': 4}})
        self.assertEqual(homes, {('suite', 'x'): 'root//:suite-shard-2',
                                ('suite', 'z'): 'root//:suite-cpus-4-shard-1'})

    def test_unmeasured_deal_keeps_ignored_positive_matches_but_not_skip_only_matches(self):
        for attrs in self.graph.values():
            attrs['costs'] = None
        def listing(_binary, _graph, _config, arguments):
            return {'prefix_a'} if '--ignored' in arguments else {'prefix_a', 'prefix_z'}
        for arguments, target in [(['prefix_'], 'root//:suite-shard-2'),
                                  (['--skip', 'unrelated'], 'root//:suite-shard-1')]:
            with self.subTest(arguments=arguments), patch.dict(self.globals,
                    graph_for=lambda *_: self.graph, listing=listing):
                targets, cases = self.tool['plan'](['suite'], [], arguments)
                self.assertEqual(targets, [target])
                self.assertEqual(cases, {'suite': {'prefix_z'}})

    def test_discovery_reads_a_closed_inventory_without_a_duplicate_list_flag(self):
        for inventory in (False, True):
            graph = {'root//:suite': {'test_binary': 'suite'}}
            if inventory:
                graph['root//:suite-inventory'] = {'test_binary': ''}
            closed = []
            @contextmanager
            def recorded(_):
                yield self.root / 'events.pb.zst'
                closed.append(True)
            def reports(_):
                self.assertEqual(closed, [True])
                return {'suite': {'details': 'z: test\n'}}
            events = self.globals['event_logs']
            with self.subTest(inventory=inventory), patch.object(events, 'recorded', recorded), \
                    patch.object(events, 'test_results', reports), \
                    patch.object(self.globals['subprocess'], 'run',
                                 return_value=SimpleNamespace(returncode=0)) as run:
                self.assertEqual(self.tool['listing']('suite', graph, [], ['z']), {'z'})
                command = run.call_args.args[0]
                self.assertIn('//:suite-inventory' if inventory else '//:suite', command)
                self.assertEqual(command.count('--test-arg=--list'), 0 if inventory else 1)

    def test_discovery_preserves_literal_filters_and_paired_values(self):
        arguments = ['--exact', '--skip', 'skip value', '--logfile', 'unused log',
                     '--format=json', '--', '--format=literal']
        self.assertEqual(self.tool['listing_arguments'](arguments),
                         ['--format=terse', '--color=never', '--exact', '--skip', 'skip value',
                          '--', '--format=literal'])

    def test_nested_harness_results_on_stderr_are_not_counted_as_outer_bodies(self):
        details = ('---- STDOUT ----\n'
                   'test outer::case ... ok <21.640s>\n'
                   '---- STDERR ----\n'
                   'test outer::case ... ok\n'
                   'test child::other_case ... FAILED\n')
        self.assertEqual(self.tool['completed_bodies'](details), [('outer::case', 'ok')])
        self.assertEqual(self.tool['completed_bodies']('test outer::case ... FAILED\n'),
                         [('outer::case', 'FAILED')])


if __name__ == '__main__':
    unittest.main()
