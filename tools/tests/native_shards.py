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
             patch.dict(corpus['native'].__globals__, heap_costs=lambda: {'conformance/': (1, 0.006, 60.0, '')}), \
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

    def test_heap_parts_are_sized_by_a_probe_or_a_ledger_row(self):
        # Round one bounds whole campaigns at 480 s of the worst planned cost a
        # site: checkpoint's ledger row says 7 ms, STORE-11-new-run has none
        # and is planned at its rule's worst row, 165 ms: int(480 / 0.165) =
        # 2909 sites. Checkpoint's current row (7 ms, clean 60 s) sizes it
        # directly: ceil(148,802 x 0.007 / (480 - 60)) = 3 parts, no probe.
        # New-run's 3000 sites have no row, so a probe of part 1 of 3000 // 400
        # = 7 runs first; it measures quarters of 0.1, 0.1, 0.2 and 0.3 s a site
        # and a clean run of 120 s, so the remaining 3000 x 6/7 = 2571.4 sites
        # are sized at the worst quarter: ceil(2571.4 x 0.3 / 360) = 3 parts
        # (the mean, 0.175 s, would give 2), each skipping the probe's part
        # 1/7. The milestone's shares are recorded with the invocation's
        # verdict: a missing part records them failed.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        args = SimpleNamespace(slices=None, parts=None, config=['smoke'], heap_parts=1, report_json=None)
        store = 'conformance/STORE/'
        checkpoint, new = store + 'STORE-11-checkpoint-run.hir', store + 'STORE-11-new-run.hir'
        costs = {store + 'STORE-11-objects-world-run.hir': (511_376, 0.165, 150.0, ''),
                 'conformance/SEM/SEM-1-': (1, 9.0, 10.0, '')}
        measured = {checkpoint: (148_802, 0.007, 60.0, '2026-11-12T00:00:00Z')}
        name = 'x86_64/regions/elide/reuse/flat-walk/heap+inject'
        report = dict(configuration_attempts=3, baseline_executable=1, baseline_minus_native_unlisted=[],
                      stale_exclusions=[])
        def shard(i, **fields):
            return {f'gate-native-selected-shard-{i}': json.dumps(dict(report, **fields), indent=2)}
        planned = {**shard(1, heap_sites_max=2909, heap_parted=[dict(case=checkpoint, configuration=name, sites=148_802)]),
                   **shard(2, heap_sites_max=2909, heap_parted=[dict(case=new, configuration=name, sites=3000)])}
        strata = [[100, 10.0], [100, 10.0], [100, 20.0], [100, 30.0]]
        probe = shard(1, configuration_attempts=1, heap_partition=dict(index=1, count=7), heap_campaigns=[dict(
            case=new, configuration=name, sites=3000, clean_seconds=120.0, campaign=dict(
                partition=dict(index=1, count=7, skip=None), seconds=190.0, strata=strata, fired_seconds=0.0))])
        def parts(count, skip=None):
            return {k: v for index in range(1, count + 1) for k, v in shard(
                index, configuration_attempts=1, heap_partition=dict(index=index, count=count),
                **(dict(heap_skip=dict(index=skip[0], count=skip[1])) if skip else {})).items()}
        commit = '000000000005' + '0' * 28
        for missing, expected, verdict in [(None, 0, 'pass'), ('gate-native-selected-shard-2', 1, 'fail')]:
            calls = []
            rest = {k: v for k, v in parts(3, (1, 7)).items() if k != missing}
            with tempfile.TemporaryDirectory() as tmp:
                ledger = Path(tmp) / 'milestones.tsv'
                with patch.dict(corpus['native'].__globals__, heap_costs=lambda: costs,
                                cost_ledger=lambda: Path(tmp) / 'costs.tsv',
                                measured_costs=lambda runs, _: {r: measured[r] for r in runs if r in measured},
                                record_costs=lambda *_: None), \
                     patch.object(corpus['event_logs'], 'recorded', return_value=contextlib.nullcontext(Path('/tmp/event'))), \
                     patch.object(corpus['subprocess'], 'run'), \
                     patch.object(corpus['subprocess'], 'check_output', return_value=commit + '\n'), \
                     patch.object(corpus['subprocess'], 'call', side_effect=lambda command, **_: calls.append(command) or 0), \
                     patch.object(corpus['conformance'], 'shard_outputs',
                                  side_effect=[planned, probe, parts(3), rest]), \
                     contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(corpus['native']([checkpoint, new], [checkpoint, new], args), expected)
            def configured(call):
                return {arg.split('=', 1)[0][len('hirsel.native_paths'):]: arg.split('=', 1)[1]
                        for arg in call if arg.startswith('hirsel.native_paths')}
            def targets(call):
                return [arg.rsplit('-', 1)[1] for arg in call if arg.startswith('//:gate-native-selected-shard-')]
            self.assertEqual(configured(calls[0])['_heap_sites_max'], '2909')
            rounds = [(configured(call)[''], configured(call)['_heap_parts'], configured(call)['_heap_skip'],
                       targets(call)) for call in calls[1:]]
            self.assertEqual(rounds, [(new, '7', '', ['1']), (checkpoint, '3', '', ['1', '2', '3']),
                                      (new, '3', '1/7', ['1', '2', '3'])])

    def test_a_cost_ledger_row_retires_when_its_case_or_the_store_sources_change(self):
        # A row a run recorded is current until its case file or a file under
        # STORE_SOURCES changes; restoring them makes it current again, and of
        # two current rows the later wins.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            case = root / 'conformance/STORE/STORE-1-a-run.hir'
            source = root / 'crates/hirsel-store-authority/src/lib.rs'
            for path, text in [(case, 'case\n'), (source, 'store\n'), (root / 'stdlib/std@1/a.hir', 'std\n')]:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            run, ledger = 'conformance/STORE/STORE-1-a-run.hir@2', root / 'costs.tsv'
            def campaign(cost):
                return [dict(case=run, configuration='c', sites=400, clean_seconds=30.0, campaign=dict(
                    partition=None, seconds=30.0 + 400 * cost, strata=[[100, 100 * cost]] * 4, fired_seconds=0.0))]
            with patch.dict(corpus['measured_costs'].__globals__, ROOT=root):
                corpus['record_costs'](campaign(0.5), 'c1', ledger)
                corpus['record_costs'](campaign(0.25), 'c2', ledger)
                current = lambda: {r: row[:3] for r, row in corpus['measured_costs']([run], ledger).items()}
                self.assertEqual(current(), {run: (400, 0.25, 30.0)})
                for path, changed in [(source, 'store 2\n'), (case, 'case 2\n')]:
                    original = path.read_text()
                    path.write_text(changed)
                    self.assertEqual(current(), {})
                    path.write_text(original)
                    self.assertEqual(current(), {run: (400, 0.25, 30.0)})

    def test_plan_separates_measured_from_estimated_and_warns_first_when_estimates_dominate(self):
        # With no configuration seconds: A (ledger, 900 sites at 4 s, clean
        # 360 s) and B (the same figures from a heap-costs row) fit
        # sites_max 1000 whole: clean twice plus 3600 s, 1.20 h each. C's
        # 4000 sites at 0.9 s (clean 360 s) are estimated and over the bound:
        # round one's clean run, a probe of 4000 / 10 = 400 sites (360 + 360
        # s), and the other 3600 sites in ceil(3240 / 120) = 27 parts each
        # running the clean run: 360 + 720 + 9720 + 3240 = 14,040 s = 3.90 h.
        # Estimated 5.10 of 6.30 CPU-h is 81%, over 25%: the first line warns.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        now = corpus['datetime'].datetime(2026, 11, 12, 2, tzinfo=corpus['datetime'].timezone.utc)
        a, b, c = 'conformance/X/X-1-a-run.hir', 'conformance/X/X-1-b-run.hir', 'conformance/X/X-1-c-run.hir'
        costs = {b: (900, 4.0, 360.0, ''), c: (4000, 0.9, 360.0, '')}
        measured = {a: (900, 4.0, 360.0, '2026-11-12T00:00:00Z')}
        def plan(runs):
            out = io.StringIO()
            with patch.dict(corpus['plan'].__globals__, NATIVE_CONFIG_SECONDS=0), contextlib.redirect_stdout(out):
                self.assertEqual(corpus['plan'](runs, costs, measured, 1000, now), 0)
            return out.getvalue().splitlines()
        self.assertEqual(plan([a, b, c]), [
            'plan: WARNING: 81% of 6.30 CPU-h is estimated from native-tests/regressions/heap-costs.tsv rows, '
            'not measured by a run',
            'plan: X-1-a-run.hir: 1 runs, 1.20 CPU-h, measured (ledger, 2.0 h old)',
            'plan: X-1-b-run.hir: 1 runs, 1.20 CPU-h, estimated (native-tests/regressions/heap-costs.tsv row; '
            'probes 0.00 CPU-h)',
            'plan: X-1-c-run.hir: 1 runs, 3.90 CPU-h, estimated (native-tests/regressions/heap-costs.tsv row; '
            'probes 0.20 CPU-h)',
            'plan: 3 runs, 6.30 CPU-h: 1.20 measured, 5.10 estimated'])
        self.assertEqual(plan([a])[0], 'plan: X-1-a-run.hir: 1 runs, 1.20 CPU-h, measured (ledger, 2.0 h old)')



    def test_corpus_refuses_a_selection_whose_slices_outrun_the_budget(self):
        # 100 cases under their full-matrix covers fit 512 slices only as five
        # parts per case: four cover sixteenths (about 924 s) per slice.
        corpus = runpy.run_path(str(Path(__file__).resolve().parents[1] / 'corpus'))
        args = SimpleNamespace(slices=None, parts=None, config=[], heap_parts=1)
        runs = [f'conformance/OWN/OWN-1-{i}-run.hir' for i in range(100)]
        with patch.dict(corpus['native'].__globals__, heap_costs=lambda: {'conformance/': (1, 0.006, 60.0, '')}), \
             patch.object(corpus['subprocess'], 'run') as run, patch.object(corpus['subprocess'], 'call') as call, \
             contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(corpus['native'](['conformance/OWN'], runs, args), 2)
        self.assertEqual([c.args[0] for c in run.call_args_list if c.args and c.args[0][0] != 'git'], [])
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
