#!/usr/bin/env python3
"""A user check must not acquire the core/std contract-test corpus."""
import argparse
import json
import os
import re
from pathlib import Path
import shutil
import tempfile
import unittest

from case_progress import Case

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compiler', type=Path, required=True)
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    args = parser.parse_args()
    compiler = args.compiler.resolve()

    class LibraryModes(unittest.TestCase):
        def setUp(self):
            self.progress = self.enterContext(Case(self._testMethodName, "cli"))

        def test_help_uses_the_parser_catalogue(self):
            def cli(*arguments):
                return self.progress.run([str(compiler), *arguments], cwd=ROOT, capture_output=True, text=True)
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
                    new = self.progress.run([str(compiler), 'new', str(app), *(['--lib'] if library else [])],
                                         cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(new.returncode, 0, new.stderr)
                    formatted = self.progress.run([str(compiler), 'fmt', '--check', str(app)],
                                                  cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(formatted.returncode, 0, formatted.stdout + formatted.stderr)
                    check = self.progress.run([str(compiler), 'check', '--libraries', str(ROOT / 'core'), '--json'],
                                              cwd=app, capture_output=True, text=True)
                    self.assertEqual(check.returncode, 0, check.stdout + check.stderr)
                    self.assertTrue(json.loads(check.stdout)['accepted'])
                    test = self.progress.run([str(compiler), 'test', '--runtime', str(runtime),
                                              '--libraries', str(ROOT / 'core')],
                                             cwd=app, capture_output=True, text=True)
                    self.assertEqual(test.returncode, 0, test.stdout + test.stderr)
                    self.assertIn('test_answer' if library else 'test_twice', test.stdout)
                    for engine in ['cranelift', 'llvm']:
                        build = self.progress.run([str(compiler), 'build', str(app), '--runtime', str(runtime),
                                                '--backend', engine, '-o', str(work / 'program'), '--json'],
                                               cwd=ROOT, capture_output=True, text=True)
                        self.assertEqual(build.returncode, 0, build.stdout + build.stderr)
                        self.assertIsNotNone(json.loads(build.stdout)['native'])
                        run = self.progress.run([str(work / 'program')], capture_output=True)
                        self.assertEqual((run.returncode, run.stdout, run.stderr), (0, b'', b''))
                    original = (app / 'main.hirsel').read_bytes()
                    again = self.progress.run([str(compiler), 'new', str(app)], cwd=ROOT, capture_output=True)
                    self.assertEqual(again.returncode, 2)
                    self.assertEqual((app / 'main.hirsel').read_bytes(), original)

        def test_debug_lines_defaults_overrides_and_cache_identity(self):
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
            with tempfile.TemporaryDirectory(prefix='hirsel-lines-') as temporary:
                work = Path(temporary)
                app = work / 'app'
                created = self.progress.run([str(compiler), 'new', str(app)], cwd=ROOT, capture_output=True)
                self.assertEqual(created.returncode, 0)
                for engine in ['cranelift', 'llvm']:
                    for release in [False, True]:
                        seen = set()
                        for choice, expected in [(None, False), ('off', False), ('on', True)]:
                            flags = ['--release'] if release else []
                            if choice is not None:
                                flags += ['--debug-lines', choice]
                            result = self.progress.run([str(compiler), 'build', str(app), '--runtime', str(runtime),
                                                     '--backend', engine, '--store', str(work / 'store'),
                                                     '-o', str(work / 'program'), '--json', *flags],
                                                    cwd=ROOT, capture_output=True, text=True)
                            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                            native = json.loads(result.stdout)['native']
                            self.assertEqual(native['debug_lines'], expected)
                            self.assertEqual(native['compiled'] == 0, expected in seen)
                            seen.add(expected)
                refused = self.progress.run([str(compiler), 'build', str(app), '--debug-lines', 'maybe'],
                                         cwd=ROOT, capture_output=True, text=True)
                self.assertEqual(refused.returncode, 2)
                self.assertIn('--debug-lines expects on or off', refused.stderr)
            explained = self.progress.run([str(compiler), 'explain', 'debug-lines', '--json'],
                                       cwd=ROOT, capture_output=True, text=True)
            self.assertEqual(explained.returncode, 0, explained.stderr)
            policy = json.loads(explained.stdout)
            self.assertEqual((policy['default_dev'], policy['default_release']), ('off', 'off'))
            self.assertIn('always enabled', policy['statement'])

        def test_native_link_uses_objects_without_c_compilation(self):
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
            real_cc = Path(os.environ['LLVM_SYS_231_PREFIX']) / 'bin/clang'
            with tempfile.TemporaryDirectory(prefix='hirsel-entry-object-') as temporary:
                work = Path(temporary)
                app = work / 'app'
                self.assertEqual(self.progress.run([str(compiler), 'new', str(app)], cwd=ROOT, capture_output=True).returncode, 0)
                calls, cc = work / 'calls.jsonl', work / 'link-only'
                cc.write_text('#!/usr/bin/python3\nimport json,os,pathlib,sys\n'
                    + 'args=sys.argv[1:]\n'
                    # `-###` is the recipe capture's dry run: it prints the
                    # link command and compiles nothing.
                    + 'if "-###" not in args and any(a in {"-c","-S","-E","-x"} or pathlib.Path(a).suffix in {".c",".cc",".cpp",".s",".S"} for a in args):\n'
                    + ' sys.stderr.write("C compilation forbidden by direct-object oracle\\n");sys.exit(69)\n'
                    + 'for a in args:\n'
                    + ' if pathlib.Path(a).name=="entry.o": assert pathlib.Path(a).read_bytes()[:4]==b"\\x7fELF"\n'
                    + 'with open(' + repr(str(calls)) + ',"a") as log: log.write(json.dumps(args)+"\\n")\n'
                    + 'os.execv(' + repr(str(real_cc)) + ',[' + repr(str(real_cc)) + ',*args])\n')
                cc.chmod(0o755)
                for engine in ['cranelift', 'llvm']:
                    result = self.progress.run([str(compiler), 'build', str(app), '--runtime', str(runtime),
                        '--backend', engine, '--cc', str(cc), '--store', str(work / engine),
                        '-o', str(work / 'program'), '--json'], cwd=ROOT, capture_output=True, text=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertTrue(json.loads(result.stdout)['native']['linked'])
                    ran = self.progress.run([str(work / 'program')], capture_output=True)
                    self.assertEqual((ran.returncode, ran.stdout, ran.stderr), (0, b'', b''))
                # Direct linking invokes the pinned link editor itself.  The
                # configured cc is only a compatibility input for recipe
                # capture; it must not be called for either executable link.
                commands = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
                links = [args for args in commands if any(Path(a).name == 'entry.o' for a in args)]
                self.assertEqual(links, [])

        def test_env_and_registered_products_work_away_from_the_checkout(self):
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
            with tempfile.TemporaryDirectory(prefix='hirsel-installed-') as temporary:
                work = Path(temporary)
                app, root = work / 'app', work / 'toolchain'
                root.mkdir()
                installed = root / 'hirsel'
                shutil.copy2(compiler, installed)
                def invoke(executable, *arguments, expected=0):
                    result = self.progress.run([str(executable), *arguments], cwd=work, capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    return result
                invoke(compiler, 'core-product', '--runtime', str(runtime), expected=2)
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

        def test_a_copied_install_runs_in_a_clean_environment_with_nothing_outside_its_prefix(self):
            # Attack 984: an install copied into an empty prefix must load
            # every native tool and library from that prefix or the system,
            # with no environment variable, no checkout and no cc on PATH.
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVE']).resolve()
            with tempfile.TemporaryDirectory(prefix='hirsel-prefix-') as temporary:
                work = Path(temporary)
                root, decoy, home = work / 'toolchain', work / 'path-bin', work / 'home'
                for directory in [root, decoy, home, work / 'data', work / 'empty']:
                    directory.mkdir()
                installed = root / 'hirsel'
                found = work / 'path-tool.log'
                for tool in ['cc', 'gcc', 'clang', 'ld', 'ld.lld']:
                    (decoy / tool).write_text('#!/bin/sh\necho ' + tool + ' >> ' + repr(str(found)) + '\nexit 1\n')
                    (decoy / tool).chmod(0o755)
                install = self.progress.run([str(compiler), 'std-product', '--install', str(root), '--runtime', str(runtime)],
                                         cwd=work, capture_output=True, text=True)
                self.assertEqual(install.returncode, 0, install.stdout + install.stderr)
                clean = {'PATH': str(decoy) + ':/usr/bin:/bin', 'HOME': str(home)}
                def invoke(*arguments, expected=0, cwd=work):
                    result = self.progress.run([str(installed), *arguments], cwd=cwd, env=clean, capture_output=True, text=True)
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                    return result
                app = work / 'app'
                app.mkdir()
                (app / 'main.hirsel').write_text(
                    'pub fn main(d: Dir) -> Result[(), Exit] waits {\n'
                    '    match std.file.read_string(d, "input.txt") {\n'
                    '        Ok(_) => Ok(()),\n'
                    '        Error(_) => Error(Exit(status: 3)),\n'
                    '    }\n'
                    '}\n\n'
                    '@test\n'
                    'fn passes() -> Result[(), Exit] {\n'
                    '    Ok(())\n'
                    '}\n')
                (work / 'data' / 'input.txt').write_text('granted\n')
                invoke('fmt', '--check', str(app))
                self.assertTrue(json.loads(invoke('check', str(app), '--json').stdout)['accepted'])
                for engine in ['cranelift', 'llvm']:
                    for profile in [[], ['--release']]:
                        flags = ['--backend', engine, *profile]
                        program = work / ('program-' + engine + ''.join(profile))
                        self.assertTrue(json.loads(invoke('build', str(app), *flags, '-o', str(program), '--json').stdout)['native']['linked'])
                        invoke('run', str(app), *flags, '--dir', str(work / 'data'))
                        # The granted directory is the one the program reads.
                        invoke('run', str(app), *flags, '--dir', str(work / 'empty'), expected=3)
                        invoke('test', str(app), *flags, '--limit', '67108864')
                # An LLVM build in a fresh store loads the prefix's engine libLLVM, and
                # with it none of the deb library's z3, ICU, libxml2, libedit, libffi,
                # zstd or libbsd: the loader's own record of what it initialised.
                trace = work / 'loader'
                traced = self.progress.run([str(installed), 'build', str(app), '--backend', 'llvm', '--release',
                                         '--store', str(work / 'traced-store'), '-o', str(work / 'traced'), '--json'],
                                        cwd=work, env=clean | {'LD_DEBUG': 'libs', 'LD_DEBUG_OUTPUT': str(trace)},
                                        capture_output=True, text=True)
                self.assertEqual(traced.returncode, 0, traced.stdout + traced.stderr)
                loaded = {Path(line.split('calling init: ', 1)[1].strip()).resolve()
                          for log in work.glob('loader.*') for line in log.read_text().splitlines() if 'calling init: ' in line}
                self.assertIn((root / 'lib/libLLVM.so.23.1').resolve(), loaded)
                self.assertEqual([p for p in loaded if re.match(r'lib(z3|icu|xml2|edit|ffi|zstd|bsd)', p.name)], [])
                flat_compiler = root / 'bin/hirsel'
                shutil.copy2(installed, flat_compiler)
                stock = ROOT / 'tools/buck2/toolchains/llvm-files/lib/libLLVM.so.23.1'
                flat_env = clean | {'LD_LIBRARY_PATH': str(stock.parent)}
                flat_app = work / 'flat-app'
                invoke('new', str(flat_app))
                def flat_build():
                    return self.progress.run([
                        str(flat_compiler), 'build', str(flat_app), '--backend', 'llvm', '--release',
                        '--store', str(work / 'flat-store'), '-o', str(work / 'flat-program'),
                    ], cwd=work, env=flat_env, capture_output=True, text=True)
                built = flat_build()
                self.assertEqual(built.returncode, 0, built.stdout + built.stderr)
                flat_run = self.progress.run([str(work / 'flat-program')], cwd=work, capture_output=True)
                self.assertEqual(flat_run.returncode, 0, flat_run.stderr)
                bundled = root / 'lib/libLLVM.so.23.1'
                original = bundled.with_suffix('.saved')
                bundled.rename(original)
                bundled.symlink_to(stock)
                refused = flat_build()
                self.assertEqual(refused.returncode, 2, refused.stdout + refused.stderr)
                self.assertIn('wrong build ID', refused.stdout + refused.stderr)
                bundled.unlink()
                refused = flat_build()
                self.assertEqual(refused.returncode, 2, refused.stdout + refused.stderr)
                self.assertIn('pinned LLVM engine missing', refused.stdout + refused.stderr)
                original.rename(bundled)
                # Another target links through the cross C driver it names, with
                # the prefix's own ld.lld: never one that driver finds on PATH.
                arm = Path(os.environ['HIRSEL_TOOLCHAIN_ARM']).resolve()
                cross = Path(os.environ['HIRSEL_CORE_RT_ARCHIVES']).resolve() / 'aarch64-unknown-linux-gnu/libhirsel_core_rt.a'
                invoke('std-product', '--install', str(root), '--runtime', str(cross), '--target', 'aarch64-unknown-linux-gnu')
                self.assertTrue(json.loads(invoke('build', str(app), '--release', '--target', 'aarch64-unknown-linux-gnu',
                                                  '--cc', str(arm / 'bin/aarch64-none-linux-gnu-gcc'),
                                                  '-o', str(work / 'program-aarch64'), '--json').stdout)['native']['linked'])
                self.assertFalse(found.exists(), found.read_text() if found.exists() else '')
                linker = root / 'bin' / 'ld.lld'
                linker.unlink()
                refused = invoke('build', str(app), '--release', '-o', str(work / 'gone'), expected=2)
                self.assertIn(str(linker), refused.stderr)

        def test_fmt_checks_writes_and_refuses_without_partial_changes(self):
            with tempfile.TemporaryDirectory(prefix='hirsel-fmt-') as temporary:
                package = Path(temporary)
                main = package / 'main.hirsel'
                original = 'pub fn main()->Result[(),Exit]{Ok(())}\n'
                main.write_text(original)
                def fmt(*arguments):
                    return self.progress.run([str(compiler), 'fmt', *arguments, str(package)], cwd=ROOT, capture_output=True, text=True)
                check = fmt('--check')
                self.assertEqual(check.returncode, 1, check.stderr)
                self.assertEqual(check.stdout.splitlines(), [str(main)])
                self.assertEqual(main.read_text(), original)
                written = fmt()
                self.assertEqual(written.returncode, 0, written.stderr)
                self.assertNotEqual(main.read_text(), original)
                self.assertEqual(fmt('--check').returncode, 0)
                main.write_text(original)
                broken = package / 'z_bad.hirsel'
                broken.write_text('pub fn broken(\n')
                refused = fmt()
                self.assertEqual(refused.returncode, 1)
                self.assertIn(str(broken), refused.stderr)
                self.assertEqual(main.read_text(), original)
                self.assertEqual(broken.read_text(), 'pub fn broken(\n')
                broken.unlink()
                main.write_text('pub fn declaration(x:u64)->u64\n')
                self.assertEqual(fmt().returncode, 1)
                self.assertEqual(fmt('--signatures').returncode, 0)

        def test_explain_reads_the_current_specification_catalogue(self):
            def explain(*arguments):
                return self.progress.run([str(compiler), 'explain', *arguments], cwd=ROOT, capture_output=True, text=True)
            result = explain('LANG-7', '--json')
            self.assertEqual(result.returncode, 0, result.stderr)
            rule = json.loads(result.stdout)
            self.assertEqual(rule['id'], 'LANG-7')
            spec = (ROOT / rule['spec_file']).read_text()
            self.assertIn('### LANG-7: ' + rule['title'], spec)
            self.assertIn(rule['statement'].split()[0], spec)
            listed = explain('--list', '--json')
            self.assertEqual(listed.returncode, 0, listed.stderr)
            rules = json.loads(listed.stdout)
            ids = [r['id'] for r in rules]
            self.assertEqual(ids, sorted(set(ids)))
            self.assertIn(rule, rules)
            text = explain('LANG-7')
            self.assertEqual(text.returncode, 0, text.stderr)
            self.assertIn(rule['statement'], text.stdout)
            self.assertIn(rule['spec_file'] + '#' + rule['anchor'], text.stdout)
            for arguments in [(), ('UNKNOWN-999',), ('--list', 'LANG-7')]:
                self.assertEqual(explain(*arguments).returncode, 2)

        def test_cross_runner_replacement_and_invalid_sysroot_are_not_cached(self):
            arm = Path(os.environ['HIRSEL_TOOLCHAIN_ARM']).resolve()
            qemu = Path(os.environ['HIRSEL_TOOLCHAIN_QEMU']).resolve() / 'qemu-aarch64'
            sysroot = arm / 'aarch64-none-linux-gnu/libc'
            runtime = Path(os.environ['HIRSEL_CORE_RT_ARCHIVES']).resolve() / 'aarch64-unknown-linux-gnu/libhirsel_core_rt.a'
            with tempfile.TemporaryDirectory(prefix='hirsel-cross-record-') as temporary:
                work = Path(temporary)
                app = work / 'app'
                self.assertEqual(self.progress.run([str(compiler), 'new', str(app)], cwd=ROOT, capture_output=True).returncode, 0)
                emulator = work / 'qemu'
                selected_root = work / 'sysroot'
                selected_root.symlink_to(sysroot, target_is_directory=True)
                def wrapper(marker):
                    import shlex
                    emulator.write_text('#!/bin/sh\n' + ("printf '%s\n' " + shlex.quote(marker) + ' >&2\n' if marker else '')
                                        + 'exec ' + shlex.quote(str(qemu)) + ' "$@"\n')
                    emulator.chmod(0o755)
                wrapper('')
                command = [str(compiler), 'run', str(app), '--backend', 'llvm', '--target', 'aarch64-unknown-linux-gnu',
                           '--runtime', str(runtime), '--cc', str(arm / 'bin/aarch64-none-linux-gnu-gcc'),
                           '--emulator', str(emulator), '--sysroot', str(selected_root), '--json', '-o', str(work / 'program')]
                def run():
                    return self.progress.run(command, cwd=ROOT, capture_output=True, text=True)
                first = run()
                self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
                self.assertEqual((work / 'program').read_bytes()[18:20], (183).to_bytes(2, 'little'))
                wrapper('replacement-emulator')
                replaced = run()
                self.assertEqual(replaced.returncode, 0, replaced.stdout + replaced.stderr)
                self.assertIn('replacement-emulator', replaced.stderr)
                self.assertEqual(json.loads(replaced.stdout)['native']['compiled'], 0)
                self.assertFalse(json.loads(replaced.stdout)['native']['linked'], f'first: {first.stdout}\nreplaced: {replaced.stdout}\n{replaced.stderr}')
                selected_root.unlink()
                empty = work / 'empty-root'
                empty.mkdir()
                selected_root.symlink_to(empty, target_is_directory=True)
                refused = run()
                self.assertEqual(refused.returncode, 2)
                self.assertIn('runtime sysroot', refused.stderr)
                self.assertIn('ld-linux-aarch64.so.1', refused.stderr)
                selected_root.unlink()
                selected_root.symlink_to(sysroot, target_is_directory=True)
                emulator.chmod(0o644)
                refused = run()
                self.assertEqual(refused.returncode, 2)
                self.assertIn('execution emulator', refused.stderr)

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
                    result = self.progress.run([str(compiler), command, *options, '--store', str(store), '--json'],
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
                result = self.progress.run([str(compiler), 'check', str(app), '--libraries', str(libraries),
                                         '--store', str(work / 'bad-source'), '--json'],
                                        cwd=ROOT, capture_output=True, text=True)
                product = json.loads(result.stdout)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(product['accepted'])
                self.assertTrue(any(d['rule'] == 'LANG-11' and d['primary']['module'] == 'std@1/user_refused'
                                    for d in product['diagnostics']), product)

    # The gate runs on the host (openat2), so without this every case would
    # share the user's real default store and host-tool records with every
    # other process on the machine. Each run gets its own.
    with tempfile.TemporaryDirectory(prefix='hirsel-library-modes-cache-') as cache:
        os.environ['XDG_CACHE_HOME'] = cache
        result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(LibraryModes))
    return int(not result.wasSuccessful())


if __name__ == '__main__':
    raise SystemExit(main())
