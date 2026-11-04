#!/usr/bin/env python3
"""Executable pool checks: missing/duplicate observations cannot pass B1."""
from pathlib import Path
import runpy
import unittest
from unittest.mock import patch

B1 = runpy.run_path(str(Path(__file__).resolve().parents[2]/'bench/multicore/b1.py'))


class Acceptance(unittest.TestCase):
    def samples(self):
        rows = []
        for case in B1['CASES']:
            for n in B1['SIZES']:
                for p,r,o,elapsed,latency in [('tokio',False,False,80,0),
                        ('free',False,False,100,0),('free',True,False,110,0),
                        ('free',False,True,120,100),('free',True,True,130,120)]:
                    for rnd in range(3):
                        rows.append(dict(case=case,n=n,provider=p,record=r,observed=o,
                            round=rnd,elapsed_ns=elapsed,latency={'p99_ns':latency},
                            counts={'instructions:u':10}))
        return rows

    def test_boundaries_and_overhead_use_distinct_populations(self):
        rows = B1['analyze'](self.samples())
        self.assertEqual(len(rows),9)
        for row in rows:
            self.assertEqual(row['off_vs_tokio'],.8)
            self.assertAlmostEqual(row['record_vs_off'],100/110)
            self.assertEqual(row['p99_record_vs_off'],1.2)
            self.assertEqual(row['observer_wall_overhead_off'],1.2)
            self.assertTrue(all(row['floors'].values()))

    def test_sibling_load_tracks_selected_cores_instead_of_fixed_host_ids(self):
        stat = 'cpu16 10 0 0 10 0 0 0 0\ncpu17 10 0 0 10 0 0 0 0\n'
        after = 'cpu16 20 0 0 20 0 0 0 0\ncpu17 10 0 0 30 0 0 0 0\n'
        globals_ = B1['sibling_load'].__globals__
        with patch.dict(globals_, siblings=lambda cpu: [cpu + 16]):
            loads = B1['sibling_load']({'affinity': [0, 1], 'proc_stat': stat}, {'proc_stat': after})
        self.assertEqual(set(loads), {'16', '17'})
        self.assertEqual(loads['16']['busy_share'], .5)
        self.assertEqual(loads['17']['busy_share'], 0)

    def test_missing_or_duplicate_rounds_refuse(self):
        rows = self.samples()
        for bad in (rows[:-1],rows+[rows[0]]):
            with self.assertRaises(AssertionError): B1['analyze'](bad)

    def test_failure_is_retained(self):
        rows = self.samples()
        for row in rows:
            if row['provider']=='free' and not row['observed']: row['elapsed_ns'] *= 4
            if row['observed'] and row['record']: row['latency']['p99_ns'] = 121
        result=B1['analyze'](rows)
        self.assertTrue(all(not row['floors']['off'] and not row['floors']['p99'] for row in result))

if __name__ == '__main__': unittest.main()
