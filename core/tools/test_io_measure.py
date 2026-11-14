import io
import unittest
from io_measure import LINE, observe_output, reused_reference


class IoMeasure(unittest.TestCase):
    def test_reused_sample_requires_same_size_and_profile(self):
        rows = {'llvm/write/rust': dict(input_bytes=64, rust_opt_level=3, instructions=32)}
        self.assertEqual(reused_reference(rows, 'llvm/write/rust', 64, '3')['instructions_per_byte'], .5)
        for size, opt in [(128, '3'), (64, '0')]:
            with self.assertRaisesRegex(ValueError, 'size/profile differs'):
                reused_reference(rows, 'llvm/write/rust', size, opt)
        with self.assertRaisesRegex(ValueError, 'size/profile differs'):
            reused_reference(rows, 'missing', 64, '3')

    def test_write_oracle_checks_complete_output(self):
        count = 16385  # Cross the bounded comparison chunk.
        data = LINE * count
        self.assertEqual(observe_output(io.BytesIO(data), 'write', count)['output_bytes'], len(data))
        for wrong in [data[:-1], data + LINE, data[:-1] + b'x']:
            with self.assertRaises(ValueError):
                observe_output(io.BytesIO(wrong), 'write', count)

    def test_read_oracle_is_exact(self):
        self.assertEqual(observe_output(io.BytesIO(b'65536 4128768\n'), 'read', 1)['output_bytes'], 14)
        with self.assertRaisesRegex(ValueError, 'wrong read output'):
            observe_output(io.BytesIO(b'65536 4128769\n'), 'read', 1)


if __name__ == '__main__':
    unittest.main()
