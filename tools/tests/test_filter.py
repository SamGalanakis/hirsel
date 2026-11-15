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
        self.graph = {'root//:suite-shard-1': dict(test_binary='suite')}
        self.globals = self.tool['plan'].__globals__
        self.patcher = patch.dict(self.globals, ROOT=self.root)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)


    def test_host_required_body_has_one_host_owner_even_under_a_positive_filter(self):
        host = 'root//:suite-openat2-1'
        self.assertEqual(self.tool['family'](host), 'suite')
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
