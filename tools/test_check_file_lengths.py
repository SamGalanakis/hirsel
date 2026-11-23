"""Exercise the file length hook in a fixture repository."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
RATCHET = 'tools/file_length_ratchet.json'
CHECK = 'tools/check_file_lengths.py'


class FileLengthRatchet(unittest.TestCase):
    def test_raise_stale_caps_in_one_commit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def run(*args, check=True):
                return subprocess.run(args, cwd=root, check=check, capture_output=True, text=True)

            run('git', 'init', '-q')
            run('git', 'config', 'user.name', 'test')
            run('git', 'config', 'user.email', 'test@example.invalid')
            for path in (CHECK, '.githooks/pre-commit'):
                (root / path).parent.mkdir(exist_ok=True)
                shutil.copy2(ROOT / path, root / path)

            targets = {'below.py': 1150, 'equal.py': 1200}
            entries = {path: {'lines': 1200, 'reason': 'fixture-lane'} for path in targets}
            (root / RATCHET).write_text(json.dumps(entries))
            for path in targets:
                (root / path).write_text('# line\n' * 1100)
            run('git', 'add', '.')
            run('git', '-c', 'core.hooksPath=/dev/null', 'commit', '-qm', 'Fixture with stale caps')
            run('git', 'config', 'core.hooksPath', '.githooks')

            for path, lines in targets.items():
                (root / path).write_text('# line\n' * lines)
            run('git', 'add', *targets)
            refused = run(sys.executable, '-B', CHECK, check=False)
            self.assertIn('equal.py: 1,200 lines, limit 1,100', refused.stderr)
            self.assertEqual(refused.returncode, 1)

            run(sys.executable, '-B', CHECK, '--update-ratchet')
            run('git', 'add', RATCHET)
            refused = run(sys.executable, '-B', CHECK, check=False)
            self.assertIn('below.py: 1,150 lines, limit 1,100', refused.stderr)
            self.assertEqual(refused.returncode, 1)
            run('git', 'restore', '--source=HEAD', '--staged', '--worktree', RATCHET)

            for path, lines in targets.items():
                run(sys.executable, '-B', CHECK, '--raise', path, str(lines), '--reason', 'fixture-lane')
            run('git', 'add', RATCHET)

            (root / 'new.py').write_text('# line\n' * 1001)
            run('git', 'add', 'new.py')
            refused = run(sys.executable, '-B', CHECK, check=False)
            self.assertIn('new.py: 1,001 lines, limit 1,000', refused.stderr)
            self.assertEqual(refused.returncode, 1)
            run('git', 'restore', '--staged', 'new.py')

            run('git', 'commit', '-qm', 'Grow both files with explicit raises')
            self.assertEqual(run('git', 'rev-list', '--count', 'HEAD').stdout.strip(), '2')
            committed = json.loads(run('git', 'show', f'HEAD:{RATCHET}').stdout)
            self.assertEqual({path: entry['lines'] for path, entry in committed.items()}, targets)


if __name__ == '__main__':
    unittest.main()
