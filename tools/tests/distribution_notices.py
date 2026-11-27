#!/usr/bin/env python3
"""Packaging controls use a pinned archive and a tiny relocatable bundle."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
DIST = ROOT / 'tools/toolchain-dist'
sys.path.insert(0, str(DIST))
SPEC = importlib.util.spec_from_file_location('notices', Path(os.environ.get('HIRSEL_DISTRIBUTION_NOTICES', ROOT / 'tools/toolchain-dist/notices.py')))
notices = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notices)
# The cited files sync.py found in the tree (--present); run outside Buck, the tree itself.
PRESENT = None


def archive(path, files, prefix='example-1.0'):
    with tarfile.open(path, 'w:gz') as output:
        for name, data in files.items():
            member = tarfile.TarInfo(prefix + '/' + name)
            member.size = len(data)
            output.addfile(member, io.BytesIO(data))


class Notices(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bundle = self.root / 'bundle'
        self.bundle.mkdir()
        (self.bundle / 'hirsel').write_bytes(b'compiler object bytes')
        (self.bundle / 'hirsel-std.products').mkdir()
        (self.bundle / 'hirsel-std.products/query-product').write_bytes(b'ordinary query bytes')
        self.home = self.root / 'cargo'
        self.crate = self.home / 'registry/cache/test/example-1.0.crate'
        self.crate.parent.mkdir(parents=True)
        archive(self.crate, {'LICENSE-MIT': b'actual copyright and MIT terms',
                             'native/LICENSE': b'actual native terms',
                             'NOTICE': b'upstream notice',
                             'native/code.c': b'/* Copyright Author; permission notice */ int code;'})
        self.package = dict(name='example', version='1.0', source='registry+test',
                            license='MIT OR Apache-2.0', license_file=None,
                            manifest_path='/irrelevant/cache/example-1.0/Cargo.toml')
        self.metadata = dict(packages=[self.package])
        self.write_lock()
        self.sources = dict(scope='fixture', bundle_entries=['hirsel', 'hirsel-std.products'],
                            files={'hirsel': dict(crates='root//:compiler', package='compiler', contents='the compiler'),
                                   'hirsel-std.products/*': dict(contents='products')},
                            rust_lock='rust-pin.json', rust_tree='rust', llvm_lock='llvm.lock', native={},
                            native_text_rule='fixture', host_runtimes={},
                            required_texts={}, adapted_source_notices=['third-party/NOTICES.md'], external=[])
        (self.root / 'llvm.lock').write_text('source_url https://example/llvm.tar.xz\nsource_sha256 0\n'
                                             f'static_lld {notices.hashlib.sha256(b"pinned linker").hexdigest()} 13 ld.lld\n')
        self.graph = {
            'root//:compiler (dist#1)': {'deps': ['root//third-party:alias (dist#1)', 'root//:macro (host#2)']},
            'root//third-party:alias (dist#1)': {'buck.deps': ['root//third-party:library (dist#1)']},
            'root//third-party:library (dist#1)': {'deps': [], 'buck.deps': ['root//third-party:example-1.0.crate (dist#1)']},
            'root//:macro (host#2)': {'proc_macro': True, 'deps': ['absent macro-only crate']},
        }
        for name in ['LICENSE', 'Cargo.toml', 'third-party/NOTICES.md', 'tools/toolchain-dist/notice-sources.json', 'tools/toolchain-dist/notices.py']:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('fixture source input')
        (self.root / 'rust-pin.json').write_text('{"rust_version":"test"}')
        rust = self.root / 'rust'
        (rust / 'share/doc/rust/licenses').mkdir(parents=True)
        (rust / '.hirsel-toolchain.json').write_text((self.root / 'rust-pin.json').read_text())
        for name in ['COPYRIGHT-library.html', 'licenses/MIT.txt', 'licenses/Apache-2.0.txt']:
            (rust / 'share/doc/rust' / name).write_text('actual upstream ' + name)

    def write_lock(self):
        (self.root / 'Cargo.lock').write_text(
            '[[package]]\nname = "compiler"\nversion = "0.1.0"\ndependencies = ["example"]\n\n'
            '[[package]]\nname = "example"\nversion = "1.0"\nsource = "registry+test"\n'
            f'checksum = "{notices.sha256(self.crate)}"\n')

    def write_notices(self, archive=None):
        # NOTICES.md as `notices.py --write` leaves it for this fixture.
        linked = {'hirsel': notices.linked_crates(self.graph, 'root//:compiler')}
        texts = notices.native_texts(archive, self.sources) if archive else {}
        (self.root / 'NOTICES.md').write_text(notices.render(notices.inventory(
            self.root, self.sources, linked=linked, metadata=self.metadata, texts=texts)))

    def generate(self, bundle=None, archive=None):
        self.write_notices(archive)
        return notices.generate(bundle or self.bundle, self.graph, self.metadata,
                                self.root, self.home, self.sources, archive)

    def test_inventory_uses_link_edges_and_excludes_proc_macro(self):
        self.assertEqual(notices.linked_crates(self.graph, 'root//:compiler'),
                         {'example-1.0': ['root//third-party:library']})

    def test_determinism_and_relocation_preserve_objects_and_native_texts(self):
        before = notices.file_inventory(self.bundle)
        copy = self.root / 'different location'
        shutil.copytree(self.bundle, copy)
        first = self.generate()
        self.generate(copy)
        self.assertEqual((copy / 'notices/manifest.json').read_bytes(),
                         (self.bundle / 'notices/manifest.json').read_bytes())
        self.assertEqual(notices.file_inventory(copy), before)
        self.assertEqual(first['files'], before)
        self.assertEqual(len(first['components'][0]['texts']), 4)
        shutil.move(copy, self.root / 'relocated')
        self.assertEqual(notices.file_inventory(self.root / 'relocated'), before)
        self.assertTrue((self.root / 'relocated/notices/crates/example-1.0/native/LICENSE').is_file())
        self.assertEqual((copy.parent / 'relocated/notices/crates/example-1.0/native/code.c').read_bytes(),
                         b'/* Copyright Author; permission notice */ int code;')
        self.assertNotIn(str(self.root), json.dumps(first))

    def test_missing_license_refuses_without_partial_output(self):
        archive(self.crate, {'src/lib.rs': b'code'})
        self.write_lock()
        with self.assertRaisesRegex(ValueError, 'missing license text'):
            self.generate()
        self.assertFalse((self.bundle / 'notices').exists())

    def test_missing_declared_license_file_refuses_even_with_other_texts(self):
        self.package['license_file'] = '/irrelevant/cache/example-1.0/LEGAL.txt'
        with self.assertRaisesRegex(ValueError, 'LEGAL.txt'):
            self.generate()

    def test_declared_license_file_with_unusual_name_is_retained(self):
        self.package['license_file'] = '/irrelevant/cache/example-1.0/LEGAL.txt'
        archive(self.crate, {'LEGAL.txt': b'actual custom terms'})
        self.write_lock()
        self.generate()
        self.assertEqual((self.bundle / 'notices/crates/example-1.0/LEGAL.txt').read_bytes(), b'actual custom terms')

    def test_tampered_archive_refuses(self):
        self.crate.write_bytes(b'corrupt')
        with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
            self.generate()

    def test_missing_archive_refuses(self):
        self.crate.unlink()
        with self.assertRaisesRegex(ValueError, 'archive unavailable'):
            self.generate()

    def test_missing_required_native_text_refuses(self):
        self.sources['required_texts'] = {'example-1.0': ['native/NOTICE']}
        with self.assertRaisesRegex(ValueError, 'native/NOTICE'):
            self.generate()

    def test_missing_rust_text_refuses(self):
        (self.root / 'rust/share/doc/rust/licenses/MIT.txt').unlink()
        with self.assertRaisesRegex(ValueError, 'missing Rust license text'):
            self.generate()
        self.assertFalse((self.bundle / 'notices').exists())

    def test_uninventoried_native_bundle_refuses(self):
        (self.bundle / 'libLLVM.so').write_bytes(b'extra code')
        with self.assertRaisesRegex(ValueError, 'uninventoried bundle entries'):
            self.generate()

    def test_cache_budget_is_refused_as_shipped_runtime_state(self):
        self.sources['bundle_entries'].append('core-packages')
        self.sources['files']['core-packages/*'] = dict(contents='core and std package images')
        budget = self.bundle / 'core-packages/image/native/.budget'
        budget.parent.mkdir(parents=True)
        budget.write_bytes(b'\0' * 8)
        with self.assertRaisesRegex(ValueError, 'runtime state'):
            notices.check_bundle(self.bundle, self.sources, {'native': []})

    def test_unclassified_file_in_a_known_entry_refuses(self):
        (self.bundle / 'hirsel-std.products/nested').mkdir()
        (self.bundle / 'hirsel-std.products/nested/libz.so.1').write_bytes(b'extra code')
        self.sources['files'] = {'hirsel': self.sources['files']['hirsel'],
                                 'hirsel-std.products/query-product': dict(contents='products')}
        with self.assertRaisesRegex(ValueError, 'libz.so.1: no notice covers'):
            self.generate()

    def test_native_file_ships_its_pinned_source_texts_and_refuses_other_bytes(self):
        source = self.root / 'llvm.tar.xz'
        with tarfile.open(source, 'w:xz') as output:
            for name, data in {'llvm/LICENSE.TXT': b'Apache-2.0 WITH LLVM-exception terms',
                               'llvm/lib/Support/regcomp.c': b'/* Copyright (c) 1992 Henry Spencer */',
                               'llvm/lib/Support/plain.cpp': b'// Part of the LLVM Project',
                               'llvm/lib/Support/test/x.c': b'/* Copyright test only */'}.items():
                member = tarfile.TarInfo('llvm-project/' + name)
                member.size = len(data)
                output.addfile(member, io.BytesIO(data))
        (self.root / 'llvm.lock').write_text(f'source_url https://example/llvm.tar.xz\nsource_sha256 {notices.sha256(source)}\n'
                                             f'static_lld {notices.hashlib.sha256(b"pinned linker").hexdigest()} 13 ld.lld\n')
        self.sources['bundle_entries'].append('bin')
        self.sources['files']['bin/ld.lld'] = dict(native='static_lld', contents='the linker')
        self.sources['native'] = dict(static_lld=dict(licenses=['llvm/LICENSE.TXT'], source_dirs=['llvm/lib'],
                                                      static=[], static_note=''))
        (self.bundle / 'bin').mkdir()
        (self.bundle / 'bin/ld.lld').write_bytes(b'pinned linker')
        copy = self.root / 'copy'
        shutil.copytree(self.bundle, copy)
        self.generate(archive=source)
        retained = self.bundle / 'notices/native/llvm-project'
        self.assertEqual(sorted(p.relative_to(retained).as_posix() for p in retained.rglob('*') if p.is_file()),
                         ['llvm/LICENSE.TXT', 'llvm/lib/Support/regcomp.c'])
        (copy / 'bin/ld.lld').write_bytes(b'another linker')
        with self.assertRaisesRegex(ValueError, 'not the llvm-23.lock static_lld pin'):
            self.generate(copy, archive=source)
        self.assertFalse((copy / 'notices').exists())

    def test_stale_notices_refuse(self):
        self.write_notices()
        self.package['license'] = 'MIT'
        with self.assertRaisesRegex(ValueError, 'NOTICES.md is stale'):
            notices.generate(self.bundle, self.graph, self.metadata, self.root, self.home, self.sources)

    def fallback_fixture(self):
        data = b'actual upstream terms at this revision'
        self.revision = 'a' * 40
        vcs = dict(git=dict(sha1=self.revision), path_in_vcs='crates/example')
        archive(self.crate, {'.cargo_vcs_info.json': json.dumps(vcs).encode()})
        self.write_lock()
        fallback = dict(source='https://example.test/upstream', path='LICENSE',
                        sha256=notices.hashlib.sha256(data).hexdigest(), license=self.package['license'],
                        packages={'example': 'crates/example'})
        self.sources['archive_omissions'] = {'upstream': fallback}
        self.fetch_license = self.enterContext(patch.object(notices.urllib.request, 'urlopen',
                                                            return_value=io.BytesIO(data)))
        self.addCleanup(notices.upstream_license.cache_clear)
        return fallback

    def test_verified_upstream_fallback_records_exact_revision(self):
        self.fallback_fixture()
        record = self.generate()['components'][0]
        url = f'https://example.test/upstream/{self.revision}/LICENSE'
        self.fetch_license.assert_called_once_with(url, timeout=30)
        self.assertEqual(record['archive_omission']['upstream'], url)
        self.assertEqual(record['archive_omission']['revision'], self.revision)
        self.assertEqual((self.bundle / 'notices/crates/example-1.0/LICENSE').read_bytes(),
                         b'actual upstream terms at this revision')

    def test_fallback_refuses_invalid_revision(self):
        self.fallback_fixture()
        archive(self.crate, {'.cargo_vcs_info.json': b'{"git":{"sha1":"main"}}'})
        self.write_lock()
        with self.assertRaisesRegex(ValueError, 'invalid fallback revision'):
            self.generate()
        self.fetch_license.assert_not_called()

    def test_fallback_refuses_another_source_path(self):
        self.fallback_fixture()['packages']['example'] = 'elsewhere'
        with self.assertRaisesRegex(ValueError, 'source path or declared license mismatch'):
            self.generate()
        self.fetch_license.assert_not_called()

    def test_fallback_refuses_another_text(self):
        self.fallback_fixture()['sha256'] = 'wrong digest'
        with self.assertRaisesRegex(ValueError, 'license checksum mismatch'):
            self.generate()

    def test_missing_expression_is_reported_without_electing_a_license(self):
        self.package['license'] = None
        record = self.generate()['components'][0]
        self.assertIn('gap', record)
        self.assertIn('no alternative license elected', record['selection'])


def distribution_build():
    sys.modules.setdefault('notices', notices)
    loader = importlib.machinery.SourceFileLoader('dist_build', str(ROOT / 'tools/toolchain-dist/build'))
    build = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
    loader.exec_module(build)
    return build


class DistributionBuild(unittest.TestCase):
    def test_darwin_prebuilt_runtime_roundtrip(self):
        import darwin_host

        target = 'aarch64-apple-darwin'
        flavours = {
            'runtime': ('none', False, False),
            'recording-runtime': ('none', True, False),
            'prof-runtime': ('prof', False, False),
            'prof-recording-runtime': ('prof', True, False),
            'observed-runtime': ('observed', False, False),
            'observed-recording-runtime': ('observed', True, False),
            'embed-runtime': ('none', False, True),
        }
        build = distribution_build()
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            linux, mac, transfer, work = (root / name for name in ('linux', 'mac', 'transfer', 'work'))

            def install(prefix, runtime_bytes):
                rows = []
                for index, (slot, (observation, record, embedded)) in enumerate(flavours.items()):
                    relative = Path('runtime') / target / f'{index:064x}' / 'libhirsel_core_rt.a'
                    archive = prefix / relative
                    archive.parent.mkdir(parents=True, exist_ok=True)
                    archive.write_bytes(runtime_bytes[slot])
                    rows.append(dict(target=target, observation=observation, record=record,
                                     embedded=embedded, path=relative.as_posix()))
                (prefix / 'hirsel-core-toolchain.json').write_text(json.dumps(dict(runtimes=rows)))
                return [prefix / row['path'] for row in rows]

            def product(*args):
                return subprocess.run([sys.executable, DIST / 'product.py', *args],
                                      capture_output=True, text=True)

            original = {slot: slot.encode() for slot in flavours}
            install(linux, original)
            exported = product('export-runtimes', target, linux, transfer)
            self.assertEqual(exported.returncode, 0, exported.stderr)

            installed = []

            def run(command, **options):
                if command[:2] == ['cargo', 'rustc']:
                    compiler = work / 'target/dist/hirsel'
                    compiler.parent.mkdir(parents=True, exist_ok=True)
                    compiler.touch()
                elif command[1] == 'std-product':
                    archives = {slot: Path(command[command.index(f'--{slot}') + 1]).read_bytes()
                                for slot in flavours}
                    installed.extend(install(mac, archives))

            args = [str(DIST / 'build'), '--host', target, '--output', str(mac),
                    '--work', str(work), '--runtime-archives', str(transfer)]
            with (patch.object(sys, 'argv', args), patch.dict(os.environ, RELEASE_TAG='v0.0.0-test'),
                  patch.object(darwin_host, 'require_host'), patch.object(darwin_host, 'run', side_effect=run),
                  patch.object(darwin_host, 'link_kit_tree', return_value=work / 'kit'),
                  patch.object(darwin_host, 'check'), patch.object(build, 'manifest')):
                build.main()

            compared = product('compare-runtimes', target, mac, transfer)
            self.assertEqual(compared.returncode, 0, compared.stderr)
            for slot, archive in zip(flavours, installed, strict=True):
                with self.subTest(slot=slot):
                    self.assertEqual(archive.read_bytes(), original[slot])
                    archive.write_bytes(b'wrong bytes')
                    compared = product('compare-runtimes', target, mac, transfer)
                    self.assertEqual(compared.returncode, 1, compared.stderr)
                    self.assertIn(f'{slot}: runtime archive bytes differ', compared.stderr)
                    archive.write_bytes(original[slot])

    def test_fresh_inventory_requires_embed_runtime_alongside_plain_runtime(self):
        build = distribution_build()
        target = 'aarch64-apple-darwin'
        flavours = [
            ('runtime', 'none', False, False),
            ('recording runtime', 'none', True, False),
            ('profiling runtime', 'prof', False, False),
            ('profiling recording runtime', 'prof', True, False),
            ('charge observation runtime', 'observed', False, False),
            ('charge observation recording runtime', 'observed', True, False),
            ('embed runtime', 'none', False, True),
        ]
        with tempfile.TemporaryDirectory() as raw, contextlib.redirect_stdout(io.StringIO()):
            prefix = Path(raw)
            lines = ['hirsel 0.0.0-test', 'compiler ' + 'a' * 64,
                     'std@1/sim/fs/serve\tfn', 'pass executed app@1/tests/test_twice']
            rows = []
            for i, (label, observation, record, embedded) in enumerate(flavours):
                content = f'{i:064x}'
                archive = Path('runtime') / target / content / 'libhirsel_core_rt.a'
                (prefix / archive).parent.mkdir(parents=True)
                (prefix / archive).touch()
                rows.append(dict(target=target, observation=observation, record=record,
                                 embedded=embedded, path=archive.as_posix()))
                lines.append(f'{label} {target} {content}')
            registration = prefix / 'hirsel-core-toolchain.json'
            relative = Path('runtime') / target / ('a' * 64) / 'hirsel-exec-helper'
            (prefix / relative).parent.mkdir()
            (prefix / relative).touch(mode=0o755)
            helpers = {target: relative.as_posix()}
            registration.write_text(json.dumps(dict(runtimes=rows, exec_helpers=helpers)))
            passed = subprocess.CompletedProcess([], 0, '\n'.join(lines), '')
            build.accept_fresh(passed, prefix, {target})
            registration.write_text(json.dumps(dict(runtimes=rows[:-1], exec_helpers=helpers)))
            with self.assertRaisesRegex(ValueError, 'runtime target/flavour mismatch'):
                build.accept_fresh(passed, prefix, {target})

    def test_requires_a_release_tag_before_preparing_buck(self):
        env = os.environ.copy()
        env.pop('RELEASE_TAG', None)
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            prepare = root / 'python3'
            prepare.write_text('#!/bin/sh\ntouch "$DIST_PREPARE_MARKER"\nexit 1\n')
            prepare.chmod(0o755)
            marker = root / 'prepared'
            env.update(PATH=f'{root}{os.pathsep}{os.environ.get("PATH", os.defpath)}',
                       DIST_PREPARE_MARKER=str(marker))
            run = subprocess.run(
                [sys.executable, ROOT / 'tools/toolchain-dist/build', '--output', root / 'dist'],
                env=env, capture_output=True, text=True,
            )
            self.assertNotEqual(run.returncode, 0)
            self.assertFalse(marker.exists(), run.stderr)
            self.assertIn('RELEASE_TAG', run.stderr)


class GlibcBaseline(unittest.TestCase):
    """The release gate refuses what the loader would import, not what .symtab records.

    tools/tests/fixtures/glibc-gate holds three links of weak.c and strong.c: weak-symtab and
    weak-dynsym were linked against glibc 2.28 (manylinux_2_28, bfd), weak-symtab with
    -z nodynamic-undefined-weak and the --emit-relocs BOLT needs, weak-dynsym with neither;
    strong-glibc239 was linked against glibc 2.39.
    """
    FIXTURES = ROOT / 'tools/tests/fixtures/glibc-gate'

    @classmethod
    def setUpClass(cls):
        cls.build = distribution_build()

    def check(self, fixture):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory)
            for relative in ('hirsel', 'lib/libLLVM.so.23.1', 'bin/ld.lld'):
                (prefix / relative).parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(self.FIXTURES / fixture, prefix / relative)
            with contextlib.redirect_stdout(io.StringIO()):
                self.build.check_glibc(prefix)

    def test_weak_reference_only_in_symtab_passes(self):
        # Red before 2760: the gate read .symtab and refused the v0.2.0-pre BOLT binary.
        self.check('weak-symtab')

    def test_unversioned_weak_import_still_refuses(self):
        with self.assertRaisesRegex(SystemExit, '^hirsel has an undefined pidfd_spawnp reference$'):
            self.check('weak-dynsym')

    def test_versioned_strong_import_still_refuses(self):
        with self.assertRaisesRegex(SystemExit, r'hirsel requires GLIBC_2\.39, newer than glibc 2\.28(.|\n)*'
                                    r'hirsel has an undefined pidfd_spawnp reference'):
            self.check('strong-glibc239')


class Citations(unittest.TestCase):
    def test_braces_and_line_ranges_name_each_file(self):
        text = ('| Source | Adapted | Where in this tree | Upstream revision | License |\n|---|---|---|---|---|\n'
                '| up | `src/x.rs:1-9` | `a/{b,c}.rs:3-4,8`, driver `d/e.hir` | `deadbeef` | MIT, `third-party/LICENSE.up`; `v1.0` |\n'
                '| bare | code | the runtime | r | MIT |\n')
        self.assertEqual(notices.citations(text), [
            ('up', ['a/b.rs', 'a/c.rs', 'd/e.hir', 'third-party/LICENSE.up']), ('bare', [])])


class Repository(unittest.TestCase):
    """The checked-in NOTICES.md against this tree's lock files and the built installation."""

    def test_notices_cover_every_locked_crate_and_shipped_file(self):
        sources, _ = notices.notice_sources()
        written, text = notices.recorded(ROOT)
        derived = notices.inventory(ROOT, sources, written)
        crates = lambda inventory: {(row['crate'], row['checksum']) for row in inventory['crates']}
        self.assertEqual(crates(derived) - crates(written), set(), 'locked crates NOTICES.md does not cover')
        self.assertEqual(derived, written, 'NOTICES.md is stale: run tools/toolchain-dist/notices.py --write')
        self.assertEqual(notices.render(written), text)
        tree = os.environ.get('HIRSEL_DISTRIBUTION_LLVM')
        notices.check_bundle(Path(os.environ['HIRSEL_TOOLCHAIN_BUNDLE']), sources, written,
                             bootstrap_tree=Path(tree) if tree else None)

    def test_adapted_sources_cite_files_in_this_tree(self):
        for source, paths in notices.citations((ROOT / notices.ADAPTED).read_text()):
            present = PRESENT if PRESENT is not None else {path for path in paths if (ROOT / path).is_file()}
            self.assertTrue(paths, f'{notices.ADAPTED}: {source} cites no file in this tree')
            self.assertEqual(sorted(set(paths) - present), [], f'{notices.ADAPTED}: {source} cites files that do not exist')


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser()
    parser.add_argument('--test-threads', type=int, choices=[1], default=1)
    parser.add_argument('--present', nargs='*')
    known, rest = parser.parse_known_args()
    if known.present is not None:
        PRESENT = set(known.present)
    unittest.main(argv=sys.argv[:1] + rest)
