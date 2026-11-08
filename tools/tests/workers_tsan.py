#!/usr/bin/env python3
"""Native TSan qualification requires four distinct full-width P witnesses."""
import contextlib
import io
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import unittest
from unittest.mock import Mock, call, patch

SCRIPT = Path(__file__).resolve().parents[1] / 'workers-tsan'


class NativeQualification(unittest.TestCase):
    def run_gate(self, width, stderr, status=0, controls=True, listing=False):
        if controls:
            stderr += "".join(
                f"MUTANT {policy} {backend} W{width}: exit 66, emitted mapper race\n"
                f"PRODUCTION {policy} {backend} W{width}: exit 0, no race, values checked\n"
                for policy in ["tasks.free@1", "tasks.cooperative@1"]
                for backend in ["llvm", "cranelift"])
        stdout = (f'native_tsan_ordered_cranelift_width_{width}: test\n' if listing else '4 passed\n')
        result = subprocess.CompletedProcess([], status, stdout, stderr)
        process = Mock(returncode=status, pid=12345)
        process.communicate.return_value = (result.stdout, result.stderr)
        output = io.StringIO()
        with patch.dict(os.environ, {'HIRSEL_NATIVE_CPU_HARNESS': 'unused-harness'}), \
                patch.object(sys, 'argv', [str(SCRIPT), '--native', str(width), *(['--list'] if listing else [])]), \
                patch.object(subprocess, 'Popen', return_value=process) as spawn, \
                patch.object(os, 'killpg') as kill_group, \
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            with self.assertRaises(SystemExit) as exit:
                runpy.run_path(str(SCRIPT), run_name='__main__')
        self.output = output.getvalue()
        self.killed = kill_group.call_args_list
        self.spawn = spawn.call_args
        return exit.exception.code

    def witnesses(self, width):
        return [f'CPU {policy} {backend} P({width}): switch-free overlap, ownership=Ok(...)\n'
                for policy in ['tasks.free@1', 'tasks.cooperative@1']
                for backend in ['llvm', 'cranelift']]

    def test_discovery_lists_cells_without_claiming_qualification(self):
        for width in [4, 8]:
            with self.subTest(width=width):
                self.assertEqual(self.run_gate(width, '', controls=False, listing=True), 0)
                self.assertIn(f'native_tsan_ordered_cranelift_width_{width}: test', self.output)
                self.assertIn('--list', self.spawn.args[0])

    def test_value_passes_without_exclusive_cpu_ownership_are_not_qualification(self):
        for width in [4, 8]:
            with self.subTest(width=width):
                refusal = (f'CPU tasks.free@1 llvm W{width}: all workers progressed; '
                           f'P({width}) NOT QUALIFIED, ownership=Err("no visible private cgroup")\n')
                self.assertNotEqual(self.run_gate(width, refusal), 0)

    def test_every_policy_and_emitter_must_have_its_own_full_width_witness(self):
        for width in [4, 8]:
            with self.subTest(width=width):
                rows = self.witnesses(width)
                self.assertEqual(self.run_gate(width, ''.join(rows)), 0)
                self.assertNotEqual(self.run_gate(width, rows[0] * 4), 0)
                self.assertNotEqual(self.run_gate(width, ''.join(rows[:-1])), 0)

    def test_full_width_witnesses_without_double_claim_detection_are_not_qualification(self):
        self.assertNotEqual(self.run_gate(4, ''.join(self.witnesses(4)), controls=False), 0)


    def test_sanitizer_failure_status_is_preserved_even_with_witnesses(self):
        self.assertEqual(self.run_gate(8, ''.join(self.witnesses(8)), 66), 66)


if __name__ == '__main__':
    unittest.main()
