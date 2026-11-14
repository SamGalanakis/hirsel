#!/usr/bin/env python3
"""A user check must not acquire the core/std contract-test corpus."""
import argparse
import json
import os
import re
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
        def test_help_uses_the_parser_catalogue(self):
            def cli(*arguments):
                return subprocess.run([str(compiler), *arguments], cwd=ROOT, capture_output=True, text=True)
            general = cli('help')
            self.assertEqual(general.returncode, 0, general.stderr)
            for arguments in [(), ('--help',), ('-h',)]:
                shown = cli(*arguments)
                self.assertEqual((shown.returncode, shown.stdout), (0, general.stdout))
            commands = re.findall(r'^  ([a-z][a-z-]*) +', general.stdout, re.M)
            self.assertTrue(commands)
            for command in commands:
                expected = cli('help', command)
                self.assertEqual(expected.returncode, 0, expected.stderr)
                for flag in ['--help', '-h']:
                    shown = cli(command, flag)
                    self.assertEqual((shown.returncode, shown.stdout), (0, expected.stdout))
            for arguments in [('check', '--safe'), ('check', '--backend', 'llvm'), ('new', '--watch'), ('unknown',)]:
                self.assertEqual(cli(*arguments).returncode, 2)

        def test_new_scaffolds_check_build_and_run_on_both_engines(self):
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
            with tempfile.TemporaryDirectory(prefix='hirsel-new-') as temporary:
                work = Path(temporary)
                for library in [False, True]:
                    app = work / ('library' if library else 'application')
                    new = subprocess.run([str(compiler), 'new', str(app), *(['--lib'] if library else [])],
                                         cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(new.returncode, 0, new.stderr)
                    check = subprocess.run([str(compiler), 'check', str(app), '--json'], cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
                    self.assertTrue(json.loads(check.stdout)['accepted'])
                    for engine in ['cranelift', 'llvm']:
                        build = subprocess.run([str(compiler), 'build', str(app), '--runtime', str(runtime),
                                                '--backend', engine, '-o', str(work / 'program'), '--json'],
                                               cwd=ROOT, capture_output=True, text=True)
                        self.assertEqual(build.returncode, 0, build.stdout + build.stderr)
                        self.assertIsNotNone(json.loads(build.stdout)['native'])
                        run = subprocess.run([str(work / 'program')], capture_output=True)
                        self.assertEqual((run.returncode, run.stdout, run.stderr), (0, b'', b''))
                    original = (app / 'main.hirsel').read_bytes()
                    again = subprocess.run([str(compiler), 'new', str(app)], cwd=ROOT, capture_output=True)
                    self.assertEqual(again.returncode, 2)
                    self.assertEqual((app / 'main.hirsel').read_bytes(), original)

        def test_env_and_registered_products_work_away_from_the_checkout(self):
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
            with tempfile.TemporaryDirectory(prefix='hirsel-installed-') as temporary:
                work = Path(temporary)
                app, root = work / 'app', work / 'toolchain'
                root.mkdir()
                installed = root / 'hirsel'
                shutil.copy2(compiler, installed)
                def invoke(executable, *arguments, expected=0):
                    result = subprocess.run([str(executable), *arguments], cwd=work, capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    return result
                invoke(compiler, 'std-product', '--runtime', str(runtime), expected=2)
                invoke(compiler, 'core-product', '--target', 'x86_64-unknown-linux-gnu', expected=2)
                initial = json.loads(invoke(compiler, 'env', '--json').stdout)
                self.assertIsNone(initial['installed_root'])
                self.assertIsNone(initial['runtime'])
                published = json.loads(invoke(compiler, 'std-product', '--install', str(root), '--runtime', str(runtime)).stdout)
                environment = json.loads(invoke(installed, 'env', '--json').stdout)
                self.assertEqual(Path(environment['installed_root']), root)
                self.assertEqual(Path(environment['package_cache']), root / 'core-packages')
                self.assertEqual(environment['products']['std']['image'], published['image'])
                self.assertTrue(environment['products']['std']['registered'])
                self.assertTrue(Path(environment['runtime']).is_file())
                invoke(installed, 'core-product', '--store', str(work / 'other-products'))
                invoke(installed, 'new', str(app))
                product = json.loads(invoke(installed, 'check', str(app), '--json').stdout)
                self.assertTrue(product['accepted'])
                self.assertEqual(product['files'], 1)
                self.assertEqual(product['diagnostics'], [])
                for engine in ['cranelift', 'llvm']:
                    built = json.loads(invoke(installed, 'build', str(app), '--backend', engine,
                                              '--json', '-o', str(work / 'program')).stdout)
                    self.assertIsNotNone(built['native'])
                    run = invoke(work / 'program')
                    self.assertEqual((run.stdout, run.stderr), ('', ''))
                moved = work / 'moved-toolchain'
                root.rename(moved)
                installed = moved / 'hirsel'
                environment = json.loads(invoke(installed, 'env', '--json').stdout)
                self.assertEqual(Path(environment['installed_root']), moved)
                self.assertTrue(Path(environment['runtime']).is_file())
                self.assertTrue(environment['products']['std']['registered'])
                self.assertTrue(json.loads(invoke(installed, 'check', str(app), '--json').stdout)['accepted'])
                selection = moved / 'hirsel-core-toolchain.json'
                previous = selection.read_bytes()
                invoke(installed, 'std-product', '--install', str(moved), '--runtime', str(work / 'missing.a'), expected=2)
                self.assertEqual(selection.read_bytes(), previous)

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
