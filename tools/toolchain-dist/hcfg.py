"""Read checked Hirsel data through the installed driver or the bootstrap exporter."""
from functools import cache
import json
import os
from pathlib import Path
import subprocess
import sys


@cache
def host_exporter():
    if provided := os.environ.get('HIRSEL_CONFIG_EXPORTER'):
        return Path(provided).resolve()
    # This host binary depends only on the syntax/data crates and their schema
    # declarations. In particular it can run before sync has emitted any BUCK file.
    root = Path(__file__).resolve().parents[2]
    sys.path.insert(0, str(root / 'tools/buck2'))
    import rust_environment
    env = rust_environment.environment(root)
    command = [env['CARGO'], 'build', '--quiet', '--offline', '--locked', '--manifest-path',
               str(root / 'crates/hirsel-config-export/Cargo.toml'), '--bin', 'hirsel-config-export',
               '--message-format=json-render-diagnostics']
    built = subprocess.check_output(command, cwd=root, env=env, text=True)
    return Path(next(msg['executable'] for line in built.splitlines()
                     if (msg := json.loads(line)).get('executable')
                     and msg['target']['name'] == 'hirsel-config-export'))


def project(path, root='--auto', compiler=None, *, input_bytes=None):
    command = ([compiler, 'config', 'export', path, '--json'] if compiler else
               [host_exporter(), root, path])
    data = json.loads(subprocess.check_output(command, input=input_bytes))
    if data['version'] != 1 or (root != '--auto' and data['root'] != root):
        raise ValueError(f'{path}: expected version 1 {root}')
    return data


def read(path, root, compiler=None):
    return project(path, root, compiler)['value']


def option(value):
    return value['fields'][0] if value['case'] == 'Some' else None


def product(path, compiler=None):
    data = read(path, 'product_v1.Product', compiler)
    for field in ('hosts', 'runtime_targets', 'runtime_slots', 'helpers'):
        data[field] = dict(data[field])
    for group, field in [('hosts', 'llvm_engine_dir'), ('runtime_targets', 'slots'),
                         ('runtime_slots', 'crate_name'), ('helpers', 'package')]:
        for row in data[group].values():
            value = option(row.pop(field))
            if value is not None:
                row[field] = value
    return data

def vector_package(package, vectors, destination, directory):
    text = subprocess.check_output([host_exporter(), '--vector-package',
                                    package / 'hirsel.hcfg', vectors / 'test.hcfg', directory.resolve()])
    (destination / 'hirsel.hcfg').write_bytes(text)


def workspace(directory, members):
    (directory / 'hirsel.hcfg').write_text(
        'pkg_v1.Workspace(members: ' + json.dumps(list(members), ensure_ascii=False) + ')\n')


def package_files(directory):
    package = read(directory / 'hirsel.hcfg', 'pkg_v1.Package')
    yield 'hirsel.hcfg'
    yield from dict(package['sources'])
    if tests := option(package['tests']):
        yield from dict(tests['sources'])
    for _, module in option(package['foreign']) or []:
        fields = module['fields']
        if module['case'] == 'Native':
            yield from (archive['file'] for archive in fields['archives'])
        else:
            yield fields['file']


def package(directory, compiler=None):
    """Index the checked fields used when assembling compiler inputs."""
    data = read(directory / 'hirsel.hcfg', 'pkg_v1.Package', compiler)
    data['sources'] = dict(data['sources'])
    data['dependencies'] = {alias: dep['fields'] for alias, dep in data['dependencies']}
    tests = option(data['tests']) or {}
    tests['sources'] = dict(tests.get('sources', []))
    data['tests'] = tests
    data['program'] = {key: option(value) for key, value in (option(data['program']) or {}).items()}
    return data
