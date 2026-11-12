#!/usr/bin/env python3
"""Bless delegates unchanged selectors and environment to the owner-aware runner."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RUNNER = '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
with Path('calls.jsonl').open('a') as output:
    print(json.dumps(sys.argv[1:]), file=output)
if os.environ.get('REFUSE'):
    print('selector matched no runnable test bodies', file=sys.stderr)
    sys.exit(2)
print('test result: ok. 1 passed; 0 failed; 0 ignored;')
'''


class CurrentTargets(unittest.TestCase):
    def run_bless(self, refuse=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tools/buck2').mkdir(parents=True)
            (root / 'tools/buck2/sync.py').write_text('')
            shutil.copyfile(ROOT / 'tools/bless.sh', root / 'tools/bless.sh')
            runner = root / 'tools/test-filter'
            runner.write_text(RUNNER)
            runner.chmod(0o755)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            result = subprocess.run(['bash', 'tools/bless.sh', 'native-inventory'], cwd=root,
                                    env=os.environ | {'REFUSE': 'yes' if refuse else ''},
                                    capture_output=True, text=True)
            calls = root / 'calls.jsonl'
            return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_bless_and_verification_preserve_the_filter_and_only_bless_has_write_authority(self):
        result, calls = self.run_bless()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 2)
        for call in calls:
            self.assertIn('//:hirsel-registry-test-native_inventory', call)
            self.assertEqual(call[-2:], ['--', 'declaration_digests_match_the_reviewed_native_inventory'])
        self.assertIn('HIRSEL_BLESS=1', calls[0])
        self.assertNotIn('HIRSEL_BLESS=1', calls[1])

    def test_owner_refusal_cannot_become_a_successful_bless(self):
        result, calls = self.run_bless(refuse=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 1)
        self.assertIn('matched no runnable test bodies', result.stdout)


if __name__ == '__main__':
    unittest.main()
