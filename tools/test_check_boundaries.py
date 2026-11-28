"""Exercise the production boundary check with fixture packages."""

from contextlib import redirect_stderr
from io import StringIO
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import check_boundaries


class ProductionBoundaries(unittest.TestCase):
    def test_manifest_boundaries(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)

            def write(path, text):
                file = root / 'core/programs' / path
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(text)

            def git(*args):
                subprocess.run(['git', *args], cwd=root, check=True)

            def check(allow_list=None, *, staged=False):
                errors = StringIO()
                with patch.object(check_boundaries, 'ALLOW_LIST', allow_list or {}), redirect_stderr(errors):
                    ok = check_boundaries.check(root, staged=staged)
                return ok, errors.getvalue()

            write('laws/hirsel.hcfg', '''pkg_v1.Package(
    name: "laws",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("main.hirsel", "main")],
    dependencies: [],
    tests: Some(Tests(
        sources: [("tools.hirsel", "helpers.extra"), ("tests.hirsel", "main")],
        limit: 1024,
        grants: [],
        runs: [],
    )),
)
''')
            write('laws/tools.hirsel', 'pub fn law_extra() {}\n')
            write('laws/tests.hirsel', '@test fn law_valid() {}\n')
            clean = '''// @test fn law_comment() { store.fixture.setup() }
const normal: String = "escaped \\" @test fn law_string() { store.fixture.setup() }"
const raw: String = r#"quoted " @test fn law_raw() { store.fixture.setup() }"#
fn main() { main.shared(); value. store.fixture.setup() }
'''
            write('laws/main.hirsel', clean)
            write('consumer/hirsel.hcfg', '''pkg_v1.Package(
    name: "consumer",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("main.hirsel", "main")],
    dependencies: [],
)
''')
            write('consumer/main.hirsel', 'fn main() {}\n')
            write('store-crash/hirsel.hcfg', '''pkg_v1.Package(
    name: "store_crash",
    version: "1.0.0",
    charge: ChargeV1,
    sources: [("cut.hirsel", "cut")],
    dependencies: [],
)
''')
            write('store-crash/cut.hirsel', 'pub fn run() {}\n')
            git('init', '-q')
            git('add', '.')

            write('laws/main.hirsel', clean + '''@test
pub fn law_crosses() { helpers.extra.law_extra() }
''')
            write('consumer/main.hirsel', '''fn main() {
    store // path continues after the comment
        .fixture.setup()
    store.signup.create()
    laws.helpers.extra.law_extra()
    store_crash.cut.run()
}
''')
            self.assertEqual(check(staged=True), (True, ''))
            expected = [
                'consumer/main.hirsel:2: test-only-module: store.fixture',
                'consumer/main.hirsel:4: test-only-module: store.signup',
                'consumer/main.hirsel:5: test-only-module: laws.helpers.extra',
                'consumer/main.hirsel:6: test-only-module: store_crash.cut',
                'laws/main.hirsel:5: test-attribute: law_crosses',
                'laws/main.hirsel:6: test-function: law_crosses',
                'laws/main.hirsel:6: test-only-module: helpers.extra',
            ]
            git('add', '.')
            for staged in (False, True):
                ok, errors = check(staged=staged)
                self.assertFalse(ok)
                self.assertEqual(errors.splitlines(), [
                    f'core/programs/{item} in [sources]. Move to [tests.sources].' for item in expected
                ])

            allowed = {('laws/main.hirsel', 'test-function'): 'law_crosses'}
            self.assertNotIn('test-function', check(allowed)[1])
            write('laws/main.hirsel', clean)
            write('consumer/main.hirsel', 'fn main() {}\n')
            ok, errors = check(allowed)
            self.assertFalse(ok)
            self.assertIn('law_crosses no longer violates the boundary', errors)
            self.assertEqual(check(), (True, ''))


if __name__ == '__main__':
    unittest.main()
