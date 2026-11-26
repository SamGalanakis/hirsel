"""Project Cargo's resolved compilation units into the existing Buck rule templates.

Cargo owns feature propagation, including resolver-v3 host and dev contexts.
Cargo documents unit graph v1 at doc.rust-lang.org/cargo/reference/unstable.html#unit-graph.
Reindeer still owns third-party sources, platform fixups and native libraries.
Only features, Rust dependency edges and their identities are replaced here.
"""
from __future__ import annotations

import ast
from concurrent.futures import ThreadPoolExecutor
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import tempfile
import tomllib

import rust_environment


FIRST_PARTY = {'hirsel_library', 'hirsel_binary', 'hirsel_test', 'hirsel_build_script'}
EXTERNAL = {'third_party_rust_library', 'third_party_rust_binary', 'third_party_buildscript_run'}
# Reindeer platforms (third-party/reindeer.toml) that only the runtime archives
# build for: Linux cross-builds the Darwin runtime (tools/buck2/defs.bzl
# RUNTIME_TARGETS), and nothing else is configured for Darwin.
CROSS_ONLY_PLATFORMS = {'macos-arm64'}


class PlatformValue:
    def __init__(self, values):
        self.values = values


def platform_value(values):
    first = next(iter(values.values()))
    return first if all(value == first for value in values.values()) else PlatformValue(values)


def configured(node, platform):
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == 'cargo_platforms':
        return ast.literal_eval(node.args[0])[platform]
    return ast.literal_eval(node)


def calls(source):
    for statement in ast.parse(source).body:
        if isinstance(statement, ast.Expr) and isinstance(statement.value, ast.Call):
            call = statement.value
            if isinstance(call.func, ast.Name):
                yield statement, call, {k.arg: k.value for k in call.keywords}


def literal(call, field):
    return ast.literal_eval(next(k.value for k in call.keywords if k.arg == field))


def replace(source, statement, fields, render):
    """Keep source globs and unrelated attributes verbatim."""
    original_render = render

    def render(value):
        if isinstance(value, PlatformValue):
            return 'cargo_platforms(' + original_render(value.values) + ')'
        return 'None' if value is None else original_render(value)
    lines = source.splitlines(keepends=True)
    offsets = [0]
    for line in lines:
        offsets.append(offsets[-1] + len(line))
    begin, end = offsets[statement.lineno - 1], offsets[statement.end_lineno - 1] + statement.end_col_offset
    edits = []
    for keyword in statement.value.keywords:
        if keyword.arg in fields:
            node = keyword.value
            edits.append((offsets[node.lineno - 1] + node.col_offset - begin,
                          offsets[node.end_lineno - 1] + node.end_col_offset - begin,
                          render(fields[keyword.arg])))
            fields.pop(keyword.arg)
    text = source[begin:end]
    for start, stop, value in sorted(edits, reverse=True):
        text = text[:start] + value + text[stop:]
    if fields:
        text = text.rstrip()[:-1] + ''.join(f'    {key} = {render(value)},\n' for key, value in fields.items()) + ')'
    return text + '\n'


def relocate(value, package_ids, source, destination):
    """Translate workspace paths and Cargo's path package IDs in a JSON value."""
    if isinstance(value, dict):
        return {key: relocate(item, package_ids, source, destination) for key, item in value.items()}
    if isinstance(value, list):
        return [relocate(item, package_ids, source, destination) for item in value]
    if isinstance(value, str):
        if value in package_ids:
            return package_ids[value]
        if value == source or value.startswith(source + '/'):
            return destination + value[len(source):]
    return value


def config_inputs(paths):
    files = {}
    while paths:
        path = paths.pop()
        if path in files:
            continue
        contents = path.read_text() if path.is_file() else None
        files[path] = contents
        if contents is not None:
            for include in tomllib.loads(contents).get('include', []):
                name = include if isinstance(include, str) else include['path']
                paths.append(Path(os.path.abspath(path.parent / name)))
    return [[str(path), contents] for path, contents in sorted(files.items())]


def query_graphs(root, commands, env, cargo_home, user_config):
    worker_count = min(4, len(commands))
    if not worker_count:
        return []

    def query_batch(home, batch):
        target_dir = home / 'target'
        target_dir.mkdir(parents=True)
        home_prefix = f'{home}/'
        offline = rust_environment.private_cargo_environment(home, env, cargo_home, user_config)
        offline['RUSTC_BOOTSTRAP'] = '1'
        results = []
        for index, args in batch:
            command = [env['CARGO'], *args, '--manifest-path', str(root / 'Cargo.toml'),
                       '--target-dir', str(target_dir), '--locked', '--unit-graph',
                       '-Z', 'unstable-options']
            result = rust_environment.run_offline(command, cwd=root, env=offline,
                                                  label=f"Cargo unit graph ({' '.join(args)})")
            graph = json.loads(result.stdout)
            if graph['version'] != 1:
                raise ValueError('unsupported Cargo unit graph version')
            units = []
            for unit in graph['units']:
                projected = {field: unit[field] for field in
                             ('pkg_id', 'target', 'mode', 'platform', 'features', 'dependencies')}
                target = projected['target']
                source = target.get('src_path', '')
                if source.startswith(home_prefix):
                    # Cargo names registry sources through the worker's symlink.
                    target['src_path'] = str(cargo_home / source.removeprefix(home_prefix))
                units.append(projected)
            results.append((index, dict(roots=graph['roots'], units=units)))
        return results

    indexed = list(enumerate(commands))
    with tempfile.TemporaryDirectory(prefix='hirsel-unit-cargo-') as temporary:
        with ThreadPoolExecutor(max_workers=worker_count) as workers:
            batches = workers.map(query_batch, (Path(temporary) / str(i) for i in range(worker_count)),
                                  (indexed[i::worker_count] for i in range(worker_count)))
            return [graph for _, graph in sorted(item for batch in batches for item in batch)]


def unit_graphs(root, meta, platform_source, commands):
    cache_home = Path(os.environ.get('XDG_CACHE_HOME') or Path.home() / '.cache') / 'hirsel/cargo-units'
    env = rust_environment.environment(root)
    cargo_home = rust_environment.cargo_home(root, env)
    user_config = rust_environment.cargo_config(cargo_home)
    marker = '<workspace>'
    package_ids = {}
    manifests = {root / name for name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')}
    for package in meta['packages']:
        manifest = Path(package['manifest_path'])
        if manifest.is_relative_to(root):
            package_ids[package['id']] = 'path+workspace:' + manifest.relative_to(root).as_posix()
        if package.get('source') is None:
            manifests.add(manifest)

    def read_inputs():
        configs = [path for ancestor in (root, *root.parents)
                   if (path := rust_environment.cargo_config(ancestor / '.cargo')) is not None]
        if user_config and user_config not in configs:
            configs.append(user_config)
        return ([[str(path), hashlib.sha256(path.read_bytes()).hexdigest()] for path in sorted(manifests)],
                config_inputs(configs))

    files = read_inputs()
    overrides = {key: value for key, value in env.items()
                 if key.startswith(('CARGO_BUILD_', 'CARGO_TARGET_', 'CARGO_PROFILE_', 'CARGO_SOURCE_',
                                    'CARGO_REGISTRIES_', 'CARGO_RESOLVER_', 'RUSTC'))
                 or key in ('CARGO_ENCODED_RUSTFLAGS', 'RUSTFLAGS', 'RUSTUP_TOOLCHAIN')}
    normalized_files = [sorted(relocate(group, package_ids, str(root), marker)) for group in files]
    inputs = ['target-graphs-v3', platform_source, commands, meta, normalized_files,
              str(cargo_home), env['CARGO'], overrides]
    normalized = relocate(inputs, package_ids, str(root), marker)
    key = hashlib.sha256(json.dumps(normalized, sort_keys=True).encode()).hexdigest()
    cache = cache_home / f'{key}.json'
    restore_ids = {relative: original for original, relative in package_ids.items()}

    def read_cache():
        try:
            return relocate(json.loads(cache.read_text()), restore_ids, marker, str(root))
        except (FileNotFoundError, ValueError):
            return None

    cached = read_cache()
    if cached is not None:
        return cached
    cache_home.mkdir(parents=True, exist_ok=True)
    # A cold worktree waits only for a producer of exactly the same graphs.
    with cache.with_suffix('.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        cached = read_cache()
        if cached is not None:
            return cached
        graphs = query_graphs(root, commands, env, cargo_home, user_config)
        if read_inputs() != files:
            raise SystemExit('Cargo inputs changed during unit graph resolution. Retry sync.')
        payload = relocate(graphs, package_ids, str(root), marker)
        with tempfile.NamedTemporaryFile(mode='w', dir=cache_home, prefix=f'.{key}.', delete=False) as output:
            pending = Path(output.name)
            try:
                json.dump(payload, output, separators=(',', ':'))
                output.close()
                pending.replace(cache)
            finally:
                pending.unlink(missing_ok=True)
    return graphs


class Resolution:
    def __init__(self, root: Path, meta: dict):
        self.root = root
        self.packages = {p['id']: p for p in meta['packages']}
        self.members = set(meta['workspace_members'])
        self.units = {}
        self.names = {}
        self.preferred = {}
        self.test_binaries = {}
        platform_source = (root / 'third-party/reindeer.toml').read_text()
        self.platforms = tomllib.loads(platform_source)['platform']
        self.host_platform = next(name for name, config in self.platforms.items() if config.get('execution-platform'))
        builds, commands = [], []
        for member in sorted(self.members, key=lambda i: self.packages[i]['name']):
            name = self.packages[member]['name']
            modes = [(False, []), (True, [])]
            if name == 'hirsel-core-rt':
                # The unit harness includes Openat2 recording cases. Keep the
                # default archive separate and resolve the optional deps of
                # record and prof, which the runtime's variants enable.
                # The same harness exercises the runtime profiler's chunks.
                optional = ['--features', 'record,prof']
                modes = [(False, []), (True, optional), (False, optional)]
            for testing, feature_args in modes:
                builds.append((member, testing, feature_args))
                for config in self.platforms.values():
                    args = ['test' if testing else 'build', '-p', name, '--target', config['target']]
                    if testing:
                        args.append('--no-run')
                    commands.append(args + feature_args)
        resolved = iter(unit_graphs(root, meta, platform_source, commands))
        for member, testing, feature_args in builds:
            graphs = {platform: next(resolved) for platform in self.platforms}
            target_keys = self.add_platforms(graphs)
            graph = graphs[self.host_platform]
            keys = target_keys[self.host_platform]
            if feature_args and not testing:
                root_index, = graph['roots']
                self.record_runtime = keys[root_index]
                continue
            if testing:
                binaries = {u['target']['name']: keys[i] for i, u in enumerate(graph['units'])
                            if u['pkg_id'] == member and u['target']['kind'] == ['bin'] and u['mode'] == 'build'}
                for index in graph['roots']:
                    self.test_binaries[keys[index]] = binaries
            for index in graph['roots']:
                unit = graph['units'][index]
                if unit['pkg_id'] == member:
                    base = self.base(unit)
                    if base and (not testing or unit['mode'] == 'test' or unit['target']['kind'] == ['example']):
                        self.preferred.setdefault(base, keys[index])
            if not testing:
                for index, unit in enumerate(graph['units']):
                    if unit['pkg_id'] == member:
                        base = self.base(unit)
                        if base:
                            self.preferred.setdefault(base, keys[index])
        for key, unit in self.units.items():
            base = self.base(unit)
            if base:
                self.names[key] = base if self.preferred.get(base) == key else base + '-features-' + key[:12]
        if len(set(self.names.values())) != len(self.names):
            raise ValueError('Cargo unit label collision')

    def add(self, graph):
        keys = {}

        def intern(index):
            if index in keys:
                return keys[index]
            unit = graph['units'][index]
            deps = [(d['extern_crate_name'], intern(d['index'])) for d in unit['dependencies']]
            package = self.packages[unit['pkg_id']]
            identity = (package['name'], package['version'], unit['target']['name'], unit['target']['kind'],
                        unit['mode'], unit['features'], sorted(deps))
            key = hashlib.sha256(json.dumps(identity, sort_keys=True).encode()).hexdigest()
            keys[index] = key
            self.units[key] = dict(unit, edges=deps)
            return key

        for i in range(len(graph['units'])):
            intern(i)
        return keys

    def add_platforms(self, graphs):
        """Pair target units across configurations; host units keep Cargo's exact identities."""
        def target_unit(unit):
            return unit.get('platform') is not None and unit['mode'] != 'run-custom-build'

        def role(unit):
            return (unit['pkg_id'], unit['target']['name'], tuple(unit['target']['kind']), unit['mode'])

        projections, roles = {}, {}
        for platform, graph in graphs.items():
            projection = Resolution.__new__(Resolution)
            projection.packages, projection.units = self.packages, {}
            keys = projection.add(graph)
            projections[platform] = keys, projection.units
            for index, unit in enumerate(graph['units']):
                if target_unit(unit):
                    variants = roles.setdefault(role(unit), {})
                    if platform in variants:
                        raise ValueError(f'duplicate Cargo compilation context: {role(unit)} on {platform}')
                    variants[platform] = keys[index]
        def family(variants):
            # Even identical Rust edges can have different native fixups on
            # host and target. Do not let a host unit overwrite target variants.
            return hashlib.sha256(json.dumps(['target', sorted(variants.items())]).encode()).hexdigest()

        # A family is named by its variants on the platforms that build the
        # whole workspace. A cross-only platform's variant joins the family it
        # pairs with, so adding one renames no Linux target. Where two families
        # agree on every naming platform but not on a cross-only one, both are
        # named by all their variants; a family seen only cross-only is named
        # by that variant.
        named = {}
        for context, variants in roles.items():
            naming = {p: k for p, k in variants.items() if p not in CROSS_ONLY_PLATFORMS} or variants
            named.setdefault(family(naming), set()).add(family(variants))
        families = {}
        for context, variants in roles.items():
            naming = family({p: k for p, k in variants.items() if p not in CROSS_ONLY_PLATFORMS} or variants)
            families[context] = naming if len(named[naming]) == 1 else family(variants)
        result = {}
        for platform, graph in graphs.items():
            keys, units = projections[platform]
            result[platform] = {index: families[role(unit)] if target_unit(unit) else keys[index]
                                for index, unit in enumerate(graph['units'])}
            for index, unit in enumerate(graph['units']):
                key = result[platform][index]
                projected = dict(units[keys[index]], edges=[
                    (edge['extern_crate_name'], result[platform][edge['index']])
                    for edge in unit['dependencies']])
                if target_unit(unit):
                    family = self.units.setdefault(key, dict(projected, variants={}))
                    family.setdefault('variants', {})[platform] = projected
                else:
                    self.units[key] = projected
        return result

    def variant(self, key, platform=None):
        unit = self.units[key]
        return unit.get('variants', {}).get(platform, unit)

    def values(self, key, value):
        unit = self.units[key]
        return (platform_value({platform: value(variant) for platform, variant in unit['variants'].items()})
                if 'variants' in unit else value(unit))

    def base(self, unit):
        package = self.packages[unit['pkg_id']]
        name, target = package['name'], unit['target']
        kind, mode = target['kind'][0], unit['mode']
        if unit['pkg_id'] not in self.members:
            # Reindeer's exact base label is filled in from its manifest env.
            return None
        if kind == 'custom-build':
            return name + '-build-script' if mode == 'run-custom-build' and name not in ('hirsel-llvm', 'hirsel-llvm-lazy') else None
        if kind in ('lib', 'rlib', 'staticlib', 'proc-macro'):
            return name + ('-unittest' if mode == 'test' else '')
        if kind == 'bin':
            return name + '-bin-' + target['name'] + ('-unittest' if mode == 'test' else '')
        if kind in ('test', 'example'):
            return name + '-' + kind + '-' + target['name']
        return None

    def deps(self, key, platform=None):
        unit = self.variant(key, platform)
        for _, dep in unit['edges']:
            if dep not in self.names and self.units[dep]['target']['kind'] != ['custom-build']:
                package = self.packages[self.units[dep]['pkg_id']]
                raise ValueError(f"Reindeer has no Rust dependency template for {package['name']} {package['version']}")
        return [self.label(dep) for _, dep in unit['edges'] if dep in self.names
                and self.units[dep]['target']['kind'] != ['custom-build']]

    def label(self, key):
        prefix = '//:' if self.units[key]['pkg_id'] in self.members else '//third-party:'
        return prefix + self.names[key]

    def script(self, key, platform=None):
        return next((self.label(dep) for _, dep in self.variant(key, platform)['edges']
                     if dep in self.names and self.units[dep]['mode'] == 'run-custom-build'), None)

    def first_party(self, source, render):
        templates = {literal(call, 'name'): statement for statement, call, _ in calls(source)
                     if call.func.id in FIRST_PARTY}
        emitted = set()
        result = []
        for statement, call, attrs in calls(source):
            if call.func.id in FIRST_PARTY:
                base = literal(call, 'name')
                # Only feature variants of the same library inherit its Cargo deps.
                if base not in self.preferred:
                    package = ast.literal_eval(attrs['package_name']) if 'package_name' in attrs else None
                    key = self.preferred.get(package)
                    if (call.func.id == 'hirsel_library' and key is not None
                            and self.root / literal(call, 'crate_root') == Path(self.units[key]['target']['src_path'])):
                        if package == 'hirsel-core-rt' and {'record', 'prof'} & set(ast.literal_eval(attrs['features'])):
                            key = self.record_runtime
                        deps = self.values(key, lambda u: sorted(
                            self.label(dep) for _, dep in u['edges'] if dep in self.names
                            and self.units[dep]['target']['kind'] != ['custom-build']))
                        result.append([replace(source, statement, dict(deps=deps), render)])
                        continue
                    result.append(source.splitlines(keepends=True)[statement.lineno - 1:statement.end_lineno])
                    continue
                for key, name in sorted(self.names.items(), key=lambda pair: pair[1]):
                    unit = self.units[key]
                    if self.base(unit) != base or key in emitted:
                        continue
                    emitted.add(key)
                    deps = self.deps(key)
                    if call.func.id == 'hirsel_build_script':
                        compiled = self.units[unit['edges'][0][1]]
                        deps = [self.label(dep) for _, dep in compiled['edges'] if dep in self.names]
                    if self.packages[unit['pkg_id']]['name'] == 'hirsel-llvm':
                        deps.append('//crates/hirsel-llvm/bridge:bridge')
                    fields = dict(name=name, features=self.values(key, lambda u: u['features']),
                                  deps=self.values(key, lambda u: sorted(
                                      [self.label(dep) for _, dep in u['edges'] if dep in self.names
                                       and self.units[dep]['target']['kind'] != ['custom-build']]
                                      + (['//crates/hirsel-llvm/bridge:bridge']
                                         if self.packages[u['pkg_id']]['name'] == 'hirsel-llvm' else [])))
                                  if 'variants' in unit else sorted(deps))
                    if call.func.id == 'hirsel_build_script':
                        fields['compile_features'] = compiled['features']
                    if call.func.id != 'hirsel_build_script':
                        fields['build_script'] = (platform_value({p: self.script(key, p) for p in unit['variants']})
                                                  if 'variants' in unit else self.script(key))
                    if 'bin_exes' in attrs:
                        binaries = ast.literal_eval(attrs['bin_exes'])
                        fields['bin_exes'] = {binary: self.label(self.test_binaries[key][binary])
                                             if binary in self.test_binaries.get(key, {}) else label
                                             for binary, label in binaries.items()}
                    if name != base and 'doctests' in attrs:
                        fields['doctests'] = False
                    result.append([replace(source, statement, fields, render)])
            else:
                result.append(source.splitlines(keepends=True)[statement.lineno - 1:statement.end_lineno])
        # The only top-level statements are calls; keep the generated header.
        output = '# @generated by tools/buck2/sync.py from Cargo compilation units; do not edit.\n\n' + '\n'.join(''.join(part) for part in result)
        self.check_first_party(output, emitted)
        return output

    def check_first_party(self, source, emitted):
        """A package-local Cargo test graph is the independent feature oracle.

        Keep this in sync's regular gate, so a template or renderer change
        cannot silently restore workspace-wide features or dependency labels.
        """
        rules = {literal(call, 'name'): attrs for _, call, attrs in calls(source)
                 if call.func.id in FIRST_PARTY}
        for key in emitted:
            unit = self.units[key]
            attrs = rules[self.names[key]]
            for platform, variant in unit.get('variants', {None: unit}).items():
                if configured(attrs['features'], platform) != variant['features']:
                    raise ValueError(f'{self.names[key]}: features differ from package-local Cargo unit')
                if unit['mode'] != 'run-custom-build':
                    expected = sorted(self.deps(key, platform))
                    actual = sorted(dep for dep in configured(attrs['deps'], platform)
                                    if dep != '//crates/hirsel-llvm/bridge:bridge')
                    if actual != expected:
                        raise ValueError(f'{self.names[key]}: dependencies differ from package-local Cargo unit')

    def external_json(self):
        return json.dumps({key: name for key, name in self.names.items()
                           if self.units[key]['pkg_id'] not in self.members}, sort_keys=True) + '\n'

    def load_external(self, source):
        external = json.loads(source)
        for key, unit in self.units.items():
            if unit['pkg_id'] not in self.members and key in external:
                self.names[key] = external[key]

    def third_party(self, source):
        templates = {}
        bases = {}
        statements = list(calls(source))
        for statement, call, attrs in statements:
            if call.func.id == 'third_party_rust_library':
                env = ast.literal_eval(attrs['env'])
                package = env['CARGO_PKG_NAME'], env['CARGO_PKG_VERSION']
                bases[package] = literal(call, 'name')
        for statement, call, attrs in statements:
            if call.func.id in EXTERNAL:
                templates[literal(call, 'name')] = call
        for key, unit in self.units.items():
            if unit['pkg_id'] in self.members:
                continue
            package = self.packages[unit['pkg_id']]
            base = bases.get((package['name'], package['version']))
            if not base:
                continue  # dependency of a build script disabled by a Reindeer fixup
            if unit['target']['kind'] == ['custom-build']:
                base += '-build-script-run' if unit['mode'] == 'run-custom-build' else '-build-script-build'
            if base in templates:
                self.names[key] = base + '-features-' + key[:12]
        rust_names = set(templates)
        output = []
        for statement, call, attrs in statements:
            if call.func.id not in EXTERNAL and call.func.id != 'alias':
                output.append(ast.unparse(statement))
        for key, name in sorted(self.names.items(), key=lambda pair: pair[1]):
            unit = self.units[key]
            if unit['pkg_id'] in self.members:
                continue
            package = self.packages[unit['pkg_id']]
            base = bases[(package['name'], package['version'])]
            if unit['target']['kind'] == ['custom-build']:
                base += '-build-script-run' if unit['mode'] == 'run-custom-build' else '-build-script-build'
            template = templates[base]
            attrs = {k.arg: k.value for k in template.keywords}
            def mapping(node):
                if isinstance(node, ast.Call):
                    return {k.arg: k.value for k in node.keywords}
                return {ast.literal_eval(k): v for k, v in zip(node.keys, node.values)}
            platforms = mapping(attrs['platform']) if 'platform' in attrs else {}
            variants = unit.get('variants') or {
                platform: unit for platform in (getattr(self, 'platforms', {}) or platforms or {None: {}})}
            projected = {}
            for platform, variant in variants.items():
                fields = {field: copy.deepcopy(value) for field, value in attrs.items()
                          if field not in ('name', 'platform')}
                for field, value in mapping(platforms.get(platform, ast.Dict(keys=[], values=[]))).items():
                    value = copy.deepcopy(value)
                    if field in fields and isinstance(value, ast.List):
                        fields[field].elts.extend(value.elts)
                    elif field in fields and isinstance(value, ast.Dict):
                        combined = mapping(fields[field]) | mapping(value)
                        fields[field] = ast.Dict(keys=[ast.Constant(k) for k in combined], values=list(combined.values()))
                    else:
                        fields[field] = value
                references = {}
                for _, dep in variant['edges']:
                    if dep in self.names and self.units[dep]['target']['kind'] == ['custom-build']:
                        suffix = '-build-script-run' if self.units[dep]['mode'] == 'run-custom-build' else '-build-script-build'
                        references[':' + bases[(package['name'], package['version'])] + suffix] = ':' + self.names[dep]
                class Rewrite(ast.NodeTransformer):
                    def visit_Constant(self, node):
                        if isinstance(node.value, str):
                            for old, new in references.items():
                                node.value = node.value.replace(old + '[', new + '[') if old + '[' in node.value else (new if node.value == old else node.value)
                        return node
                fields = {field: Rewrite().visit(value) for field, value in fields.items()}
                if 'deps' in fields:
                    # Native fixups remain; Cargo supplies every Rust edge.
                    fields['deps'].elts = [d for d in fields['deps'].elts if not isinstance(d, ast.Constant)
                                           or not isinstance(d.value, str) or d.value.removeprefix(':') not in rust_names]
                fields['features'] = ast.parse(repr(variant['features']), mode='eval').body
                fields['visibility'] = ast.parse(repr(['PUBLIC']), mode='eval').body
                self.deps(key, platform)
                if variant['mode'] != 'run-custom-build':
                    named = {extern: self.label(dep) for extern, dep in variant['edges']
                             if dep in self.names and self.units[dep]['target']['kind'] != ['custom-build']}
                    fields['named_deps'] = ast.parse(repr(named), mode='eval').body
                projected[platform] = fields
            fields, conditional = {}, {platform: {} for platform in projected if platform is not None}
            for field in sorted({field for attrs in projected.values() for field in attrs}):
                values = [attrs.get(field) for attrs in projected.values()]
                if all(value is not None and ast.dump(value) == ast.dump(values[0]) for value in values):
                    fields[field] = values[0]
                else:
                    for platform, attrs in projected.items():
                        if field in attrs:
                            conditional[platform][field] = attrs[field]
            fields['name'] = ast.Constant(name)
            if conditional:
                fields['platform'] = ast.Dict(
                    keys=[ast.Constant(platform) for platform in conditional],
                    values=[ast.Dict(keys=[ast.Constant(k) for k in values], values=list(values.values()))
                            for values in conditional.values()])
            call = ast.Call(func=copy.deepcopy(template.func), args=[],
                            keywords=[ast.keyword(arg=field, value=value) for field, value in fields.items()])
            output.append(ast.unparse(ast.fix_missing_locations(call)))
        # Stable aliases for hand-written tools; generated first-party edges
        # always name their exact Cargo unit instead. Hand-written users cannot
        # declare Cargo features, so prefer each alias's feature-rich units and
        # then align shared transitive dependencies with aliases already chosen.
        aliases = []
        for position, (_, alias, attrs) in enumerate(statements):
            if alias.func.id != 'alias':
                continue
            base = ast.literal_eval(attrs['actual']).removeprefix(':')
            candidates = [key for key in self.names if self.names[key].startswith(base + '-features-')]
            if candidates:
                aliases.append((position, attrs, base, candidates))

        def unit_base(key):
            return self.names[key].rsplit('-features-', 1)[0]

        closures = {}
        def closure(key):
            if key in closures:
                return closures[key]
            found = {}
            for _, dep in self.units[key]['edges']:
                if dep not in self.names or self.units[dep]['target']['kind'] == ['custom-build']:
                    continue
                found[unit_base(dep)] = dep
                for nested, nested_key in closure(dep).items():
                    found.setdefault(nested, nested_key)
            closures[key] = found
            return found

        def stable_edges(key):
            variants = self.units[key].get('variants') or {None: self.units[key]}
            shapes = {tuple(sorted(variant['edges'])) for variant in variants.values()}
            return len(shapes) == 1

        def feature_count(key):
            return len(set(self.units[key]['features']))

        # Feature-rich aliases tend to own the trait-defining shared crates. Let
        # them establish the dependency identities that later aliases prefer.
        aliases.sort(key=lambda item: (-max(feature_count(key) for key in item[3]), item[2]))
        canonical, selected = {}, {}
        for _, _, base, candidates in aliases:
            def score(key):
                edges = closure(key)
                compatible = sum(edges[name] == canonical[name] for name in edges.keys() & canonical.keys())
                conflicts = sum(edges[name] != canonical[name] for name in edges.keys() & canonical.keys())
                return feature_count(key), stable_edges(key), compatible, -conflicts, key
            chosen = max(candidates, key=score)
            selected[base] = chosen
            for name, dep in closure(chosen).items():
                canonical.setdefault(name, dep)

        for _, attrs, base, _ in sorted(aliases):
            name = ast.literal_eval(attrs['name'])
            actual = ':' + self.names[selected[base]]
            output.append(f'alias(name={name!r}, actual={actual!r}, visibility=["PUBLIC"])')
        return '# @generated by tools/buck2/sync.py from Cargo compilation units and Reindeer.\n\n' + '\n\n'.join(output) + '\n'
