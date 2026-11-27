"""Drive the Mac unittest runner with libtest-shaped subprocess output."""

from contextlib import redirect_stdout
from io import StringIO
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import mac_native


HARNESS = """\
import os
import sys

args = sys.argv[1:]
if args == ["--list"]:
    print("first: test\\nsecond: test\\nopt_in: test\\n\\n3 tests, 0 benchmarks")
elif args == ["--list", "--ignored"]:
    print("opt_in: test\\n\\n1 test, 0 benchmarks")
else:
    flag, case, threads = args
    assert flag == "--exact" and threads == "--test-threads=1"
    assert case in ("first", "second")
    assert os.environ["HIRSEL_JOBS"] == "1"
    assert os.path.isfile(os.environ["HIRSEL_CORE_RT_SCRIPTED_RECORD_ARCHIVE"])
    passed = int(case != os.environ.get("SKIP_CASE"))
    print(f"test result: ok. {passed} passed; 0 failed; {1 - passed} ignored; "
          "0 measured; 2 filtered out; finished in 0.00s")
"""


class NativeDriverTest(unittest.TestCase):
    def setUp(self):
        work = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.harness = work / "libtest"
        self.harness.write_text(f"#!{sys.executable}\n{HARNESS}")
        self.harness.chmod(0o755)
        self.binaries = work / "binaries.json"
        self.scripted_record = work / "libhirsel_core_rt.a"
        self.scripted_record.touch()
        self.binaries.write_text(json.dumps({
            "harness": str(self.harness),
            "compiler": str(work / "hirsel"),
            "scripted_record": str(self.scripted_record),
        }))
        self.inputs = work / "inputs"
        self.inputs.mkdir()
        (self.inputs / "kit").mkdir()
        (self.inputs / "kit/manifest.json").touch()
        for archive in mac_native.DRIVER_RUNTIMES.values():
            (self.inputs / f"{archive}.a").touch()
        self.output = self.enterContext(redirect_stdout(StringIO()))
        self.enterContext(patch.object(mac_native, "seed_dev_cache"))

    def test_runs_all_default_cases_with_explicit_exclusions(self):
        self.assertFalse(mac_native.run_driver(self.binaries, self.inputs))
        summary = self.output.getvalue()
        self.assertIn("PASS first\n", summary)
        self.assertIn("PASS second\n", summary)
        self.assertIn("2/2 default driver tests passed.", summary)
        self.assertIn("1 explicitly ignored driver tests\nopt_in\n", summary)
        self.assertIn("launcher::laws::cgroup_lifecycle (Linux cgroups)", summary)

    def test_zero_passes_with_exit_zero_fails_the_job(self):
        with patch.dict(os.environ, SKIP_CASE="first"):
            self.assertTrue(mac_native.run_driver(self.binaries, self.inputs))
        self.assertIn("FAIL first (exit 0)", self.output.getvalue())
        self.assertIn("1/2 default driver tests passed.", self.output.getvalue())

    def test_absent_scripted_record_runtime_fails_before_running_tests(self):
        self.scripted_record.unlink()
        with self.assertRaises(FileNotFoundError):
            mac_native.run_driver(self.binaries, self.inputs)
        self.assertEqual(self.output.getvalue(), "")

    def test_list_count_must_match_the_parsed_cases(self):
        with patch.object(mac_native.subprocess, "check_output",
                          return_value="first: test\n2 tests, 0 benchmarks\n"):
            with self.assertRaisesRegex(SystemExit, "Invalid hirsel-core-driver inventory"):
                mac_native.driver_cases(self.harness, {})

    def test_wrong_platform_and_empty_default_partition_are_rejected(self):
        for all_cases, ignored in (
            ({"first", "launcher::laws::cgroup_lifecycle"}, set()),
            ({"opt_in"}, {"opt_in"}),
            ({"first"}, {"unknown_ignore"}),
        ):
            with (self.subTest(all_cases=all_cases, ignored=ignored),
                  patch.object(mac_native, "driver_inventory", side_effect=[all_cases, ignored])):
                with self.assertRaisesRegex(SystemExit, "Unexpected Mac driver inventory"):
                    mac_native.driver_cases(self.harness, {})


if __name__ == "__main__":
    unittest.main()
