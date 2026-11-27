#!/usr/bin/env python3
"""Collect release notices from the configured link graph, pinned crate archives and native pins.

    notices.py --write [--bundle DIR]   rewrite NOTICES.md
    notices.py --check-crates           check Linux and Darwin crate texts

No SPDX-to-text substitution: retain the actual upstream texts, including nested
C/assembly notices. Paths in the result are relative and timestamps are absent.
"""
import fnmatch
from functools import lru_cache
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
SOURCES = Path(__file__).with_name('notice-sources.json')
LICENSE_NAME = re.compile(r'^(licen[cs]e|copying|copyright|notice)([._-].*)?$', re.I)
NOTICES = 'NOTICES.md'
NATIVE_SKIP = {'test', 'tests', 'unittests', 'docs', 'utils'}
# Written only by a PGO-use build.
OPTIONAL_ENTRIES = {'hirsel.profdata'}


def notice_sources(target='x86_64-unknown-linux-gnu'):
    sources = json.loads(SOURCES.read_text())
    product = tomllib.loads(SOURCES.with_name('product.toml').read_text())
    llvm = product['hosts'][target]['llvm_library']
    files = sources['files']
    files[llvm] = files.pop('@llvm_library@')
    return sources, llvm


def sha256(path):
    with path.open('rb') as handle:
        return hashlib.file_digest(handle, 'sha256').hexdigest()


def linked_crates(graph, root):
    """Follow link edges, not build scripts, proc macros or implicit toolchains.

    Archive source edges identify packages without guessing Reindeer's aliases.
    The inventory is of link inputs, before linker dead-code elimination.
    """
    seen, crates = set(), {}

    def visit(label):
        if label in seen:
            return
        seen.add(label)
        attrs = graph[label]
        if attrs.get('proc_macro'):
            return
        for dependency in attrs.get('buck.deps', []):
            match = re.match(r'root//third-party:([^ :]+)\.crate \(', dependency)
            if match:
                crates.setdefault(match[1], set()).add(label.split(' (')[0])
        edges = attrs.get('deps', []) + attrs.get('exported_deps', [])
        edges += list(attrs.get('named_deps', {}).values())
        edges += [edge[0] for edge in attrs.get('flagged_deps', [])]
        if not any(key in attrs for key in ('deps', 'named_deps', 'proc_macro')):
            # Alias rules have only implicit buck.deps; stop at toolchain cells.
            edges = attrs.get('buck.deps', [])
        for dependency in edges:
            if dependency.startswith('root//'):
                visit(dependency)

    matches = [label for label in graph if label.startswith(root + ' (')]
    if len(matches) != 1:
        raise ValueError(f'expected one configured root {root}, found {len(matches)}')
    visit(matches[0])
    if not crates:
        raise ValueError('empty linked crate inventory')
    return {name: sorted(targets) for name, targets in sorted(crates.items())}


def cargo_linked_crates(metadata, root):
    """Walk the normal dependency edges in Cargo's platform-filtered graph."""
    packages = {package['id']: package for package in metadata['packages']}
    nodes = {node['id']: node for node in metadata['resolve']['nodes']}
    (root_id,) = (package_id for package_id in metadata['workspace_members']
                  if packages[package_id]['name'] == root)
    seen, pending, crates = set(), [root_id], {}
    while pending:
        package_id = pending.pop()
        if package_id in seen:
            continue
        seen.add(package_id)
        package = packages[package_id]
        if any('proc-macro' in target['kind'] for target in package['targets']):
            continue
        if package['source']:
            crates[package['name'] + '-' + package['version']] = [root]
        pending.extend(dep['pkg'] for dep in nodes[package_id]['deps']
                       if any(kind['kind'] is None for kind in dep['dep_kinds']))
    if not crates:
        raise ValueError(f'empty linked crate inventory for {root}')
    return crates


def crate_texts(archive, checksum, package, required=(), allow_empty=False):
    if sha256(archive) != checksum:
        raise ValueError(f'{package}: crate archive checksum mismatch')
    texts = {}
    primary = False
    prefix = package['name'] + '-' + package['version'] + '/'
    declared = package.get('license_file')
    if declared:
        if Path(declared).is_absolute():
            declared = os.path.relpath(declared, Path(package['manifest_path']).parent)
    with tarfile.open(archive) as source:
        for member in source:
            if not member.name.startswith(prefix) or not member.isfile():
                continue
            relative = member.name[len(prefix):]
            path = PurePosixPath(relative)
            if path.is_absolute() or '..' in path.parts:
                raise ValueError('unsafe crate path: ' + relative)
            data = source.extractfile(member).read()
            named = LICENSE_NAME.fullmatch(path.name) or relative == declared
            if named:
                if not data.strip():
                    raise ValueError('empty license: ' + relative)
                primary = True
            # Preserve full upstream files with embedded notices: extracting a
            # guessed comment prefix can truncate a copyright or permission grant.
            embedded = b'\0' not in data and re.search(rb'copyright|SPDX-License-Identifier', data, re.I)
            if named or embedded:
                texts[relative] = data
    missing = set(required) - texts.keys()
    if declared and declared not in texts:
        missing.add(declared)
    if missing or (not primary and not allow_empty):
        raise ValueError(f"{prefix}missing license text: {sorted(missing) or 'no upstream license files'}")
    return texts, primary


def pinned_archive(cargo_home, name, checksum):
    archives = sorted((cargo_home / 'registry/cache').glob('*/' + name + '.crate'))
    for archive in archives:
        if sha256(archive) == checksum:
            return archive
    if archives:
        raise ValueError(f'{name}: crate archive checksum mismatch')
    raise ValueError(f'{name}: pinned crate archive unavailable in Cargo cache')


@lru_cache
def upstream_license(url, checksum):
    with urllib.request.urlopen(url, timeout=30) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError(f'{url}: fallback license checksum mismatch')
    return data


def omitted_text(archive, name, package, fallback):
    """The archive checksum binds the crate to the immutable upstream revision."""
    with tarfile.open(archive) as source:
        vcs = json.load(source.extractfile(name + '/.cargo_vcs_info.json'))
    git = vcs['git']
    revision = git['sha1']
    path = fallback['path']
    if git.get('dirty') or not re.fullmatch(r'[0-9a-f]{40}', revision):
        raise ValueError(f'{name}: invalid fallback revision')
    if vcs['path_in_vcs'] != fallback['packages'][package['name']] or package['license'] != fallback['license']:
        raise ValueError(f'{name}: fallback source path or declared license mismatch')
    url = f"{fallback['source']}/{revision}/{path}"
    data = upstream_license(url, fallback['sha256'])
    return data, dict(upstream=url, revision=revision, path=path, sha256=fallback['sha256'])


def crate_notices(linked, metadata, sources, root=ROOT, cargo_home=None):
    cargo_home = cargo_home or Path(os.environ.get('CARGO_HOME', Path.home() / '.cargo'))
    packages = {p['name'] + '-' + p['version']: p for p in metadata['packages'] if p['source']}
    lock = tomllib.loads((root / 'Cargo.lock').read_text())
    locked = {p['name'] + '-' + p['version']: p for p in lock['package'] if 'source' in p}
    fallbacks = {name: fallback for fallback in sources.get('archive_omissions', {}).values()
                 for name in fallback['packages']}
    shipped = {}
    for crates in linked.values():
        for name, targets in crates.items():
            shipped.setdefault(name, set()).update(targets)
    for name, targets in sorted(shipped.items()):
        package, pin = packages[name], locked[name]
        if package['source'] != pin['source'] or not pin['source'].startswith('registry+'):
            raise ValueError(f'{name}: unsupported or mismatched package source')
        archive = pinned_archive(cargo_home, name, pin['checksum'])
        fallback = fallbacks.get(package['name'])
        texts, primary = crate_texts(archive, pin['checksum'], package,
                                     sources['required_texts'].get(name, []), allow_empty=bool(fallback))
        record = dict(name=package['name'], version=package['version'],
                      source=pin['source'], archive_sha256=pin['checksum'],
                      url=f"https://static.crates.io/crates/{package['name']}/{package['version']}/download",
                      declared_license=package['license'], targets=sorted(targets),
                      selection='All upstream license/notice files and full text files with embedded copyright/SPDX notices retained; no alternative license elected.')
        if not primary:
            data, provenance = omitted_text(archive, name, package, fallback)
            texts[provenance['path']] = data
            record['archive_omission'] = provenance
        if not package['license']:
            record['gap'] = 'No Cargo license expression; inspect upstream license files.'
        yield name, record, texts


def file_inventory(bundle):
    return {p.relative_to(bundle).as_posix(): sha256(p)
            for p in sorted(bundle.rglob('*'))
            if p.is_file() and 'notices' not in p.relative_to(bundle).parts
            and p.name != 'hirsel-build.json'}


def lock_records(path):
    """llvm-23.lock: one record per line, `key field...`; `#` starts a comment."""
    records = {}
    for line in path.read_text().splitlines():
        fields = line.split('#', 1)[0].split()
        if fields:
            records.setdefault(fields[0], []).append(fields[1:])
    return records


def lock_closure(lock, packages):
    """Every Cargo.lock registry package the named workspace packages reach,
    by any dependency kind and for any platform: the lock does not say which."""
    named = {}
    for package in lock['package']:
        named.setdefault(package['name'], []).append(package)

    def find(dependency):
        name, *rest = dependency.split(' ')
        candidates = [p for p in named.get(name, []) if not rest or p['version'] == rest[0]]
        if len(candidates) != 1:
            raise ValueError(f'Cargo.lock: cannot resolve dependency {dependency!r}')
        return candidates[0]

    seen, pending = {}, [find(name) for name in packages]
    while pending:
        package = pending.pop()
        key = package['name'] + '-' + package['version']
        if key not in seen:
            seen[key] = package
            pending += [find(d) for d in package.get('dependencies', [])]
    return {key: package for key, package in sorted(seen.items()) if 'source' in package}


def native_texts(archive, sources):
    """The rule in notice-sources.json `native_text_rule`, over the locked source archive."""
    components = sources['native'].values()
    directories = {d for component in components for d in component['source_dirs']}
    named = {path for component in components for path in component['licenses']}
    texts = {}
    with tarfile.open(archive) as source:
        for member in source:
            parts = PurePosixPath(member.name).parts[1:]
            if not member.isfile() or not parts or '..' in parts:
                continue
            relative = '/'.join(parts)
            if relative in named:
                texts[relative] = source.extractfile(member).read()
            if not any(relative.startswith(d + '/') for d in directories) or NATIVE_SKIP & set(parts[:-1]):
                continue
            data = source.extractfile(member).read()
            if LICENSE_NAME.fullmatch(parts[-1]) or (b'\0' not in data and re.search(rb'copyright', data, re.I)):
                texts[relative] = data
    if named - texts.keys():
        raise ValueError(f'source archive lacks license texts: {sorted(named - texts.keys())}')
    return texts


def llvm_archive(pins):
    """The locked LLVM source archive: HIRSEL_LLVM_SOURCE_ARCHIVE, or the cache, or a download into it."""
    url, digest = pins['source_url'][0][0], pins['source_sha256'][0][0]
    given = os.environ.get('HIRSEL_LLVM_SOURCE_ARCHIVE')
    cache = Path(os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache') / 'hirsel/llvm-source'
    path = Path(given) if given else cache / (digest + '.tar.xz')
    if not path.exists():
        cache.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=cache, delete=False) as partial, urllib.request.urlopen(url) as response:
            shutil.copyfileobj(response, partial)
        if sha256(Path(partial.name)) != digest:
            os.unlink(partial.name)
            raise ValueError(f'{url} does not have the locked SHA-256')
        os.replace(partial.name, path)
    if sha256(path) != digest:
        raise ValueError(f'{path} does not have the locked source_sha256')
    return path


def inventory(root, sources, recorded=None, linked=None, metadata=None, texts=None):
    """NOTICES.md's inventory, from Cargo.lock, llvm-23.lock and notice-sources.json.

    `linked` (pattern -> linked crates, from the link graph), `metadata` (Cargo's
    license expressions) and `texts` (llvm-project path -> bytes) need Buck,
    Cargo and the source archive; without one, its facts are taken from `recorded`.
    """
    recorded = recorded or {}
    files = sources['files']
    lock = tomllib.loads((root / 'Cargo.lock').read_text())
    crate_files = {pattern: entry for pattern, entry in files.items() if 'crates' in entry}
    closure = lock_closure(lock, [entry['package'] for entry in crate_files.values()])
    if linked is not None:
        stray = set().union(*map(set, linked.values())) - closure.keys()
        if stray:
            raise ValueError(f'linked crates outside the Cargo.lock closure: {sorted(stray)}')
    known = {row['crate']: row for row in recorded.get('crates', [])}
    licenses = {p['name'] + '-' + p['version']: p['license'] for p in metadata['packages'] if p['source']} if metadata else None
    crates = []
    for name, package in closure.items():
        old = known.get(name, {})
        crates.append(dict(
            crate=name, checksum=package['checksum'],
            license=licenses.get(name) if licenses is not None else old.get('license'),
            linked=sorted(p for p in crate_files if name in linked[p]) if linked is not None else old.get('linked')))
    pins = lock_records(root / sources['llvm_lock'])
    source = dict(url=pins['source_url'][0][0], sha256=pins['source_sha256'][0][0])
    old_native = {row['path']: row for row in recorded.get('native', [])}
    native = []
    for pattern, entry in sorted(files.items()):
        if 'native' not in entry:
            continue
        component = sources['native'][entry['native']]
        (digest, size, asset), = pins[entry['native']]
        if texts is None:
            old = old_native.get(pattern, {})
            kept = old.get('texts') if old.get('source') == source else None
        else:
            kept = [dict(path=path, sha256=hashlib.sha256(data).hexdigest()) for path, data in sorted(texts.items())
                    if path in component['licenses'] or any(path.startswith(d + '/') for d in component['source_dirs'])]
        native.append(dict(path=pattern, lock=entry['native'], sha256=digest, size=int(size), asset=asset,
                           source=source, static=component['static'], note=component['static_note'], texts=kept))
    runtimes = {name: dict({k: v for k, v in runtime.items() if k != 'texts'},
                           texts=[dict(path=path, upstream=upstream, sha256=sha256(root / path))
                                  for path, upstream in sorted(runtime['texts'].items())])
                for name, runtime in sorted(sources['host_runtimes'].items())}
    return dict(schema=1, files=[dict(path=pattern, **entry) for pattern, entry in files.items()],
                crates=crates, native=native, host_runtimes=runtimes, external=sources['external'])


def render(recorded, darwin_scope=None):
    """NOTICES.md: tables for readers, then the inventory they are rendered from."""
    intro = darwin_scope or (
        'Generated by `tools/toolchain-dist/notices.py --write`; edit `tools/toolchain-dist/notice-sources.json`, '
        'not this file. `//:gate-distribution-notices` fails when this file is not what Cargo.lock, '
        '`tools/native-toolchain/llvm-23.lock` and the installation derive, and release packaging '
        '(`tools/toolchain-dist/build`) refuses an installation it does not cover. Packaging retains every '
        'text named here under the installation\'s `notices/`.')
    out = ['# Distribution notices', '', intro, '',
           'Code Hirsel adapts into its own sources is listed in [third-party/NOTICES.md](third-party/NOTICES.md). '
           'Hirsel itself is under [LICENSE](LICENSE).', '',
           '## Shipped files', '', '| Path | Contents | Notices |', '|---|---|---|']
    for entry in recorded['files']:
        if 'native' in entry:
            notices = f'native component `{entry["native"]}`, below'
        elif 'crates' in entry:
            notices = (f'Hirsel; the Rust standard library; crate notices for `{entry["path"]}`, below'
                       if darwin_scope else
                       f'Hirsel; the Rust standard library; crates linked into `{entry["path"]}`, below')
        elif 'runtimes' in entry:
            notices = ', '.join(f'`{name}`' for name in entry['runtimes'])
        else:
            notices = 'Hirsel'
        out.append(f'| `{entry["path"]}` | {entry["contents"]} | {notices} |')
    out += ['', '## Rust crates', '']
    if darwin_scope:
        out += ['Registry crates that supply the compiler, runtime and Exec helper link inputs in Cargo\'s Darwin dependency graph.',
                '', '| Crate | License | Linked into | Cargo.lock checksum |',
                '|---|---|---|---|']
    else:
        out += [
            'Every registry package in Cargo.lock that the shipped files\' workspace packages reach, by any '
            'dependency kind and for any platform. "Linked into" names the shipped files whose link inputs '
            'include the crate; one linked into none is a build script, proc macro, test or other-platform '
            'dependency and ships no code. The Rust standard library\'s own notices are retained from the pinned '
            'toolchain.', '', '| Crate | License | Linked into | Cargo.lock checksum |', '|---|---|---|---|']
    for row in recorded['crates']:
        into = ', '.join(f'`{p}`' for p in row['linked'] or []) or 'none'
        out.append(f'| {row["crate"]} | {row["license"] or "none declared"} | {into} | `{row["checksum"]}` |')
    out += ['', '## Native components', '']
    for row in recorded['native']:
        out += [f'### `{row["path"]}`', '',
                f'llvm-23.lock `{row["lock"]}`: `{row["asset"]}`, {row["size"]} bytes, SHA-256 `{row["sha256"]}`. '
                f'Built from {row["source"]["url"]} (SHA-256 `{row["source"]["sha256"]}`), whose notices follow. '
                f'Runtimes linked statically: {", ".join(row["static"])}. {row["note"]}', '',
                '| Source file | SHA-256 |', '|---|---|']
        out += [f'| `{text["path"]}` | `{text["sha256"]}` |' for text in row['texts'] or []]
        out.append('')
    out += ['## C and C++ runtime notices', '']
    for name, runtime in recorded['host_runtimes'].items():
        out += [f'### {name}', '', f'{runtime["distribution"]} {runtime["package"]} {runtime["version"]}. {runtime["source"]}', '',
                '| Text | From | SHA-256 |', '|---|---|---|']
        out += [f'| [`{t["path"]}`]({t["path"]}) | `{t["upstream"]}` | `{t["sha256"]}` |' for t in runtime['texts']]
        out.append('')
    out += ['## Not shipped', '']
    out += [f'- {item["component"]}: {item["status"]}' for item in recorded['external']]
    out += ['', '## Inventory', '', '```json', json.dumps(recorded, indent=1, sort_keys=True), '```', '']
    return '\n'.join(out)


def recorded(root):
    text = (root / NOTICES).read_text()
    match = re.search(r'\n```json\n(.*)\n```\n$', text, re.S)
    if not match:
        raise ValueError(f'{NOTICES} has no inventory')
    return json.loads(match[1]), text


ADAPTED = 'third-party/NOTICES.md'


def citations(text):
    """Each adapted-source row of third-party/NOTICES.md and the repository files it
    cites: every backticked span under "Where in this tree", and each backticked
    path under "License". `{a,b}` expands and a trailing `:lines` is dropped."""
    def expand(span):
        match = re.search(r'\{([^{}]*)\}', span)
        if not match:
            return [span]
        return [path for choice in match[1].split(',')
                for path in expand(span[:match.start()] + choice + span[match.end():])]
    rows = []
    for line in text.split('\n'):
        cells = [cell.strip() for cell in line.strip().strip('|').split('|')]
        if not line.startswith('| ') or len(cells) != 5 or cells[0] == 'Source':
            continue
        spans = re.findall(r'`([^`]+)`', cells[2]) + [span for span in re.findall(r'`([^`]+)`', cells[4]) if '/' in span]
        rows.append((cells[0], [re.sub(r':[0-9][0-9,-]*$', '', path) for span in spans for path in expand(span)]))
    return rows


def check_bundle(bundle, sources, inventory, llvm_tree=None, *, bootstrap_tree=None):
    """Every shipped file is in exactly one `files` class, and each native file is its pin."""
    allowed = set(sources['bundle_entries']) | OPTIONAL_ENTRIES | {'notices', 'hirsel-build.json'}
    present = {p.name for p in bundle.iterdir()}
    if present - allowed:
        raise ValueError(f'uninventoried bundle entries: {sorted(present - allowed)}')
    if set(sources['bundle_entries']) - present - OPTIONAL_ENTRIES:
        raise ValueError(f'missing bundle entries: {sorted(set(sources["bundle_entries"]) - present - OPTIONAL_ENTRIES)}')
    files = file_inventory(bundle)
    for path in files:
        if PurePosixPath(path).name == '.budget':
            raise ValueError(f'{path}: cache-size estimate is runtime state, not shipped content')
        classes = [p for p in sources['files'] if fnmatch.fnmatchcase(path, p)]
        if len(classes) != 1:
            raise ValueError(f'{path}: {"no notice covers this shipped file" if not classes else "classified twice: " + str(classes)}')
    for row in inventory['native']:
        # Compare members of a pinned tool archive with the verified unpacked tree.
        tree = llvm_tree
        if row['lock'] == 'bootstrap_llvm':
            tree = bootstrap_tree or ROOT / 'tools/buck2/toolchains/llvm-files'
        pinned = tree / row['path'] if tree is not None else None
        digest = sha256(pinned) if pinned else row['sha256']
        size = pinned.stat().st_size if pinned else row['size']
        if files.get(row['path']) != digest or (bundle / row['path']).stat().st_size != size:
            raise ValueError(f'{row["path"]} is not the llvm-23.lock {row["lock"]} pin its notices cover')
    for pattern, entry in sources['files'].items():
        if 'crates' in entry and not any(fnmatch.fnmatchcase(path, pattern) for path in files):
            raise ValueError(f'no shipped file matches {pattern}')
    return files


def generate(bundle, graph, metadata, root=ROOT, cargo_home=None, sources=None, archive=None, *, llvm_tree=None):
    """Only add notices; refuse missing inputs without leaving a partial directory."""
    sources = sources or notice_sources()[0]
    if llvm_tree is None:
        linked = {pattern: linked_crates(graph, entry['crates'])
                  for pattern, entry in sources['files'].items() if 'crates' in entry}
        written, text = recorded(root)
        checked = inventory(root, sources, written)
    else:
        linked = {pattern: cargo_linked_crates(metadata, entry['package'])
                  for pattern, entry in sources['files'].items() if 'crates' in entry}
        checked = inventory(root, sources)
    before = check_bundle(bundle, sources, checked, llvm_tree)
    if not before or 'hirsel' not in before:
        raise ValueError('bundle has no compiler')
    native = {}
    if any('native' in entry for entry in sources['files'].values()):
        pins = lock_records(root / sources['llvm_lock'])
        native = native_texts(archive or llvm_archive(pins), sources)
    current = inventory(root, sources, linked=linked, metadata=metadata, texts=native)
    if llvm_tree is not None:
        current['crates'] = [crate for crate in current['crates'] if crate['linked']]
        text = render(current, darwin_scope=sources['scope'])
    elif current != written or render(written) != text:
        raise ValueError(f'{NOTICES} is stale: run tools/toolchain-dist/notices.py --write')
    destination = bundle / 'notices'
    if destination.exists():
        raise ValueError('notices already exists; use a fresh bundle')
    manifest = dict(schema=1, scope=sources['scope'], components=[],
                    external=sources['external'], files=before,
                    inputs={'Cargo.lock': sha256(root / 'Cargo.lock')})
    with tempfile.TemporaryDirectory(prefix='.notices-', dir=bundle.parent) as raw:
        stage = Path(raw)

        def retain(name, data):
            path = stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            return dict(path=name, sha256=hashlib.sha256(data).hexdigest())

        for name, record, texts in crate_notices(linked, metadata, sources, root, cargo_home):
            record['texts'] = [dict(upstream=path, **retain(f'crates/{name}/{path}', data))
                               for path, data in sorted(texts.items())]
            manifest['components'].append(record)

        for row in current['native']:
            manifest['components'].append(dict(
                name=row['path'], lock=row['lock'], sha256=row['sha256'], source=row['source'], static=row['static'],
                selection=sources['native_text_rule'],
                texts=[dict(upstream=t['path'], **retain('native/llvm-project/' + t['path'], native[t['path']]))
                       for t in row['texts']]))
        for name, runtime in current['host_runtimes'].items():
            manifest['components'].append(dict(
                name=name, package=runtime['package'], version=runtime['version'], source=runtime['source'],
                texts=[dict(upstream=t['upstream'], **retain(f'native/{name}/' + PurePosixPath(t['path']).name,
                                                            (root / t['path']).read_bytes()))
                       for t in runtime['texts']]))

        if llvm_tree is None:
            rust_lock = json.loads((root / sources['rust_lock']).read_text())
            rust = root / sources['rust_tree']
            if json.loads((rust / '.hirsel-toolchain.json').read_text()) != rust_lock:
                raise ValueError('Rust license inputs do not match the pinned toolchain')
        else:
            channel = tomllib.loads((root / sources['rust_lock']).read_text())['toolchain']['channel']
            rust_lock = dict(rust_version=channel, host='aarch64-apple-darwin')
            rust = Path(subprocess.check_output(['rustc', '+' + channel, '--print', 'sysroot'],
                                                cwd=root, text=True).strip())
        docs = rust / 'share/doc/rust'
        copyright_file = docs / 'COPYRIGHT-library.html'
        if not copyright_file.is_file() or not (docs / 'licenses').is_dir():
            raise ValueError('pinned Rust standard-library license inputs unavailable')
        rust_texts = [copyright_file, *sorted((docs / 'licenses').glob('*'))]
        if not (docs / 'licenses/MIT.txt').is_file() or not (docs / 'licenses/Apache-2.0.txt').is_file():
            raise ValueError('missing Rust license text')
        manifest['components'].append(dict(
            name='Rust standard library', version=rust_lock['rust_version'],
            source=rust_lock, selection='Upstream library copyright inventory and complete accompanying license directory.',
            texts=[dict(upstream=p.relative_to(rust).as_posix(),
                        **retain('rust/' + p.relative_to(docs).as_posix(), p.read_bytes())) for p in rust_texts]))
        for name in sources['adapted_source_notices']:
            retain('adapted-source/' + name, (root / name).read_bytes())
        retain('HIRSEL-LICENSE', (root / 'LICENSE').read_bytes())
        retain(NOTICES, text.encode())
        for name in [sources['rust_lock'], sources['llvm_lock'], 'tools/toolchain-dist/notice-sources.json',
                     'tools/toolchain-dist/notices.py', 'Cargo.toml',
                     *[pin for component in sources['external'] for pin in component.get('pins', [])], *sorted(str(p.relative_to(root)) for p in (root / 'crates').glob('*/Cargo.toml')),
                     *sorted(str(p.relative_to(root)) for p in (root / 'third-party/fixups').rglob('*') if p.is_file())]:
            manifest['inputs'][name] = sha256(root / name)
        manifest['notice_files'] = {p.relative_to(stage).as_posix(): sha256(p) for p in sorted(stage.rglob('*')) if p.is_file()}
        (stage / 'manifest.json').write_text(json.dumps(manifest, indent=2, sort_keys=True) + '\n')
        if file_inventory(bundle) != before:
            raise ValueError('bundle changed while collecting notices')
        shutil.copytree(stage, destination)
    return manifest


def facts(mode='off'):
    """The link graph of every `crates` root and Cargo's metadata: the inputs only Buck and Cargo know."""
    sources, _ = notice_sources()
    roots = [entry['crates'] for entry in sources['files'].values() if 'crates' in entry]
    configuration = ['--target-platforms', '//tools/buck2:dist', '-c', 'hirsel.dist_pgo=' + mode]
    graph = json.loads(subprocess.check_output([
        str(ROOT / '.buck2/bin/buck2'), 'cquery', *configuration,
        ' + '.join(f'deps({root})' for root in roots),
        '--output-attribute', r'^(buck\.deps|deps|exported_deps|named_deps|flagged_deps|proc_macro)$', '--json'], cwd=ROOT))
    # Unfiltered: the inventory names every locked crate, other platforms' included.
    metadata = json.loads(subprocess.check_output([
        os.environ.get('CARGO', 'cargo'), 'metadata', '--locked', '--format-version', '1'], cwd=ROOT))
    return graph, metadata


def collect(bundle, mode):
    return generate(bundle, *facts(mode))


def darwin_metadata():
    from darwin_host import PRODUCT, TARGET
    from product import runtime_slots

    features = sorted({feature for slot in runtime_slots(PRODUCT, TARGET).values()
                       for feature in slot['features']})
    return json.loads(subprocess.check_output([
        os.environ.get('CARGO', 'cargo'), 'metadata', '--locked', '--format-version', '1',
        '--filter-platform', TARGET, '--features', ','.join('hirsel-core-rt/' + feature for feature in features)],
        cwd=ROOT))


def collect_darwin(bundle, llvm_tree):
    from darwin_host import TARGET

    sources, llvm = notice_sources(TARGET)
    files = sources['files']
    del files['libexec/hirsel/.hirsel-debug/*.debug']
    del files['link/kit/*-linux-gnu/*'], files['link/kit/aarch64-apple-ios*/*.tbd'], files['hirsel.profdata']
    files['bin/ld.lld'] = dict(native='darwin_llvm', contents='the Mach-O linker')
    for path in ('bin/llvm-ar', 'bin/llvm-objcopy'):
        if (llvm_tree / path).is_file():
            files[path]['native'] = 'darwin_llvm'
        else:
            del files[path]
    files[llvm] = dict(native='darwin_llvm', contents="the compiler's LLVM library")
    files['libexec/hirsel/*.dSYM/*'] = dict(contents='detached debug information for the platform executables')
    sources.update(
        scope='Apple Silicon macOS installation prefix. Crate notices cover the compiler, runtime and Exec helper link inputs.',
        rust_lock='rust-toolchain.toml',
        host_runtimes={},
        external=[dict(component='macOS libSystem, CoreFoundation, Security, libiconv, libc++ and libz',
                       status='System dependencies. The bundle supplies Hirsel-generated link stubs for programs.')])
    return generate(bundle, None, darwin_metadata(), sources=sources, llvm_tree=llvm_tree)


def check_crates():
    """Run the packaging crate scan for both hosts without building a bundle."""
    sources, _ = notice_sources()
    entries = {pattern: entry for pattern, entry in sources['files'].items() if 'crates' in entry}
    graph, metadata = facts()
    linux = {pattern: linked_crates(graph, entry['crates']) for pattern, entry in entries.items()}
    mac_metadata = darwin_metadata()
    darwin = {pattern: cargo_linked_crates(mac_metadata, entry['package']) for pattern, entry in entries.items()}
    for host, links, meta in [('Linux', linux, metadata), ('Darwin', darwin, mac_metadata)]:
        counts = [bool(record.get('archive_omission')) for _, record, _ in crate_notices(links, meta, sources)]
        print(f'{host}: verified {len(counts)} crate archives, {sum(counts)} upstream license fallbacks')


def write(bundle):
    """Rewrite NOTICES.md from the lock files, Buck, Cargo, the source archive and an installation."""
    sources, _ = notice_sources()
    graph, metadata = facts()
    linked = {pattern: linked_crates(graph, entry['crates']) for pattern, entry in sources['files'].items() if 'crates' in entry}
    pins = lock_records(ROOT / sources['llvm_lock'])
    current = inventory(ROOT, sources, linked=linked, metadata=metadata,
                        texts=native_texts(llvm_archive(pins), sources))
    check_bundle(bundle, sources, current)
    (ROOT / NOTICES).write_text(render(current))


if __name__ == '__main__':
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    action = parser.add_mutually_exclusive_group(required=True)
    action.add_argument('--write', action='store_true')
    action.add_argument('--check-crates', action='store_true')
    parser.add_argument('--bundle', type=Path, help='an installation (default: build //:hirsel-toolchain)')
    args = parser.parse_args()
    if args.check_crates:
        if args.bundle is not None:
            parser.error('--bundle requires --write')
        check_crates()
    else:
        if args.bundle is None:
            built = subprocess.check_output([str(ROOT / '.buck2/bin/buck2'), 'build', '//:hirsel-toolchain',
                                             '--show-full-simple-output'], cwd=ROOT, text=True)
            args.bundle = Path(built.strip())
        write(args.bundle)
