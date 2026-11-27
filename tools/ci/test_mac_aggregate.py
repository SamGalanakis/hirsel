"""Exercise the Mac CI result and both kinds of driver exclusion."""

import unittest

from mac_aggregate import aggregate


def regular_run():
    return {
        "plan": {"result": "success", "outputs": {
            "nightly_cross_reason": "scheduled only",
            "nightly_native_reason": "scheduled only",
        }},
        "cross": {"result": "success"},
        "mac": {"result": "success"},
        "mac_driver_inputs": {"result": "success"},
        "mac_driver": {"result": "success", "outputs": {"tests": "success"}},
        "nightly_cross": {"result": "skipped"},
        "nightly_native": {"result": "skipped"},
    }


class AggregateTest(unittest.TestCase):
    def test_regular_run_requires_both_driver_jobs(self):
        jobs = regular_run()
        self.assertEqual(aggregate(jobs)[0], {})
        for job in ("mac_driver_inputs", "mac_driver"):
            with self.subTest(job=job):
                missing = jobs.copy()
                del missing[job]
                self.assertIn(f"mac-{job}", aggregate(missing)[0])

    def test_failed_and_cancelled_driver_cannot_use_an_exclusion(self):
        for result in ("failure", "cancelled"):
            with self.subTest(result=result):
                jobs = regular_run()
                jobs["plan"]["outputs"]["mac_driver_reason"] = "intentional control"
                jobs["mac_driver"]["result"] = result
                self.assertIn(f"mac-mac_driver", aggregate(jobs)[0])

    def test_successful_job_cannot_hide_a_skipped_test_step(self):
        jobs = regular_run()
        jobs["mac_driver"]["outputs"]["tests"] = "skipped"
        self.assertIn("mac-mac_driver", aggregate(jobs)[0])

    def test_unexplained_skip_control_flags_both_unplanned_skips(self):
        jobs = regular_run()
        jobs["plan"]["outputs"].update({
            "control": "unexplained-skip",
            "mac_reason": "fixture build deliberately skipped",
            "mac_driver_inputs_reason": "negative control needs no fixtures",
        })
        for job in ("cross", "mac", "mac_driver_inputs", "mac_driver"):
            jobs[job] = {"result": "skipped"}
        failures, _ = aggregate(jobs)
        self.assertEqual(set(failures), {"mac-cross", "mac-mac_driver"})

    def test_declared_driver_exclusions_are_visible(self):
        jobs = regular_run()
        for job in ("mac_driver_inputs", "mac_driver"):
            jobs["plan"]["outputs"][f"{job}_reason"] = "platform control"
            jobs[job] = {"result": "skipped"}
        failures, rows = aggregate(jobs)
        self.assertEqual(failures, {})
        self.assertIn("- Native Mac driver unittests: skipped · platform control", rows)


if __name__ == "__main__":
    unittest.main()
