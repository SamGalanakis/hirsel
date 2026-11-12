#!/usr/bin/env python3
"""Native selections share the gate's cost TSVs and dynamically declared shards."""
import contextlib
import io
import json
import runpy
from types import SimpleNamespace
import tempfile
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'buck2'))
import native
import test_costs
import event_logs


class NativeShards(unittest.TestCase):
    def test_corpus_requires_every_fault_partition_and_its_metadata(self):
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        args = SimpleNamespace(slices=2, parts=1, config=['x86_64/heap+inject'], heap_parts=3, report_json=None)
        runs = ['conformance/STORE/STORE-1-a-run.hir', 'conformance/STORE/STORE-1-b-run.hir']
        outputs = {f'gate-native-selected-shard-{i}': json.dumps(dict(
            configuration_attempts=1, baseline_executable=1,
            baseline_minus_native_unlisted=[], stale_exclusions=[],
            heap_partition=dict(index=(i-1)//2+1, count=3)), indent=2) for i in range(1, 7)}
        with patch.object(corpus['event_logs'], 'recorded', return_value=contextlib.nullcontext(Path('/tmp/event'))), \
             patch.object(corpus['subprocess'], 'run'), patch.object(corpus['subprocess'], 'call', return_value=0), \
             patch.object(corpus['conformance'], 'shard_outputs', return_value=outputs), \
             patch.dict(corpus['native'].__globals__, heap_costs=lambda: {'conformance/': (1, 0.006, '')}), \
             contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(corpus['native'](['conformance/STORE'], runs, args), 0)
            last = outputs.pop('gate-native-selected-shard-6')
            self.assertEqual(corpus['native'](['conformance/STORE'], runs, args), 1)
            report = json.loads(last)
            report['heap_partition']['index'] = 2
            outputs['gate-native-selected-shard-6'] = json.dumps(report, indent=2)
            self.assertEqual(corpus['native'](['conformance/STORE'], runs, args), 1)
            del report['heap_partition']
            outputs['gate-native-selected-shard-6'] = json.dumps(report, indent=2)
            self.assertEqual(corpus['native'](['conformance/STORE'], runs, args), 1)

    def test_heap_parts_are_sized_by_measured_cost_and_a_rotated_family_injects_one_open_share(self):
        # Costs a site: checkpoint 7 ms (measured), objects-world 165 ms (the
        # worst of rule STORE-11), SEM-1 9 s (worse, another rule), nodes 18 ms
        # in 10 shares. Round one bounds whole campaigns at 480 s of the worst
        # planned cost: int(480 / 0.165) = 2909 sites. STORE-11-new-run has no
        # row, so it is planned at its rule's worst, 3000 x 0.165 / 480 -> 2
        # parts (at 6 ms or the 9 s overall worst it would be 1 or 57);
        # checkpoint 148,802 x 0.007 / 480 -> 3 parts. Nodes injects one share:
        # 300,000 / 10 x 0.018 / 480 -> 2 parts. Its ledger has shares 1, 2 and
        # 4 passed in this cycle, so the open shares are 3, 5, 6, 7, 8, 9, 10;
        # commit 0x000000000005... picks open share 5 % 7 = index 5: share 9.
        # Its two parts are the stride partitions 9/20 and 19/20, sites
        # congruent to 8 mod 10 (share 9's), and the run records 9/10 with its
        # verdict; a missing part records a fail, and a selection without
        # every run of the family records nothing.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        args = SimpleNamespace(slices=None, parts=None, config=['smoke'], heap_parts=1, report_json=None)
        store = 'conformance/STORE/'
        checkpoint, new = store + 'STORE-11-checkpoint-run.hir', store + 'STORE-11-new-run.hir'
        nodes, family = store + 'STORE-11-checkpoint-nodes-crash-run.hir@3', store + 'STORE-11-checkpoint-nodes-'
        costs = {checkpoint: (148_802, 0.007, ''), store + 'STORE-11-objects-world-run.hir': (511_376, 0.165, ''),
                 'conformance/SEM/SEM-1-': (1, 9.0, ''), family: (69_062, 0.018, '')}
        name = 'x86_64/regions/elide/reuse/flat-walk/heap+inject'
        report = dict(configuration_attempts=3, baseline_executable=1, baseline_minus_native_unlisted=[],
                      stale_exclusions=[])
        def shard(i, **fields):
            return {f'gate-native-selected-shard-{i}': json.dumps(dict(report, **fields), indent=2)}
        planned = {**shard(1, heap_sites_max=2909, heap_parted=[dict(case=checkpoint, configuration=name, sites=148_802)]),
                   **shard(2, heap_sites_max=2909, heap_parted=[dict(case=new, configuration=name, sites=3000)]),
                   **shard(3, heap_sites_max=2909, heap_parted=[dict(case=nodes, configuration=name, sites=300_000)])}
        def parts(indices, count):
            return {k: v for i, index in enumerate(indices, 1) for k, v in shard(
                i, configuration_attempts=1, heap_partition=dict(index=index, count=count)).items()}
        commit = '000000000005' + '0' * 28
        for missing, expected, verdict, family_runs in [
                (None, 0, 'pass', [nodes]), ('gate-native-selected-shard-2', 1, 'fail', [nodes]),
                (None, 0, None, [nodes, nodes.replace('@3', '@4')])]:
            calls = []
            rotated = {k: v for k, v in parts([9, 19], 20).items() if k != missing}
            with tempfile.TemporaryDirectory() as tmp:
                ledger = Path(tmp) / 'rotations.tsv'
                ledger.write_text(''.join(f'2026-11-12T00:00:0{n}Z\tc{n}\t{family}\t{n}/10\tpass\n' for n in (1, 2, 4))
                                  + f'2026-11-12T00:00:05Z\tc3\t{family}\t3/10\tfail\n')
                with patch.dict(corpus['native'].__globals__, heap_costs=lambda: costs,
                                heap_rotations=lambda: {family: 10}, rotation_ledger=lambda: ledger,
                                family_runs=lambda key: family_runs), \
                     patch.object(corpus['event_logs'], 'recorded', return_value=contextlib.nullcontext(Path('/tmp/event'))), \
                     patch.object(corpus['subprocess'], 'run'), \
                     patch.object(corpus['subprocess'], 'check_output', return_value=commit + '\n'), \
                     patch.object(corpus['subprocess'], 'call', side_effect=lambda command, **_: calls.append(command) or 0), \
                     patch.object(corpus['conformance'], 'shard_outputs',
                                  side_effect=[planned, parts([1, 2], 2), rotated, parts([1, 2, 3], 3)]), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(corpus['native']([checkpoint, new, nodes], [checkpoint, new, nodes], args), expected)
                last = ledger.read_text().splitlines()[-1].split('\t')[1:]
                self.assertEqual(last, [commit, family, '9/10', verdict] if verdict else ['c3', family, '3/10', 'fail'])
            self.assertIn('hirsel.native_paths_heap_sites_max=2909', calls[0])
            self.assertIn('hirsel.native_paths_heap_parts=2', calls[1])
            self.assertIn(f'hirsel.native_paths={new}', calls[1])
            self.assertIn('hirsel.native_paths_heap_rotation=', calls[1])
            self.assertIn('hirsel.native_paths_heap_parts=2', calls[2])
            self.assertIn(f'hirsel.native_paths={nodes}', calls[2])
            self.assertIn('hirsel.native_paths_heap_rotation=9/10', calls[2])
            self.assertIn('hirsel.native_paths_heap_parts=3', calls[3])
            self.assertIn(f'hirsel.native_paths={checkpoint}', calls[3])

    def test_store_land_rotation_covers_each_family_cut_once_a_cycle(self):
        # Rows: crash-cuts-crash (cuts 1..=17) in thirds, the five objects-cuts
        # kinds (cuts 1..=38 each) in sixths. Commit count n picks third n % 3
        # and the (n // 6) % 5'th kind's sixth n % 6. At n = 6463: third 2 is
        # cuts 2, 5, ..., 17; 6463 // 6 = 1077, 1077 % 5 = 2, the third kind in
        # name order (crash, drop, lose, ...): lose, sixth 2, cuts 2, 8, ..., 38.
        # Over 30 consecutive commits each row selects every cut of its family
        # exactly once, and no two consecutive commits select the same cuts.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        store = 'conformance/STORE/STORE-11-'
        crash, objects = store + 'crash-cuts-crash-run.hir', store + 'objects-cuts-'
        corpus['named_selection'].__globals__['land_rows'] = lambda: [(crash, 3), (objects, 6)]
        cases = corpus['cases'](corpus['ROOT'])

        def chosen(count):
            with patch('subprocess.check_output', return_value=f'{count}\n'), \
                    contextlib.redirect_stderr(io.StringIO()):
                return corpus['named_selection']('store-land', cases, 'HEAD^')

        self.assertEqual(chosen(6463), [f'{crash}@{k}' for k in range(2, 18, 3)]
                         + [f'{objects}lose-run.hir@{k}' for k in range(2, 39, 6)])
        selections = [chosen(n) for n in range(6463, 6493)]
        every = sorted(run for runs in selections for run in runs)
        crashes = [run for run in every if run.startswith(crash)]
        self.assertEqual(crashes, sorted(f'{crash}@{k}' for k in range(1, 18) for _ in range(10)))
        kinds = ['crash', 'drop', 'lose', 'refuse', 'vanish']
        self.assertEqual([run for run in every if run.startswith(objects)],
                         sorted(f'{objects}{kind}-run.hir@{k}' for kind in kinds for k in range(1, 39)))
        self.assertTrue(all(a != b for a, b in zip(selections, selections[1:])))

    def test_corpus_refuses_a_selection_whose_slices_outrun_the_budget(self):
        # 100 cases under their full-matrix covers fit 512 slices only as five
        # parts per case: four cover sixteenths (about 924 s) per slice.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        args = SimpleNamespace(slices=None, parts=None, config=[], heap_parts=1)
        runs = [f'conformance/OWN/OWN-1-{i}-run.hir' for i in range(100)]
        with patch.dict(corpus['native'].__globals__, heap_costs=lambda: {'conformance/': (1, 0.006, '')}), \
             patch.object(corpus['subprocess'], 'run') as run, patch.object(corpus['subprocess'], 'call') as call, \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(corpus['native'](['conformance/OWN'], runs, args), 2)
        run.assert_not_called()
        call.assert_not_called()

    def test_unknown_fifty_are_bounded_and_keep_both_backends(self):
        programs = {f'generated/success-{i}' for i in range(50)}
        self.assertEqual(len(native.targets(programs)), 100)
        parts = native.config(programs)[1].split('=', 1)[1].split(';')
        self.assertEqual(set(parts), programs)
        self.assertEqual(len(parts), len(programs))
        command = native.command(programs)
        self.assertEqual(command[command.index(native.NATIVE_TARGETS[0] + '-shard-1'):], native.targets(programs))

    def test_measured_costs_pack_longest_first_with_the_slower_backend(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tables = root / 'tools/buck2/test-costs'
            tables.mkdir(parents=True)
            (tables / 'gate-native-changed.tsv').write_text('a\t400\nb\t200\nc\t200\nd\t100\nheavy\t850\n')
            (tables / 'gate-native-cranelift-changed.tsv').write_text('d\t300\n')
            parts = native.native_slices({'a', 'b', 'c', 'd', 'heavy', 'unknown'}, root)
            self.assertEqual(parts, [['heavy'], ['unknown'], ['a', 'c'], ['b', 'd']])
            self.assertEqual(sorted(p for part in parts for p in part), ['a', 'b', 'c', 'd', 'heavy', 'unknown'])
            self.assertEqual(parts, native.native_slices(reversed(['a', 'b', 'c', 'd', 'heavy', 'unknown']), root))

    def test_empty_and_single_selection_preserve_existing_target_names(self):
        self.assertEqual(native.targets(set()), [])
        self.assertEqual(native.targets({'x'}), native.NATIVE_TARGETS)

    def test_bad_measurements_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            table = root / 'tools/buck2/test-costs/gate-native-changed.tsv'
            table.parent.mkdir(parents=True)
            for bad in ['0', '-1', 'nan', 'inf']:
                table.write_text(f'a\t{bad}\n')
                with self.subTest(bad=bad), self.assertRaises(ValueError):
                    native.native_slices({'a'}, root)

    def test_native_costs_survive_the_gate_history_round_trip(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'history.tsv'
            event_logs.TsvTestHistory(path).append([dict(
                gate='example', target='gate-native-changed', shard=1,
                duration_seconds=12, date='2026-10-20', status=0,
                case_costs='{"a": 11.85}')])
            self.assertEqual(test_costs.history_costs(path, 'gate-native-changed', ''),
                             {'gate-native-changed': {'a': 11.85}})
        self.assertEqual(event_logs.case_costs('native case\ta\t11.850\t750000'), {'a': 11.85})
        self.assertEqual(event_logs.case_costs('native failed case\ta\t11.850\t750000'), {})

    def test_native_partial_measurements_preserve_unobserved_cases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'gate-native-changed.tsv').write_text('old\t100.000\n')
            log = root / 'output.log'
            log.write_text('✓ Pass: root//:gate-native-changed (12.0s)\n'
                           'native case\tnew\t11.850\t750000\n')
            with patch.object(test_costs, 'TABLES', root), \
                 patch.object(sys, 'argv', ['test_costs.py', str(log)]):
                test_costs.main()
            self.assertEqual(test_costs.read_table(root / 'gate-native-changed.tsv'), {'old': 100, 'new': 11.85})

    def test_native_worker_measurements_are_accepted_by_the_cost_importer(self):
        walls, rows = test_costs.parse([
            '✓ Pass: root//:gate-native-changed-shard-1 (12.0s)',
            'native case conformance/SEM/a.hir 11.850 750000',
            '✓ Pass: root//:gate-native-cranelift-changed-shard-1 (9.0s)',
            'native case\tconformance/SEM/a.hir\t8.800\t600000'])
        self.assertEqual(rows, {'gate-native-changed': {'conformance/SEM/a.hir': 11.85},
                                'gate-native-cranelift-changed': {'conformance/SEM/a.hir': 8.8}})
        self.assertEqual(len(walls), 2)


if __name__ == '__main__':
    unittest.main()
