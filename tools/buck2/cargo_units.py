"""Project Cargo's resolved compilation units into the existing Buck rule templates.

Cargo owns feature propagation, including resolver-v3 host and dev contexts.
Cargo documents unit graph v1 at doc.rust-lang.org/cargo/reference/unstable.html#unit-graph.
Reindeer still owns third-party sources, platform fixups and native libraries.
Only features, Rust dependency edges and their identities are replaced here.
"""
from __future__ import annotations

import ast
import copy
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

import rust_environment


FIRST_PARTY = {'hirsel_library', 'hirsel_binary', 'hirsel_test', 'hirsel_build_script'}
EXTERNAL = {'third_party_rust_library', 'third_party_rust_binary', 'third_party_buildscript_run'}


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


class Resolution:
    def __init__(self, root: Path, meta: dict):
        self.root = root
        self.packages = {p['id']: p for p in meta['packages']}
        self.members = set(meta['workspace_members'])
        self.units = {}
        self.names = {}
        self.preferred = {}
        self.runtime = {}
        self.test_binaries = {}
        fingerprint = hashlib.sha256(json.dumps([meta['packages'], meta['workspace_members'], (root / 'Cargo.toml').read_text(), (root / 'rust-toolchain.toml').read_text()], sort_keys=True).encode()).hexdigest()
        self.cache_path = root / '.buck2/cargo-units.json'
        try:
            cached = json.loads(self.cache_path.read_text())
        except (OSError, ValueError):
            cached = {}
        self.graphs = cached.get('graphs', {}) if cached.get('fingerprint') == fingerprint else {}
        env = rust_environment.environment(root)
        # --unit-graph is unstable in the pinned Cargo; it executes no compiler.
        env['RUSTC_BOOTSTRAP'] = '1'
        for member in sorted(self.members, key=lambda i: self.packages[i]['name']):
            package = self.packages[member]
            for testing in (False, True):
                command = [env['CARGO'], 'test' if testing else 'build', '-p', package['name'],
                           '--manifest-path', str(root / 'Cargo.toml'), '--locked',
                           '--unit-graph', '-Z', 'unstable-options']
                if testing:
                    command.append('--no-run')
                graph = self.graph(command, env)
                if graph['version'] != 1:
                    raise ValueError('unsupported Cargo unit graph version')
                keys = self.add(graph)
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
                    if package['name'] == 'hirsel-rt':
                        self.runtime['x86_64-unknown-linux-gnu'] = next(keys[i] for i in graph['roots']
                                                                                    if graph['units'][i]['target']['kind'] != ['custom-build'])
        # Runtime archives are cross compiled; Cargo must resolve their target
        # dependency edges separately from the build-script host dependencies.
        graph = self.graph([env['CARGO'], 'build', '-p', 'hirsel-rt', '--locked',
                            '--manifest-path', str(root / 'Cargo.toml'), '--target', 'aarch64-unknown-linux-gnu',
                            '--unit-graph', '-Z', 'unstable-options'], env)
        keys = self.add(graph)
        self.runtime['aarch64-unknown-linux-gnu'] = next(keys[i] for i in graph['roots'])
        for key, unit in self.units.items():
            base = self.base(unit)
            if base:
                self.names[key] = base if self.preferred.get(base) == key else base + '-features-' + key[:12]
        self.cache_path.parent.mkdir(parents=True, exist_ok=True)
        # Cache only Cargo's answer. A manifest or target-layout change changes
        # the metadata fingerprint, and bypasses this cache completely.
        with tempfile.NamedTemporaryFile(mode='w', dir=self.cache_path.parent, delete=False) as pending:
            json.dump(dict(fingerprint=fingerprint, graphs=self.graphs), pending, separators=(',', ':'))
        Path(pending.name).replace(self.cache_path)
        if len(set(self.names.values())) != len(self.names):
            raise ValueError('Cargo unit label collision')

    def graph(self, command, env):
        key = json.dumps(command)
        if key not in self.graphs:
            graph = json.loads(subprocess.check_output(command, cwd=self.root, env=env))
            self.graphs[key] = dict(version=graph['version'], roots=graph['roots'], units=[
                {field: unit[field] for field in ('pkg_id', 'target', 'mode', 'features', 'dependencies')}
                for unit in graph['units']])
        return self.graphs[key]

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

    def base(self, unit):
        package = self.packages[unit['pkg_id']]
        name, target = package['name'], unit['target']
        kind, mode = target['kind'][0], unit['mode']
        if unit['pkg_id'] not in self.members:
            # Reindeer's exact base label is filled in from its manifest env.
            return None
        if kind == 'custom-build':
            return name + '-build-script' if mode == 'run-custom-build' and name != 'hirsel-llvm' else None
        if kind in ('lib', 'rlib', 'staticlib', 'proc-macro'):
            return name + ('-unittest' if mode == 'test' else '')
        if kind == 'bin':
            return name + '-bin-' + target['name'] + ('-unittest' if mode == 'test' else '')
        if kind in ('test', 'example'):
            return name + '-' + kind + '-' + target['name']
        return None

    def deps(self, key):
        for _, dep in self.units[key]['edges']:
            if dep not in self.names and self.units[dep]['target']['kind'] != ['custom-build']:
                package = self.packages[self.units[dep]['pkg_id']]
                raise ValueError(f"Reindeer has no Rust dependency template for {package['name']} {package['version']}")
        return [self.label(dep) for _, dep in self.units[key]['edges'] if dep in self.names
                and self.units[dep]['target']['kind'] != ['custom-build']]

    def label(self, key):
        prefix = '//:' if self.units[key]['pkg_id'] in self.members else '//third-party:'
        return prefix + self.names[key]

    def script(self, key):
        return next((self.label(dep) for _, dep in self.units[key]['edges']
                     if dep in self.names and self.units[dep]['mode'] == 'run-custom-build'), None)

    def first_party(self, source, render):
        templates = {literal(call, 'name'): statement for statement, call, _ in calls(source)
                     if call.func.id in FIRST_PARTY}
        emitted = set()
        result = []
        for statement, call, attrs in calls(source):
            if call.func.id in FIRST_PARTY:
                base = literal(call, 'name')
                # Manually declared tools and harnesses have no Cargo unit.
                if base not in self.preferred:
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
                    fields = dict(name=name, features=unit['features'], deps=sorted(deps))
                    if call.func.id != 'hirsel_build_script':
                        fields['build_script'] = self.script(key)
                    if 'bin_exes' in attrs:
                        binaries = ast.literal_eval(attrs['bin_exes'])
                        fields['bin_exes'] = {binary: self.label(self.test_binaries[key][binary])
                                             if binary in self.test_binaries.get(key, {}) else label
                                             for binary, label in binaries.items()}
                    if name != base and 'doctests' in attrs:
                        fields['doctests'] = False
                    result.append([replace(source, statement, fields, render)])
            elif call.func.id == 'hirsel_runtime_archives':
                fields = {'deps': sorted(self.deps(self.runtime['x86_64-unknown-linux-gnu'])),
                          'target_deps': {triple: sorted(self.deps(key)) for triple, key in self.runtime.items()}}
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
            if ast.literal_eval(attrs['features']) != unit['features']:
                raise ValueError(f'{self.names[key]}: features differ from package-local Cargo unit')
            if unit['mode'] != 'run-custom-build':
                expected = sorted(self.deps(key))
                actual = sorted(dep for dep in ast.literal_eval(attrs['deps'])
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
            call = copy.deepcopy(templates[base])
            # References to the owning package's build script are not Rust
            # externs. Keep them, but point to this context's resolved script.
            references = {}
            for _, dep in unit['edges']:
                if dep in self.names and self.units[dep]['target']['kind'] == ['custom-build']:
                    suffix = '-build-script-run' if self.units[dep]['mode'] == 'run-custom-build' else '-build-script-build'
                    references[':' + bases[(package['name'], package['version'])] + suffix] = ':' + self.names[dep]
            class Rewrite(ast.NodeTransformer):
                def visit_Constant(self, node):
                    if isinstance(node.value, str):
                        for old, new in references.items():
                            node.value = node.value.replace(old + '[', new + '[') if old + '[' in node.value else (new if node.value == old else node.value)
                    return node

                def visit_keyword(self, node):
                    if node.arg == 'features':
                        node.value = ast.List(elts=[], ctx=ast.Load())
                    if node.arg == 'named_deps':
                        node.value = ast.Dict(keys=[], values=[])
                    if node.arg == 'deps':
                        # Retain native fixup dependencies. Cargo supplies Rust edges.
                        node.value.elts = [d for d in node.value.elts if not isinstance(d, ast.Constant)
                                           or not isinstance(d.value, str) or d.value.removeprefix(':') not in rust_names]
                    return self.generic_visit(node)
            call = Rewrite().visit(call)
            fields = {k.arg: k for k in call.keywords}
            values = dict(name=name, features=unit['features'], visibility=['PUBLIC'])
            self.deps(key)  # refuse missing active Rust dependency templates
            if unit['mode'] != 'run-custom-build':
                values['named_deps'] = {extern: self.label(dep) for extern, dep in unit['edges']
                                       if dep in self.names and self.units[dep]['target']['kind'] != ['custom-build']}
            for field, value in values.items():
                expr = ast.parse(repr(value), mode='eval').body
                if field in fields:
                    fields[field].value = expr
                else:
                    call.keywords.append(ast.keyword(arg=field, value=expr))
            output.append(ast.unparse(ast.fix_missing_locations(call)))
        # Stable aliases for hand-written tools; generated first-party edges
        # always name their exact Cargo unit instead.
        for _, alias, attrs in statements:
            if alias.func.id != 'alias':
                continue
            base = ast.literal_eval(attrs['actual']).removeprefix(':')
            candidates = [key for key in self.names if self.names[key].startswith(base + '-features-')]
            if candidates:
                output.append(f'alias(name={ast.literal_eval(attrs["name"])!r}, actual={":" + self.names[sorted(candidates)[0]]!r}, visibility=["PUBLIC"])')
        return '# @generated by tools/buck2/sync.py from Cargo compilation units and Reindeer.\n\n' + '\n\n'.join(output) + '\n'
