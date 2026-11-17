import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest

loader = importlib.machinery.SourceFileLoader('http_instructions', str(Path(__file__).with_name('instructions')))
spec = importlib.util.spec_from_loader(loader.name, loader)
benchmark = importlib.util.module_from_spec(spec)
loader.exec_module(benchmark)


class InstructionTests(unittest.TestCase):
    def test_startup_cost_cancels(self):
        startup, cost = 1234567, 4312
        small, large = 1024, 8192
        self.assertEqual(benchmark.per_request(startup + small * cost,
                                             startup + large * cost, small, large), cost)

    def test_invalid_counts_and_nonpositive_delta_refuse(self):
        for low, high, small, large in ((1, 2, 0, 2), (1, 2, 2, 2),
                                        (1, 2, 3, 2), (2, 2, 1, 2), (3, 2, 1, 2)):
            with self.subTest(values=(low, high, small, large)):
                with self.assertRaises(ValueError):
                    benchmark.per_request(low, high, small, large)

    def test_reads_instruction_event_not_first_event(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory) / 'callgrind.out'
            profile.write_text('events: Dr Ir Dw\nsummary: 99 12345 88\n')
            self.assertEqual(benchmark.instructions(profile), 12345)

    def test_missing_summary_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            profile = Path(directory) / 'callgrind.out'
            profile.write_text('events: Ir\n')
            with self.assertRaises(ValueError):
                benchmark.instructions(profile)


if __name__ == '__main__':
    unittest.main()
