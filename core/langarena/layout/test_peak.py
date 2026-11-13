import unittest

from peak import massif_peak


class MassifPeak(unittest.TestCase):
    def test_requested_heap_peak_excludes_admin_and_later_smaller_snapshots(self):
        source = ('mem_heap_B=12\nmem_heap_extra_B=99\nheap_tree=detailed\n'
                  'mem_heap_B=31\nmem_heap_extra_B=7\nheap_tree=peak\n'
                  'mem_heap_B=0\nmem_heap_extra_B=0\nheap_tree=empty\n')
        self.assertEqual(massif_peak(source), 31)

    def test_missing_peak_is_not_a_zero(self):
        with self.assertRaisesRegex(ValueError, 'did not record'):
            massif_peak('mem_heap_B=0\nheap_tree=empty\n')


if __name__ == '__main__':
    unittest.main()
