#!/usr/bin/env python3
"""A user check must not acquire the core/std contract-test corpus."""
import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    compiler = args.compiler.resolve()

    class LibraryModes(unittest.TestCase):
        def test_user_check_in_every_cli_library_mode(self):
            with tempfile.TemporaryDirectory(prefix='hirsel-library-modes-') as temporary:
                work = Path(temporary)
                app = work / 'app'
                app.mkdir()
                source = app / 'main.hirsel'
                source.write_text('pub fn main() -> Result[(), ()] { Ok(()) }\n')
                cache = work / 'packages'
                libraries = work / 'libraries'
                for package in ['core', 'std']:
                    shutil.copytree(ROOT / 'core' / package, libraries / package)
                    nested = libraries / package / 'tests' / 'nested'
                    nested.mkdir(parents=True, exist_ok=True)
                    (nested / 'no_header.hirsel').write_text('deliberately malformed test fixture\n')
                core_id, std_id, app_id = [dict(name=name, version='1') for name in ['core', 'std', 'app']]
                def sources(package):
                    return [dict(file=str(p), modules=[p.stem], signatures=True)
                            for p in sorted((ROOT / 'core' / package).glob('*.hirsel'))]
                inputs = work / 'inputs.json'
                inputs.write_text(json.dumps(dict(core=core_id, packages=[
                    dict(package=core_id, sources=sources('core'), dependencies=[]),
                    dict(package=std_id, sources=sources('std'), dependencies=[core_id]),
                    dict(package=app_id, sources=[dict(file=str(source), modules=['main'], signatures=False)],
                         dependencies=[core_id, std_id])])))

                def invoke(command, options, store):
                    result = subprocess.run([str(compiler), command, *options, '--store', str(store), '--json'],
                                            cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, f'{command} {options}: {result.stdout}\n{result.stderr}')
                    product = json.loads(result.stdout)
                    self.assertTrue(product['accepted'], product)
                    self.assertEqual(product['diagnostics'], [], product)
                    self.assertEqual(product['incomplete'], [], product)
                    return product

                modes = [
                    ('default', []),
                    ('source-core', ['--source-core']),
                    ('source-std', ['--source-std']),
                    ('source-libraries', ['--libraries', str(libraries)]),
                    ('published', ['--core-product', str(cache)]),
                    ('published-core-source-std', ['--core-product', str(cache), '--source-std']),
                    ('explicit-inputs', ['--inputs', str(inputs)]),
                ]
                for name, options in modes:
                    with self.subTest(mode=name):
                        invoke('check', [str(app), *options], work / name)
                        print(f'{name}: accepted, zero diagnostics', flush=True)
                with self.subTest(mode='image'):
                    image = work / 'image'
                    invoke('image', ['--libraries', str(libraries)], image)
                    invoke('check', [str(app), '--image', str(image)], work / 'image-check')
                    print('image: accepted, zero diagnostics', flush=True)
                # Structural exclusion must not conceal a refusal in library code.
                (libraries / 'std' / 'user_refused.hirsel').write_text('pub fn bad() -> u64 { true }\n')
                result = subprocess.run([str(compiler), 'check', str(app), '--libraries', str(libraries),
                                         '--store', str(work / 'bad-source'), '--json'],
                                        cwd=ROOT, capture_output=True, text=True)
                product = json.loads(result.stdout)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(product['accepted'])
                self.assertTrue(any(d['rule'] == 'LANG-11' and d['primary']['module'] == 'std@1/user_refused'
                                    for d in product['diagnostics']), product)

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(LibraryModes))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
