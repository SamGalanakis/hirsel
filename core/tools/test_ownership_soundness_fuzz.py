#!/usr/bin/env python3
"""Unit and smoke tests for core/tools/ownership_soundness_fuzz.py and ownership_soundness_gen.py."""
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ownership_soundness_fuzz as fuzz
import ownership_soundness_gen as gen


class OwnershipSoundnessFuzzTests(unittest.TestCase):
    def test_deterministic_stream_and_draw_replay(self):
        p1 = gen.generate_program(42)
        p2 = gen.generate_program(42)
        self.assertEqual(p1.surface, p2.surface)
        self.assertEqual(p1.kind, p2.kind)
        self.assertEqual(p1.mutation, p2.mutation)
        self.assertEqual(p1.stmts, p2.stmts)
        self.assertEqual(p1.draws, p2.draws)
        p3 = gen.generate_program(0, draws=p1.draws, forced_surface=p1.surface, forced_kind=p1.kind)
        self.assertEqual(p1.stmts, p3.stmts)

    def test_corpus_covers_all_checker_surfaces_and_unique_names(self):
        names = [c.name for c in gen.NEGATIVE_CONTROLS]
        self.assertEqual(len(names), len(set(names)))
        by_surface = {s: [c for c in gen.NEGATIVE_CONTROLS if c.surface == s] for s in gen.CHECKER_SURFACES}
        for surface, items in by_surface.items():
            self.assertGreaterEqual(len(items), 10, f'surface {surface} under-covered')

    def test_high_valid_ratio_and_interaction_unwinding_bias(self):
        progs = [gen.generate_program(i) for i in range(200)]
        valid_ratio = sum(1 for p in progs if p.kind == 'valid') / len(progs)
        self.assertGreaterEqual(valid_ratio, 0.72)
        deep_ratio = sum(1 for p in progs if p.surface in ('interaction', 'unwinding', *gen.FEATURE_SURFACES)) / len(progs)
        self.assertGreaterEqual(deep_ratio, 0.50)
        for surface in gen.FEATURE_SURFACES:
            self.assertGreaterEqual(sum(1 for p in progs if p.surface == surface), 10, f'{surface} under-generated')

    def test_batch_rendering_and_record_parsing(self):
        progs = [gen.generate_program(i, forced_kind='valid') for i in range(4)]
        source = gen.render_batch(progs)
        for i in range(4):
            self.assertIn(f'pub fn case_{i}(', source)
        parsed = fuzz.parse_case_records(b'case 0 111\nnoise\ncase 2 333\n')
        self.assertEqual(parsed, {0: 111, 2: 333})

    def test_differential_and_ir_wf_oracle_catches_reds(self):
        ok_same = {
            'evaluator': {'kind': 'ok', 'wf_checked': True, 'wf_ok': True, 'records': {0: 77}, 'cause': None},
            'cranelift-dev': {'kind': 'ok', 'records': {0: 77}, 'cause': None},
            'llvm-dev': {'kind': 'ok', 'records': {0: 77}, 'cause': None},
        }
        self.assertEqual(fuzz.compare_engine_outcomes(0, 1, 'interaction', ok_same), [])

        wf_fail = dict(ok_same)
        wf_fail['evaluator'] = {'kind': 'ir-wf-own', 'wf_checked': True, 'wf_ok': False,
                                'detail': 'not well formed: double drop'}
        reds = fuzz.compare_engine_outcomes(0, 1, 'unwinding', wf_fail)
        self.assertEqual(reds[0]['key'], 'soundness:ir-wf-own:unwinding:evaluator')

        diverged = dict(ok_same)
        diverged['llvm-dev'] = {'kind': 'ok', 'records': {0: 78}, 'cause': None}
        reds = fuzz.compare_engine_outcomes(0, 1, 'aliasing', diverged)
        self.assertEqual(reds[0]['key'], 'soundness:diverge-value:aliasing:llvm-dev')

        crashed = dict(ok_same)
        crashed['cranelift-dev'] = {'kind': 'crash', 'status': -11, 'detail': 'SIGSEGV'}
        reds = fuzz.compare_engine_outcomes(0, 1, 'move', crashed)
        self.assertEqual(reds[0]['key'], 'soundness:crash:move:cranelift-dev')

    def test_seed_parsing_and_missing_runtime_refusal(self):
        self.assertEqual(fuzz.parse_seeds('10..14', 0, 100), [10, 11, 12, 13])
        self.assertEqual(fuzz.parse_seeds('3, 7, 9', 0, 100), [3, 7, 9])
        self.assertEqual(fuzz.parse_seeds(None, 5, 3), [5, 6, 7])
        with tempfile.TemporaryDirectory() as tmp:
            fake_compiler = Path(tmp) / 'hirsel'
            fake_compiler.touch()
            with self.assertRaises(SystemExit) as ctx2:
                fuzz.main(['controls', '--compiler', str(fake_compiler),
                           '--sanitized-runtime', str(Path(tmp) / 'missing.a'), '--out', tmp])
            self.assertNotEqual(ctx2.exception.code, 0)

    def test_alert_formatting(self):
        with tempfile.TemporaryDirectory() as tmp:
            alert = Path(tmp) / 'alert.txt'
            summary = {
                'reds': [{'key': 'soundness:unsound-accept:move:mut_move_after_take',
                          'seed': 12, 'build': 'frontend', 'detail': 'accepted'}],
                'minimized': {'soundness:unsound-accept:move:mut_move_after_take': {'path': '/tmp/min.hirsel', 'stmts': 1}},
            }
            fuzz.write_alerts(summary, alert)
            text = alert.read_text()
            self.assertIn('soundness:unsound-accept:move:mut_move_after_take', text)
            self.assertIn('minimized /tmp/min.hirsel', text)


if __name__ == '__main__':
    unittest.main()
