import copy
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
from unittest.mock import patch
import unittest
HERE = Path(__file__).resolve().parents[1]
sys.path.insert(0,str(HERE))
from suite import analyze, quantity, report


class AcceptanceTests(unittest.TestCase):
    def data(self):
        manifest = json.loads((HERE/'manifest.json').read_text())
        manifest['workloads'] = {'channel':manifest['workloads']['channel']}
        manifest['widths'] = [1]
        manifest['headroom'] = [{'row':'recording','metrics':['cut_pause_ns'],'workloads':['channel']}]
        samples = []
        for provider,mode,elapsed in [('tokio','off',100),('tasks.free@1','off',110),
                ('tasks.free@1','record',115),('tasks.free@1','durable',190),
                ('tasks.cooperative@1','durable',200)]:
            for round_number in range(10):
                samples.append({'provider':provider,'case':'channel','width':1,'mode':mode,
                    'round':round_number,'elapsed_ns':elapsed,'operations':4,'counts':{'instructions':200},
                    'rss_bytes':1000,'throttled_usec':0,
                    'metrics':{'latency_p99_ns':100,'compressed_bytes_per_contended_event':3,'cut_pause_ns':0}})
        return {'samples':samples,'manifest':manifest,'seed':17,'rounds':10,'cpus':[31],'errors':[]}

    def test_all_independent_floors_pass(self):
        self.assertEqual(analyze(self.data())['status'],'pass')

    def test_each_floor_can_fail_independently(self):
        for mode,field,value in [('off','elapsed_ns',200),('record','elapsed_ns',200),
                ('record','latency_p99_ns',121),('record','compressed_bytes_per_contended_event',4.1),
                ('durable','elapsed_ns',201)]:
            with self.subTest(mode=mode,field=field):
                data = self.data()
                for sample in data['samples']:
                    if sample['provider']=='tasks.free@1' and sample['mode']==mode:
                        (sample['metrics'] if field not in sample else sample)[field]=value
                self.assertEqual(analyze(data)['status'],'fail')

    def test_missing_free_is_dependency_owned(self):
        data=self.data(); data['samples']=[s for s in data['samples'] if s['provider']!='tasks.free@1']
        self.assertEqual(analyze(data)['status'],'dependency-owned')
        self.assertIsNone(analyze(data)['floors'][0]['free_vs_tokio'])

    def test_process_timings_never_substitute_for_p99(self):
        data=self.data()
        for s in data['samples']: s['metrics'].pop('latency_p99_ns')
        self.assertEqual(analyze(data)['status'],'dependency-owned')

    def test_whole_tape_size_does_not_substitute_compressed_event_bytes(self):
        data=self.data()
        for s in data['samples']:
            s['tape_bytes']=1
            s['metrics'].pop('compressed_bytes_per_contended_event')
        self.assertEqual(analyze(data)['status'],'dependency-owned')

    def test_missing_headroom_prevents_complete_acceptance(self):
        data=self.data()
        for s in data['samples']: s['metrics'].pop('cut_pause_ns')
        self.assertEqual(analyze(data)['status'],'dependency-owned')

    def test_invalid_telemetry_refuses(self):
        data=self.data(); data['samples'][0]['metrics']['cut_pause_ns']=float('nan')
        with self.assertRaises(ValueError): analyze(data)

    def test_incomplete_rounds_do_not_pass(self):
        data=self.data(); data['samples'].pop()
        self.assertEqual(analyze(data)['status'],'dependency-owned')

    def test_duplicate_rounds_do_not_pass(self):
        data=self.data(); data['samples'].append(copy.deepcopy(data['samples'][0]))
        self.assertEqual(analyze(data)['status'],'dependency-owned')

    def test_all_headroom_metrics_have_owners(self):
        manifest=json.loads((HERE/'manifest.json').read_text())
        owners=json.loads((HERE/'telemetry.json').read_text())['owners']
        self.assertEqual(len(manifest['headroom']),11)
        self.assertTrue({metric for row in manifest['headroom'] for metric in row['metrics']} <= owners.keys())

    def test_workload_oracles(self):
        manifest=json.loads((HERE/'manifest.json').read_text())
        self.assertEqual(quantity(manifest['workloads']['late_set']['checksum'],7),42)
        self.assertEqual(quantity(manifest['workloads']['channel']['checksum'],7),28)
        self.assertEqual(quantity(manifest['workloads']['namespace_mix']['operations'],7),168)

    def test_generated_programs_are_current(self):
        before={p:p.read_bytes() for p in (HERE/'programs').glob('*.hir')}
        subprocess.run([str(HERE/'generate')],check=True)
        self.assertEqual(before,{p:p.read_bytes() for p in before})

    def test_report_keeps_missing_rows_visible(self):
        data=self.data(); data['samples']=[]
        text=report(data)
        self.assertIn('dependency-owned',text)
        self.assertIn('| recording | channel | 1 | cut_pause_ns | — | — | — |',text)

class DeadlineVerdict(unittest.TestCase):
    def test_user_instructions_decide_even_when_cycles_disagree(self):
        loader = importlib.machinery.SourceFileLoader('deadline_run', str(HERE / 'deadline-readiness/run'))
        deadline = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
        loader.exec_module(deadline)
        for instructions_ratio, cycles_ratio, expected in [(1.21, 1, 1), (1, 2, 0), (1.2, 2, 0)]:
            with self.subTest(instructions=instructions_ratio, cycles=cycles_ratio), tempfile.TemporaryDirectory() as work:
                binary = Path(work) / 'program'
                binary.write_bytes(b'fixture')
                output = Path(work) / 'session'
                def measured(argv, **kw):
                    grant = Path(argv[argv.index('--hirsel-grants') + 1])
                    width = json.loads(grant.read_text())['placement']['cores']
                    counter = Path(argv[argv.index('-o') + 1])
                    counter.write_text('\n'.join(
                        f'{value};;{event};' for event, value in {
                            'cycles': 100 * (cycles_ratio if width > 1 else 1),
                            'instructions:u': 100 * (instructions_ratio if width > 1 else 1),
                            'instructions': 100,
                            'context-switches': 0, 'duration_time': 1000}.items()))
                    return SimpleNamespace(returncode=0, stdout=b'', stderr=b'')
                with patch.object(sys, 'argv', ['run', '--binary', str(binary), '--output', str(output), '--cpus', '0,1,2,3']), \
                        patch.object(deadline.subprocess, 'run', side_effect=measured):
                    self.assertEqual(deadline.main(), expected)
                receipts = json.loads((output / 'receipts.json').read_text())
                self.assertEqual(len(receipts), 18)
                self.assertTrue(all('instructions:u' in r['argv'][r['argv'].index('-e') + 1].split(',') for r in receipts))


if __name__=='__main__': unittest.main()
