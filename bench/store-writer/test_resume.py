"""Resume must never label saved measurements with a different source/workload."""
import copy
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader('store_writer_run', str(Path(__file__).with_name('run')))
spec = importlib.util.spec_from_loader(loader.name, loader)
runner = importlib.util.module_from_spec(spec)
loader.exec_module(runner)


class ResumeTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.out = self.root / 'evidence'
        self.out.mkdir()
        self.workload = dict(accounts=8, initial_balance=1000000, transfers=512, amount=3,
                             groups=[1, 8], submitters=[1, 4],
                             mix='round_robin_to_next_amount_1_2_3', durability='Durable')
        (self.root / 'workload.json').write_text(json.dumps(self.workload))
        self.cells = {}
        for group in [1, 8]:
            for clients in [1, 4]:
                self.cell('metal', 'log-host', group, clients)
                for cell in runner.CELLS:
                    for kind in ['native', 'served', 'interp'] if cell == 'log-host' else ['native', 'interp']:
                        self.cell(kind, cell, group, clients)
                        if kind in ('native', 'served') and cell == 'log-host':
                            self.cell(kind, cell, group, clients, allocations=True)
        self.cell('raw', 'log-host', 1, 1, workload=dict(self.workload, groups=[1], submitters=[1]))
        for kind in ['native', 'interp']:
            self.cell(kind, 'log-host', 1, 1, profile=True)

    def cell(self, kind, cell, group, clients, profile=False, allocations=False, workload=None):
        suffix = '-profile' if profile else '-allocations' if allocations else ''
        path = self.out / f'{kind}-{cell}-g{group}-c{clients}{suffix}' / 'result.json'
        path.parent.mkdir()
        row = dict(binding=dict(source_sha='measured-revision', workload=workload or self.workload,
                                engine=kind, cell=cell, group=group, submitters=clients,
                                profile=profile, allocations=allocations),
                   allocations_commit=None if kind in ('native', 'served') and cell == 'log-host' and not allocations else 12,
                   instructions_commit=100, transfers_s=1000)
        path.write_text(json.dumps(row))
        self.cells[path] = row

    def resume(self):
        with patch.object(runner, 'HERE', self.root), \
             patch.object(runner.sys, 'argv', ['run', '--output', str(self.out), '--compiler',
                                              str(self.root / 'compiler'), '--skip-build', '--resume', '--profile',
                                              '--engines', 'metal,native,served,interp']), \
             patch.object(runner.os, 'sched_getaffinity', return_value={0, 1}), \
             patch.object(runner.os, 'sched_setaffinity'), \
             patch.object(runner.subprocess, 'check_output', return_value='measured-revision\n'), \
             patch.object(runner.subprocess, 'call', return_value=0), \
             patch.object(runner, 'native_allocation_probes') as probes, \
             patch.object(runner, 'sample', side_effect=AssertionError('must reuse bound cells')):
            probes.return_value.__enter__.return_value = ['allocation-event']
            runner.main()

    def test_matching_cells_keep_their_measured_revision(self):
        self.resume()
        summary = json.loads((self.out / 'results.json').read_text())
        self.assertEqual(summary['source_sha'], 'measured-revision')
        self.assertEqual(len(summary['rows']), 41)
        for row in summary['rows']:
            self.assertEqual(row['binding']['source_sha'], 'measured-revision')
        self.assertEqual(summary['rows'][1]['allocations_commit'], 12)

    def test_each_cell_refuses_foreign_source_workload_and_legacy_evidence(self):
        for path, original in self.cells.items():
            for defect in ['revision', 'workload', 'legacy', 'mode']:
                with self.subTest(cell=path.parent.name, defect=defect):
                    row = copy.deepcopy(original)
                    if defect == 'revision':
                        row['binding']['source_sha'] = 'older-revision'
                    elif defect == 'workload':
                        row['binding']['workload']['accounts'] += 1
                    elif defect == 'mode':
                        row['binding']['profile'] = not row['binding']['profile']
                    else:
                        del row['binding']
                    encoded = json.dumps(row)
                    path.write_text(encoded)
                    with self.assertRaisesRegex(RuntimeError, 'binding differs or is missing'):
                        self.resume()
                    self.assertEqual(path.read_text(), encoded)
                    self.assertFalse((self.out / 'results.json').exists())
                    path.write_text(json.dumps(original))


if __name__ == '__main__':
    unittest.main()
