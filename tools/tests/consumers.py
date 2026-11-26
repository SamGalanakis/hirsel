#!/usr/bin/env python3
"""Consumer selection follows real launch bodies, source uses and shard assignment."""
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader('consumers', str(ROOT / 'tools/consumers'))
consumers = importlib.util.module_from_spec(importlib.util.spec_from_loader('consumers', loader))
loader.exec_module(consumers)


class Consumers(unittest.TestCase):
    def test_direct_sources_precede_transitive_shards_and_data_does_not_own_sources(self):
        path = 'crates/hirsel-check/src/lib.rs'
        attrs = {'root//:hirsel-check-unittest-harness': {'srcs': ['root///' + path]},
                 'root//:ships-source-as-data': {}}
        with patch.object(consumers, 'buck', return_value=json.dumps(attrs)):
            owners = consumers.direct_owners(ROOT, [path])
        self.assertEqual(owners, {'root//:hirsel-check-unittest-harness': 'direct source owner'})
        graph = {'root//:a-transitive': {'test_binary': 'a-transitive'},
                 'root//:hirsel-check-unittest': {'test_binary': 'hirsel-check-unittest'}}
        reasons = {}
        selected = consumers.select(ROOT, [path], graph, {}, owners, reasons)
        self.assertEqual(selected[0], 'root//:hirsel-check-unittest')
        self.assertEqual(reasons[selected[0]], 'direct source owner')
        self.assertEqual(reasons['root//:a-transitive'], 'transitive shard')

    def test_changed_test_input_owns_its_harness(self):
        target = 'root//:hirsel-check-test-law'
        graph = {target: {'test_binary': 'hirsel-check-test-law'}}
        reasons = {}
        selected = consumers.select(ROOT, ['crates/hirsel-check/tests/law.rs'], graph, {},
                                    {target + '-harness'}, reasons)
        self.assertEqual(selected, [target])
        self.assertEqual(reasons[target], 'direct test owner')

    def test_tool_consumers_keep_graph_dependent_scripts(self):
        graph = {'root//:direct-script': {'script': 'root///tools/tests/consumers.py'},
                 'root//:dependent-script': {'script': 'root///tools/tests/buck2_sync.py'},
                 'root//:direct': {'harness': 'root//:direct-script'},
                 'root//:dependent': {'harness': 'root//:dependent-script'}}
        reasons = {}
        selected = consumers.select(ROOT, ['tools/tests/consumers.py', 'tools/consumers'], graph, {},
                                    {'root//:direct-script': 'direct test owner'}, reasons)
        self.assertEqual(selected, ['root//:direct', 'root//:dependent'])
        self.assertEqual(reasons['root//:dependent'], 'transitive shard')

    def test_data_dependency_resolves_a_harness_outside_the_reverse_graph(self):
        target = 'root//:dependent'
        graph = {target: {'harness': target + '-script'}}
        harness = {target + '-script': {'script': 'root///tools/tests/buck2_sync.py'}}
        with patch.object(consumers, 'buck', side_effect=[json.dumps(graph), json.dumps(harness)]):
            found = consumers.test_graph(ROOT, ['owner("tools/buck2/sync.py")'])
        selected = consumers.select(ROOT, ['tools/buck2/sync.py'], found, {})
        self.assertEqual(selected, [target])

    def test_exported_files_own_their_exporting_target(self):
        path = 'tools/buck2/native-toolchain/link_kit.py'
        target = 'root//tools/buck2/native-toolchain:link_kit.py'
        with patch.object(consumers, 'buck', return_value=json.dumps({target: {'src': 'root//' + path}})):
            owners = consumers.direct_owners(ROOT, [path])
        self.assertEqual(owners, {target: 'direct source owner'})

    def test_darwin_laws_follow_archive_inputs_but_not_unrelated_crates(self):
        law = 'root//:hirsel-core-rt-archives-darwin-sync-ban'
        graph = {'root//:hirsel-core-rt': {},
                 'root//:hirsel-core-rt-archives': {'buck.deps': ['root//:hirsel-core-rt']},
                 law: {'buck.type': '_darwin_sync_ban',
                       'archives': 'root//:hirsel-core-rt-archives',
                       'paths': ['aarch64-apple-darwin/libhirsel_core_rt.a'],
                       'buck.deps': ['root//:hirsel-core-rt-archives']}}
        selected = consumers.select(ROOT, ['crates/hirsel-core-rt/src/abi.rs'], graph, {},
                                    {'root//:hirsel-core-rt': 'direct source owner'})
        self.assertIn(law, selected)
        selected = consumers.select(ROOT, ['crates/hirsel-core-syntax/src/lexer.rs'], graph, {},
                                    {'root//:hirsel-core-syntax': 'direct source owner'})
        self.assertNotIn(law, selected)

    def test_the_cap_keeps_the_nearest_owner_and_reports_the_cut(self):
        # place-store-681: 100 of 4,261 kept by name, with no word on what was cut.
        graph = {'root//:lib': {}, 'root//:mid': {'buck.deps': ['root//:lib']},
                 'root//:z-near-harness': {'buck.deps': ['root//:lib']},
                 'root//:a-far-harness': {'buck.deps': ['root//:mid']}}
        for name in ('z-near', 'a-far'):
            graph[f'root//:{name}'] = dict(test_binary=name, harness=f'root//:{name}-harness',
                                           **{'buck.deps': [f'root//:{name}-harness']})
        cut = []
        selected = consumers.select(ROOT, ['crates/hirsel-check/src/lib.rs'], graph, {},
                                    {'root//:lib': 'direct source owner'}, {}, 1, cut)
        self.assertEqual((selected, cut), (['root//:z-near'], ['root//:a-far']))





if __name__ == '__main__':
    unittest.main()
