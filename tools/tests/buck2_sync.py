#!/usr/bin/env python3
"""Check Buck2 inventories and exclusive temporary-file publication."""
import importlib.util
import ast
import contextlib
import io
from pathlib import Path
import sys
import tempfile
import unittest
import json
import subprocess
import threading
from concurrent.futures import ThreadPoolExecutor
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'buck2'))
SPEC = importlib.util.spec_from_file_location('buck2_sync', Path(__file__).resolve().parents[1] / 'buck2/sync.py')
sync = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sync)


class ExecutorConfiguration(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.local = self.root / '.buckconfig.local'

    def test_missing_execution_configuration_is_not_created(self):
        self.assertFalse(sync.prepare.configure_test_executor(self.root))
        self.assertFalse(self.local.exists())

    def test_remote_hook_configuration_gets_a_checkout_absolute_executor(self):
        remote = '[buck2_re_client]\naddress = pool\n'
        self.local.write_text(remote + '[test]\nv2_test_executor = tools/buck2/test_executor.py\n')
        self.assertTrue(sync.prepare.configure_test_executor(self.root))
        configured = self.local.read_text()
        self.assertTrue(configured.startswith(remote))
        self.assertIn(f'v2_test_executor = {self.root}/tools/buck2/test_executor.py\n', configured)
        self.assertEqual(configured.count('v2_test_executor'), 1)
        self.assertFalse(sync.prepare.configure_test_executor(self.root))
        self.assertEqual(self.local.read_text(), configured)

    def test_if_stale_still_repairs_executor_after_hook_rewrites_configuration(self):
        self.local.write_text('[buck2_re_client]\naddress = pool\n')
        reindeer = self.root / 'reindeer'
        reindeer.touch()
        with patch.object(sync, 'ROOT', self.root), patch.object(sync, 'REINDEER', reindeer), \
             patch.object(sync.generated, 'stale', return_value=[]), \
             patch.object(sys, 'argv', ['sync.py', '--if-stale']):
            self.assertEqual(sync.main(), 0)
        self.assertIn(f'v2_test_executor = {self.root}/tools/buck2/test_executor.py', self.local.read_text())


class ProductionFeatures(unittest.TestCase):
    def graph(self, *, leaked=True, build=False):
        return (sync.call('hirsel_binary', name='compiler', package_name='compiler', features=[],
                          deps=['//:middle'])
                + sync.call('hirsel_library', name='middle', package_name='middle', features=[],
                            deps=[] if build else ['//:fixture'],
                            build_script=':fixture' if build else None)
                + sync.call('hirsel_build_script' if build else 'hirsel_library',
                            name='fixture', package_name='fixture',
                            features=['testing'] if leaked else [], deps=[]))

    def policy(self, exceptions=()):
        return {'features': {'fixture/testing': 'test-only'}, 'roots': {'compiler': list(exceptions)}}

    def test_strict_finds_a_transitive_feature_even_with_a_baseline_exception(self):
        with self.assertRaisesRegex(SystemExit, 'compiler: unapproved test feature fixture/testing'):
            sync.check_production_features(self.graph(), self.policy(['fixture/testing']), strict=True)

    def test_production_graph_without_test_features_passes_strict(self):
        self.assertEqual(sync.check_production_features(self.graph(leaked=False), self.policy(), strict=True), [])

    def test_exact_exceptions_pass_but_new_leaks_and_stale_exceptions_fail(self):
        self.assertEqual(sync.check_production_features(self.graph(), self.policy(['fixture/testing'])),
                         ['compiler: fixture/testing'])
        with self.assertRaisesRegex(SystemExit, 'unapproved test feature'):
            sync.check_production_features(self.graph(), self.policy())
        with self.assertRaisesRegex(SystemExit, 'remove resolved feature exception'):
            sync.check_production_features(self.graph(leaked=False), self.policy(['fixture/testing']))

    def test_build_script_features_are_audited(self):
        with self.assertRaisesRegex(SystemExit, 'fixture/testing'):
            sync.check_production_features(self.graph(build=True), self.policy(), strict=True)

    def test_unclassified_features_and_missing_roots_are_refused(self):
        policy = self.policy()
        policy['features'] = {}
        with self.assertRaisesRegex(SystemExit, 'no classification'):
            sync.check_production_features(self.graph(), policy)
        with self.assertRaisesRegex(SystemExit, 'no first-party target'):
            sync.check_production_features('', self.policy())



class CargoUnits(unittest.TestCase):
    def test_shared_graphs_relocate_and_follow_cargo_inputs(self):
        from cargo_units import Resolution

        with tempfile.TemporaryDirectory() as temporary:
            scratch = Path(temporary)
            cargo_home = scratch / 'cargo-home'
            dependency = 'registry/src/dependency-1.0.0'
            registry = 'registry+https://github.com/rust-lang/crates.io'
            external = dict(id=f'{registry}#dependency@1.0.0', name='dependency', version='1.0.0',
                            manifest_path=str(cargo_home / dependency / 'Cargo.toml'), source=registry)
            roots = [scratch / name for name in ('first', 'second # worktree')]
            metadata = []
            for root in roots:
                (root / 'app/src').mkdir(parents=True)
                (root / 'third-party').mkdir()
                for name, contents in {
                    'Cargo.toml': '[workspace]\nmembers = ["app"]\n',
                    'Cargo.lock': 'version = 4\n',
                    'app/Cargo.toml': '[package]\nname = "app"\nversion = "1.0.0"\n',
                    'rust-toolchain.toml': '[toolchain]\nchannel = "1.99.0"\n',
                    'third-party/reindeer.toml': ('[platform.linux-x86_64]\n'
                                                  'target = "x86_64-unknown-linux-gnu"\n'
                                                  'execution-platform = true\n'),
                }.items():
                    (root / name).write_text(contents)
                member = f'path+{(root / "app").as_uri()}#1.0.0'
                target = dict(name='app', kind=['lib'], src_path=str(root / 'app/src/lib.rs'))
                package = dict(id=member, name='app', version='1.0.0', targets=[target],
                               manifest_path=str(root / 'app/Cargo.toml'), source=None)
                metadata.append(dict(packages=[package, external], workspace_members=[member],
                                     workspace_root=str(root), target_directory=str(root / 'target')))

            def cargo(command, cwd, **kwargs):
                package = metadata[roots.index(cwd)]['packages'][0]
                registry_source = Path(kwargs['env']['CARGO_HOME']) / dependency / 'src/lib.rs'
                library = dict(pkg_id=external['id'], mode='build', platform='x86_64-unknown-linux-gnu',
                               target=dict(name='dependency', kind=['lib'], src_path=str(registry_source)),
                               features=[], dependencies=[])
                unit = dict(pkg_id=package['id'], target=package['targets'][0],
                            mode=command[1], platform='x86_64-unknown-linux-gnu',
                            features=[], dependencies=[dict(index=0, extern_crate_name='dependency')])
                return subprocess.CompletedProcess(command, 0, json.dumps(
                    dict(version=1, roots=[1], units=[library, unit])), '')

            with patch.dict('os.environ', XDG_CACHE_HOME=str(scratch / 'cache')), \
                 patch('cargo_units.rust_environment.environment', return_value={
                     'CARGO': 'cargo', 'CARGO_HOME': str(cargo_home)}), \
                 patch('cargo_units.subprocess.run', side_effect=cargo) as queries:
                Resolution(roots[0], metadata[0])
                self.assertEqual(queries.call_count, 2)
                warm = Resolution(roots[1], metadata[1])
                self.assertEqual(queries.call_count, 2, 'the second checkout reuses both units')
                paths = {unit['pkg_id']: unit['target']['src_path'] for unit in warm.units.values()}
                self.assertEqual(paths, {
                    metadata[1]['workspace_members'][0]: str(roots[1] / 'app/src/lib.rs'),
                    external['id']: str(cargo_home / dependency / 'src/lib.rs'),
                })
                for name in ('Cargo.lock', 'app/Cargo.toml', 'rust-toolchain.toml'):
                    with self.subTest(name=name):
                        path = roots[1] / name
                        original = path.read_text()
                        path.write_text(original + '# changed input\n')
                        queries.reset_mock()
                        Resolution(roots[1], metadata[1])
                        self.assertEqual(queries.call_count, 2, f'{name} changes the cache key')
                        path.write_text(original)

    def test_target_graphs_preserve_cross_only_edges_and_native_fixups(self):
        from cargo_units import Resolution, calls, literal, configured
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'Cargo.toml').write_text('[workspace]\n')
            (root / 'Cargo.lock').write_text('version = 4\n')
            (root / 'rust-toolchain.toml').write_text('[toolchain]\nchannel="stable"\n')
            (root / 'third-party').mkdir()
            (root / 'third-party/reindeer.toml').write_text(
                '[platform.linux-x86_64]\ntarget="x86_64-unknown-linux-gnu"\nexecution-platform=true\n'
                '[platform.linux-arm64]\ntarget="aarch64-unknown-linux-gnu"\nexecution-platform=false\n')
            targets = []
            def graph(command, **kwargs):
                target = command[command.index('--target') + 1] if '--target' in command else None
                targets.append(target)
                arm = target == 'aarch64-unknown-linux-gnu'
                def unit(name, deps=(), features=(), mode='build'):
                    return dict(pkg_id=name, target=dict(name=name, kind=['lib']), mode=mode,
                                platform=target, features=list(features),
                                dependencies=[dict(index=i, extern_crate_name=n) for n, i in deps])
                return subprocess.CompletedProcess(command, 0, json.dumps(dict(version=1, roots=[2], units=[
                    unit('system'), unit('probe', [('system', 0)] if arm else []),
                    unit('app', [('probe', 1)] + ([('system', 0)] if arm else []),
                         ['arm'] if arm else [], 'test' if command[1] == 'test' else 'build'),
                    dict(unit('system'), platform=None)])), '')
            meta = dict(packages=[dict(id=n, name=n, version='1.0.0',
                                       manifest_path=str(root / 'Cargo.toml'), source=None)
                                  for n in ('app', 'probe', 'system')],
                        workspace_members=['app'])
            with patch.dict('os.environ', XDG_CACHE_HOME=str(root / 'cache')), \
                 patch('cargo_units.rust_environment.environment', return_value={
                     'CARGO': 'cargo', 'CARGO_HOME': str(root / 'cargo-home')}), \
                 patch('cargo_units.subprocess.run', side_effect=graph):
                resolution = Resolution(root, meta)
            systems = [u for u in resolution.units.values() if u['pkg_id'] == 'system']
            self.assertEqual(len(systems), 2)
            self.assertEqual(next(u['variants'].keys() for u in systems if 'variants' in u),
                             {'linux-x86_64', 'linux-arm64'})
            source = ''.join(sync.call('third_party_rust_library', name=n + '-1', features=[], deps=[],
                                      platform={'linux-x86_64': {}, 'linux-arm64':
                                                {'deps': [':system-1', ':native-hook']} if n == 'probe' else {}},
                                      env={'CARGO_PKG_NAME': n, 'CARGO_PKG_VERSION': '1.0.0'})
                             for n in ('probe', 'system'))
            projected = {literal(call, 'name'): attrs for _, call, attrs in calls(resolution.third_party(source))}
            probe = next(attrs for attrs in projected.values()
                         if ast.literal_eval(attrs['env'])['CARGO_PKG_NAME'] == 'probe')
            platforms = ast.literal_eval(probe['platform'])
            for platform, expected in [('linux-x86_64', {}), ('linux-arm64', {'system'})]:
                names = ast.literal_eval(probe.get('named_deps', ast.Dict(keys=[], values=[])))
                names.update(platforms[platform].get('named_deps', {}))
                self.assertEqual(set(names), set(expected))
            self.assertIn(':native-hook', platforms['linux-arm64']['deps'])
            self.assertNotIn(':system-1', platforms['linux-arm64']['deps'])
            first = resolution.first_party(sync.call('hirsel_library', name='app', package_name='app', features=[], deps=[]), sync.starlark)
            app = next(attrs for _, call, attrs in calls(first) if literal(call, 'name') == 'app')
            self.assertEqual(configured(app['features'], 'linux-arm64'), ['arm'])
            self.assertEqual(configured(app['features'], 'linux-x86_64'), [])
            self.assertEqual(len(configured(app['deps'], 'linux-arm64')), 2)
            self.assertEqual(len(configured(app['deps'], 'linux-x86_64')), 1)
            policy = {'features': {'app/arm': 'test-only'}, 'roots': {'app': []}}
            with self.assertRaisesRegex(SystemExit, 'app/arm'):
                sync.check_production_features(first, policy, strict=True)
            self.assertEqual(set(targets), {'x86_64-unknown-linux-gnu', 'aarch64-unknown-linux-gnu'})

    def test_new_runtime_archives_keep_their_own_cargo_library_and_namespace(self):
        from cargo_units import Resolution, calls, literal
        r = Resolution.__new__(Resolution)
        r.packages = {name: {'name': name, 'version': '1'} for name in ('layout', 'core', 'old')}
        r.members = set(r.packages)
        r.units, r.names, r.preferred, r.test_binaries = {}, {}, {}, {}
        graph = {'units': [
            dict(pkg_id='layout',target=dict(name='layout',kind=['lib']),mode='build',features=[],dependencies=[]),
            dict(pkg_id='core',target=dict(name='core',kind=['lib']),mode='build',features=[],
                 dependencies=[dict(index=0,extern_crate_name='layout')]),
            dict(pkg_id='old',target=dict(name='old',kind=['lib']),mode='build',features=[],dependencies=[]),
        ]}
        keys = r.add(graph)
        for index in range(3):
            base = r.base(r.units[keys[index]])
            r.preferred[base] = keys[index]
            r.names[keys[index]] = base
        r.runtime = {'x86_64-unknown-linux-gnu': keys[2], 'aarch64-unknown-linux-gnu': keys[2]}
        source = ''.join(sync.call('hirsel_library',name=n,package_name=n,features=[],deps=[],crate_root='src/lib.rs')
                         for n in ('layout','core','old'))
        source += sync.static_archive_rule({'name':'core'}, {}, {'crate':'core'})
        projected = {literal(call,'name'):(call.func.id,attrs) for _,call,attrs in calls(r.first_party(source,sync.starlark))}
        self.assertEqual(ast.literal_eval(projected['core'][1]['deps']), ['//:layout'])
        kind, archive = projected['core-archives']
        self.assertEqual(kind, 'hirsel_core_runtime_archives')
        self.assertEqual(ast.literal_eval(archive['library']), '//:core')
        self.assertEqual(ast.literal_eval(archive['archive_name']), 'libcore.a')
        self.assertNotIn('target_deps', archive)

    def test_standalone_runtime_owns_allocator_only_in_shipping_archive(self):
        from cargo_units import calls, literal
        source = sync.static_archive_rule({'name': 'hirsel-core-rt', 'dependencies': []}, {},
                                         {'crate': 'hirsel_core_rt', 'crate_root': 'src/lib.rs',
                                          'features': []})
        targets = {literal(call, 'name'): (call.func.id, attrs)
                   for _, call, attrs in calls(source)}
        kind, standalone = targets['hirsel-core-rt-standalone']
        self.assertEqual(kind, 'hirsel_library')
        self.assertEqual(ast.literal_eval(standalone['features']), ['standalone'])
        _, archive = targets['hirsel-core-rt-archives']
        self.assertEqual(ast.literal_eval(archive['library']), '//:hirsel-core-rt-standalone')

    def test_dev_features_fork_transitive_users_but_share_unchanged_leaves(self):
        from cargo_units import Resolution, calls, literal
        r = Resolution.__new__(Resolution)
        r.packages = {name: {'name': name, 'version': '1'} for name in ('leaf', 'fixture', 'app')}
        r.members = set(r.packages)
        r.units, r.names, r.preferred, r.test_binaries = {}, {}, {}, {}
        def unit(package, features=(), deps=(), mode='build'):
            return dict(pkg_id=package, target=dict(name=package, kind=['lib']), mode=mode,
                        features=list(features), dependencies=[dict(index=i, extern_crate_name=name) for name, i in deps])
        graph = {'units': [unit('leaf'), unit('fixture', deps=[('leaf', 0)]),
                           unit('app', deps=[('fixture', 1)]),
                           unit('fixture', ['testing'], [('leaf', 0)]),
                           unit('app', deps=[('fixture', 3)]),
                           unit('app', deps=[('fixture', 3)], mode='test')]}
        keys = r.add(graph)
        for index in (0, 1, 2, 5):
            r.preferred[r.base(r.units[keys[index]])] = keys[index]
        for key, value in r.units.items():
            base = r.base(value)
            r.names[key] = base if r.preferred.get(base) == key else base + '-features-' + key[:12]
        source = ''.join(sync.call('hirsel_library' if name != 'app-unittest' else 'hirsel_test',
                                   name=name, package_name=name.split('-')[0], features=['testing'],
                                   deps=['//:fixture'], crate_root='src/lib.rs')
                         for name in ('leaf', 'fixture', 'app', 'app-unittest'))
        rules = {literal(call, 'name'): {name: ast.literal_eval(value) for name, value in attrs.items()}
                 for _, call, attrs in calls(r.first_party(source, sync.starlark))}
        self.assertEqual(rules['fixture']['features'], [])
        self.assertEqual(rules['app']['deps'], ['//:fixture'])
        test_fixture = rules['app-unittest']['deps'][0].removeprefix('//:')
        self.assertEqual(rules[test_fixture]['features'], ['testing'])
        self.assertEqual(rules[test_fixture]['deps'], ['//:leaf'])
        self.assertEqual(sum(name.startswith('leaf') for name in rules), 1)
        self.assertEqual(rules[r.names[keys[4]]]['deps'], ['//:' + test_fixture])

    def test_external_build_script_and_host_features_follow_their_own_units(self):
        from cargo_units import Resolution, calls, literal
        import ast
        r = Resolution.__new__(Resolution)
        r.packages = {'ext': {'name': 'ext', 'version': '1.0.0'}}
        r.members, r.units, r.names = set(), {}, {}
        graph = {'units': [
            dict(pkg_id='ext', target=dict(name='ext', kind=['lib']), mode='build', features=[], dependencies=[]),
            dict(pkg_id='ext', target=dict(name='build-script-build', kind=['custom-build']), mode='build',
                 features=['testing'], dependencies=[]),
            dict(pkg_id='ext', target=dict(name='build-script-build', kind=['custom-build']), mode='run-custom-build',
                 features=['testing'], dependencies=[dict(index=1, extern_crate_name='build_script_build')]),
            dict(pkg_id='ext', target=dict(name='ext', kind=['lib']), mode='build', features=['testing'],
                 dependencies=[dict(index=2, extern_crate_name='build_script_build')])]}
        keys = r.add(graph)
        source = (sync.call('third_party_rust_library', name='ext-1', features=['testing'], deps=[],
                            env={'CARGO_PKG_NAME': 'ext', 'CARGO_PKG_VERSION': '1.0.0',
                                 'OUT_DIR': '$(location :ext-1-build-script-run[out_dir])'})
                  + sync.call('third_party_rust_binary', name='ext-1-build-script-build', features=['testing'], deps=[])
                  + sync.call('third_party_buildscript_run', name='ext-1-build-script-run', features=['testing'],
                              buildscript_rule=':ext-1-build-script-build'))
        projected = {literal(call, 'name'): {name: ast.literal_eval(value) for name, value in attrs.items()}
                     for _, call, attrs in calls(r.third_party(source))}
        self.assertEqual(projected[r.names[keys[0]]]['features'], [])
        selected = projected[r.names[keys[3]]]
        self.assertEqual(selected['features'], ['testing'])
        self.assertEqual(selected['env']['OUT_DIR'], '$(location :' + r.names[keys[2]] + '[out_dir])')
        self.assertEqual(projected[r.names[keys[2]]]['buildscript_rule'], ':' + r.names[keys[1]])

    def test_external_aliases_prefer_features_and_align_shared_dependencies(self):
        from cargo_units import Resolution, calls, literal
        import ast
        r = Resolution.__new__(Resolution)
        r.packages = {name: {'name': name, 'version': '1.0.0'} for name in ('consumer', 'external', 'leaf')}
        r.members, r.units, r.names = set(), {}, {}
        keys = r.add({'units': [
            dict(pkg_id='leaf', target=dict(name='leaf', kind=['lib']), mode='build', features=[], dependencies=[]),
            dict(pkg_id='leaf', target=dict(name='leaf', kind=['lib']), mode='build', features=['rc3'], dependencies=[]),
            dict(pkg_id='external', target=dict(name='external', kind=['lib']), mode='build', features=[],
                 dependencies=[dict(index=0, extern_crate_name='renamed_leaf')]),
            dict(pkg_id='external', target=dict(name='external', kind=['lib']), mode='build', features=['derive'],
                 dependencies=[dict(index=1, extern_crate_name='renamed_leaf')]),
            dict(pkg_id='consumer', target=dict(name='consumer', kind=['lib']), mode='build', features=[],
                 dependencies=[dict(index=0, extern_crate_name='renamed_leaf')]),
            dict(pkg_id='consumer', target=dict(name='consumer', kind=['lib']), mode='build', features=[],
                 dependencies=[dict(index=1, extern_crate_name='renamed_leaf')])]})
        source = ''.join(sync.call('third_party_rust_library', name=name + '-1', features=['testing'], deps=[],
                                  named_deps={'renamed_leaf': ':leaf-1', 'unused': ':leaf-1'} if name != 'leaf' else {},
                                  env={'CARGO_PKG_NAME': name, 'CARGO_PKG_VERSION': '1.0.0'})
                         for name in ('leaf', 'external', 'consumer'))
        source += sync.call('alias', name='external', actual=':external-1', visibility=['PUBLIC'])
        source += sync.call('alias', name='consumer', actual=':consumer-1', visibility=['PUBLIC'])
        projected = {literal(call, 'name'): attrs for _, call, attrs in calls(r.third_party(source))}
        self.assertEqual(ast.literal_eval(projected[r.names[keys[2]]]['named_deps']),
                         {'renamed_leaf': '//third-party:' + r.names[keys[0]]})
        self.assertEqual(ast.literal_eval(projected['external']['actual']), ':' + r.names[keys[3]])
        self.assertEqual(ast.literal_eval(projected['consumer']['actual']), ':' + r.names[keys[5]])



class AtomicScratch(unittest.TestCase):
    def concurrent_writes(self, writer, output):
        barrier = threading.Barrier(2)
        replace = Path.replace

        def publish(path, destination):
            barrier.wait()
            return replace(path, destination)

        with patch.object(Path, 'replace', publish), ThreadPoolExecutor(2) as workers:
            futures = [workers.submit(writer, i) for i in range(2)]
            for future in futures:
                future.result()
        self.assertTrue(output.is_file())
        self.assertEqual(list(output.parent.iterdir()), [output])

    def test_inventory_writers_use_distinct_exclusive_temporary_files(self):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / 'BUCK'
            self.concurrent_writes(lambda i: sync.write(output, str(i)), output)
            self.assertIn(output.read_text(), ['0', '1'])

    def test_stamp_writers_use_distinct_exclusive_temporary_files(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            reads = sync.generated.Reads(root)
            output = root / sync.generated.STAMP
            self.concurrent_writes(lambda _: sync.generated.record(root, 'first-party', reads), output)
            self.assertIn('first-party', json.loads(output.read_text())['parts'])


class GeneratedStamp(unittest.TestCase):
    """generated.py's stamp: a part is current while every output is as written
    and everything its generation read reads the same."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        (self.root / 'src').mkdir()
        (self.root / 'src/lib.rs').write_text('fn f() {}\n')
        (self.root / 'README.md').write_text('notes\n')
        for part in sync.generated.PARTS:
            self.generate(part)

    def generate(self, part):
        reads = sync.generated.Reads(self.root)
        reads.text(self.root / 'src/lib.rs')
        reads.is_file(self.root / 'src/main.rs')
        reads.glob(self.root / 'src', '**/*.rs')
        for path in sync.generated.PARTS[part]:
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            (self.root / path).write_text(path + '\n')
        sync.generated.record(self.root, part, reads)

    def test_a_part_is_current_until_a_read_a_probe_a_glob_or_an_output_changes(self):
        stale = lambda: sync.generated.stale(self.root)  # noqa: E731
        self.assertEqual(stale(), [])
        (self.root / 'README.md').write_text('more notes\n')
        self.assertEqual(stale(), [], 'generation never read it')
        for change, undo, parts in [
                (lambda: (self.root / 'src/lib.rs').write_text('fn g() {}\n'),
                 lambda: (self.root / 'src/lib.rs').write_text('fn f() {}\n'), list(sync.generated.PARTS)),
                (lambda: (self.root / 'src/main.rs').write_text(''), lambda: (self.root / 'src/main.rs').unlink(),
                 list(sync.generated.PARTS)),
                (lambda: (self.root / 'src/deep').mkdir() or (self.root / 'src/deep/x.rs').write_text(''),
                 lambda: (self.root / 'src/deep/x.rs').unlink() or (self.root / 'src/deep').rmdir(),
                 list(sync.generated.PARTS)),
                (lambda: (self.root / 'BUCK').write_text('edited\n'), lambda: (self.root / 'BUCK').write_text('BUCK\n'),
                 ['first-party']),
                (lambda: (self.root / 'third-party/BUCK').unlink(),
                 lambda: (self.root / 'third-party/BUCK').write_text('third-party/BUCK\n'), ['third-party'])]:
            change()
            self.assertEqual(stale(), parts)
            undo()
            self.assertEqual(stale(), [])

    def test_a_file_that_changes_while_generation_reads_it_leaves_the_part_stale(self):
        reads = sync.generated.Reads(self.root)
        reads.text(self.root / 'src/lib.rs')
        (self.root / 'src/lib.rs').write_text('fn g() {}\n')
        reads.text(self.root / 'src/lib.rs')
        sync.generated.record(self.root, 'first-party', reads)
        for content in ('fn f() {}\n', 'fn g() {}\n'):
            (self.root / 'src/lib.rs').write_text(content)
            self.assertIn('first-party', sync.generated.stale(self.root))

    def test_a_lane_change_touching_a_generated_file_is_refused_with_the_commands_that_drop_it(self):
        self.assertEqual(sync.generated.refusal(['README.md', 'crates/a/src/lib.rs']), [])
        refused = '\n'.join(sync.generated.refusal(['BUCK', 'README.md']))
        self.assertIn('no lane commits: BUCK', refused)
        self.assertIn('git rm -q --cached BUCK', refused)


class GeneratedParts(unittest.TestCase):
    """The real generation on a copy of HEAD's workspace: each part reruns for
    exactly its inputs, and an unchanged output is never rewritten."""

    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix='hirsel-sync-')
        cls.root = Path(cls.temp.name).resolve()
        archive = subprocess.run(['git', 'archive', 'HEAD', '--', 'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                                  'crates', 'third-party', 'tools/buck2', 'tools/native-toolchain',
                                  'tools/toolchain-dist', 'NOTICES.md', 'README.md', 'deny.toml'],
                                 cwd=sync.ROOT, check=True, capture_output=True).stdout
        subprocess.run(['tar', '-x', '-C', str(cls.root)], input=archive, check=True)
        cls.addClassCleanup(cls.temp.cleanup)
        cls.patches = [patch.object(sync, 'ROOT', cls.root), patch.object(sync, 'HERE', cls.root / 'tools/buck2'),
                       patch.object(sync, 'THIRD_PARTY', cls.root / 'third-party')]
        for active in cls.patches:
            active.start()
            cls.addClassCleanup(active.stop)
        cls.sync()

    @classmethod
    def tearDownClass(cls):
        for active in cls.patches:
            active.stop()
        cls.temp.cleanup()

    @classmethod
    def sync(cls, *flags):
        """Run sync.py; returns the parts it regenerated."""
        with patch.object(sys, 'argv', ['sync.py', *flags]), patch.object(sync, 'generate', wraps=sync.generate) as run:
            assert sync.main() == 0
        return list(run.call_args.args[1]) if run.called else []

    def setUp(self):
        self.assertEqual(sync.generated.stale(self.root), [])
        self.written = self.identities()

    def identities(self):
        return {path: (lambda found: (found.st_ino, found.st_mtime_ns))((self.root / path).stat())
                for path in sync.generated.BUILD_INPUTS}

    def rewritten(self):
        return sorted(path for path, identity in self.identities().items() if identity != self.written[path])

    def edit(self, path, text):
        """Write a file, restoring it (and the generated files) after the test."""
        file = self.root / path
        before = file.read_bytes() if file.exists() else None
        self.addCleanup(self.sync, '--if-stale')
        self.addCleanup(lambda: file.write_bytes(before) if before is not None else file.unlink())
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(text)

    def buck(self):
        return (self.root / 'BUCK').read_text()

    def test_a_documentation_edit_regenerates_nothing(self):
        self.edit('README.md', 'A different readme\n')
        self.edit('docs/notes.md', 'notes\n')
        self.assertEqual(self.sync('--if-stale'), [])

    def test_a_manifest_edit_regenerates_both_parts_and_rewrites_only_what_changed(self):
        # Cargo's dev profile reaches no Buck configuration: both parts are
        # regenerated from the edited manifest, and neither output changes.
        manifest = (self.root / 'Cargo.toml').read_text()
        old = '[profile.dev.package.hirsel-intern]\nopt-level = 1\n'
        self.assertIn(old, manifest)
        self.edit('Cargo.toml', manifest.replace(old, '[profile.dev.package.hirsel-intern]\nopt-level = 2\n'))
        self.assertEqual(self.sync('--if-stale'), list(sync.generated.PARTS))
        self.assertEqual(self.rewritten(), [])

    def test_an_added_test_file_adds_its_target_without_rerunning_reindeer(self):
        self.edit('crates/hirsel-intern/tests/added.rs', 'hirsel_guard::install!();\n\n#[test]\nfn added() {}\n')
        self.assertEqual(self.sync('--if-stale'), ['first-party'])
        self.assertIn('    name = "hirsel-intern-test-added",\n', self.buck())
        self.assertEqual(self.rewritten(), ['BUCK'])

    def library(self, name):
        """The crate's hirsel_library call in the root BUCK."""
        return next(block.split('\n)\n')[0] for block in self.buck().split('hirsel_library(\n')[1:]
                    if f'    name = "{name}",\n' in block.split('\n)\n')[0])

    def test_a_doc_comment_code_block_turns_on_the_crates_doctests(self):
        crate = next(path for path in sorted((self.root / 'crates').glob('*/src/lib.rs'))
                     if 'doctests' not in self.library(path.parents[1].name))
        self.edit(crate.relative_to(self.root), crate.read_text() + '\n/// ```\n/// assert!(true);\n/// ```\nfn documented() {}\n')
        self.assertEqual(self.sync('--if-stale'), ['first-party'])
        self.assertIn('doctests = True', self.library(crate.parents[1].name))


class NativePoolRouting(unittest.TestCase):
    def test_generated_tests_have_no_host_partition(self):
        buck = (sync.ROOT / 'BUCK').read_text()
        self.assertNotIn('host_cases =', buck)
        rust_tests = '\n'.join(block.split('\n)\n', 1)[0] for block in buck.split('hirsel_test(\n')[1:])
        self.assertNotIn('host = True', rust_tests)


class GuardedRoots(unittest.TestCase):
    def test_a_root_or_a_path_module_installs_the_guard(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'plain.rs').write_text('//! A harness.\nuse std::fs;\n// hirsel_guard::install!();\n')
            (root / 'lib.rs').write_text('//! A library.\n#[cfg(test)]\nhirsel_guard::install!();\n')
            (root / 'support.rs').write_text('hirsel_guard::install!(Counting);\n')
            (root / 'main.rs').write_text('#[path = "support.rs"]\nmod support;\n')
            self.assertFalse(sync.guarded(root / 'plain.rs'))
            self.assertTrue(sync.guarded(root / 'lib.rs'))
            self.assertTrue(sync.guarded(root / 'main.rs'))


if __name__ == '__main__':
    unittest.main()
